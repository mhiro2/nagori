use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use nagori_core::{
    AppError, AppSettings, AuditLog, ClipboardContent, ClipboardSequence, EntryFactory, EntryId,
    EntryRepository, Result, SecretAction, Sensitivity, SensitivityClassifier,
};
use nagori_platform::{CapturedSnapshot, ClipboardReader, WindowBehavior};
use tokio::sync::watch;
use tracing::{info, warn};

use crate::search_cache::{SharedSearchCache, lock_or_recover};

/// Minimum gap between two consecutive warnings of the same error kind.
///
/// The OS-level clipboard read can fail repeatedly (e.g. after a
/// permission revocation) at the polling cadence, which would flood the
/// log if we warned on every tick. One warn per minute is enough to
/// make the failure visible without burying everything else. The
/// suppression is now keyed on the error variant so a sudden second
/// failure mode (e.g. AX permission drop on top of a pasteboard read
/// failure) still surfaces immediately instead of being shadowed by an
/// in-flight platform suppression.
const ERROR_WARN_INTERVAL: Duration = Duration::from_mins(1);

/// Number of consecutive `capture_once` failures after which the
/// polling loop starts pacing itself with an exponential backoff.
///
/// Below this threshold a transient hiccup just retries on the next
/// tick (the loop's normal cadence is short enough that one missed
/// poll is invisible to the user). Above it we treat the failure as
/// persistent — a permission revocation, a wedged `AppKit`, a corrupted
/// DB write — and stretch the inter-tick wait to avoid flooding logs
/// and burning CPU on something that isn't going to recover this
/// second.
const BACKOFF_AFTER_CONSECUTIVE_FAILURES: u32 = 3;

/// Cap for the exponential backoff applied to the capture loop's
/// inter-tick sleep when failures persist.
///
/// 30 seconds keeps the loop responsive once the underlying problem
/// (permission re-grant, transient FFI failure) clears, while still
/// giving long-running outages enough headroom that the daemon isn't
/// hammering the OS clipboard subsystem several times a second.
const MAX_BACKOFF: Duration = Duration::from_secs(30);

/// Discriminator for `note_capture_error` rate-limiting buckets.
///
/// Suppression is per-kind so a sudden second failure mode surfaces
/// immediately even if the existing suppression interval for another
/// kind hasn't elapsed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CaptureErrorKind {
    Platform,
    Other,
}

impl CaptureErrorKind {
    const fn from_error(err: &AppError) -> Self {
        match err {
            AppError::Platform(_) => Self::Platform,
            _ => Self::Other,
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Platform => "platform",
            Self::Other => "other",
        }
    }
}

/// Number of consecutive `frontmost_focused_is_secure` failures after
/// which we flip from fail-open (`unwrap_or(false)`) to fail-closed
/// (treat focus as secure and skip capture).
///
/// One transient AX error is normal — accessibility queries can fail
/// during app switches, sleep/wake transitions, or briefly after a
/// permission grant. A sustained run of failures, however, means we've
/// genuinely lost visibility into whether the user is typing into a
/// password field, and the safer default is to refuse to capture rather
/// than silently letting password keystrokes through. 3 picks up
/// "permission revoked" / "AX subsystem stuck" without triggering on a
/// single hiccup.
const SECURE_FOCUS_FAIL_CLOSED_THRESHOLD: u32 = 3;

/// Bundle identifiers of system password / authentication UIs.
///
/// These windows host secure text fields whose state isn't always
/// reachable through the public AX API (the OS deliberately scrubs them
/// to defeat keyloggers). Treating them as secure regardless of what
/// `frontmost_focused_is_secure` returns means we don't have to trust
/// AX visibility for the cases that matter most.
const SECURE_FOCUS_BUNDLE_OVERRIDES: &[&str] = &[
    "com.apple.SecurityAgent",
    "com.apple.LocalAuthentication.UIService",
    "com.apple.loginwindow",
];

/// Inter-tick wall-clock gap that we treat as "the host paused" (sleep,
/// suspend, lid close, container freeze). On macOS the pasteboard
/// `changeCount` can lap silently across a sleep cycle, so a post-wake clip
/// whose sequence happens to collide with the pre-sleep value would be
/// skipped as a duplicate. Above this gap we cross-check the next read
/// against the last captured content hash before trusting the sequence
/// dedup. We deliberately use `SystemTime` (wall clock) rather than
/// `Instant`: Rust's `Instant` on Darwin is `CLOCK_UPTIME_RAW` and does
/// **not** advance while the system is asleep, so a monotonic-clock
/// heuristic would never see a sleep gap on the very platform we care
/// about. `SystemTime` jitters under NTP and is theoretically vulnerable
/// to manual clock changes, but the false-positive cost is just one
/// extra body read and content-hash comparison. The 30-second threshold
/// sits well above any normal scheduling jitter at the default 500 ms
/// cadence (60x headroom) yet small enough to catch even short naps.
const RESYNC_GAP_THRESHOLD: Duration = Duration::from_secs(30);

pub struct CaptureLoop<R, E, A> {
    reader: R,
    entries: E,
    audit: A,
    settings: AppSettings,
    last_sequence: Option<ClipboardSequence>,
    window: Option<Arc<dyn WindowBehavior>>,
    /// Per-kind warn suppression. Earlier we kept a single
    /// `last_platform_warn_at` and any non-platform error logged
    /// unconditionally; the consequence was that two distinct platform
    /// failures within the suppression window collapsed to one log line
    /// and AX-permission losses on top of pasteboard outages were
    /// effectively invisible.
    last_warn_at: [Option<Instant>; 2],
    /// Counter of suppressed warnings since the last emitted log line,
    /// reset on every emit. Surfaced as a tracing field so suppressed
    /// runs are still observable (the original cadence dropped them
    /// silently).
    suppressed_warns: [u32; 2],
    /// Number of consecutive `capture_once` failures (any kind) that
    /// we've observed. Drives the exponential backoff in
    /// `run_polling[_with_settings]` and resets to zero on the next
    /// successful tick.
    consecutive_failures: u32,
    /// Number of consecutive `frontmost_focused_is_secure` errors. Once
    /// this crosses `SECURE_FOCUS_FAIL_CLOSED_THRESHOLD` the loop flips
    /// to fail-closed (assume the focus is secure) so a sustained AX
    /// outage can't silently let password keystrokes through. Reset on
    /// the next successful AX query.
    consecutive_secure_ax_failures: u32,
    search_cache: Option<SharedSearchCache>,
    /// `true` until the loop has observed and acted on its first sequence.
    /// When `capture_initial_clipboard_on_launch` is `false`, the first
    /// observed sequence is recorded as `last_sequence` and the body read is
    /// skipped, so whatever was already on the pasteboard at startup never
    /// reaches storage.
    pristine: bool,
    /// Wall-clock anchor for the previous `capture_once` invocation. Used to
    /// spot host-paused gaps (sleep / suspend) and resync the dedup baseline.
    /// `SystemTime` rather than `Instant` because Darwin's `Instant` is
    /// `CLOCK_UPTIME_RAW` and freezes during sleep — see the
    /// `RESYNC_GAP_THRESHOLD` doc comment for details.
    last_tick_at: Option<SystemTime>,
    /// Content hash of the most recent snapshot we observed (captured or
    /// otherwise). Used to confirm a post-resync sequence collision is a
    /// genuine duplicate before re-inserting the same content.
    last_content_hash: Option<String>,
    /// One-shot flag that survives across one tick boundary. When set, the
    /// next `capture_once` invocation bypasses the cheap sequence-based
    /// dedup short-circuit and instead reads the body so the content hash
    /// can be compared against `last_content_hash`. We set this on a
    /// detected wake gap to defend against a potentially lapped pasteboard
    /// `changeCount`.
    force_content_check: bool,
}

impl<R, E, A> CaptureLoop<R, E, A>
where
    R: ClipboardReader,
    E: EntryRepository,
    A: AuditLog,
{
    pub const fn new(reader: R, entries: E, audit: A, settings: AppSettings) -> Self {
        Self {
            reader,
            entries,
            audit,
            settings,
            last_sequence: None,
            window: None,
            last_warn_at: [None, None],
            suppressed_warns: [0, 0],
            consecutive_failures: 0,
            consecutive_secure_ax_failures: 0,
            search_cache: None,
            pristine: true,
            last_tick_at: None,
            last_content_hash: None,
            force_content_check: false,
        }
    }

    /// Reset the dedup baseline so the next observed sequence is treated as
    /// fresh content. Useful after macOS sleep/wake when the pasteboard
    /// counter can lap silently and we'd otherwise skip a real change as a
    /// duplicate.
    pub fn reset_sequence_baseline(&mut self) {
        self.last_sequence = None;
    }

    fn note_capture_error(&mut self, err: &AppError) {
        // Track persistent failure for the polling-loop backoff. Any
        // tick that reaches `note_capture_error` has, by definition,
        // failed; the counter is reset only on a successful
        // `capture_once`.
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);

        let kind = CaptureErrorKind::from_error(err);
        let slot = kind as usize;
        let now = Instant::now();
        let should_warn = self.last_warn_at[slot]
            .is_none_or(|prev| now.duration_since(prev) >= ERROR_WARN_INTERVAL);
        if should_warn {
            let suppressed = std::mem::take(&mut self.suppressed_warns[slot]);
            warn!(
                error = %err,
                kind = kind.label(),
                consecutive_failures = self.consecutive_failures,
                suppressed_since_last_warn = suppressed,
                "capture_failed",
            );
            self.last_warn_at[slot] = Some(now);
        } else {
            self.suppressed_warns[slot] = self.suppressed_warns[slot].saturating_add(1);
        }
    }

    const fn note_capture_success(&mut self) {
        self.consecutive_failures = 0;
    }

    /// Compute the inter-tick sleep applied after a failed
    /// `capture_once`. Below `BACKOFF_AFTER_CONSECUTIVE_FAILURES` we
    /// keep the user-configured cadence; above it we apply an
    /// exponential backoff capped at `MAX_BACKOFF`.
    fn backoff_for_failures(base: Duration, consecutive_failures: u32) -> Duration {
        if consecutive_failures < BACKOFF_AFTER_CONSECUTIVE_FAILURES {
            return base;
        }
        // `consecutive_failures` is at least the threshold here; the
        // first overshoot doubles the base sleep, the next quadruples
        // it, and so on, until the cap is reached.
        let overshoot = consecutive_failures - BACKOFF_AFTER_CONSECUTIVE_FAILURES + 1;
        let shift = overshoot.min(20); // 2^20 * base ≫ MAX_BACKOFF; clamp before the multiply.
        let multiplier = 1u64 << shift;
        let scaled = base.saturating_mul(u32::try_from(multiplier).unwrap_or(u32::MAX));
        scaled.min(MAX_BACKOFF)
    }

    #[must_use]
    pub fn with_window(mut self, window: Arc<dyn WindowBehavior>) -> Self {
        self.window = Some(window);
        self
    }

    /// Wire a [`SharedSearchCache`] so successful captures invalidate stale
    /// hits in front of `SearchService`. Without this, the runtime cache
    /// would keep serving pre-capture results until another mutation
    /// (delete / pin / clear) eventually flushed it.
    #[must_use]
    pub fn with_search_cache(mut self, cache: SharedSearchCache) -> Self {
        self.search_cache = Some(cache);
        self
    }

    pub fn update_settings(&mut self, settings: AppSettings) {
        self.settings = settings;
    }

    pub async fn capture_once(&mut self) -> Result<Option<EntryId>> {
        self.capture_once_at(SystemTime::now()).await
    }

    /// Test seam for `capture_once` that lets the caller pin the wall-clock
    /// "now" used for gap detection. Production callers should use
    /// `capture_once`; tests use this to simulate sleep gaps without driving
    /// real time.
    #[allow(clippy::too_many_lines)]
    pub async fn capture_once_at(&mut self, now: SystemTime) -> Result<Option<EntryId>> {
        // Detect a host-paused gap (sleep / suspend / lid close). We do not
        // clear `last_sequence` here — clearing the baseline outright would
        // re-capture an unchanged pre-launch clipboard once
        // `capture_initial_clipboard_on_launch=false` had already discarded
        // it. Instead, arm a one-shot `force_content_check` flag that makes
        // the next tick's dedup decision content-aware.
        if let Some(prev) = self.last_tick_at {
            // `duration_since` is `Err` if the wall clock was rolled back
            // (NTP step backwards, manual change). Treat that as zero gap
            // rather than a wake signal — the user changing their clock is
            // not a sleep cycle.
            let gap = now.duration_since(prev).unwrap_or(Duration::ZERO);
            if gap >= RESYNC_GAP_THRESHOLD {
                info!(gap_secs = gap.as_secs(), "capture_loop_resync_after_gap");
                self.force_content_check = true;
            }
        }
        self.last_tick_at = Some(now);

        if !self.settings.capture_enabled {
            return Ok(None);
        }

        // Detect the change cheaply via the pasteboard sequence first, then
        // capture the frontmost app *before* we incur the cost of reading
        // the clipboard body. On macOS `arboard::Clipboard::get_text` does
        // several pasteboard round-trips; if the user is fast enough to
        // switch apps between copy and our read, the frontmost we'd capture
        // afterwards would be the destination app rather than the source.
        // This lets the password-manager / app-denylist rules in
        // `SensitivityClassifier` actually fire for things like 1Password
        // ⌘C → ⌘Tab → paste flows.
        let sequence = self.reader.current_sequence().await?;
        // Peek without consuming. We only clear `force_content_check` after
        // the body read succeeds — otherwise a transient `current_snapshot`
        // failure between the gap-detection tick and the actual recheck
        // would drop the flag, and the next tick would dedup-skip the
        // colliding sequence again. Re-trying with the flag still set is
        // safe because the body-read path is idempotent.
        let force_content_check = self.force_content_check;
        if !force_content_check && self.last_sequence.as_ref() == Some(&sequence) {
            return Ok(None);
        }
        // Honour the "skip whatever was on the clipboard before launch" flag
        // by anchoring `last_sequence` (and `last_content_hash`, so a future
        // wake-resync can recognise the unchanged pre-launch content) on the
        // first observation. Without the hash anchor here, a sleep gap
        // entered before any user copy would force a body read on the next
        // tick and re-introduce the pre-launch clipboard. Flip `pristine`
        // last — only after the snapshot read succeeds — so a transient
        // platform error keeps us in the pristine state and we retry on
        // the next tick instead of stranding the loop with no baseline.
        if self.pristine && !self.settings.capture_initial_clipboard_on_launch {
            let snapshot = self.reader.current_snapshot().await?;
            self.last_sequence = Some(snapshot.sequence.clone());
            if let Some(entry) = EntryFactory::from_snapshot(snapshot) {
                self.last_content_hash = Some(entry.metadata.content_hash.value);
            }
            self.pristine = false;
            return Ok(None);
        }
        // Run both AX queries concurrently — each spawns its own
        // system-wide AX walk via spawn_blocking, so the wall-clock
        // cost is parallel rather than additive on the per-tick hot
        // path.
        //
        // A *single* error from `frontmost_focused_is_secure` degrades
        // to `false` so a transient FFI hiccup or in-flight permission
        // grant doesn't strand the capture loop; the
        // `SensitivityClassifier` secret detector and password-manager
        // bundle denylist still run downstream as the second line of
        // defence. But a *sustained* run of AX failures means we've
        // genuinely lost visibility, and the safer default at that
        // point is to fail closed and skip the next clip — see
        // `SECURE_FOCUS_FAIL_CLOSED_THRESHOLD`. Likewise, a frontmost
        // bundle id matching `SECURE_FOCUS_BUNDLE_OVERRIDES` (system
        // password UIs) forces secure regardless of the AX result, so
        // we don't depend on AX accurately reporting on windows whose
        // entire purpose is to defeat keyloggers.
        let (frontmost_source, secure_focus) = if let Some(window) = &self.window {
            let (front_res, secure_res) =
                tokio::join!(window.frontmost_app(), window.frontmost_focused_is_secure(),);
            let source = front_res.ok().flatten().map(|front| front.source);
            let bundle_override = source
                .as_ref()
                .and_then(|src| src.bundle_id.as_deref())
                .is_some_and(|bid| SECURE_FOCUS_BUNDLE_OVERRIDES.contains(&bid));
            let secure_focus = match secure_res {
                Ok(value) => {
                    self.consecutive_secure_ax_failures = 0;
                    value || bundle_override
                }
                Err(err) => {
                    self.consecutive_secure_ax_failures =
                        self.consecutive_secure_ax_failures.saturating_add(1);
                    if self.consecutive_secure_ax_failures >= SECURE_FOCUS_FAIL_CLOSED_THRESHOLD
                        || bundle_override
                    {
                        warn!(
                            error = %err,
                            consecutive_failures = self.consecutive_secure_ax_failures,
                            bundle_override,
                            "secure_focus_fail_closed",
                        );
                        true
                    } else {
                        false
                    }
                }
            };
            (source, secure_focus)
        } else {
            (None, false)
        };

        // Anchor `last_sequence` so a steady-state focus on the same
        // field doesn't loop the AX query every poll for the same
        // clip. We deliberately do *not* clear `force_content_check`
        // here: the wake-gap one-shot was armed to defend against a
        // lapped pasteboard `changeCount`, which only matters for the
        // next *captured* clip. Leaving the flag set means that when
        // the user moves out of the secure field, the very next tick
        // still does the content-hash cross-check before trusting the
        // dedup.
        if secure_focus {
            info!("capture_skipped reason=secure_field");
            let _ = self
                .audit
                .record("capture_skipped", None, Some("secure_field"))
                .await;
            self.last_sequence = Some(sequence);
            self.pristine = false;
            return Ok(None);
        }

        let mut snapshot = match self
            .reader
            .current_snapshot_with_max(self.settings.max_entry_size_bytes)
            .await?
        {
            CapturedSnapshot::Captured(snapshot) => snapshot,
            CapturedSnapshot::Oversized {
                sequence,
                observed_bytes,
                limit,
            } => {
                warn!(
                    bytes = observed_bytes,
                    limit, "capture_skipped reason=oversized stage=pre_read"
                );
                let _ = self
                    .audit
                    .record(
                        "capture_skipped",
                        None,
                        Some(&format!("oversized:pre_read:{observed_bytes}>{limit}")),
                    )
                    .await;
                // Anchor the sequence so the next poll skips this same
                // oversized clip without re-probing pasteboard sizes.
                self.force_content_check = false;
                self.pristine = false;
                self.last_sequence = Some(sequence);
                return Ok(None);
            }
        };
        // Snapshot succeeded — only now is it safe to consume the wake-gap
        // flag and flip pristine.
        self.force_content_check = false;
        self.pristine = false;
        self.last_sequence = Some(snapshot.sequence.clone());
        if snapshot.source.is_none() {
            snapshot.source = frontmost_source;
        }

        let Some(mut entry) = EntryFactory::from_snapshot(snapshot) else {
            return Ok(None);
        };
        // Wake-gap content cross-check: if a sleep gap forced the body read
        // and the resulting hash matches the last captured content, treat
        // the changeCount nudge as spurious and skip without inserting.
        // Refresh `last_content_hash` either way so subsequent gaps still
        // have something to compare against.
        if force_content_check
            && self.last_content_hash.as_deref() == Some(entry.metadata.content_hash.value.as_str())
        {
            return Ok(None);
        }
        self.last_content_hash = Some(entry.metadata.content_hash.value.clone());
        if !self.settings.capture_kinds.contains(&entry.content_kind()) {
            info!(kind = ?entry.content_kind(), "capture_skipped reason=kind_disabled");
            let _ = self
                .audit
                .record("capture_skipped", Some(entry.id), Some("kind_disabled"))
                .await;
            return Ok(None);
        }
        // Image entries don't carry plain text, so size them by their byte
        // payload instead — otherwise the empty-text guard below silently
        // dropped every image snapshot and the README's image-capture promise
        // never reached storage.
        let payload_bytes = match &entry.content {
            ClipboardContent::Image(img) => img.byte_count,
            _ => entry.plain_text().map_or(0, str::len),
        };
        if payload_bytes == 0 {
            return Ok(None);
        }
        if payload_bytes > self.settings.max_entry_size_bytes {
            warn!(bytes = payload_bytes, "capture_skipped reason=oversized");
            let _ = self
                .audit
                .record("capture_skipped", Some(entry.id), Some("oversized"))
                .await;
            return Ok(None);
        }
        // Fail closed if the persisted regex_denylist contains an
        // uncompilable pattern — silently dropping it would let secret
        // matches the user explicitly asked us to redact slip into history.
        let classifier = SensitivityClassifier::try_new(self.settings.clone())?;
        let classification = classifier.classify(&entry);
        entry.sensitivity = classification.sensitivity;
        if matches!(classification.sensitivity, Sensitivity::Blocked) {
            info!(reasons = ?classification.reasons, "entry_blocked");
            let _ = self
                .audit
                .record(
                    "entry_blocked",
                    Some(entry.id),
                    Some(&format!("{:?}", classification.reasons)),
                )
                .await;
            return Ok(None);
        }
        if let Some(preview) = classification.redacted_preview {
            entry.search.preview = preview;
        }
        if matches!(
            classifier.apply_secret_handling(&mut entry, self.settings.secret_handling),
            SecretAction::Drop,
        ) {
            info!(reasons = ?classification.reasons, "secret_blocked");
            let _ = self
                .audit
                .record(
                    "secret_blocked",
                    Some(entry.id),
                    Some(&format!("{:?}", classification.reasons)),
                )
                .await;
            return Ok(None);
        }

        // Invalidate before *and* after the insert. Without the pre-call,
        // a concurrent `runtime.search()` could lock the cache between
        // SQLite commit and our post-invalidate and serve a pre-insert hit
        // even though the new row is already durable.
        if let Some(cache) = &self.search_cache {
            lock_or_recover(cache).invalidate();
        }
        let id = self.entries.insert(entry).await?;
        info!(entry_id = %id, "entry_inserted");
        if let Some(cache) = &self.search_cache {
            lock_or_recover(cache).invalidate();
        }
        Ok(Some(id))
    }

    pub async fn run_polling(
        &mut self,
        interval: std::time::Duration,
        shutdown: impl std::future::Future<Output = ()>,
    ) -> Result<()> {
        tokio::pin!(shutdown);
        loop {
            let sleep_for = Self::backoff_for_failures(interval, self.consecutive_failures);
            tokio::select! {
                () = &mut shutdown => return Ok(()),
                () = tokio::time::sleep(sleep_for) => {
                    match self.capture_once().await {
                        Ok(_) => self.note_capture_success(),
                        Err(err) => self.note_capture_error(&err),
                    }
                }
            }
        }
    }

    pub async fn run_polling_with_settings(
        &mut self,
        interval: std::time::Duration,
        mut settings_rx: watch::Receiver<AppSettings>,
        shutdown: impl std::future::Future<Output = ()>,
    ) -> Result<()> {
        tokio::pin!(shutdown);
        loop {
            let sleep_for = Self::backoff_for_failures(interval, self.consecutive_failures);
            tokio::select! {
                () = &mut shutdown => return Ok(()),
                changed = settings_rx.changed() => {
                    if changed.is_err() {
                        return Ok(());
                    }
                    let next = settings_rx.borrow().clone();
                    self.update_settings(next);
                }
                () = tokio::time::sleep(sleep_for) => {
                    match self.capture_once().await {
                        Ok(_) => self.note_capture_success(),
                        Err(err) => self.note_capture_error(&err),
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use nagori_core::{AppSettings, EntryRepository, Sensitivity, settings::SecretHandling};
    use nagori_platform::{ClipboardWriter, MemoryClipboard};
    use nagori_storage::SqliteStore;

    use super::*;

    fn loop_for(
        clipboard: Arc<MemoryClipboard>,
        store: SqliteStore,
        settings: AppSettings,
    ) -> CaptureLoop<Arc<MemoryClipboard>, SqliteStore, SqliteStore> {
        CaptureLoop::new(clipboard, store.clone(), store, settings)
    }

    #[tokio::test]
    async fn capture_once_dedupes_repeated_clipboard_text() {
        let clipboard = Arc::new(MemoryClipboard::new());
        let store = SqliteStore::open_memory().expect("memory store");
        let mut loop_ = loop_for(clipboard.clone(), store.clone(), AppSettings::default());

        // Empty clipboard text → no snapshot text → nothing inserted.
        assert!(loop_.capture_once().await.unwrap().is_none());

        clipboard
            .write_text("captured value alpha")
            .await
            .expect("clipboard write");
        let first = loop_
            .capture_once()
            .await
            .unwrap()
            .expect("first capture should record an entry");

        // Same text → same sequence → skipped.
        assert!(loop_.capture_once().await.unwrap().is_none());

        clipboard
            .write_text("captured value bravo")
            .await
            .expect("clipboard write");
        let second = loop_
            .capture_once()
            .await
            .unwrap()
            .expect("new text should record a new entry");
        assert_ne!(first, second);

        let entries = store.list_recent(10).await.unwrap();
        assert_eq!(entries.len(), 2);
    }

    #[tokio::test]
    async fn capture_once_skips_when_capture_disabled() {
        let clipboard = Arc::new(MemoryClipboard::new());
        let store = SqliteStore::open_memory().expect("memory store");
        let settings = AppSettings {
            capture_enabled: false,
            ..AppSettings::default()
        };
        let mut loop_ = loop_for(clipboard.clone(), store.clone(), settings);
        clipboard
            .write_text("ignored value")
            .await
            .expect("clipboard write");

        assert!(loop_.capture_once().await.unwrap().is_none());
        assert!(store.list_recent(10).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn capture_once_skips_disabled_content_kind_before_classification() {
        let clipboard = Arc::new(MemoryClipboard::new());
        let store = SqliteStore::open_memory().expect("memory store");
        let settings = AppSettings {
            capture_kinds: std::iter::once(nagori_core::ContentKind::Image).collect(),
            ..AppSettings::default()
        };
        let mut loop_ = loop_for(clipboard.clone(), store.clone(), settings);
        clipboard
            .write_text("plain text should be ignored")
            .await
            .expect("clipboard write");

        assert!(loop_.capture_once().await.unwrap().is_none());
        assert!(store.list_recent(10).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn capture_once_skips_oversized_blocked_text() {
        let clipboard = Arc::new(MemoryClipboard::new());
        let store = SqliteStore::open_memory().expect("memory store");
        // Drop max_entry_size_bytes so any short clip is classified as
        // oversized and the capture loop must skip insertion.
        let settings = AppSettings {
            max_entry_size_bytes: 4,
            ..AppSettings::default()
        };
        let mut loop_ = loop_for(clipboard.clone(), store.clone(), settings);
        clipboard
            .write_text("this is too long for the policy")
            .await
            .expect("clipboard write");

        assert!(loop_.capture_once().await.unwrap().is_none());
        assert!(store.list_recent(10).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn capture_once_drops_user_regex_match() {
        // Regex_denylist UI promises "Captures matching any pattern are
        // dropped" — so a UserRegex-matched clip must never reach SQLite,
        // not even with a redacted body. Regression for the original
        // behaviour where UserRegex classified as Private and the raw
        // text was persisted as `entry.content`.
        let clipboard = Arc::new(MemoryClipboard::new());
        let store = SqliteStore::open_memory().expect("memory store");
        let settings = AppSettings {
            regex_denylist: vec![r"INTERNAL-\d+".to_owned()],
            ..AppSettings::default()
        };
        let mut loop_ = loop_for(clipboard.clone(), store.clone(), settings);
        clipboard
            .write_text("ticket INTERNAL-123 must stay local")
            .await
            .expect("clipboard write");

        assert!(loop_.capture_once().await.unwrap().is_none());
        assert!(store.list_recent(10).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn capture_once_blocks_secret_when_handling_is_block() {
        // SecretHandling::Block must drop Secret-tagged content entirely
        // (not just redact it). Regression for the original behaviour where
        // the secret_handling setting was ignored and every Secret payload
        // was persisted verbatim.
        let clipboard = Arc::new(MemoryClipboard::new());
        let store = SqliteStore::open_memory().expect("memory store");
        let settings = AppSettings {
            secret_handling: SecretHandling::Block,
            ..AppSettings::default()
        };
        let mut loop_ = loop_for(clipboard.clone(), store.clone(), settings);
        clipboard
            .write_text("token = ghp_abcdefghijklmnopqrstuvwxyz123456")
            .await
            .expect("clipboard write");

        assert!(loop_.capture_once().await.unwrap().is_none());
        assert!(store.list_recent(10).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn capture_once_redacts_secret_by_default() {
        // The default SecretHandling::StoreRedacted has to land a row whose
        // durable body is the redacted form. An exported DB must never
        // expose the raw token.
        let clipboard = Arc::new(MemoryClipboard::new());
        let store = SqliteStore::open_memory().expect("memory store");
        let mut loop_ = loop_for(clipboard.clone(), store.clone(), AppSettings::default());
        clipboard
            .write_text("token = ghp_abcdefghijklmnopqrstuvwxyz123456")
            .await
            .expect("clipboard write");

        let id = loop_
            .capture_once()
            .await
            .unwrap()
            .expect("redacted secret should be persisted");
        let stored = store.get(id).await.unwrap().expect("stored row");
        assert_eq!(stored.sensitivity, Sensitivity::Secret);
        let body = stored.plain_text().expect("body").to_owned();
        assert!(
            !body.contains("ghp_abcdefghijklmnopqrstuvwxyz123456"),
            "default secret_handling must not store the raw token: {body:?}",
        );
        assert!(body.contains("[REDACTED]"));
    }

    #[tokio::test]
    async fn capture_once_keeps_secret_full_when_opted_in() {
        let clipboard = Arc::new(MemoryClipboard::new());
        let store = SqliteStore::open_memory().expect("memory store");
        let settings = AppSettings {
            secret_handling: SecretHandling::StoreFull,
            ..AppSettings::default()
        };
        let mut loop_ = loop_for(clipboard.clone(), store.clone(), settings);
        clipboard
            .write_text("token = ghp_abcdefghijklmnopqrstuvwxyz123456")
            .await
            .expect("clipboard write");

        let id = loop_.capture_once().await.unwrap().expect("entry id");
        let stored = store.get(id).await.unwrap().expect("stored row");
        assert_eq!(
            stored.plain_text(),
            Some("token = ghp_abcdefghijklmnopqrstuvwxyz123456"),
        );
        // Even with the raw body retained, the search preview must still be
        // the redacted form so UI surfaces never leak the secret.
        assert!(stored.search.preview.contains("[REDACTED]"));
    }

    #[tokio::test]
    async fn capture_once_attributes_frontmost_app_to_snapshot() {
        // Regression for the polling race where we read the clipboard text
        // before grabbing frontmost — by the time `frontmost_app` was
        // queried, the user could have switched away and the password-
        // manager source attribution (which the denylist relies on) would
        // be lost. We now capture frontmost immediately after the sequence
        // change, before reading the (potentially slower) clipboard body.
        use async_trait::async_trait;
        use nagori_core::SourceApp;
        use nagori_platform::{FrontmostApp, WindowBehavior};

        #[derive(Default)]
        struct FixedFrontmost;

        #[async_trait]
        impl WindowBehavior for FixedFrontmost {
            async fn frontmost_app(&self) -> Result<Option<FrontmostApp>> {
                Ok(Some(FrontmostApp {
                    source: SourceApp {
                        bundle_id: Some("com.agilebits.onepassword".to_owned()),
                        name: Some("1Password".to_owned()),
                        executable_path: None,
                    },
                    window_title: None,
                }))
            }
            async fn show_palette(&self) -> Result<()> {
                Ok(())
            }
            async fn hide_palette(&self) -> Result<()> {
                Ok(())
            }
        }

        let clipboard = Arc::new(MemoryClipboard::new());
        let store = SqliteStore::open_memory().expect("memory store");
        let mut loop_ = CaptureLoop::new(
            clipboard.clone(),
            store.clone(),
            store.clone(),
            AppSettings::default(),
        )
        .with_window(Arc::new(FixedFrontmost));

        clipboard
            .write_text("safe-looking value")
            .await
            .expect("clipboard write");

        // 1Password is on the default app_denylist, so the entry must be
        // dropped (Sensitivity::Blocked) once the source attribution is
        // attached. If the frontmost weren't picked up, the entry would be
        // persisted as Public and the test would observe a stored row.
        assert!(loop_.capture_once().await.unwrap().is_none());
        assert!(store.list_recent(10).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn capture_once_skips_when_frontmost_focus_is_secure() {
        // The AX-driven password-field guard must drop a clip before any
        // body-level processing. Regression for the case where a user
        // focuses a password input and the pasteboard happens to update
        // (e.g. because the same app autofills) — we must not commit the
        // value to history regardless of how the SensitivityClassifier
        // would have tagged it on its own.
        use async_trait::async_trait;
        use nagori_core::SourceApp;
        use nagori_platform::{FrontmostApp, WindowBehavior};

        #[derive(Default)]
        struct SecureFocus;

        #[async_trait]
        impl WindowBehavior for SecureFocus {
            async fn frontmost_app(&self) -> Result<Option<FrontmostApp>> {
                Ok(Some(FrontmostApp {
                    source: SourceApp {
                        bundle_id: Some("com.example.notes".to_owned()),
                        name: Some("Notes".to_owned()),
                        executable_path: None,
                    },
                    window_title: None,
                }))
            }
            async fn show_palette(&self) -> Result<()> {
                Ok(())
            }
            async fn hide_palette(&self) -> Result<()> {
                Ok(())
            }
            async fn frontmost_focused_is_secure(&self) -> Result<bool> {
                Ok(true)
            }
        }

        let clipboard = Arc::new(MemoryClipboard::new());
        let store = SqliteStore::open_memory().expect("memory store");
        let mut loop_ = CaptureLoop::new(
            clipboard.clone(),
            store.clone(),
            store.clone(),
            AppSettings::default(),
        )
        .with_window(Arc::new(SecureFocus));

        clipboard
            .write_text("hunter2")
            .await
            .expect("clipboard write");

        // Suppressed at the secure-field gate before classification runs.
        assert!(loop_.capture_once().await.unwrap().is_none());
        assert!(store.list_recent(10).await.unwrap().is_empty());

        // Steady-state focus on the same field must not loop the AX query
        // every poll: the second tick short-circuits on the dedup
        // baseline anchored by the first call.
        assert!(loop_.capture_once().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn capture_once_proceeds_when_secure_check_errors() {
        // A platform error from the AX call must not stop normal capture.
        // We degrade open so a missing Accessibility grant or an FFI
        // hiccup doesn't silently disable the clipboard feature.
        use async_trait::async_trait;
        use nagori_core::{AppError, SourceApp};
        use nagori_platform::{FrontmostApp, WindowBehavior};

        #[derive(Default)]
        struct ErroringSecure;

        #[async_trait]
        impl WindowBehavior for ErroringSecure {
            async fn frontmost_app(&self) -> Result<Option<FrontmostApp>> {
                Ok(Some(FrontmostApp {
                    source: SourceApp {
                        bundle_id: Some("com.example.editor".to_owned()),
                        name: Some("Editor".to_owned()),
                        executable_path: None,
                    },
                    window_title: None,
                }))
            }
            async fn show_palette(&self) -> Result<()> {
                Ok(())
            }
            async fn hide_palette(&self) -> Result<()> {
                Ok(())
            }
            async fn frontmost_focused_is_secure(&self) -> Result<bool> {
                Err(AppError::Platform("AX call failed".to_owned()))
            }
        }

        let clipboard = Arc::new(MemoryClipboard::new());
        let store = SqliteStore::open_memory().expect("memory store");
        let mut loop_ = CaptureLoop::new(
            clipboard.clone(),
            store.clone(),
            store.clone(),
            AppSettings::default(),
        )
        .with_window(Arc::new(ErroringSecure));

        clipboard
            .write_text("benign value")
            .await
            .expect("clipboard write");

        let id = loop_
            .capture_once()
            .await
            .unwrap()
            .expect("AX error must fail open and capture proceeds");
        assert!(store.get(id).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn sustained_secure_ax_errors_flip_to_fail_closed() {
        // Below the threshold a single AX error fails open; once the
        // counter crosses `SECURE_FOCUS_FAIL_CLOSED_THRESHOLD` we must
        // refuse to capture even if the classifier *would* have allowed
        // the clip. Otherwise a permanent AX outage (revoked grant,
        // wedged AX subsystem) silently resumes flowing every keystroke
        // through history.
        use async_trait::async_trait;
        use nagori_core::{AppError, SourceApp};
        use nagori_platform::{FrontmostApp, WindowBehavior};

        struct ErroringSecure;
        #[async_trait]
        impl WindowBehavior for ErroringSecure {
            async fn frontmost_app(&self) -> Result<Option<FrontmostApp>> {
                Ok(Some(FrontmostApp {
                    source: SourceApp {
                        bundle_id: Some("com.example.editor".to_owned()),
                        name: Some("Editor".to_owned()),
                        executable_path: None,
                    },
                    window_title: None,
                }))
            }
            async fn show_palette(&self) -> Result<()> {
                Ok(())
            }
            async fn hide_palette(&self) -> Result<()> {
                Ok(())
            }
            async fn frontmost_focused_is_secure(&self) -> Result<bool> {
                Err(AppError::Platform("AX wedged".to_owned()))
            }
        }

        let clipboard = Arc::new(MemoryClipboard::new());
        let store = SqliteStore::open_memory().expect("memory store");
        let mut loop_ = CaptureLoop::new(
            clipboard.clone(),
            store.clone(),
            store.clone(),
            AppSettings::default(),
        )
        .with_window(Arc::new(ErroringSecure));

        // Drive one capture per distinct clip so we don't dedup on
        // sequence. The first `THRESHOLD - 1` ticks fail open and
        // capture; subsequent ticks fail closed and skip.
        for n in 0..SECURE_FOCUS_FAIL_CLOSED_THRESHOLD - 1 {
            clipboard
                .write_text(&format!("clip-{n}"))
                .await
                .expect("clipboard write");
            assert!(
                loop_.capture_once().await.unwrap().is_some(),
                "tick {n} below threshold must fail open"
            );
        }
        clipboard
            .write_text("after-threshold")
            .await
            .expect("clipboard write");
        assert!(
            loop_.capture_once().await.unwrap().is_none(),
            "tick at/after threshold must fail closed"
        );
    }

    #[tokio::test]
    async fn secure_ax_failure_counter_resets_on_success() {
        // Recovery path: once the AX subsystem starts answering again
        // we must clear the counter, otherwise the loop would stay
        // pinned at "fail closed" forever after one outage.
        use async_trait::async_trait;
        use nagori_core::{AppError, SourceApp};
        use nagori_platform::{FrontmostApp, WindowBehavior};
        use std::sync::atomic::{AtomicU32, Ordering};

        struct FlakySecure {
            calls: AtomicU32,
        }
        #[async_trait]
        impl WindowBehavior for FlakySecure {
            async fn frontmost_app(&self) -> Result<Option<FrontmostApp>> {
                Ok(Some(FrontmostApp {
                    source: SourceApp {
                        bundle_id: Some("com.example.editor".to_owned()),
                        name: Some("Editor".to_owned()),
                        executable_path: None,
                    },
                    window_title: None,
                }))
            }
            async fn show_palette(&self) -> Result<()> {
                Ok(())
            }
            async fn hide_palette(&self) -> Result<()> {
                Ok(())
            }
            async fn frontmost_focused_is_secure(&self) -> Result<bool> {
                let n = self.calls.fetch_add(1, Ordering::Relaxed);
                if n == 0 {
                    Err(AppError::Platform("AX briefly down".to_owned()))
                } else {
                    Ok(false)
                }
            }
        }

        let clipboard = Arc::new(MemoryClipboard::new());
        let store = SqliteStore::open_memory().expect("memory store");
        let mut loop_ = CaptureLoop::new(
            clipboard.clone(),
            store.clone(),
            store.clone(),
            AppSettings::default(),
        )
        .with_window(Arc::new(FlakySecure {
            calls: AtomicU32::new(0),
        }));

        clipboard.write_text("a").await.expect("clipboard write");
        assert!(loop_.capture_once().await.unwrap().is_some());
        assert_eq!(loop_.consecutive_secure_ax_failures, 1);
        clipboard.write_text("b").await.expect("clipboard write");
        assert!(loop_.capture_once().await.unwrap().is_some());
        assert_eq!(loop_.consecutive_secure_ax_failures, 0);
    }

    #[tokio::test]
    async fn secure_focus_bundle_override_forces_skip_even_when_ax_says_clear() {
        // Some system password / authentication UIs deliberately scrub
        // their AX state to defeat keyloggers, so `is_secure` will
        // legitimately return `Ok(false)` even though the user is at a
        // password prompt. The bundle-id override list must force a
        // skip in that case.
        use async_trait::async_trait;
        use nagori_core::SourceApp;
        use nagori_platform::{FrontmostApp, WindowBehavior};

        struct AuthDialog;
        #[async_trait]
        impl WindowBehavior for AuthDialog {
            async fn frontmost_app(&self) -> Result<Option<FrontmostApp>> {
                Ok(Some(FrontmostApp {
                    source: SourceApp {
                        bundle_id: Some("com.apple.SecurityAgent".to_owned()),
                        name: Some("SecurityAgent".to_owned()),
                        executable_path: None,
                    },
                    window_title: None,
                }))
            }
            async fn show_palette(&self) -> Result<()> {
                Ok(())
            }
            async fn hide_palette(&self) -> Result<()> {
                Ok(())
            }
            async fn frontmost_focused_is_secure(&self) -> Result<bool> {
                Ok(false)
            }
        }

        let clipboard = Arc::new(MemoryClipboard::new());
        let store = SqliteStore::open_memory().expect("memory store");
        let mut loop_ = CaptureLoop::new(
            clipboard.clone(),
            store.clone(),
            store.clone(),
            AppSettings::default(),
        )
        .with_window(Arc::new(AuthDialog));

        clipboard.write_text("hunter2").await.expect("clipboard");
        assert!(loop_.capture_once().await.unwrap().is_none());
        assert!(store.list_recent(10).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn capture_once_persists_image_clipboard_entries() {
        // The capture loop must keep image snapshots flowing through to the
        // store even though they have no plain text — otherwise the
        // README's "Captures text/URL/image" promise quietly turns into
        // text-only and image rows never reach search/preview.
        use std::sync::Mutex;

        use async_trait::async_trait;
        use nagori_core::{
            ClipboardData, ClipboardRepresentation, ClipboardSequence, ClipboardSnapshot,
            ContentHash,
        };
        use nagori_platform::ClipboardReader;
        use time::OffsetDateTime;

        struct ImageReader {
            bytes: Vec<u8>,
            mime: &'static str,
            // Pretend the user only just copied — read once then "stable" so
            // capture_once's sequence-dedup short-circuit does not fire on a
            // second tick within the same test.
            seq_called: Mutex<bool>,
        }

        #[async_trait]
        impl ClipboardReader for ImageReader {
            async fn current_snapshot(&self) -> Result<ClipboardSnapshot> {
                Ok(ClipboardSnapshot {
                    sequence: ClipboardSequence(ContentHash::sha256(&self.bytes).value),
                    captured_at: OffsetDateTime::now_utc(),
                    source: None,
                    representations: vec![ClipboardRepresentation {
                        mime_type: self.mime.to_owned(),
                        data: ClipboardData::Bytes(self.bytes.clone()),
                    }],
                })
            }

            async fn current_sequence(&self) -> Result<ClipboardSequence> {
                let mut guard = self.seq_called.lock().unwrap();
                let _ = &*guard;
                *guard = true;
                Ok(ClipboardSequence(ContentHash::sha256(&self.bytes).value))
            }
        }

        let bytes = vec![137u8, 80, 78, 71, 13, 10, 26, 10, 1, 2, 3, 4];
        let reader = ImageReader {
            bytes: bytes.clone(),
            mime: "image/png",
            seq_called: Mutex::new(false),
        };
        let store = SqliteStore::open_memory().expect("memory store");
        let mut loop_ =
            CaptureLoop::new(reader, store.clone(), store.clone(), AppSettings::default());

        let id = loop_
            .capture_once()
            .await
            .unwrap()
            .expect("image entry should be inserted");
        let stored = store.get(id).await.unwrap().expect("row");
        match &stored.content {
            ClipboardContent::Image(img) => {
                assert_eq!(img.byte_count, bytes.len());
                assert_eq!(img.mime_type.as_deref(), Some("image/png"));
            }
            other => panic!("expected Image content, got {other:?}"),
        }
        let payload = store.get_payload(id).await.unwrap();
        assert_eq!(payload, Some((bytes, "image/png".to_owned())));
    }

    #[tokio::test]
    async fn capture_once_skips_oversized_image_payloads() {
        // The size guard must be denominated in image byte_count for image
        // snapshots — pre-fix, the guard saw `text.len() == 0` and let any
        // image through regardless of payload size.
        use std::sync::Mutex;

        use async_trait::async_trait;
        use nagori_core::{
            ClipboardData, ClipboardRepresentation, ClipboardSequence, ClipboardSnapshot,
            ContentHash,
        };
        use nagori_platform::ClipboardReader;
        use time::OffsetDateTime;

        struct ImageReader {
            bytes: Vec<u8>,
            seq_called: Mutex<bool>,
        }

        #[async_trait]
        impl ClipboardReader for ImageReader {
            async fn current_snapshot(&self) -> Result<ClipboardSnapshot> {
                Ok(ClipboardSnapshot {
                    sequence: ClipboardSequence(ContentHash::sha256(&self.bytes).value),
                    captured_at: OffsetDateTime::now_utc(),
                    source: None,
                    representations: vec![ClipboardRepresentation {
                        mime_type: "image/png".to_owned(),
                        data: ClipboardData::Bytes(self.bytes.clone()),
                    }],
                })
            }

            async fn current_sequence(&self) -> Result<ClipboardSequence> {
                let mut guard = self.seq_called.lock().unwrap();
                let _ = &*guard;
                *guard = true;
                Ok(ClipboardSequence(ContentHash::sha256(&self.bytes).value))
            }
        }

        let bytes = vec![0u8; 256];
        let reader = ImageReader {
            bytes: bytes.clone(),
            seq_called: Mutex::new(false),
        };
        let store = SqliteStore::open_memory().expect("memory store");
        let settings = AppSettings {
            max_entry_size_bytes: 64,
            ..AppSettings::default()
        };
        let mut loop_ = CaptureLoop::new(reader, store.clone(), store.clone(), settings);

        assert!(loop_.capture_once().await.unwrap().is_none());
        assert!(store.list_recent(10).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn capture_once_invalidates_attached_search_cache() {
        // The runtime serves repeat empty-query searches from
        // `RecentSearchCache`, but a successful capture must drop those
        // hits — otherwise newly captured clips would not surface in the
        // palette until some other mutation flushed the cache.
        use crate::search_cache::{CacheKey, RecentSearchCache};
        use nagori_core::{
            ContentKind, EntryId, RankReason, SearchFilters, SearchMode, SearchResult, Sensitivity,
        };
        use std::sync::{Arc, Mutex};
        use time::OffsetDateTime;

        let cache = Arc::new(Mutex::new(RecentSearchCache::default()));
        cache.lock().unwrap().put(
            CacheKey {
                normalized: String::new(),
                mode: SearchMode::Auto,
                recent_order: nagori_core::RecentOrder::ByRecency,
                limit: 10,
                filters: SearchFilters::default(),
            },
            vec![SearchResult {
                entry_id: EntryId::new(),
                score: 1.0,
                rank_reason: vec![RankReason::Recent],
                preview: String::new(),
                content_kind: ContentKind::Text,
                created_at: OffsetDateTime::now_utc(),
                pinned: false,
                sensitivity: Sensitivity::Public,
                source_app_name: None,
            }],
        );
        assert_eq!(cache.lock().unwrap().len(), 1);

        let clipboard = Arc::new(MemoryClipboard::new());
        let store = SqliteStore::open_memory().expect("memory store");
        let mut loop_ = CaptureLoop::new(
            clipboard.clone(),
            store.clone(),
            store,
            AppSettings::default(),
        )
        .with_search_cache(cache.clone());
        clipboard
            .write_text("captured value")
            .await
            .expect("clipboard write");

        loop_
            .capture_once()
            .await
            .expect("capture")
            .expect("entry inserted");

        assert!(
            cache.lock().unwrap().is_empty(),
            "successful capture must invalidate the attached search cache",
        );
    }

    #[tokio::test]
    async fn capture_once_skips_existing_clipboard_when_disabled_on_launch() {
        // capture_initial_clipboard_on_launch=false: whatever was on the
        // pasteboard before Nagori started must be ignored, but a *new*
        // clip after that point should still be captured.
        let clipboard = Arc::new(MemoryClipboard::new());
        clipboard
            .write_text("preexisting clipboard value")
            .await
            .expect("seed clipboard");
        let store = SqliteStore::open_memory().expect("memory store");
        let settings = AppSettings {
            capture_initial_clipboard_on_launch: false,
            ..AppSettings::default()
        };
        let mut loop_ = loop_for(clipboard.clone(), store.clone(), settings);

        // First tick observes the existing sequence and discards it.
        assert!(loop_.capture_once().await.unwrap().is_none());
        assert!(store.list_recent(10).await.unwrap().is_empty());

        // A user-initiated clip after launch must still flow through.
        clipboard
            .write_text("post launch clip")
            .await
            .expect("clipboard write");
        let id = loop_
            .capture_once()
            .await
            .unwrap()
            .expect("post-launch clip should be inserted");
        let stored = store.get(id).await.unwrap().expect("stored row");
        assert_eq!(stored.plain_text(), Some("post launch clip"));
    }

    #[tokio::test]
    async fn capture_once_resyncs_dedup_baseline_after_long_gap() {
        // macOS pasteboard's `changeCount` can lap silently across a sleep
        // cycle, so a fresh post-wake clip may collide with `last_sequence`
        // and get skipped as a duplicate. Simulate that with a reader that
        // returns the *same* sequence value for two distinct payloads, then
        // drive a >30s wall-clock gap between two `capture_once` calls. The
        // first capture lands; without the gap-based resync the second
        // capture would dedupe out and storage would never see the new text.
        use std::sync::Mutex;

        use async_trait::async_trait;
        use nagori_core::{
            ClipboardData, ClipboardRepresentation, ClipboardSequence, ClipboardSnapshot,
        };
        use nagori_platform::ClipboardReader;
        use time::OffsetDateTime;

        struct StubReader {
            sequence: ClipboardSequence,
            text: Mutex<String>,
        }

        #[async_trait]
        impl ClipboardReader for StubReader {
            async fn current_snapshot(&self) -> Result<ClipboardSnapshot> {
                let text = self.text.lock().unwrap().clone();
                Ok(ClipboardSnapshot {
                    sequence: self.sequence.clone(),
                    captured_at: OffsetDateTime::now_utc(),
                    source: None,
                    representations: vec![ClipboardRepresentation {
                        mime_type: "text/plain".to_owned(),
                        data: ClipboardData::Text(text),
                    }],
                })
            }
            async fn current_sequence(&self) -> Result<ClipboardSequence> {
                Ok(self.sequence.clone())
            }
        }

        let reader = StubReader {
            sequence: ClipboardSequence("colliding-seq".to_owned()),
            text: Mutex::new("pre-sleep clip".to_owned()),
        };
        let store = SqliteStore::open_memory().expect("memory store");
        let mut loop_ =
            CaptureLoop::new(reader, store.clone(), store.clone(), AppSettings::default());

        let t0 = SystemTime::now();
        let pre_id = loop_
            .capture_once_at(t0)
            .await
            .unwrap()
            .expect("pre-sleep capture should record an entry");

        // Swap in a different payload while keeping the sequence pinned —
        // the bug we're guarding against is that the platform-level dedup
        // counter has lapped, not that the content is the same.
        *loop_.reader.text.lock().unwrap() = "post-wake clip".to_owned();

        // Same sequence, no gap → still skipped (sanity check that the
        // dedup short-circuit is otherwise live).
        let no_gap = loop_.capture_once_at(t0 + Duration::from_secs(1)).await;
        assert!(
            no_gap.unwrap().is_none(),
            "without a gap the dedup must hold"
        );

        // Long gap → resync triggers, snapshot is read, fresh row lands.
        let post_id = loop_
            .capture_once_at(t0 + Duration::from_secs(45))
            .await
            .unwrap()
            .expect("post-wake capture should bypass the lapped sequence");
        assert_ne!(pre_id, post_id);

        let entries = store.list_recent(10).await.unwrap();
        assert_eq!(entries.len(), 2);
        let texts: Vec<_> = entries
            .iter()
            .filter_map(|e| e.plain_text().map(str::to_owned))
            .collect();
        assert!(texts.iter().any(|t| t == "pre-sleep clip"));
        assert!(texts.iter().any(|t| t == "post-wake clip"));
    }

    #[tokio::test]
    async fn capture_once_skips_unchanged_pre_launch_clip_after_resync_gap() {
        // Regression for the privacy interaction between
        // `capture_initial_clipboard_on_launch=false` and the wake-gap
        // resync: if the user wakes the host without copying anything, the
        // resync must not promote the still-pre-launch clipboard into the
        // store. The pristine launch path now anchors `last_content_hash`
        // to the initial clip's hash so the post-gap content cross-check
        // recognises it as unchanged and skips.
        let clipboard = Arc::new(MemoryClipboard::new());
        clipboard
            .write_text("preexisting clipboard value")
            .await
            .expect("seed clipboard");
        let store = SqliteStore::open_memory().expect("memory store");
        let settings = AppSettings {
            capture_initial_clipboard_on_launch: false,
            ..AppSettings::default()
        };
        let mut loop_ = loop_for(clipboard.clone(), store.clone(), settings);

        let t0 = SystemTime::now();
        // First tick anchors the pre-launch clipboard without inserting it.
        assert!(loop_.capture_once_at(t0).await.unwrap().is_none());
        assert!(store.list_recent(10).await.unwrap().is_empty());

        // Wake gap with no user copy in between — clipboard contents are
        // identical to the pre-launch value. The resync must not insert.
        assert!(
            loop_
                .capture_once_at(t0 + Duration::from_secs(45))
                .await
                .unwrap()
                .is_none(),
        );
        assert!(
            store.list_recent(10).await.unwrap().is_empty(),
            "wake-gap resync must not promote the unchanged pre-launch clip",
        );

        // A genuine post-wake copy still flows through.
        clipboard
            .write_text("post wake user copy")
            .await
            .expect("clipboard write");
        let id = loop_
            .capture_once_at(t0 + Duration::from_secs(46))
            .await
            .unwrap()
            .expect("a real post-wake copy must still be captured");
        let stored = store.get(id).await.unwrap().expect("stored row");
        assert_eq!(stored.plain_text(), Some("post wake user copy"));
    }

    #[tokio::test]
    async fn pristine_skip_retries_on_snapshot_failure() {
        // Regression: the pristine launch path under
        // `capture_initial_clipboard_on_launch=false` must not flip
        // `pristine` until the snapshot read succeeds. Otherwise a single
        // platform-level read failure on tick 1 leaves the loop with no
        // baseline (pristine=false, last_sequence=None) and tick 2 happily
        // captures the still-pre-launch clipboard.
        use std::sync::Mutex;

        use async_trait::async_trait;
        use nagori_core::{
            ClipboardData, ClipboardRepresentation, ClipboardSequence, ClipboardSnapshot,
            ContentHash,
        };
        use nagori_platform::ClipboardReader;
        use time::OffsetDateTime;

        struct FlakyReader {
            text: String,
            snapshot_attempts: Mutex<u32>,
            fail_until_attempt: u32,
        }

        #[async_trait]
        impl ClipboardReader for FlakyReader {
            async fn current_snapshot(&self) -> Result<ClipboardSnapshot> {
                let attempt = {
                    let mut guard = self.snapshot_attempts.lock().unwrap();
                    *guard += 1;
                    *guard
                };
                if attempt < self.fail_until_attempt {
                    return Err(AppError::Platform(
                        "simulated transient read failure".to_owned(),
                    ));
                }
                Ok(ClipboardSnapshot {
                    sequence: ClipboardSequence(ContentHash::sha256(self.text.as_bytes()).value),
                    captured_at: OffsetDateTime::now_utc(),
                    source: None,
                    representations: vec![ClipboardRepresentation {
                        mime_type: "text/plain".to_owned(),
                        data: ClipboardData::Text(self.text.clone()),
                    }],
                })
            }
            async fn current_sequence(&self) -> Result<ClipboardSequence> {
                Ok(ClipboardSequence(
                    ContentHash::sha256(self.text.as_bytes()).value,
                ))
            }
        }

        let reader = FlakyReader {
            text: "preexisting clipboard value".to_owned(),
            snapshot_attempts: Mutex::new(0),
            fail_until_attempt: 2,
        };
        let store = SqliteStore::open_memory().expect("memory store");
        let settings = AppSettings {
            capture_initial_clipboard_on_launch: false,
            ..AppSettings::default()
        };
        let mut loop_ = CaptureLoop::new(reader, store.clone(), store.clone(), settings);

        // Tick 1: snapshot fails. pristine must stay true so tick 2 retries
        // the launch-skip semantic instead of falling through to the
        // body-read path with no baseline.
        assert!(loop_.capture_once().await.is_err());
        assert!(store.list_recent(10).await.unwrap().is_empty());

        // Tick 2: snapshot succeeds. Pre-launch content anchored, no row.
        assert!(loop_.capture_once().await.unwrap().is_none());
        assert!(
            store.list_recent(10).await.unwrap().is_empty(),
            "after a failed launch-tick retry the pre-launch clip must still be skipped",
        );
    }

    #[tokio::test]
    async fn force_content_check_survives_snapshot_failure() {
        // Regression: a wake gap arms `force_content_check` so the next
        // tick re-reads the body even if the sequence still matches. If
        // that read fails transiently, the flag must persist through to
        // the following tick — otherwise the colliding sequence would be
        // dedup-skipped again and the post-wake content lost.
        use std::sync::Mutex;

        use async_trait::async_trait;
        use nagori_core::{
            ClipboardData, ClipboardRepresentation, ClipboardSequence, ClipboardSnapshot,
        };
        use nagori_platform::ClipboardReader;
        use time::OffsetDateTime;

        struct ScriptedReader {
            sequence: ClipboardSequence,
            text: Mutex<String>,
            snapshot_attempts: Mutex<u32>,
            fail_attempt: u32,
        }

        #[async_trait]
        impl ClipboardReader for ScriptedReader {
            async fn current_snapshot(&self) -> Result<ClipboardSnapshot> {
                let attempt = {
                    let mut guard = self.snapshot_attempts.lock().unwrap();
                    *guard += 1;
                    *guard
                };
                if attempt == self.fail_attempt {
                    return Err(AppError::Platform("simulated flake".to_owned()));
                }
                Ok(ClipboardSnapshot {
                    sequence: self.sequence.clone(),
                    captured_at: OffsetDateTime::now_utc(),
                    source: None,
                    representations: vec![ClipboardRepresentation {
                        mime_type: "text/plain".to_owned(),
                        data: ClipboardData::Text(self.text.lock().unwrap().clone()),
                    }],
                })
            }
            async fn current_sequence(&self) -> Result<ClipboardSequence> {
                Ok(self.sequence.clone())
            }
        }

        let reader = ScriptedReader {
            sequence: ClipboardSequence("colliding-seq".to_owned()),
            text: Mutex::new("pre-sleep clip".to_owned()),
            snapshot_attempts: Mutex::new(0),
            fail_attempt: 2,
        };
        let store = SqliteStore::open_memory().expect("memory store");
        let mut loop_ =
            CaptureLoop::new(reader, store.clone(), store.clone(), AppSettings::default());

        let t0 = SystemTime::now();
        // Tick 1 (attempt 1 of current_snapshot succeeds): pre-sleep clip
        // captured.
        loop_
            .capture_once_at(t0)
            .await
            .unwrap()
            .expect("pre-sleep capture");

        // Swap content but keep sequence pinned (the lapped-changeCount
        // case the resync defends against).
        *loop_.reader.text.lock().unwrap() = "post-wake clip".to_owned();

        // Tick 2 (attempt 2 fails): wake gap arms force; snapshot fails;
        // force must NOT be cleared.
        assert!(
            loop_
                .capture_once_at(t0 + Duration::from_secs(45))
                .await
                .is_err(),
        );

        // Tick 3 (attempt 3 succeeds): no fresh gap, but force from tick 2
        // is still set → body re-read despite sequence collision → captured.
        let post_id = loop_
            .capture_once_at(t0 + Duration::from_secs(46))
            .await
            .unwrap()
            .expect("post-wake clip should land on the retry tick");
        let stored = store.get(post_id).await.unwrap().expect("stored row");
        assert_eq!(stored.plain_text(), Some("post-wake clip"));
    }

    #[tokio::test]
    async fn update_settings_takes_effect_on_next_capture() {
        let clipboard = Arc::new(MemoryClipboard::new());
        let store = SqliteStore::open_memory().expect("memory store");
        let mut loop_ = loop_for(clipboard.clone(), store.clone(), AppSettings::default());

        clipboard
            .write_text("first value")
            .await
            .expect("clipboard write");
        loop_
            .capture_once()
            .await
            .expect("capture")
            .expect("entry inserted");

        // Disable capture mid-flight; the next clipboard change must be
        // ignored even though the reader returns fresh content.
        loop_.update_settings(AppSettings {
            capture_enabled: false,
            ..AppSettings::default()
        });
        clipboard
            .write_text("second value")
            .await
            .expect("clipboard write");
        assert!(loop_.capture_once().await.unwrap().is_none());

        let entries = store.list_recent(10).await.unwrap();
        assert_eq!(entries.len(), 1);
    }

    type Loop = CaptureLoop<Arc<MemoryClipboard>, SqliteStore, SqliteStore>;

    #[test]
    fn backoff_keeps_base_interval_below_threshold() {
        // Below the threshold the loop must keep its configured cadence —
        // a single transient hiccup should not stretch poll spacing or
        // hide the next clip behind a several-second wait.
        let base = Duration::from_millis(500);
        for failures in 0..BACKOFF_AFTER_CONSECUTIVE_FAILURES {
            assert_eq!(Loop::backoff_for_failures(base, failures), base);
        }
    }

    #[test]
    fn backoff_grows_exponentially_then_caps() {
        // Above the threshold the spacing doubles each consecutive
        // failure (1× → 2× → 4× …) until MAX_BACKOFF clamps it. The cap
        // matters: without it a sustained outage would push the next
        // tick out by minutes.
        let base = Duration::from_millis(500);
        let first = Loop::backoff_for_failures(base, BACKOFF_AFTER_CONSECUTIVE_FAILURES);
        assert_eq!(first, base * 2);
        let second = Loop::backoff_for_failures(base, BACKOFF_AFTER_CONSECUTIVE_FAILURES + 1);
        assert_eq!(second, base * 4);
        let huge = Loop::backoff_for_failures(base, 1_000);
        assert_eq!(huge, MAX_BACKOFF);
    }

    #[tokio::test]
    async fn note_capture_error_resets_consecutive_failures_on_success() {
        // The polling loop drives the backoff off `consecutive_failures`,
        // which must reset on the next successful tick — otherwise a
        // recovered daemon stays paced at MAX_BACKOFF forever.
        let clipboard = Arc::new(MemoryClipboard::new());
        let store = SqliteStore::open_memory().expect("memory store");
        let mut loop_ = loop_for(clipboard, store, AppSettings::default());

        loop_.note_capture_error(&AppError::Platform("simulated".to_owned()));
        loop_.note_capture_error(&AppError::Platform("simulated".to_owned()));
        assert_eq!(loop_.consecutive_failures, 2);
        loop_.note_capture_success();
        assert_eq!(loop_.consecutive_failures, 0);
    }

    #[tokio::test]
    async fn note_capture_error_buckets_warns_per_kind() {
        // Two distinct error kinds within the suppression window must
        // each emit at least one warn — otherwise an in-flight platform
        // suppression would shadow a sudden second failure mode (e.g.
        // AX permission loss landing while pasteboard reads are still
        // failing).
        let clipboard = Arc::new(MemoryClipboard::new());
        let store = SqliteStore::open_memory().expect("memory store");
        let mut loop_ = loop_for(clipboard, store, AppSettings::default());

        loop_.note_capture_error(&AppError::Platform("first".to_owned()));
        // Same kind: suppressed, but counter increments.
        loop_.note_capture_error(&AppError::Platform("second".to_owned()));
        let platform_slot = CaptureErrorKind::Platform as usize;
        assert_eq!(loop_.suppressed_warns[platform_slot], 1);

        // Different kind: emits its own warn line, independent of the
        // platform suppression timer.
        loop_.note_capture_error(&AppError::Policy("policy hit".to_owned()));
        let other_slot = CaptureErrorKind::Other as usize;
        // After emitting, suppressed counter is consumed back to 0.
        assert_eq!(loop_.suppressed_warns[other_slot], 0);
        assert!(loop_.last_warn_at[other_slot].is_some());
    }
}

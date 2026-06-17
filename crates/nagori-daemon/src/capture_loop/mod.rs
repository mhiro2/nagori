use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use nagori_core::{
    AppError, AppSettings, AuditLog, ClipboardContent, ClipboardSequence, EntryFactory, EntryId,
    EntryRepository, MAX_ENTRY_SIZE_BYTES, Result, SecretAction, Sensitivity,
    SensitivityClassifier, StoredClipboardRepresentation, factory::compute_representation_set_hash,
};
use nagori_ipc::CaptureEventCategory;
use nagori_platform::{CapturedSnapshot, ClipboardExclusionKind, ClipboardReader, WindowBehavior};
use time::OffsetDateTime;
use tokio::sync::watch;
use tracing::{info, warn};

use crate::health::CaptureHealth;
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
///
/// We enumerate every `AppError` variant rather than collapsing the
/// long tail into a generic `Other`: previously a `Storage` error and
/// an `InvalidInput` error fell into the same suppression bucket, so a
/// burst of one would shadow the other for a full `ERROR_WARN_INTERVAL`.
/// Note that this only sub-divides errors *across* `AppError` variants;
/// disambiguating *within* `Platform` (e.g. pasteboard vs. AX) would
/// require structured context on the error itself, which would be a
/// cross-crate refactor of the error type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CaptureErrorKind {
    Storage,
    Search,
    Platform,
    Permission,
    Ai,
    Policy,
    NotFound,
    InvalidInput,
    Unsupported,
    Configuration,
}

impl CaptureErrorKind {
    /// Number of distinct buckets — used to size the per-kind warn
    /// state arrays. Keep in sync with the variants above.
    const COUNT: usize = 10;

    const fn from_error(err: &AppError) -> Self {
        match err {
            AppError::Storage { .. } => Self::Storage,
            AppError::Search { .. } => Self::Search,
            // Auto-paste never runs inside the capture loop, so a `Paste` error
            // here would only be a wiring mistake; bucket it with the other
            // adapter-level failures rather than adding a dead variant.
            AppError::Platform(_) | AppError::Paste { .. } => Self::Platform,
            AppError::Permission(_) => Self::Permission,
            AppError::Ai(_) => Self::Ai,
            AppError::Policy(_) => Self::Policy,
            AppError::NotFound => Self::NotFound,
            AppError::InvalidInput(_) => Self::InvalidInput,
            AppError::Unsupported(_) => Self::Unsupported,
            // A `Conflict` is the settings compare-and-swap rejecting a stale
            // write; it cannot originate inside the capture loop, so — like
            // `Paste` above — bucket it with the other wiring-mistake errors
            // rather than adding a dead variant.
            AppError::Configuration(_) | AppError::Conflict(_) => Self::Configuration,
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Storage => "storage",
            Self::Search => "search",
            Self::Platform => "platform",
            Self::Permission => "permission",
            Self::Ai => "ai",
            Self::Policy => "policy",
            Self::NotFound => "not_found",
            Self::InvalidInput => "invalid_input",
            Self::Unsupported => "unsupported",
            Self::Configuration => "configuration",
        }
    }

    /// Map an internal error bucket onto the public `CaptureHealth`
    /// category. `Configuration` lands as `SettingsLoad` because the only
    /// in-loop site that returns `AppError::Configuration` is
    /// `SensitivityClassifier::try_new` (uncompilable `regex_denylist`),
    /// which means classification is silently failing on every clip.
    /// `Storage` is broken out so the doctor hint points at disk / DB
    /// rather than clipboard permissions. Everything else collapses to
    /// `Adapter`: from the user's perspective the loop has lost
    /// visibility into the clipboard regardless of which sub-component
    /// broke.
    const fn capture_event_category(self) -> CaptureEventCategory {
        match self {
            Self::Configuration => CaptureEventCategory::SettingsLoad,
            Self::Storage => CaptureEventCategory::Storage,
            _ => CaptureEventCategory::Adapter,
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
/// suspend, lid close, container freeze).
///
/// **Capability: macOS-specific defence; harmless on Windows/Linux.** On
/// macOS the pasteboard `changeCount` can lap silently across a sleep
/// cycle, so a post-wake clip whose sequence happens to collide with the
/// pre-sleep value would be skipped as a duplicate — the resync forces a
/// content-hash recheck above the threshold to defeat that lap. Windows'
/// `GetClipboardSequenceNumber` is a 32-bit monotonic counter that does
/// not lap across sleep, and Linux's content-hash sequence is the SHA-256
/// of the body itself (impossible to "lap" without an actual content
/// collision), so the resync logic is structurally a no-op on those
/// platforms: the next tick's sequence either matches `last_sequence`
/// (genuine duplicate) or differs (genuine new content). Forcing one
/// extra body read on those platforms after a 30 s gap costs nothing in
/// the steady state.
///
/// We deliberately use `SystemTime` (wall clock) rather than `Instant`:
/// Rust's `Instant` on Darwin is `CLOCK_UPTIME_RAW` and does **not**
/// advance while the system is asleep, so a monotonic-clock heuristic
/// would never see a sleep gap on the very platform we care about.
/// `SystemTime` jitters under NTP and is theoretically vulnerable to
/// manual clock changes, but the false-positive cost is just one extra
/// body read and content-hash comparison. The 30-second threshold sits
/// well above any normal scheduling jitter at the default 500 ms cadence
/// (60x headroom) yet small enough to catch even short naps.
const RESYNC_GAP_THRESHOLD: Duration = Duration::from_secs(30);

/// Cheap, non-cryptographic entropy source for the backoff jitter in
/// [`CaptureFailurePolicy::jittered_backoff`]. The wall clock is already imported
/// for the gap-detection path; jitter only needs enough variation that
/// two co-tenant daemons crashing at the same shared event (sleep wake,
/// network re-attach) don't retry on identical ticks. We deliberately
/// avoid pulling in `getrandom`/`rand` for this — the threat model is
/// "synchronised retry storm", not adversarial prediction.
fn jitter_entropy() -> u64 {
    SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX))
}

/// Dedup / baseline state the capture loop uses to decide whether a tick's
/// snapshot is new content.
///
/// The fields move together: once a snapshot read succeeds the loop refreshes
/// them *optimistically*, and a transient failure between that refresh and
/// the durable insert must restore **all** of them to their pre-clip values —
/// see [`DedupState::begin_clip`] / [`DedupState::rollback`]. Restoring only
/// `last_sequence` is not enough: after an empty-snapshot one-shot the real
/// content lands at the *same* changeCount, so the retry relies on
/// `force_content_check` being re-armed and `last_content_hash` still holding
/// the previous capture's hash to clear both dedup gates.
struct DedupState {
    /// Sequence of the last snapshot we acted on. `None` until the first
    /// observation (or after [`CaptureLoop::reset_sequence_baseline`]).
    last_sequence: Option<ClipboardSequence>,
    /// Representation-set hash of the most recent snapshot we observed
    /// (captured or otherwise). Used to confirm a post-resync sequence
    /// collision is a genuine duplicate before re-inserting the same
    /// content. Mirrors the storage dedupe key (`representation_set_hash`
    /// in `entries`), with a fallback to the primary content hash for
    /// snapshot-less entries — so a wake-gap that lands two snapshots
    /// with the same primary text but different HTML/RTF alternatives
    /// is not silently squelched here before storage gets to record
    /// them as distinct rows.
    last_content_hash: Option<String>,
    /// One-shot flag that survives across one tick boundary. When set, the
    /// next `capture_once` invocation bypasses the cheap sequence-based
    /// dedup short-circuit and instead reads the body so the content hash
    /// can be compared against `last_content_hash`. We set this on a
    /// detected wake gap to defend against a potentially lapped pasteboard
    /// `changeCount`.
    force_content_check: bool,
    /// `true` until the loop has observed and acted on its first sequence.
    /// When `capture_initial_clipboard_on_launch` is `false`, the first
    /// observed sequence is recorded as `last_sequence` and the body read is
    /// skipped, so whatever was already on the pasteboard at startup never
    /// reaches storage.
    pristine: bool,
}

/// Pre-clip snapshot of [`DedupState`], returned by
/// [`DedupState::begin_clip`] so a failure after the optimistic refresh can
/// undo it.
struct DedupRollback {
    sequence: Option<ClipboardSequence>,
    content_hash: Option<String>,
    force_content_check: bool,
}

impl DedupState {
    const fn new() -> Self {
        Self {
            last_sequence: None,
            last_content_hash: None,
            force_content_check: false,
            pristine: true,
        }
    }

    /// Optimistically anchor the accepted snapshot's sequence, consume the
    /// one-shot content-check flag, and leave the pristine phase.
    ///
    /// Returns the pre-clip state. The clip is only truly finalised once it
    /// is persisted or intentionally dropped by policy; a transient failure
    /// after this point (denylist regex that won't compile, or a durable
    /// insert that hits DB busy / disk full) must hand the snapshot back to
    /// [`Self::rollback`] so the next tick re-reads and retries the same
    /// clip instead of dedup-skipping it and losing it forever.
    ///
    /// `force_flag_at_tick_start` is the `force_content_check` value read at
    /// the top of the tick (it already folds in any wake-gap arming) — that,
    /// not the just-cleared field, is what a rollback must restore.
    fn begin_clip(
        &mut self,
        sequence: ClipboardSequence,
        force_flag_at_tick_start: bool,
    ) -> DedupRollback {
        self.force_content_check = false;
        self.pristine = false;
        DedupRollback {
            sequence: self.last_sequence.replace(sequence),
            content_hash: self.last_content_hash.clone(),
            force_content_check: force_flag_at_tick_start,
        }
    }

    /// Restore the pre-clip dedup state captured by [`Self::begin_clip`].
    ///
    /// `pristine` is deliberately *not* restored: the loop has acted on an
    /// observation by the time a clip can fail, and re-entering the pristine
    /// phase would re-run the skip-pre-launch-clipboard logic against a clip
    /// the user copied after launch.
    fn rollback(&mut self, saved: DedupRollback) {
        self.last_sequence = saved.sequence;
        self.last_content_hash = saved.content_hash;
        self.force_content_check = saved.force_content_check;
    }
}

/// Failure reporting and pacing for the polling loop: per-kind rate-limited
/// warns, the consecutive-failure counter, and the exponential backoff (with
/// jitter) that counter drives.
///
/// Earlier the loop kept a single `last_platform_warn_at` and any
/// non-platform error logged unconditionally; the consequence was that two
/// distinct platform failures within the suppression window collapsed to one
/// log line and AX-permission losses on top of pasteboard outages were
/// effectively invisible. Suppression is therefore per
/// [`CaptureErrorKind`] bucket.
struct CaptureFailurePolicy {
    /// Per-kind timestamp of the last emitted warn, for rate limiting.
    last_warn_at: [Option<Instant>; CaptureErrorKind::COUNT],
    /// Counter of suppressed warnings since the last emitted log line,
    /// reset on every emit. Surfaced as a tracing field so suppressed
    /// runs are still observable (the original cadence dropped them
    /// silently).
    suppressed_warns: [u32; CaptureErrorKind::COUNT],
    /// Number of consecutive `capture_once` failures (any kind) that
    /// we've observed. Drives the exponential backoff in
    /// `run_polling[_with_settings]` and resets to zero on the next
    /// successful tick.
    consecutive_failures: u32,
}

impl CaptureFailurePolicy {
    const fn new() -> Self {
        Self {
            last_warn_at: [None; CaptureErrorKind::COUNT],
            suppressed_warns: [0; CaptureErrorKind::COUNT],
            consecutive_failures: 0,
        }
    }

    /// Record a failed tick: bump the failure counter, emit (or suppress)
    /// the rate-limited warn for the error's kind, and reflect the error in
    /// the shared health snapshot when one is wired.
    fn note_error(&mut self, err: &AppError, health: Option<&CaptureHealth>) {
        // Track persistent failure for the polling-loop backoff. Any
        // tick that reaches `note_error` has, by definition, failed; the
        // counter is reset only on a successful `capture_once`.
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
        if let Some(health) = health {
            health.record_error(
                kind.capture_event_category(),
                err.to_string(),
                OffsetDateTime::now_utc(),
            );
        }
    }

    /// Record a successful tick: reset the backoff counter and reflect the
    /// success in the shared health snapshot when one is wired.
    fn note_success(&mut self, health: Option<&CaptureHealth>) {
        self.consecutive_failures = 0;
        if let Some(health) = health {
            health.record_success(OffsetDateTime::now_utc());
        }
    }

    /// The inter-tick sleep for the polling loop given the current failure
    /// streak: the jittered backoff below.
    fn next_sleep(&self, base: Duration) -> Duration {
        Self::jittered_backoff(base, self.consecutive_failures)
    }

    /// Compute the inter-tick sleep applied after a failed
    /// `capture_once`. Below `BACKOFF_AFTER_CONSECUTIVE_FAILURES` we
    /// keep the user-configured cadence; above it we apply an
    /// exponential backoff capped at `MAX_BACKOFF`. Deterministic so
    /// the ladder is easy to test; the polling loop adds jitter on top
    /// via [`Self::jittered_backoff`] before the actual sleep.
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

    /// Polling-loop wrapper around [`Self::backoff_for_failures`] that
    /// adds `±10%` jitter once the loop is actually backing off. Without
    /// it, every co-tenant daemon that crashes during a shared event
    /// (sleep wake, network re-attach, OS service restart) would retry
    /// in lockstep at exactly the next doubled interval. Jitter is only
    /// applied above [`BACKOFF_AFTER_CONSECUTIVE_FAILURES`] because the
    /// pre-backoff cadence is the user's configured capture interval —
    /// scrambling that would visibly slow first-tick capture. The
    /// post-jitter sleep is re-capped at [`MAX_BACKOFF`] so the ceiling
    /// stays a true hard limit (a saturated `+10%` swing would otherwise
    /// push a 30 s cap to 33 s).
    fn jittered_backoff(base: Duration, consecutive_failures: u32) -> Duration {
        let scaled = Self::backoff_for_failures(base, consecutive_failures);
        if consecutive_failures < BACKOFF_AFTER_CONSECUTIVE_FAILURES {
            return scaled;
        }
        Self::apply_jitter(scaled, jitter_entropy()).min(MAX_BACKOFF)
    }

    /// Pure jitter math, split out so tests can pin `entropy` and assert
    /// the result stays within `±10%` of the input.
    fn apply_jitter(d: Duration, entropy: u64) -> Duration {
        let nanos = u64::try_from(d.as_nanos()).unwrap_or(u64::MAX);
        let range = nanos / 10; // ±10%
        if range == 0 {
            return d;
        }
        // Map `entropy % span` (with span = 2*range + 1) into the symmetric
        // window `[-range, +range]` by translating up before subtracting,
        // avoiding any signed-integer intermediate.
        let span = range.saturating_mul(2).saturating_add(1);
        let offset_abs = entropy % span;
        let jittered = nanos.saturating_add(offset_abs).saturating_sub(range);
        Duration::from_nanos(jittered)
    }
}

/// Outcome of [`CaptureLoop::admit_entry`]'s policy / admission stage.
enum Admission {
    /// Entry passed every gate, was classified, and is ready for the
    /// durable insert. Boxed because `ClipboardEntry` carries the full
    /// payload and the enum would otherwise be its size on every return.
    Ready(Box<nagori_core::ClipboardEntry>),
    /// The loop intentionally dropped the clip (kind filter, size budget,
    /// policy block, secret drop, empty payload). The dedup state stays
    /// anchored — the clip is decided, not retried.
    Dropped,
}

pub struct CaptureLoop<R, E, A> {
    reader: R,
    entries: E,
    audit: A,
    /// `Arc` so the per-tick snapshot in `capture_once_at` is a refcount bump
    /// rather than a deep clone of the whole `AppSettings` (denylist `Vec`s
    /// included) on every poll.
    settings: Arc<AppSettings>,
    /// Classifier rebuilt only when settings change, not on every admitted
    /// clip — building it recompiles every `regex_denylist` pattern, so doing
    /// it per capture made a steady copy stream pay that cost repeatedly. `Err`
    /// preserves the fail-closed contract: while a denylist pattern won't
    /// compile, no clip can be admitted (the caller rolls back and retries),
    /// because silently dropping the broken rule would let secret matches the
    /// user asked us to redact slip into history.
    classifier: std::result::Result<SensitivityClassifier, String>,
    /// Dedup / baseline state. Grouped so the optimistic refresh and its
    /// failure rollback stay a single operation instead of three hand-kept
    /// fields — see [`DedupState`].
    dedup: DedupState,
    window: Option<Arc<dyn WindowBehavior>>,
    /// Failure reporting + backoff pacing for the polling loop — see
    /// [`CaptureFailurePolicy`].
    failures: CaptureFailurePolicy,
    /// Number of consecutive `frontmost_focused_is_secure` errors. Once
    /// this crosses `SECURE_FOCUS_FAIL_CLOSED_THRESHOLD` the loop flips
    /// to fail-closed (assume the focus is secure) so a sustained AX
    /// outage can't silently let password keystrokes through. Reset on
    /// the next successful AX query.
    consecutive_secure_ax_failures: u32,
    search_cache: Option<SharedSearchCache>,
    /// Wall-clock anchor for the previous `capture_once` invocation. Used to
    /// spot host-paused gaps (sleep / suspend) and resync the dedup baseline.
    /// `SystemTime` rather than `Instant` because Darwin's `Instant` is
    /// `CLOCK_UPTIME_RAW` and freezes during sleep — see the
    /// `RESYNC_GAP_THRESHOLD` doc comment for details.
    last_tick_at: Option<SystemTime>,
    /// When `false`, sustained AX errors no longer flip the loop to
    /// fail-closed: the loop keeps treating an AX-errored tick as
    /// "unknown → not secure" indefinitely. Production runs leave this
    /// `true` (the default) so that a revoked Accessibility grant or a
    /// wedged AX subsystem can't silently let password keystrokes through
    /// history. Test harnesses where Accessibility can't be granted
    /// programmatically (notably `scripts/e2e-macos.sh` running against a
    /// freshly built binary) flip it off so the rest of the capture
    /// pipeline can be exercised end-to-end. The bundle-id override list
    /// still fires regardless: those system password UIs are positively
    /// identified, not assumed.
    secure_focus_fail_closed_enabled: bool,
    /// Optional shared snapshot for steady-state capture health. When
    /// wired (production via `with_capture_health`), every `capture_once`
    /// outcome — success, intentional drop, error — is reflected here so
    /// `nagori doctor` and the desktop tray can flag a silently filtering
    /// loop. `None` in unit tests keeps the loop independent of the
    /// daemon's shared-state plumbing.
    capture_health: Option<CaptureHealth>,
    /// Optional hook invoked after a new entry has been durably inserted.
    /// Desktop uses this to wake the palette without coupling the daemon
    /// crate to Tauri; CLI/server callers leave it unset.
    capture_notifier: Option<Arc<dyn Fn(EntryId) + Send + Sync>>,
}

impl<R, E, A> CaptureLoop<R, E, A>
where
    R: ClipboardReader,
    E: EntryRepository,
    A: AuditLog,
{
    pub fn new(reader: R, entries: E, audit: A, settings: AppSettings) -> Self {
        let classifier = build_classifier(&settings);
        Self {
            reader,
            entries,
            audit,
            settings: Arc::new(settings),
            classifier,
            dedup: DedupState::new(),
            window: None,
            failures: CaptureFailurePolicy::new(),
            consecutive_secure_ax_failures: 0,
            search_cache: None,
            last_tick_at: None,
            secure_focus_fail_closed_enabled: true,
            capture_health: None,
            capture_notifier: None,
        }
    }

    /// Disable the AX-error fail-closed escalation. See the field doc on
    /// `secure_focus_fail_closed_enabled` for the production vs. test
    /// trade-off; the bundle-id override list still applies.
    #[must_use]
    pub const fn without_secure_focus_fail_closed(mut self) -> Self {
        self.secure_focus_fail_closed_enabled = false;
        self
    }

    /// Reset the dedup baseline so the next observed sequence is treated as
    /// fresh content. Useful after macOS sleep/wake when the pasteboard
    /// counter can lap silently and we'd otherwise skip a real change as a
    /// duplicate.
    pub fn reset_sequence_baseline(&mut self) {
        self.dedup.last_sequence = None;
    }

    fn note_capture_error(&mut self, err: &AppError) {
        self.failures.note_error(err, self.capture_health.as_ref());
    }

    fn note_capture_success(&mut self) {
        self.failures.note_success(self.capture_health.as_ref());
    }

    /// Record an intentional in-loop drop (oversized payload, policy /
    /// secret refusal). Does not bump the failure counter — the loop did
    /// its job — but updates the shared `CaptureHealth` snapshot so the
    /// UI can distinguish "we lost visibility" from "we're rejecting on
    /// purpose".
    fn note_capture_drop(&self, category: CaptureEventCategory) {
        if let Some(health) = &self.capture_health {
            health.record_drop(category, OffsetDateTime::now_utc());
        }
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

    /// Wire a shared [`CaptureHealth`] handle so every `capture_once`
    /// outcome is reflected in the daemon's per-tick health snapshot.
    /// Production callers (`serve/lifecycle.rs` for the daemon, `state/startup.rs` for the
    /// desktop) wire this from `runtime.capture_health()` so the
    /// `nagori doctor` capture row and the desktop tray see one source
    /// of truth; tests omit it and exercise the capture path in
    /// isolation.
    #[must_use]
    pub fn with_capture_health(mut self, health: CaptureHealth) -> Self {
        self.capture_health = Some(health);
        self
    }

    /// Wire a callback that runs after every successful capture insert.
    #[must_use]
    pub fn with_capture_notifier(mut self, notifier: Arc<dyn Fn(EntryId) + Send + Sync>) -> Self {
        self.capture_notifier = Some(notifier);
        self
    }

    pub fn update_settings(&mut self, settings: AppSettings) {
        // Rebuild the cached classifier in lockstep with the settings snapshot
        // so admission always classifies against the live `regex_denylist` /
        // `app_denylist` — and never recompiles those patterns on the capture
        // hot path.
        self.classifier = build_classifier(&settings);
        self.settings = Arc::new(settings);
    }

    pub async fn capture_once(&mut self) -> Result<Option<EntryId>> {
        self.capture_once_at(SystemTime::now()).await
    }

    /// Detect a host-paused gap (sleep / suspend / lid close) since the
    /// previous tick. We do not clear `last_sequence` here — clearing the
    /// baseline outright would re-capture an unchanged pre-launch clipboard
    /// once `capture_initial_clipboard_on_launch=false` had already
    /// discarded it. Instead, arm a one-shot `force_content_check` flag that
    /// makes the next tick's dedup decision content-aware.
    async fn detect_wake_gap(&mut self, now: SystemTime) {
        if let Some(prev) = self.last_tick_at {
            // `duration_since` is `Err` if the wall clock was rolled back
            // (NTP step backwards, manual change). Treat that as zero gap
            // rather than a wake signal — the user changing their clock is
            // not a sleep cycle.
            let gap = now.duration_since(prev).unwrap_or(Duration::ZERO);
            if gap >= RESYNC_GAP_THRESHOLD {
                let gap_secs = gap.as_secs();
                info!(gap_secs, "wake_gap_resync");
                // Persist the resync in `audit_events` so a support
                // investigation into "why did my clip not get captured
                // after lunch" can correlate the missing entry with
                // either a sleep cycle (legitimate) or an NTP forward
                // jump (spurious force_content_check). Failure to log
                // is non-fatal — the resync still proceeds.
                let _ = self
                    .audit
                    .record(
                        "wake_gap_resync",
                        None,
                        Some(&format!("gap_secs={gap_secs}")),
                    )
                    .await;
                self.dedup.force_content_check = true;
            }
        }
        self.last_tick_at = Some(now);
    }

    /// Anchor the dedup baseline to whatever was on the clipboard before
    /// launch, without capturing it.
    ///
    /// Reads through the bounded path rather than the unbounded
    /// `current_snapshot`, so a huge pre-launch text/image isn't fully
    /// materialised just to seed the dedup state and blow up startup
    /// latency / memory. Bound it by the internal hard limit
    /// (`MAX_ENTRY_SIZE_BYTES`), *not* the live `max_entry_size_bytes`:
    /// since the setting is validated to never exceed the hard limit, any
    /// clip that could ever become capturable (even after the user later
    /// raises the setting) is within this read and gets its dedup hash
    /// anchored here. Without that, a baseline left unhashed because it
    /// was over the *current* setting would be re-captured on a post-raise
    /// wake-resync instead of being recognised as the pre-launch clip.
    /// `current_snapshot_with_max` also enforces the internal
    /// decoded-pixel cap, so a forged-dimension image can't OOM the probe.
    ///
    /// `pristine` flips only after the snapshot read succeeds — a transient
    /// platform error propagates first, keeping us in the pristine state so
    /// the next tick retries instead of stranding the loop with no baseline.
    async fn seed_pristine_baseline(&mut self) -> Result<()> {
        match self
            .reader
            .current_snapshot_with_max(MAX_ENTRY_SIZE_BYTES)
            .await?
        {
            CapturedSnapshot::Captured(snapshot) => {
                self.dedup.last_sequence = Some(snapshot.sequence.clone());
                if let Some(entry) = EntryFactory::from_snapshot(snapshot) {
                    self.dedup.last_content_hash = Some(effective_dedupe_hash(&entry));
                }
            }
            CapturedSnapshot::Oversized { sequence, .. } => {
                // Larger than the hard limit, so it can never be captured
                // under any setting — there's no body worth hashing. Anchor
                // the sequence so the next poll skips it without re-probing;
                // a later wake-resync re-reads through the bounded steady-state
                // path, hits the same oversize guard, and skips it again.
                self.dedup.last_sequence = Some(sequence);
            }
            CapturedSnapshot::Excluded { sequence, .. } => {
                // The pre-launch clipboard carries an owner exclusion marker
                // (concealed / transient), so its body was never read and
                // there is nothing to hash. Anchor the sequence like the
                // oversized case so the next poll skips it without re-probing.
                self.dedup.last_sequence = Some(sequence);
            }
        }
        self.dedup.pristine = false;
        Ok(())
    }

    /// Resolve the frontmost app and whether a secure text field has focus.
    ///
    /// Runs both AX queries concurrently — each spawns its own system-wide
    /// AX walk via `spawn_blocking`, so the wall-clock cost is parallel
    /// rather than additive on the per-tick hot path.
    ///
    /// A *single* error from `frontmost_focused_is_secure` degrades to
    /// `false` so a transient FFI hiccup or in-flight permission grant
    /// doesn't strand the capture loop; the `SensitivityClassifier` secret
    /// detector and password-manager bundle denylist still run downstream
    /// as the second line of defence. But a *sustained* run of AX failures
    /// means we've genuinely lost visibility, and the safer default at that
    /// point is to fail closed and skip the next clip — see
    /// `SECURE_FOCUS_FAIL_CLOSED_THRESHOLD`. Likewise, a frontmost bundle
    /// id matching `SECURE_FOCUS_BUNDLE_OVERRIDES` (system password UIs)
    /// forces secure regardless of the AX result, so we don't depend on AX
    /// accurately reporting on windows whose entire purpose is to defeat
    /// keyloggers.
    async fn resolve_secure_focus(&mut self) -> (Option<nagori_core::SourceApp>, bool) {
        let Some(window) = &self.window else {
            return (None, false);
        };
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
                let ax_threshold_tripped = self.secure_focus_fail_closed_enabled
                    && self.consecutive_secure_ax_failures >= SECURE_FOCUS_FAIL_CLOSED_THRESHOLD;
                if ax_threshold_tripped || bundle_override {
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
    }

    /// Honour an owner-declared exclusion marker (nspasteboard.org Concealed /
    /// Transient) surfaced by the bounded read.
    ///
    /// The adapter detected the marker before reading the body, so no secret
    /// reached us; we record the skip reason and count it as an intentional
    /// policy drop — not a lost-visibility failure — then anchor the dedup
    /// sequence so the next poll doesn't re-probe the same marked clip.
    async fn skip_owner_exclusion(
        &mut self,
        sequence: ClipboardSequence,
        kind: ClipboardExclusionKind,
    ) {
        let reason = match kind {
            ClipboardExclusionKind::Concealed => "concealed_marker",
            ClipboardExclusionKind::Transient => "transient_marker",
        };
        info!(?kind, "capture_skipped reason=clipboard_exclusion");
        let _ = self
            .audit
            .record("capture_skipped", None, Some(reason))
            .await;
        self.note_capture_drop(CaptureEventCategory::Policy);
        self.dedup.force_content_check = false;
        self.dedup.pristine = false;
        self.dedup.last_sequence = Some(sequence);
    }

    /// Run the admission gates and policy classification for one
    /// snapshot-derived entry.
    ///
    /// Covers, in order: the `capture_kinds` filter, the storage-payload
    /// size budget, sensitivity classification (fail closed if the persisted
    /// `regex_denylist` contains an uncompilable pattern — silently dropping
    /// it would let secret matches the user explicitly asked us to redact
    /// slip into history), the Blocked / secret-drop refusals, the
    /// alternatives trim, and the image-dimension probe. Intentional drops
    /// resolve to [`Admission::Dropped`] with the dedup state left anchored
    /// (the clip is decided); only a config failure propagates as `Err`, and
    /// the caller owns the dedup rollback for that case.
    async fn admit_entry(
        &self,
        mut entry: nagori_core::ClipboardEntry,
        settings: &AppSettings,
    ) -> Result<Admission> {
        if !settings.capture_kinds.contains(&entry.content_kind()) {
            info!(kind = ?entry.content_kind(), "capture_skipped reason=kind_disabled");
            let _ = self
                .audit
                .record("capture_skipped", Some(entry.id), Some("kind_disabled"))
                .await;
            self.note_capture_drop(CaptureEventCategory::Policy);
            return Ok(Admission::Dropped);
        }
        // Size each entry by the bytes that will actually land in storage,
        // not the plain-text projection. RichText's primary is HTML/RTF
        // markup, so a large markup body with short plain text used to slip
        // past this guard and write an oversized primary representation
        // row. Image entries don't carry plain text either, so size them by
        // the captured byte payload. Synthesised entries that never built a
        // representation set (CLI `add_text`, post-secret-clear rows) keep
        // the legacy plain-text length so existing oversize semantics hold.
        let payload_bytes = match &entry.content {
            ClipboardContent::Image(img) => img.byte_count,
            _ => entry.pending_representations.first().map_or_else(
                || entry.plain_text().map_or(0, str::len),
                StoredClipboardRepresentation::byte_count,
            ),
        };
        if payload_bytes == 0 {
            return Ok(Admission::Dropped);
        }
        if payload_bytes > settings.max_entry_size_bytes {
            warn!(bytes = payload_bytes, "capture_skipped reason=oversized");
            let _ = self
                .audit
                .record("capture_skipped", Some(entry.id), Some("oversized"))
                .await;
            self.note_capture_drop(CaptureEventCategory::OversizedDrop);
            return Ok(Admission::Dropped);
        }
        // Use the classifier cached at the last settings change rather than
        // recompiling the `regex_denylist` for every admitted clip. A cached
        // build failure (an uncompilable pattern) fails closed exactly as the
        // per-call `try_new` did: the clip is not admitted, the caller rolls
        // back the dedup state, and the next tick retries.
        let classifier = self
            .classifier
            .as_ref()
            .map_err(|message| AppError::Policy(message.clone()))?;
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
            self.note_capture_drop(CaptureEventCategory::Policy);
            return Ok(Admission::Dropped);
        }
        if settings.block_sensitive_captures
            && matches!(
                classification.sensitivity,
                Sensitivity::Private | Sensitivity::Secret
            )
        {
            info!(sensitivity = ?classification.sensitivity, "sensitive_blocked");
            let _ = self
                .audit
                .record(
                    "sensitive_blocked",
                    Some(entry.id),
                    Some(&format!("{:?}", classification.reasons)),
                )
                .await;
            self.note_capture_drop(CaptureEventCategory::Policy);
            return Ok(Admission::Dropped);
        }
        if let Some(preview) = classification.redacted_preview {
            entry.search.preview = preview;
        }
        if matches!(
            classifier.apply_secret_handling(&mut entry, settings.secret_handling),
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
            self.note_capture_drop(CaptureEventCategory::Policy);
            return Ok(Admission::Dropped);
        }

        // Secret entries had their `pending_representations` dropped (and the
        // set hash realigned to the primary) inside `apply_secret_handling`,
        // so the source's HTML / RTF / plain alternatives can no longer leak
        // the raw secret here. Non-secret entries keep their alternatives.

        // Enforce the user's `max_entry_size_bytes` budget across the full
        // representation set, not just the primary. The pre-classify guard
        // above already rejected entries whose primary exceeds the cap, so
        // here we only have to trim alternatives; when anything is dropped
        // the set hash has to be recomputed so dedupe matches what storage
        // actually wrote.
        if entry.trim_alternatives_to_budget(settings.max_entry_size_bytes) {
            let new_hash = compute_representation_set_hash(&entry.pending_representations);
            entry.metadata.representation_set_hash = Some(new_hash);
        }

        // Record image pixel dimensions from a header-only probe so the
        // preview pane and result rows can show "1920×1080" without decoding
        // the full payload. Done here (just before insert) so dropped clips
        // never pay for the probe; the factory leaves `width`/`height` `None`
        // because `nagori-core` deliberately has no `image` dependency.
        if let ClipboardContent::Image(image) = &mut entry.content
            && image.width.is_none()
            && let Some(bytes) = image.pending_bytes.as_deref()
            && let Some((w, h)) = probe_image_dimensions(bytes)
        {
            image.width = Some(w);
            image.height = Some(h);
        }

        Ok(Admission::Ready(Box::new(entry)))
    }

    /// Test seam for `capture_once` that lets the caller pin the wall-clock
    /// "now" used for gap detection. Production callers should use
    /// `capture_once`; tests use this to simulate sleep gaps without driving
    /// real time.
    pub async fn capture_once_at(&mut self, now: SystemTime) -> Result<Option<EntryId>> {
        // Snapshot settings at the start of the tick. Today the polling
        // loop's `tokio::select!` already serialises `update_settings`
        // and `capture_once`, so settings can't actually change between
        // the `capture_enabled` check at the top and the secret-handling
        // check at the bottom. But a future refactor that adds an extra
        // `.await` boundary, or moves capture into its own task, would
        // re-open that race — and the consequence is observably wrong:
        // a tick could read the *new* `max_entry_size_bytes` for the
        // pre-read cap and the *old* `capture_kinds` for the post-read
        // filter, producing inconsistent admission decisions for one
        // clip. Take one snapshot and use it everywhere — an `Arc` bump,
        // so this is cheap to do every poll.
        let settings = Arc::clone(&self.settings);

        self.detect_wake_gap(now).await;

        if !settings.capture_enabled {
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
        let sequence = self
            .reader
            .current_sequence_with_max(settings.max_entry_size_bytes)
            .await?;
        // Peek without consuming. We only clear `force_content_check` after
        // the body read succeeds — otherwise a transient `current_snapshot`
        // failure between the gap-detection tick and the actual recheck
        // would drop the flag, and the next tick would dedup-skip the
        // colliding sequence again. Re-trying with the flag still set is
        // safe because the body-read path is idempotent.
        let force_content_check = self.dedup.force_content_check;
        if !force_content_check && self.dedup.last_sequence.as_ref() == Some(&sequence) {
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
        if self.dedup.pristine && !settings.capture_initial_clipboard_on_launch {
            self.seed_pristine_baseline().await?;
            return Ok(None);
        }
        let (frontmost_source, secure_focus) = self.resolve_secure_focus().await;

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
            self.dedup.last_sequence = Some(sequence);
            self.dedup.pristine = false;
            return Ok(None);
        }

        let mut snapshot = match self
            .reader
            .current_snapshot_with_max(settings.max_entry_size_bytes)
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
                self.note_capture_drop(CaptureEventCategory::OversizedDrop);
                // Anchor the sequence so the next poll skips this same
                // oversized clip without re-probing pasteboard sizes.
                // (Equivalent to `begin_clip` minus the rollback handle —
                // an oversized pre-read is a decided outcome, never undone.)
                self.dedup.force_content_check = false;
                self.dedup.pristine = false;
                self.dedup.last_sequence = Some(sequence);
                return Ok(None);
            }
            CapturedSnapshot::Excluded { sequence, kind } => {
                // The clipboard owner published a "do not record" marker and
                // the adapter skipped the body read — honour the contract.
                self.skip_owner_exclusion(sequence, kind).await;
                return Ok(None);
            }
        };
        // Snapshot succeeded — only now is it safe to consume the wake-gap
        // flag and flip pristine. `begin_clip` refreshes the dedup state
        // *optimistically* so the paths below all observe it and hands back
        // the pre-clip snapshot; the failure paths below (uncompilable
        // denylist regex, durable insert hitting DB busy / disk full) feed it
        // to `DedupState::rollback` so the next tick re-reads and retries the
        // same clip instead of dedup-skipping it and losing it forever.
        let rollback = self
            .dedup
            .begin_clip(snapshot.sequence.clone(), force_content_check);
        if snapshot.source.is_none() {
            snapshot.source = frontmost_source;
        }

        let Some(entry) = EntryFactory::from_snapshot(snapshot) else {
            // An empty snapshot can mean we read the pasteboard mid-write: an
            // external writer's `clearContents()` advanced the changeCount but
            // the following `writeObjects()` / `setData` hasn't landed yet, and
            // on macOS the whole clear-then-write is a *single* changeCount
            // bump. We anchored `last_sequence` to that changeCount above, so
            // without intervention the next tick would dedup-skip the real
            // content that lands at the *same* changeCount and the clip would
            // be stranded until some later, unrelated copy. Arm the one-shot
            // body re-read so the following tick re-examines the sequence
            // instead of trusting the dedup; it clears as soon as a snapshot
            // yields content (or stays armed harmlessly while the clipboard is
            // genuinely empty).
            self.dedup.force_content_check = true;
            return Ok(None);
        };
        // Wake-gap content cross-check: if a sleep gap forced the body read
        // and the resulting hash matches the last captured content, treat
        // the changeCount nudge as spurious and skip without inserting.
        // Compare via the same representation-set hash the storage layer
        // uses for dedupe so a snapshot whose primary text matches the
        // last copy but whose HTML/RTF alternatives differ is still
        // forwarded — otherwise storage never gets a chance to record
        // it as a distinct row. Refresh `last_content_hash` either way
        // so subsequent gaps still have something to compare against.
        let dedupe_hash = effective_dedupe_hash(&entry);
        if force_content_check
            && self.dedup.last_content_hash.as_deref() == Some(dedupe_hash.as_str())
        {
            return Ok(None);
        }
        self.dedup.last_content_hash = Some(dedupe_hash);

        let entry = match self.admit_entry(entry, &settings).await {
            Ok(Admission::Ready(entry)) => *entry,
            Ok(Admission::Dropped) => return Ok(None),
            Err(err) => {
                // A denylist regex that won't compile is a config failure,
                // not a decided outcome for this clip. Roll the dedup state
                // back so the next tick re-reads and retries rather than
                // treating the clip as already-seen and dropping it.
                self.dedup.rollback(rollback);
                return Err(err);
            }
        };

        let id = self.persist_entry(entry, rollback).await?;
        Ok(Some(id))
    }

    /// Durably insert an admitted entry and fan out the post-insert
    /// notifications (search-cache invalidation, capture notifier).
    ///
    /// Invalidates the search cache before *and* after the insert. Without
    /// the pre-call, a concurrent `runtime.search()` could lock the cache
    /// between `SQLite` commit and our post-invalidate and serve a
    /// pre-insert hit even though the new row is already durable.
    async fn persist_entry(
        &mut self,
        entry: nagori_core::ClipboardEntry,
        rollback: DedupRollback,
    ) -> Result<EntryId> {
        if let Some(cache) = &self.search_cache {
            lock_or_recover(cache).invalidate();
        }
        let id = match self.entries.insert(entry).await {
            Ok(id) => id,
            Err(err) => {
                // Durable insert failed (DB busy, disk full, …). Restore the
                // full pre-clip dedup state so the next tick re-reads and
                // retries this clip instead of dedup-skipping it and dropping
                // it permanently — including the empty-snapshot one-shot case
                // where the content lands at the same changeCount and the
                // retry needs `force_content_check` re-armed.
                self.dedup.rollback(rollback);
                return Err(err);
            }
        };
        info!(entry_id = %id, "entry_inserted");
        if let Some(cache) = &self.search_cache {
            lock_or_recover(cache).invalidate();
        }
        if let Some(notifier) = &self.capture_notifier {
            // A panic from the desktop-side hook (e.g. a Tauri emit on a
            // torn-down app handle) must not kill the capture loop —
            // notification is auxiliary; durable insert already succeeded.
            let notifier = Arc::clone(notifier);
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || notifier(id))).is_err()
            {
                tracing::warn!(entry_id = %id, "capture_notifier_panicked");
            }
        }
        Ok(id)
    }

    pub async fn run_polling(
        &mut self,
        interval: std::time::Duration,
        shutdown: impl std::future::Future<Output = ()>,
    ) -> Result<()> {
        tokio::pin!(shutdown);
        loop {
            let sleep_for = self.failures.next_sleep(interval);
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
            let sleep_for = self.failures.next_sleep(interval);
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

/// Build the cached [`SensitivityClassifier`] for the current settings,
/// collapsing the error to its message so the loop can hold it across ticks
/// ([`AppError`] is not `Clone`). `try_new` only ever fails with
/// [`AppError::Policy`] (an uncompilable `regex_denylist` pattern), so
/// `admit_entry` faithfully reconstructs that variant from the stored message.
fn build_classifier(settings: &AppSettings) -> std::result::Result<SensitivityClassifier, String> {
    SensitivityClassifier::try_new(settings.clone()).map_err(|err| err.to_string())
}

/// Effective dedupe key for the wake-gap cross-check.
///
/// Approximates the storage layer's `representation_set_hash` dedupe:
/// when the entry has a captured representation set, use its set hash;
/// otherwise (CLI `add_text`, synthesised rows) fall back to the
/// primary content hash. Without this fallback agreement, the loop
/// would key on `content_hash` while storage keys on
/// `representation_set_hash` and a snapshot whose alternatives differ
/// from the last copy would be silently skipped before storage saw it.
///
/// The hash is sampled before downstream secret-classification (which
/// may clear alternatives and reset `representation_set_hash` back to
/// `content_hash`) and the budget trim (which recomputes the set
/// hash). That's intentional for the wake-gap check — we want to
/// detect "the snapshot the user actually copied looks identical to
/// the last one we captured" using the as-captured representation set,
/// not the post-policy projection. The persisted dedupe key in storage
/// may therefore differ for Secret / over-budget entries; the storage
/// layer's own dedupe handles the persisted side.
fn effective_dedupe_hash(entry: &nagori_core::ClipboardEntry) -> String {
    entry.metadata.representation_set_hash.as_ref().map_or_else(
        || entry.metadata.content_hash.value.clone(),
        |hash| hash.value.clone(),
    )
}

/// Read an encoded image's pixel dimensions from its header alone.
///
/// Header-only (`into_dimensions`) so even a multi-MB screenshot costs a few
/// bytes, not a full decode. Returns `None` for formats `image` can't parse a
/// header for, or for a payload whose advertised canvas exceeds
/// [`MAX_DECODED_IMAGE_PIXELS`] — the same forged-dimension guard the
/// thumbnail pipeline applies, so a bogus IHDR can't poison the stored
/// metadata. Capture proceeds with `None` dimensions on any failure.
fn probe_image_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    use std::io::Cursor;

    use image::ImageReader;

    let (width, height) = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?
        .into_dimensions()
        .ok()?;
    let pixels = u64::from(width).saturating_mul(u64::from(height));
    if pixels == 0 || pixels > nagori_core::MAX_DECODED_IMAGE_PIXELS {
        return None;
    }
    Some((width, height))
}

#[cfg(test)]
mod tests;

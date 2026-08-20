//! Tests for the clipboard lease: one publish-then-paste operation at a time,
//! and no keystroke once the clip that was published is no longer the clip the
//! OS would paste.

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering},
};
use std::time::{Duration, Instant, SystemTime};

use async_trait::async_trait;
use nagori_core::{
    AppError, ClipboardEntry, ClipboardSequence, ClipboardSnapshot, PasteFailureReason,
    PasteFormat, Result, SettingsRepository, StoredClipboardRepresentation,
};
use nagori_platform::{
    ClipboardReader, ClipboardWriter, PasteResult, PreparedClipboardWrite, SelfWriteTracker,
    SelfWriteTracking,
};
use time::OffsetDateTime;

use super::super::*;

/// A clipboard that behaves like a native adapter for the purposes of the
/// lease: it holds one text, records the order of writes, bumps a monotonic
/// sequence on every write, and remembers the sequence its own last write
/// produced (so `matches_self_write` is authoritative, as it is on macOS /
/// Windows). [`Self::simulate_external_copy`] is the third writer the lease
/// cannot lock out — a user copying in another app.
#[derive(Debug, Default)]
struct FakeNativeClipboard {
    current: Mutex<Option<String>>,
    /// Every publish, with the instant it landed — the timing tests measure the
    /// lease hold from these.
    writes: Mutex<Vec<(String, Instant)>>,
    sequence: AtomicI64,
    self_write: SelfWriteTracker,
    probe_fails: AtomicBool,
    probe_gated: AtomicBool,
    probe_entered: tokio::sync::Notify,
    probe_release: tokio::sync::Notify,
    preparation_gated: AtomicBool,
    preparation_entered: tokio::sync::Notify,
    preparation_release: tokio::sync::Notify,
    publish_gated: AtomicBool,
    publish_entered: tokio::sync::Notify,
    publish_release: tokio::sync::Notify,
    sequence_may_lap: AtomicBool,
}

impl FakeNativeClipboard {
    fn current_text(&self) -> Option<String> {
        self.current.lock().unwrap().clone()
    }

    fn writes(&self) -> Vec<String> {
        self.writes
            .lock()
            .unwrap()
            .iter()
            .map(|(text, _)| text.clone())
            .collect()
    }

    /// When the `index`-th publish landed.
    fn write_instant(&self, index: usize) -> Instant {
        self.writes.lock().unwrap()[index].1
    }

    /// Make every sequence probe fail, standing in for a clipboard too wedged
    /// to answer (an adapter read that timed out).
    fn fail_sequence_probe(&self) {
        self.probe_fails.store(true, Ordering::SeqCst);
    }

    /// Hold every sequence probe until released, so a test can cancel a request
    /// while it is mid-probe.
    fn gate_sequence_probe(&self) {
        self.probe_gated.store(true, Ordering::SeqCst);
    }

    fn gate_preparation(&self) {
        self.preparation_gated.store(true, Ordering::SeqCst);
    }

    fn finish_preparation(&self) {
        self.preparation_gated.store(false, Ordering::SeqCst);
        self.preparation_release.notify_waiters();
    }

    fn gate_publish(&self) {
        self.publish_gated.store(true, Ordering::SeqCst);
    }

    fn finish_publish(&self) {
        self.publish_gated.store(false, Ordering::SeqCst);
        self.publish_release.notify_waiters();
    }

    fn allow_sequence_lap(&self) {
        self.sequence_may_lap.store(true, Ordering::SeqCst);
    }

    /// Another app puts its own clip on the board: the sequence moves but no
    /// self-write is recorded, exactly as the OS would report it.
    fn simulate_external_copy(&self, text: &str) {
        *self.current.lock().unwrap() = Some(text.to_owned());
        self.sequence.fetch_add(1, Ordering::SeqCst);
    }

    /// Model a post-wake macOS write whose lapped `changeCount` happens to
    /// equal the sequence recorded for our pre-sleep publish.
    fn simulate_lapped_external_copy(&self, text: &str) {
        *self.current.lock().unwrap() = Some(text.to_owned());
    }

    async fn wait_for_preparation(&self) {
        if self.preparation_gated.load(Ordering::SeqCst) {
            self.preparation_entered.notify_one();
            self.preparation_release.notified().await;
        }
    }

    fn publish(&self, text: String) {
        self.writes
            .lock()
            .unwrap()
            .push((text.clone(), Instant::now()));
        *self.current.lock().unwrap() = Some(text);
        let sequence = self.sequence.fetch_add(1, Ordering::SeqCst) + 1;
        self.self_write.record(ClipboardSequence::native(sequence));
    }

    async fn publish_when_released(&self, text: String) {
        if self.publish_gated.load(Ordering::SeqCst) {
            self.publish_entered.notify_one();
            self.publish_release.notified().await;
        }
        self.publish(text);
    }
}

#[async_trait]
impl ClipboardWriter for FakeNativeClipboard {
    async fn write_entry(&self, entry: &ClipboardEntry) -> Result<()> {
        let text = entry
            .plain_text()
            .ok_or_else(|| AppError::Unsupported("non-text clipboard entry".to_owned()))?;
        self.publish_when_released(text.to_owned()).await;
        Ok(())
    }

    async fn write_plain(&self, entry: &ClipboardEntry) -> Result<()> {
        self.write_entry(entry).await
    }

    async fn write_text(&self, text: &str) -> Result<()> {
        self.publish_when_released(text.to_owned()).await;
        Ok(())
    }

    async fn write_representations(
        &self,
        entry: &ClipboardEntry,
        _representations: &[StoredClipboardRepresentation],
    ) -> Result<()> {
        self.write_entry(entry).await
    }

    async fn prepare_entry(
        self: Arc<Self>,
        entry: ClipboardEntry,
    ) -> Result<PreparedClipboardWrite> {
        self.wait_for_preparation().await;
        Ok(PreparedClipboardWrite::new(async move {
            self.write_entry(&entry).await
        }))
    }

    async fn prepare_plain(
        self: Arc<Self>,
        entry: ClipboardEntry,
    ) -> Result<PreparedClipboardWrite> {
        self.wait_for_preparation().await;
        Ok(PreparedClipboardWrite::new(async move {
            self.write_plain(&entry).await
        }))
    }

    async fn prepare_representations(
        self: Arc<Self>,
        entry: ClipboardEntry,
        representations: Vec<StoredClipboardRepresentation>,
    ) -> Result<PreparedClipboardWrite> {
        self.wait_for_preparation().await;
        Ok(PreparedClipboardWrite::new(async move {
            self.write_representations(&entry, &representations).await
        }))
    }
}

#[async_trait]
impl ClipboardReader for FakeNativeClipboard {
    async fn current_snapshot(&self) -> Result<ClipboardSnapshot> {
        Ok(ClipboardSnapshot {
            sequence: ClipboardSequence::native(self.sequence.load(Ordering::SeqCst)),
            captured_at: OffsetDateTime::now_utc(),
            source: None,
            representations: Vec::new(),
        })
    }

    async fn current_sequence(&self) -> Result<ClipboardSequence> {
        if self.probe_gated.load(Ordering::SeqCst) {
            self.probe_entered.notify_one();
            self.probe_release.notified().await;
        }
        if self.probe_fails.load(Ordering::SeqCst) {
            return Err(AppError::Platform("clipboard probe timed out".to_owned()));
        }
        Ok(ClipboardSequence::native(
            self.sequence.load(Ordering::SeqCst),
        ))
    }

    fn matches_self_write(&self, sequence: &ClipboardSequence) -> bool {
        self.self_write.matches(sequence)
    }

    fn self_write_tracking(&self) -> SelfWriteTracking {
        if self.sequence_may_lap.load(Ordering::SeqCst) {
            SelfWriteTracking::MayLapAfterHostPause
        } else {
            SelfWriteTracking::Stable
        }
    }
}

/// A paste controller that records what was on the clipboard at the moment the
/// keystroke fired, and can be held there until the test releases it — the
/// barrier that lets a second request try to interleave.
#[derive(Debug)]
struct ObservingPaste {
    clipboard: Arc<FakeNativeClipboard>,
    observed: Mutex<Vec<Option<String>>>,
    calls: AtomicUsize,
    /// When the controller last returned — the point past which the lease is
    /// still deliberately held (the target app has not read the clipboard yet).
    returned_at: Mutex<Option<Instant>>,
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
    /// Whether the controller waits for [`Self::release`] before returning.
    gated: bool,
    /// Report `pasted: false`, i.e. a synthesis that may already have posted
    /// part of the keystroke before failing.
    fails: AtomicBool,
}

impl ObservingPaste {
    fn new(clipboard: Arc<FakeNativeClipboard>, gated: bool) -> Self {
        Self {
            clipboard,
            observed: Mutex::new(Vec::new()),
            calls: AtomicUsize::new(0),
            returned_at: Mutex::new(None),
            entered: tokio::sync::Notify::new(),
            release: tokio::sync::Notify::new(),
            gated,
            fails: AtomicBool::new(false),
        }
    }

    fn fail_synthesis(&self) {
        self.fails.store(true, Ordering::SeqCst);
    }

    fn returned_at(&self) -> Option<Instant> {
        *self.returned_at.lock().unwrap()
    }

    fn observed(&self) -> Vec<Option<String>> {
        self.observed.lock().unwrap().clone()
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl PasteController for ObservingPaste {
    async fn paste_frontmost(&self) -> Result<PasteResult> {
        self.entered.notify_one();
        if self.gated {
            self.release.notified().await;
        }
        // Sample *after* the gate: this stands in for the instant the OS
        // keystroke lands, which is what a competing publish would corrupt.
        self.observed
            .lock()
            .unwrap()
            .push(self.clipboard.current_text());
        self.calls.fetch_add(1, Ordering::SeqCst);
        *self.returned_at.lock().unwrap() = Some(Instant::now());
        Ok(PasteResult {
            pasted: !self.fails.load(Ordering::SeqCst),
            message: None,
        })
    }
}

fn runtime_with_fake_native() -> (NagoriRuntime, Arc<FakeNativeClipboard>, Arc<ObservingPaste>) {
    runtime_with_fake_native_gated(false)
}

fn runtime_with_fake_native_gated(
    gated: bool,
) -> (NagoriRuntime, Arc<FakeNativeClipboard>, Arc<ObservingPaste>) {
    let store = SqliteStore::open_memory().expect("memory store should open");
    let clipboard = Arc::new(FakeNativeClipboard::default());
    let paste = Arc::new(ObservingPaste::new(clipboard.clone(), gated));
    let runtime = NagoriRuntime::builder(store)
        .clipboard(clipboard.clone())
        .clipboard_reader(clipboard.clone())
        .paste(paste.clone())
        .build_for_test();
    (runtime, clipboard, paste)
}

async fn enable_auto_paste(runtime: &NagoriRuntime) {
    runtime
        .store()
        .save_settings(AppSettings {
            auto_paste_enabled: true,
            ..AppSettings::default()
        })
        .await
        .expect("save settings");
}

#[tokio::test]
async fn concurrent_paste_requests_never_paste_each_others_clip() {
    // The race this pins down: request A publishes, request B publishes over
    // it, then A synthesises the keystroke — and B's body (possibly a secret)
    // lands in A's target app. A's paste is held open here, so B's publish has
    // to want the clipboard while A still owns it. With the lease serialising
    // the pair, B waits: A pastes its own clip and B's write lands afterwards.
    let (runtime, clipboard, paste) = runtime_with_fake_native_gated(true);
    enable_auto_paste(&runtime).await;
    let first = runtime
        .add_text("first clip".to_owned())
        .await
        .expect("add first");
    let second = runtime
        .add_text("second secret clip".to_owned())
        .await
        .expect("add second");

    let paster = {
        let runtime = runtime.clone();
        tokio::spawn(async move { runtime.paste_entry(first, None).await })
    };
    // A is inside the paste controller now, holding the lease.
    paste.entered.notified().await;
    let copier = {
        let runtime = runtime.clone();
        tokio::spawn(async move { runtime.copy_entry(second).await })
    };
    // Give B every chance to reach (and block on) the lease.
    for _ in 0..16 {
        tokio::task::yield_now().await;
    }
    assert_eq!(
        clipboard.current_text().as_deref(),
        Some("first clip"),
        "B must not publish while A holds the lease",
    );

    paste.release.notify_one();
    paster.await.expect("join A").expect("A should paste");
    copier.await.expect("join B").expect("B should copy");

    assert_eq!(
        paste.observed(),
        vec![Some("first clip".to_owned())],
        "the keystroke must fire on the clip its own request published",
    );
    assert_eq!(
        clipboard.writes(),
        vec!["first clip".to_owned(), "second secret clip".to_owned()],
        "B's publish must land after A's paste, not between A's copy and paste",
    );
}

#[tokio::test]
async fn paste_is_refused_when_an_external_app_copies_after_the_publish() {
    // The one writer the lease cannot lock out: the user copying in another app
    // during the palette's hide → refocus → delay window. The pre-paste
    // sequence check catches it, and the keystroke must not fire — synthesising
    // it would type the other app's clip into the target.
    let (runtime, clipboard, paste) = runtime_with_fake_native();
    let id = runtime
        .add_text("mine".to_owned())
        .await
        .expect("add entry");
    let mut lease = runtime.clipboard_lease().await;
    let publish = lease
        .copy_entry_with_format(id, PasteFormat::Preserve)
        .await
        .expect("copy should succeed");
    clipboard.simulate_external_copy("someone else's password");

    let err = lease
        .paste_frontmost(publish)
        .await
        .expect_err("a changed clipboard must refuse the paste");

    assert!(
        matches!(
            err,
            AppError::Paste {
                reason: PasteFailureReason::ClipboardChanged,
                ..
            }
        ),
        "got {err:?}",
    );
    assert_eq!(paste.calls(), 0, "no keystroke may be synthesised");
    assert_eq!(
        clipboard.current_text().as_deref(),
        Some("someone else's password"),
        "the refusal must not touch the clipboard either",
    );
}

#[tokio::test]
async fn paste_is_refused_when_a_macos_sequence_may_have_lapped_during_sleep() {
    // macOS changeCount can return to the value recorded for our own write
    // across sleep/wake. Equality is therefore not proof of ownership once a
    // publish is old enough to have crossed the host-pause threshold.
    let (runtime, clipboard, paste) = runtime_with_fake_native();
    clipboard.allow_sequence_lap();
    let id = runtime
        .add_text("mine".to_owned())
        .await
        .expect("add entry");
    let mut lease = runtime.clipboard_lease().await;
    let publish = lease
        .copy_entry_with_format(id, PasteFormat::Preserve)
        .await
        .expect("copy should succeed")
        .with_write_started_at(SystemTime::UNIX_EPOCH);
    clipboard.simulate_lapped_external_copy("someone else's password");

    let err = lease
        .paste_frontmost(publish)
        .await
        .expect_err("a possibly lapped sequence must refuse the paste");

    assert!(
        matches!(
            err,
            AppError::Paste {
                reason: PasteFailureReason::ClipboardChanged,
                ..
            }
        ),
        "got {err:?}",
    );
    assert_eq!(paste.calls(), 0, "no keystroke may be synthesised");
    assert_eq!(
        clipboard.current_text().as_deref(),
        Some("someone else's password"),
    );
}

#[tokio::test]
async fn paste_is_refused_when_clock_rollback_makes_a_lapping_sequence_token_ambiguous() {
    // A wall-clock rollback makes the publish age unknowable. Treating the
    // negative duration as zero would authorise the same lapped post-wake
    // collision as a fresh self-write.
    let (runtime, clipboard, paste) = runtime_with_fake_native();
    clipboard.allow_sequence_lap();
    let id = runtime
        .add_text("mine".to_owned())
        .await
        .expect("add entry");
    let mut lease = runtime.clipboard_lease().await;
    let publish = lease
        .copy_entry_with_format(id, PasteFormat::Preserve)
        .await
        .expect("copy should succeed")
        .with_write_started_at(SystemTime::now() + Duration::from_secs(1));
    clipboard.simulate_lapped_external_copy("post-rollback foreign clip");

    let err = lease
        .paste_frontmost(publish)
        .await
        .expect_err("an unknowable publish age must refuse the paste");

    assert!(
        matches!(
            err,
            AppError::Paste {
                reason: PasteFailureReason::ClipboardChanged,
                ..
            }
        ),
        "got {err:?}",
    );
    assert_eq!(paste.calls(), 0, "no keystroke may be synthesised");
    assert_eq!(
        clipboard.current_text().as_deref(),
        Some("post-rollback foreign clip"),
    );
}

#[tokio::test]
async fn a_stable_sequence_remains_verifiable_after_the_host_pause_threshold() {
    // Windows' sequence remains stable across suspend/resume, so applying the
    // macOS age rule globally would turn an ordinary delayed paste into a
    // false ClipboardChanged refusal.
    let (runtime, _clipboard, paste) = runtime_with_fake_native();
    let id = runtime
        .add_text("mine".to_owned())
        .await
        .expect("add entry");
    let mut lease = runtime.clipboard_lease().await;
    let publish = lease
        .copy_entry_with_format(id, PasteFormat::Preserve)
        .await
        .expect("copy should succeed")
        .with_write_started_at(SystemTime::UNIX_EPOCH);

    lease
        .paste_frontmost(publish)
        .await
        .expect("stable native sequence should remain authoritative");

    assert_eq!(paste.calls(), 1);
}

#[tokio::test]
async fn paste_is_refused_for_a_publish_token_from_an_earlier_lease() {
    // Type-level backstop for the split copy/paste front-ends: a token minted
    // under one lease cannot authorise a paste under another, so a refactor
    // that drops the lease between the copy and the keystroke fails closed
    // instead of pasting whatever the newest publish left behind.
    let (runtime, clipboard, paste) = runtime_with_fake_native();
    let mine = runtime.add_text("mine".to_owned()).await.expect("add mine");
    let theirs = runtime
        .add_text("theirs".to_owned())
        .await
        .expect("add theirs");

    let stale = {
        let mut lease = runtime.clipboard_lease().await;
        lease
            .copy_entry_with_format(mine, PasteFormat::Preserve)
            .await
            .expect("first copy")
    };
    let mut lease = runtime.clipboard_lease().await;
    let _superseding = lease
        .copy_entry_with_format(theirs, PasteFormat::Preserve)
        .await
        .expect("second copy");

    let err = lease
        .paste_frontmost(stale)
        .await
        .expect_err("a superseded publish must refuse the paste");

    assert!(
        matches!(
            err,
            AppError::Paste {
                reason: PasteFailureReason::ClipboardChanged,
                ..
            }
        ),
        "got {err:?}",
    );
    assert_eq!(paste.calls(), 0, "no keystroke may be synthesised");
    assert_eq!(clipboard.current_text().as_deref(), Some("theirs"));
}

#[tokio::test]
async fn paste_is_refused_after_the_lease_was_dropped_and_retaken() {
    // The lease half of the token, on its own: nothing published in between, so
    // the publish generation still matches — only the lease identity differs.
    // A front-end that dropped the lease between the copy and the keystroke no
    // longer holds the exclusion the paste depends on, so it must fail closed
    // even though the clipboard happens to still hold its clip.
    let (runtime, clipboard, paste) = runtime_with_fake_native();
    let id = runtime
        .add_text("mine".to_owned())
        .await
        .expect("add entry");

    let stale = {
        let mut lease = runtime.clipboard_lease().await;
        lease
            .copy_entry_with_format(id, PasteFormat::Preserve)
            .await
            .expect("copy")
    };
    let mut lease = runtime.clipboard_lease().await;

    let err = lease
        .paste_frontmost(stale)
        .await
        .expect_err("a token from a released lease must refuse the paste");

    assert!(
        matches!(
            err,
            AppError::Paste {
                reason: PasteFailureReason::ClipboardChanged,
                ..
            }
        ),
        "got {err:?}",
    );
    assert_eq!(paste.calls(), 0, "no keystroke may be synthesised");
    assert_eq!(clipboard.current_text().as_deref(), Some("mine"));
}

#[tokio::test]
async fn a_cancelled_request_keeps_the_lease_until_its_keystroke_returns() {
    // Dropping the lease has to mean "no side effect of mine is still in
    // flight". An IPC handler future is dropped on peer disconnect and on the
    // server deadline, and by then the synthesis has already handed
    // uncancellable work to the blocking pool — so if the guard travelled with
    // the caller, the next request could publish underneath a keystroke that
    // has not landed yet, and that keystroke would paste *its* clip.
    let (runtime, clipboard, paste) = runtime_with_fake_native_gated(true);
    enable_auto_paste(&runtime).await;
    let first = runtime
        .add_text("first clip".to_owned())
        .await
        .expect("add first");
    let second = runtime
        .add_text("second secret clip".to_owned())
        .await
        .expect("add second");

    let paster = {
        let runtime = runtime.clone();
        tokio::spawn(async move { runtime.paste_entry(first, None).await })
    };
    paste.entered.notified().await;
    // The peer went away mid-keystroke.
    paster.abort();

    let copier = {
        let runtime = runtime.clone();
        tokio::spawn(async move { runtime.copy_entry(second).await })
    };
    for _ in 0..16 {
        tokio::task::yield_now().await;
    }
    assert_eq!(
        clipboard.current_text().as_deref(),
        Some("first clip"),
        "the cancelled request still owns the clipboard until its keystroke returns",
    );

    // The OS call returns; only now may the lease change hands.
    paste.release.notify_one();
    copier
        .await
        .expect("join copier")
        .expect("copy should land");
    assert_eq!(
        clipboard.writes(),
        vec!["first clip".to_owned(), "second secret clip".to_owned()],
    );
}

#[tokio::test]
async fn cancelling_adapter_preparation_never_publishes_later() {
    // Windows image and multi-format writes decode before touching the OS
    // clipboard. That work belongs to the request future: a disconnected IPC
    // caller must not finish decoding and overwrite a newer user clip later.
    let (runtime, clipboard, _paste) = runtime_with_fake_native();
    let first = runtime
        .add_text("abandoned clip".to_owned())
        .await
        .expect("add first");
    let second = runtime
        .add_text("new clip".to_owned())
        .await
        .expect("add second");
    clipboard.gate_preparation();

    let abandoned = {
        let runtime = runtime.clone();
        tokio::spawn(async move { runtime.copy_entry(first).await })
    };
    clipboard.preparation_entered.notified().await;
    abandoned.abort();
    let _ = abandoned.await;
    clipboard.finish_preparation();

    runtime
        .copy_entry(second)
        .await
        .expect("a later copy should land");

    assert_eq!(clipboard.writes(), vec!["new clip".to_owned()]);
}

#[tokio::test]
async fn cancelling_a_started_publish_keeps_the_lease_until_it_finishes() {
    // Once preparation returns, the adapter may hand an uncancellable write
    // to the OS. The guarded task must outlive its caller and retain the lease
    // until that write returns, keeping a queued publish from overtaking it.
    let (runtime, clipboard, _paste) = runtime_with_fake_native();
    let first = runtime
        .add_text("first clip".to_owned())
        .await
        .expect("add first");
    let second = runtime
        .add_text("second clip".to_owned())
        .await
        .expect("add second");
    clipboard.gate_publish();

    let abandoned = {
        let runtime = runtime.clone();
        tokio::spawn(async move { runtime.copy_entry(first).await })
    };
    clipboard.publish_entered.notified().await;
    abandoned.abort();

    let queued = {
        let runtime = runtime.clone();
        tokio::spawn(async move { runtime.copy_entry(second).await })
    };
    for _ in 0..16 {
        tokio::task::yield_now().await;
    }
    assert!(
        clipboard.writes().is_empty(),
        "the queued publish must wait for the abandoned OS write",
    );

    clipboard.finish_publish();
    queued
        .await
        .expect("join queued copy")
        .expect("queued copy should land");
    assert_eq!(
        clipboard.writes(),
        vec!["first clip".to_owned(), "second clip".to_owned()],
    );
}

#[tokio::test]
async fn paste_runs_when_the_clipboard_still_holds_the_published_clip() {
    // The happy path of the same check: nothing wrote after the publish, so the
    // sequence still matches our own write and the keystroke fires.
    let (runtime, _clipboard, paste) = runtime_with_fake_native();
    enable_auto_paste(&runtime).await;
    let id = runtime
        .add_text("paste me".to_owned())
        .await
        .expect("add entry");

    runtime
        .paste_entry(id, None)
        .await
        .expect("paste should succeed");

    assert_eq!(paste.calls(), 1);
    assert_eq!(paste.observed(), vec![Some("paste me".to_owned())]);
}

#[tokio::test]
async fn paste_is_refused_when_the_sequence_probe_fails_on_a_host_that_tracks_writes() {
    // A host that can normally answer "is my clip still there" but doesn't is
    // not the same as a host that never could. A clipboard too wedged to report
    // its sequence is exactly where an unnoticed foreign write is plausible, and
    // the copy has already landed — so refuse rather than paste blind.
    let store = SqliteStore::open_memory().expect("memory store should open");
    let clipboard = Arc::new(FakeNativeClipboard::default());
    clipboard.fail_sequence_probe();
    let paste = Arc::new(ObservingPaste::new(clipboard.clone(), false));
    let runtime = NagoriRuntime::builder(store)
        .clipboard(clipboard.clone())
        .clipboard_reader(clipboard.clone())
        .paste(paste.clone())
        .build_for_test();
    enable_auto_paste(&runtime).await;
    let id = runtime
        .add_text("mine".to_owned())
        .await
        .expect("add entry");

    let err = runtime
        .paste_entry(id, None)
        .await
        .expect_err("an unverifiable clipboard must refuse the paste");

    assert!(
        matches!(
            err,
            AppError::Paste {
                reason: PasteFailureReason::ClipboardChanged,
                ..
            }
        ),
        "got {err:?}",
    );
    assert_eq!(paste.calls(), 0, "no keystroke may be synthesised");
    assert_eq!(
        clipboard.current_text().as_deref(),
        Some("mine"),
        "the copy still landed — only the keystroke was refused",
    );
}

/// Drive one paste to the point where its synthesis call has returned, with a
/// copy already queued behind it, and report how long after that return the
/// queued publish actually landed. The synthesis is gated so the queued copy is
/// provably waiting *before* the controller returns — the interval is then the
/// lease hold, not scheduling luck.
async fn measure_hold_past_synthesis(fail_synthesis: bool) -> Duration {
    let (runtime, clipboard, paste) = runtime_with_fake_native_gated(true);
    if fail_synthesis {
        paste.fail_synthesis();
    }
    enable_auto_paste(&runtime).await;
    let first = runtime
        .add_text("first clip".to_owned())
        .await
        .expect("add first");
    let second = runtime
        .add_text("second clip".to_owned())
        .await
        .expect("add second");

    let paster = {
        let runtime = runtime.clone();
        tokio::spawn(async move { runtime.paste_entry(first, None).await })
    };
    paste.entered.notified().await;
    let copier = {
        let runtime = runtime.clone();
        tokio::spawn(async move { runtime.copy_entry(second).await })
    };
    for _ in 0..16 {
        tokio::task::yield_now().await;
    }

    // The synthesis call returns here; the queued copy is already waiting.
    paste.release.notify_one();
    let outcome = paster.await.expect("join paster");
    assert_eq!(
        outcome.is_err(),
        fail_synthesis,
        "paste outcome: {outcome:?}"
    );
    copier
        .await
        .expect("join copier")
        .expect("copy should land");

    assert_eq!(
        clipboard.writes(),
        vec!["first clip".to_owned(), "second clip".to_owned()],
    );
    let returned_at = paste.returned_at().expect("controller recorded its return");
    clipboard
        .write_instant(1)
        .saturating_duration_since(returned_at)
}

#[tokio::test]
async fn the_lease_is_held_past_the_synthesis_call() {
    // `CGEventPost` / `SendInput` return once the keystroke is *posted*; the
    // target app reads the clipboard when it processes it. So the lease is held
    // a further `PASTE_CONSUMPTION_GRACE` past the call, keeping the queue out
    // of the start of that gap.
    let held = measure_hold_past_synthesis(false).await;
    assert!(
        held >= Duration::from_millis(50),
        "queued publish landed only {held:?} after the synthesis returned",
    );
}

#[tokio::test]
async fn the_lease_is_held_even_when_the_synthesis_reports_failure() {
    // A synthesis that fails part-way (a released modifier, a partially
    // inserted `SendInput` batch) may already have posted enough to trigger a
    // paste, so the hold cannot be conditional on success — otherwise the error
    // path reopens exactly the race the hold exists for.
    let held = measure_hold_past_synthesis(true).await;
    assert!(
        held >= Duration::from_millis(50),
        "queued publish landed only {held:?} after the failed synthesis returned",
    );
}

#[tokio::test]
async fn a_request_cancelled_during_the_verify_probe_synthesises_nothing() {
    // The pre-paste probe is an await, so the request can go away during it.
    // The step still owns the lease at that point (that is the whole design),
    // but it must not go on to type into whatever window holds focus now —
    // nothing has been synthesised yet, so there is nothing to finish.
    let (runtime, clipboard, paste) = runtime_with_fake_native();
    clipboard.gate_sequence_probe();
    enable_auto_paste(&runtime).await;
    let first = runtime
        .add_text("first clip".to_owned())
        .await
        .expect("add first");
    let second = runtime
        .add_text("second clip".to_owned())
        .await
        .expect("add second");

    let paster = {
        let runtime = runtime.clone();
        tokio::spawn(async move { runtime.paste_entry(first, None).await })
    };
    clipboard.probe_entered.notified().await;
    // The peer went away mid-probe.
    paster.abort();
    clipboard.probe_release.notify_one();

    // The lease is released once the abandoned step unwinds, so this lands.
    runtime
        .copy_entry(second)
        .await
        .expect("a later copy should still work");

    assert_eq!(paste.calls(), 0, "no keystroke may be synthesised");
    assert_eq!(
        clipboard.writes(),
        vec!["first clip".to_owned(), "second clip".to_owned()],
    );
}

#[tokio::test]
async fn paste_runs_when_the_host_cannot_verify_its_own_write() {
    // A host with no wired reader (or an adapter that does not track its own
    // writes, e.g. Linux Wayland's content-hash sequence) cannot answer the
    // question. That must not degrade auto-paste to copy-only, so the lease
    // treats it as unverifiable and pastes.
    let paste = Arc::new(super::CountingPaste::default());
    let (runtime, _clipboard) = super::runtime_with_paste(paste.clone());
    enable_auto_paste(&runtime).await;
    let id = runtime
        .add_text("paste me".to_owned())
        .await
        .expect("add entry");

    runtime
        .paste_entry(id, None)
        .await
        .expect("paste should succeed without verification");

    assert_eq!(paste.calls(), 1);
}

use super::super::*;

use nagori_platform::{ClipboardWriter, MemoryClipboard};
use nagori_storage::SqliteStore;

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
                    // Pick a bundle ID that is on the default
                    // password-manager preset so the typed
                    // `SourceAppDenylist` rule fires. The matcher
                    // now uses exact bundle-ID equality, so
                    // older 1Password identifiers that are not in
                    // the preset would no longer trigger.
                    bundle_id: Some("com.agilebits.onepassword7".to_owned()),
                    name: Some("1Password 7".to_owned()),
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

    // 1Password is on the default app_denylist (preset), so the
    // entry must be dropped (Sensitivity::Blocked) once the
    // source attribution is attached. If the frontmost weren't
    // picked up, the entry would be persisted as Public and the
    // test would observe a stored row.
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

/// Drive a single owner-exclusion-marker capture and assert the clip is
/// skipped, the right audit reason is logged, and the sequence is anchored
/// so the next poll dedup-skips without re-reading the body. Shared by the
/// concealed / transient cases so the two only differ by the expected
/// reason string.
async fn assert_owner_marker_skipped(kind: ClipboardExclusionKind, expected_reason: &str) {
    use std::sync::Mutex;

    use async_trait::async_trait;
    use nagori_core::{ClipboardSnapshot, ReadBudget};

    // Reader whose bounded read reports an owner-marked clip without ever
    // producing a body — mirroring the macOS adapter detecting the marker
    // before `get_text`. `snapshot_reads` lets the test prove the second
    // tick short-circuits on the dedup baseline instead of re-reading.
    struct MarkerReader {
        sequence: ClipboardSequence,
        kind: ClipboardExclusionKind,
        snapshot_reads: Mutex<u32>,
    }

    #[async_trait]
    impl ClipboardReader for MarkerReader {
        async fn current_snapshot(&self) -> Result<ClipboardSnapshot> {
            // Not exercised on the bounded capture path; return a benign
            // empty snapshot so the trait is satisfiable.
            Ok(ClipboardSnapshot {
                sequence: self.sequence.clone(),
                captured_at: OffsetDateTime::now_utc(),
                source: None,
                representations: Vec::new(),
            })
        }
        async fn current_sequence(&self) -> Result<ClipboardSequence> {
            Ok(self.sequence.clone())
        }
        async fn current_snapshot_with_max(&self, _budget: ReadBudget) -> Result<CapturedSnapshot> {
            *self.snapshot_reads.lock().unwrap() += 1;
            Ok(CapturedSnapshot::Excluded {
                sequence: self.sequence.clone(),
                kind: self.kind,
            })
        }
    }

    // Audit recorder that captures `(kind, message)` so we can assert the
    // skip *reason*, not merely that something was recorded. Cloning
    // shares the inner log so the loop and the test observe the same Vec.
    type AuditRecords = Arc<Mutex<Vec<(String, Option<String>)>>>;
    #[derive(Clone, Default)]
    struct RecordingAudit {
        records: AuditRecords,
    }
    #[async_trait]
    impl AuditLog for RecordingAudit {
        async fn record(
            &self,
            kind: &str,
            _entry_id: Option<EntryId>,
            message: Option<&str>,
        ) -> Result<()> {
            self.records
                .lock()
                .unwrap()
                .push((kind.to_owned(), message.map(str::to_owned)));
            Ok(())
        }
    }

    let reader = Arc::new(MarkerReader {
        sequence: ClipboardSequence::native(7),
        kind,
        snapshot_reads: Mutex::new(0),
    });
    let store = SqliteStore::open_memory().expect("memory store");
    let audit = RecordingAudit::default();
    let mut loop_ = CaptureLoop::new(
        reader.clone(),
        store.clone(),
        audit.clone(),
        AppSettings::default(),
    );

    // First tick: the marker is honoured before any body read, so nothing
    // lands in history and the skip reason is recorded.
    assert!(loop_.capture_once().await.unwrap().is_none());
    assert!(store.list_recent(10).await.unwrap().is_empty());
    {
        let records = audit.records.lock().unwrap();
        assert_eq!(records.len(), 1, "exactly one audit record on first tick");
        assert_eq!(records[0].0, "capture_skipped");
        assert_eq!(records[0].1.as_deref(), Some(expected_reason));
    }

    // Second tick: the sequence was anchored, so the cheap dedup
    // short-circuit fires — no extra body read, no extra audit record.
    assert!(loop_.capture_once().await.unwrap().is_none());
    assert_eq!(*reader.snapshot_reads.lock().unwrap(), 1);
    assert_eq!(audit.records.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn capture_once_skips_concealed_owner_marker() {
    assert_owner_marker_skipped(ClipboardExclusionKind::Concealed, "concealed_marker").await;
}

#[tokio::test]
async fn capture_once_skips_transient_owner_marker() {
    assert_owner_marker_skipped(ClipboardExclusionKind::Transient, "transient_marker").await;
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
async fn fail_closed_bypass_keeps_capturing_through_sustained_ax_errors() {
    // Test harnesses that can't grant the daemon Accessibility (so AX
    // queries fail every tick) need a way to exercise the rest of the
    // capture pipeline. `without_secure_focus_fail_closed` flips off
    // the threshold escalation; ticks past the threshold must still
    // capture instead of being silently skipped.
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
    .with_window(Arc::new(ErroringSecure))
    .without_secure_focus_fail_closed();

    // Push N > THRESHOLD distinct clips and assert every one of them
    // lands. Without the bypass the third clip would be the first to
    // fail closed; we add a comfortable margin so a future bump to
    // THRESHOLD still exercises post-threshold behaviour.
    let ticks = SECURE_FOCUS_FAIL_CLOSED_THRESHOLD + 3;
    for n in 0..ticks {
        clipboard
            .write_text(&format!("clip-{n}"))
            .await
            .expect("clipboard write");
        assert!(
            loop_.capture_once().await.unwrap().is_some(),
            "tick {n} must capture even past the AX-fail threshold",
        );
    }
}

#[tokio::test]
async fn fail_closed_bypass_still_honors_bundle_override() {
    // The bypass turns off "after N AX errors, assume secure" because
    // those failures are inferred. Bundle-id matches are positively
    // identified system password UIs and must keep skipping captures
    // even with the bypass on.
    use async_trait::async_trait;
    use nagori_core::{AppError, SourceApp};
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
    .with_window(Arc::new(AuthDialog))
    .without_secure_focus_fail_closed();

    clipboard.write_text("hunter2").await.expect("clipboard");
    assert!(loop_.capture_once().await.unwrap().is_none());
    assert!(store.list_recent(10).await.unwrap().is_empty());
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
async fn fail_closed_recovers_clip_copied_during_ax_outage() {
    // A clip the user copies while AX is blind (errors past the fail-closed
    // threshold) must not be lost. We skip it while blind, but once AX answers
    // again the clip — still on the OS clipboard — has to be re-examined and
    // captured, instead of staying stranded out of history forever.
    use async_trait::async_trait;
    use nagori_core::{AppError, SourceApp};
    use nagori_platform::{FrontmostApp, WindowBehavior};
    use std::sync::atomic::{AtomicBool, Ordering};

    struct ToggleSecure {
        blind: Arc<AtomicBool>,
    }
    #[async_trait]
    impl WindowBehavior for ToggleSecure {
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
            if self.blind.load(Ordering::Relaxed) {
                Err(AppError::Platform("AX wedged".to_owned()))
            } else {
                Ok(false)
            }
        }
    }

    let blind = Arc::new(AtomicBool::new(true));
    let clipboard = Arc::new(MemoryClipboard::new());
    let store = SqliteStore::open_memory().expect("memory store");
    let mut loop_ = CaptureLoop::new(
        clipboard.clone(),
        store.clone(),
        store.clone(),
        AppSettings::default(),
    )
    .with_window(Arc::new(ToggleSecure {
        blind: blind.clone(),
    }));

    // Drive the AX-error counter up to the fail-closed threshold. The ticks
    // below the threshold fail open and capture — they only prime the counter;
    // distinct clips avoid the cheap sequence dedup.
    for n in 0..SECURE_FOCUS_FAIL_CLOSED_THRESHOLD - 1 {
        clipboard
            .write_text(&format!("prime-{n}"))
            .await
            .expect("clipboard write");
        assert!(loop_.capture_once().await.unwrap().is_some());
    }

    // The victim clip lands while AX is blind past the threshold: skipped, and
    // — unlike a genuine secure skip — deliberately *not* anchored.
    clipboard.write_text("during-outage").await.expect("write");
    assert!(
        loop_.capture_once().await.unwrap().is_none(),
        "clip copied during the AX outage is skipped while blind",
    );
    let before = store.list_recent(50).await.unwrap().len();

    // AX recovers; the clip is still on the clipboard and must now be captured
    // rather than stranded.
    blind.store(false, Ordering::Relaxed);
    let id = loop_
        .capture_once()
        .await
        .unwrap()
        .expect("clip recovered once AX answers again");
    let entry = store
        .get(id)
        .await
        .unwrap()
        .expect("recovered entry stored");
    assert_eq!(entry.plain_text(), Some("during-outage"));
    assert_eq!(store.list_recent(50).await.unwrap().len(), before + 1);
}

#[tokio::test]
async fn genuine_secure_field_clip_not_recovered_after_focus_leaves() {
    // The flip side of the fail-closed recovery: a clip copied while a
    // *positively identified* secure field had focus (AX returned
    // `AXSecureTextField`) must stay out of history even after the user moves
    // focus away. Only blind (fail-closed) skips are recoverable; genuine
    // secure skips anchor and are never reconsidered.
    use async_trait::async_trait;
    use nagori_core::SourceApp;
    use nagori_platform::{FrontmostApp, WindowBehavior};
    use std::sync::atomic::{AtomicBool, Ordering};

    struct ToggleGenuine {
        secure: Arc<AtomicBool>,
    }
    #[async_trait]
    impl WindowBehavior for ToggleGenuine {
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
            Ok(self.secure.load(Ordering::Relaxed))
        }
    }

    let secure = Arc::new(AtomicBool::new(true));
    let clipboard = Arc::new(MemoryClipboard::new());
    let store = SqliteStore::open_memory().expect("memory store");
    let mut loop_ = CaptureLoop::new(
        clipboard.clone(),
        store.clone(),
        store.clone(),
        AppSettings::default(),
    )
    .with_window(Arc::new(ToggleGenuine {
        secure: secure.clone(),
    }));

    clipboard.write_text("hunter2").await.expect("clipboard");
    assert!(loop_.capture_once().await.unwrap().is_none());
    assert!(store.list_recent(10).await.unwrap().is_empty());

    // Focus leaves the secure field, but the clip it covered is unchanged and
    // must remain suppressed (anchored on the first skip).
    secure.store(false, Ordering::Relaxed);
    assert!(
        loop_.capture_once().await.unwrap().is_none(),
        "a genuinely secure clip stays out of history after focus leaves",
    );
    assert!(store.list_recent(10).await.unwrap().is_empty());
}

#[tokio::test]
async fn genuine_secure_skip_clears_armed_force_content_check() {
    // A genuine secure skip must clear `force_content_check`, otherwise a clip
    // seen while AX was blind (which armed the flag) and then positively
    // reported secure would be re-read and captured once focus leaves the
    // field — leaking a password. AX here goes blind (errors) → positively
    // secure (Ok(true)) → cleared (Ok(false)), with the clipboard static.
    use async_trait::async_trait;
    use nagori_core::{AppError, SourceApp};
    use nagori_platform::{FrontmostApp, WindowBehavior};
    use std::sync::atomic::{AtomicI8, Ordering};

    struct PhasedSecure {
        // -1 = AX error (blind), 0 = Ok(false) (clear), 1 = Ok(true) (secure).
        phase: Arc<AtomicI8>,
    }
    #[async_trait]
    impl WindowBehavior for PhasedSecure {
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
            match self.phase.load(Ordering::Relaxed) {
                1 => Ok(true),
                0 => Ok(false),
                _ => Err(AppError::Platform("AX wedged".to_owned())),
            }
        }
    }

    let phase = Arc::new(AtomicI8::new(-1));
    let clipboard = Arc::new(MemoryClipboard::new());
    let store = SqliteStore::open_memory().expect("memory store");
    let mut loop_ = CaptureLoop::new(
        clipboard.clone(),
        store.clone(),
        store.clone(),
        AppSettings::default(),
    )
    .with_window(Arc::new(PhasedSecure {
        phase: phase.clone(),
    }));

    // Prime the AX-error counter to the fail-closed threshold (distinct clips
    // avoid the cheap sequence dedup).
    for n in 0..SECURE_FOCUS_FAIL_CLOSED_THRESHOLD - 1 {
        clipboard
            .write_text(&format!("prime-{n}"))
            .await
            .expect("clipboard write");
        assert!(loop_.capture_once().await.unwrap().is_some());
    }

    // Victim clip lands while blind: fail-closed skip arms force_content_check.
    clipboard
        .write_text("secret-in-field")
        .await
        .expect("write");
    assert!(loop_.capture_once().await.unwrap().is_none());

    // AX recovers and now positively reports a secure field for the same clip:
    // it must be skipped *and* clear the armed flag.
    phase.store(1, Ordering::Relaxed);
    assert!(loop_.capture_once().await.unwrap().is_none());

    // Focus leaves the field. With the flag cleared and the sequence anchored,
    // the clip must stay out of history rather than being recovered.
    phase.store(0, Ordering::Relaxed);
    assert!(
        loop_.capture_once().await.unwrap().is_none(),
        "a positively-secure clip must not be recovered after focus leaves",
    );
    let rows = store.list_recent(50).await.unwrap();
    assert_eq!(
        rows.len(),
        (SECURE_FOCUS_FAIL_CLOSED_THRESHOLD - 1) as usize
    );
    assert!(
        !rows
            .iter()
            .any(|e| e.plain_text() == Some("secret-in-field")),
        "the secure clip must never reach history",
    );
}

#[tokio::test]
async fn fail_closed_recovery_preserves_source_for_denylist() {
    // A clip copied from a denylisted source (a password manager) while AX is
    // blind must stay subject to the denylist when it is recovered, even if the
    // user has switched to a non-denylisted app by the time AX answers again.
    // Otherwise the recovery would re-attribute it to the recovery-tick
    // frontmost and silently bypass the source denylist.
    use async_trait::async_trait;
    use nagori_core::{AppError, SourceApp};
    use nagori_platform::{FrontmostApp, WindowBehavior};
    use std::sync::Mutex as StdMutex;
    use std::sync::atomic::{AtomicBool, Ordering};

    struct SwitchingWindow {
        bundle: Arc<StdMutex<String>>,
        blind: Arc<AtomicBool>,
    }
    #[async_trait]
    impl WindowBehavior for SwitchingWindow {
        async fn frontmost_app(&self) -> Result<Option<FrontmostApp>> {
            Ok(Some(FrontmostApp {
                source: SourceApp {
                    bundle_id: Some(self.bundle.lock().unwrap().clone()),
                    name: None,
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
            if self.blind.load(Ordering::Relaxed) {
                Err(AppError::Platform("AX wedged".to_owned()))
            } else {
                Ok(false)
            }
        }
    }

    let bundle = Arc::new(StdMutex::new("com.example.editor".to_owned()));
    let blind = Arc::new(AtomicBool::new(true));
    let clipboard = Arc::new(MemoryClipboard::new());
    let store = SqliteStore::open_memory().expect("memory store");
    let mut loop_ = CaptureLoop::new(
        clipboard.clone(),
        store.clone(),
        store.clone(),
        AppSettings::default(),
    )
    .with_window(Arc::new(SwitchingWindow {
        bundle: bundle.clone(),
        blind: blind.clone(),
    }));

    // Prime the AX-error counter while a benign app is frontmost.
    for n in 0..SECURE_FOCUS_FAIL_CLOSED_THRESHOLD - 1 {
        clipboard
            .write_text(&format!("prime-{n}"))
            .await
            .expect("clipboard write");
        assert!(loop_.capture_once().await.unwrap().is_some());
    }

    // The victim clip is copied while a denylisted password manager is
    // frontmost and AX is blind: fail-closed skip remembers that source.
    *bundle.lock().unwrap() = "com.agilebits.onepassword7".to_owned();
    clipboard.write_text("from-1password").await.expect("write");
    assert!(loop_.capture_once().await.unwrap().is_none());

    // AX recovers and focus has moved to a benign app. The recovery must
    // classify against the *original* (denylisted) source and drop the clip,
    // not store it under the recovery-tick frontmost.
    blind.store(false, Ordering::Relaxed);
    *bundle.lock().unwrap() = "com.example.editor".to_owned();
    assert!(
        loop_.capture_once().await.unwrap().is_none(),
        "a denylisted-source clip must stay blocked when recovered",
    );
    let rows = store.list_recent(50).await.unwrap();
    assert!(
        !rows
            .iter()
            .any(|e| e.plain_text() == Some("from-1password")),
        "recovering a fail-closed clip must not bypass the source denylist",
    );
}

#[tokio::test]
async fn fail_closed_skip_audit_is_coalesced_per_clip() {
    // While AX is blind the recovery path arms force_content_check, so the
    // fail-closed branch runs every poll. Its warn + audit must be coalesced to
    // one per skipped clip; otherwise a sustained AX outage floods the log and
    // the audit_events table at the poll cadence.
    use std::sync::Mutex as StdMutex;

    use async_trait::async_trait;
    use nagori_core::{AppError, SourceApp};
    use nagori_platform::{FrontmostApp, WindowBehavior};

    struct AlwaysBlind;
    #[async_trait]
    impl WindowBehavior for AlwaysBlind {
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

    type AuditRecords = Arc<StdMutex<Vec<(String, Option<String>)>>>;
    #[derive(Clone, Default)]
    struct RecordingAudit {
        records: AuditRecords,
    }
    #[async_trait]
    impl AuditLog for RecordingAudit {
        async fn record(
            &self,
            kind: &str,
            _entry_id: Option<EntryId>,
            message: Option<&str>,
        ) -> Result<()> {
            self.records
                .lock()
                .unwrap()
                .push((kind.to_owned(), message.map(str::to_owned)));
            Ok(())
        }
    }

    let clipboard = Arc::new(MemoryClipboard::new());
    let store = SqliteStore::open_memory().expect("memory store");
    let audit = RecordingAudit::default();
    let mut loop_ = CaptureLoop::new(
        clipboard.clone(),
        store.clone(),
        audit.clone(),
        AppSettings::default(),
    )
    .with_window(Arc::new(AlwaysBlind));

    // Prime to the fail-closed threshold with distinct clips.
    for n in 0..SECURE_FOCUS_FAIL_CLOSED_THRESHOLD - 1 {
        clipboard
            .write_text(&format!("prime-{n}"))
            .await
            .expect("clipboard write");
        assert!(loop_.capture_once().await.unwrap().is_some());
    }

    // One static clip skipped over many blind ticks.
    clipboard.write_text("static").await.expect("write");
    for _ in 0..5 {
        assert!(loop_.capture_once().await.unwrap().is_none());
    }

    let records = audit.records.lock().unwrap();
    let fail_closed = records
        .iter()
        .filter(|(kind, msg)| {
            kind == "capture_skipped" && msg.as_deref() == Some("secure_field_fail_closed")
        })
        .count();
    assert_eq!(
        fail_closed, 1,
        "fail-closed audit must be coalesced to one record per static clip",
    );
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn fail_closed_recovery_survives_empty_snapshot_midwrite() {
    // The recovery source must survive an empty (mid-write) snapshot on the
    // first sighted tick after AX recovers. We peek it rather than consume it,
    // so when the real body lands at the same sequence on the following tick it
    // is still attributed to — and denylist-checked against — where it was
    // copied. Consuming it on the empty read would let a denylisted-source clip
    // slip through the empty-snapshot retry window.
    use async_trait::async_trait;
    use nagori_core::{
        AppError, ClipboardData, ClipboardRepresentation, ClipboardSnapshot, SourceApp,
    };
    use nagori_platform::{FrontmostApp, WindowBehavior};
    use std::sync::Mutex as StdMutex;
    use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};

    struct ScriptedReader {
        seq: Arc<AtomicI64>,
        text: Arc<StdMutex<String>>,
        // When set, the next snapshot is empty (modelling a clear-then-write
        // read mid-flight) and the flag resets so the following read sees body.
        empty: Arc<AtomicBool>,
    }
    #[async_trait]
    impl ClipboardReader for ScriptedReader {
        async fn current_snapshot(&self) -> Result<ClipboardSnapshot> {
            let representations = if self.empty.swap(false, Ordering::SeqCst) {
                Vec::new()
            } else {
                vec![ClipboardRepresentation {
                    mime_type: "text/plain".to_owned(),
                    data: ClipboardData::Text(self.text.lock().unwrap().clone()),
                }]
            };
            Ok(ClipboardSnapshot {
                sequence: ClipboardSequence::native(self.seq.load(Ordering::SeqCst)),
                captured_at: OffsetDateTime::now_utc(),
                source: None,
                representations,
            })
        }
        async fn current_sequence(&self) -> Result<ClipboardSequence> {
            Ok(ClipboardSequence::native(self.seq.load(Ordering::SeqCst)))
        }
    }

    struct SwitchingWindow {
        bundle: Arc<StdMutex<String>>,
        blind: Arc<AtomicBool>,
    }
    #[async_trait]
    impl WindowBehavior for SwitchingWindow {
        async fn frontmost_app(&self) -> Result<Option<FrontmostApp>> {
            Ok(Some(FrontmostApp {
                source: SourceApp {
                    bundle_id: Some(self.bundle.lock().unwrap().clone()),
                    name: None,
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
            if self.blind.load(Ordering::Relaxed) {
                Err(AppError::Platform("AX wedged".to_owned()))
            } else {
                Ok(false)
            }
        }
    }

    let seq = Arc::new(AtomicI64::new(1));
    let text = Arc::new(StdMutex::new("prime".to_owned()));
    let empty = Arc::new(AtomicBool::new(false));
    let bundle = Arc::new(StdMutex::new("com.example.editor".to_owned()));
    let blind = Arc::new(AtomicBool::new(true));
    let store = SqliteStore::open_memory().expect("memory store");
    let mut loop_ = CaptureLoop::new(
        ScriptedReader {
            seq: seq.clone(),
            text: text.clone(),
            empty: empty.clone(),
        },
        store.clone(),
        store.clone(),
        AppSettings::default(),
    )
    .with_window(Arc::new(SwitchingWindow {
        bundle: bundle.clone(),
        blind: blind.clone(),
    }));

    // Prime the AX-error counter to the threshold with distinct sequences while
    // a benign app is frontmost (these fail open and capture).
    for n in 0..SECURE_FOCUS_FAIL_CLOSED_THRESHOLD - 1 {
        seq.store(i64::from(n) + 1, Ordering::SeqCst);
        *text.lock().unwrap() = format!("prime-{n}");
        assert!(loop_.capture_once().await.unwrap().is_some());
    }

    // Victim copied from a denylisted password manager while blind: the
    // fail-closed skip remembers that source at this sequence.
    seq.store(100, Ordering::SeqCst);
    *text.lock().unwrap() = "from-1password".to_owned();
    *bundle.lock().unwrap() = "com.agilebits.onepassword7".to_owned();
    assert!(loop_.capture_once().await.unwrap().is_none());

    // AX recovers and focus has moved to a benign app, but the first sighted
    // read is an empty mid-write snapshot at the same sequence: it must retry
    // without consuming the remembered source.
    blind.store(false, Ordering::Relaxed);
    *bundle.lock().unwrap() = "com.example.editor".to_owned();
    empty.store(true, Ordering::SeqCst);
    assert!(loop_.capture_once().await.unwrap().is_none());

    // The real body lands at the same sequence: it must still be classified
    // against the original (denylisted) source and dropped — not stored under
    // the recovery-tick frontmost.
    assert!(
        loop_.capture_once().await.unwrap().is_none(),
        "denylisted-source clip must stay blocked across an empty-snapshot recovery",
    );
    let rows = store.list_recent(50).await.unwrap();
    assert!(
        !rows
            .iter()
            .any(|e| e.plain_text() == Some("from-1password")),
        "an empty-snapshot retry must not drop the recovery source and bypass the denylist",
    );
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn stale_recovery_cleared_when_wake_gap_follows_a_decided_clip() {
    // A decided fail-closed recovery leaves its source pending until the next
    // non-forced tick clears it. If that next tick is instead a sleep/wake
    // resync (which also arms force_content_check), the stale clear must still
    // run — gated on the tick-start retry flag, not the post-wake-gap one.
    // Otherwise a lapped `changeCount` (the very collision the resync defends
    // against) could re-apply the old benign source to a new denylisted clip.
    use async_trait::async_trait;
    use nagori_core::{
        AppError, ClipboardData, ClipboardRepresentation, ClipboardSnapshot, SourceApp,
    };
    use nagori_platform::{FrontmostApp, WindowBehavior};
    use std::sync::Mutex as StdMutex;
    use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
    use std::time::{Duration, UNIX_EPOCH};

    struct ScriptedReader {
        seq: Arc<AtomicI64>,
        text: Arc<StdMutex<String>>,
    }
    #[async_trait]
    impl ClipboardReader for ScriptedReader {
        async fn current_snapshot(&self) -> Result<ClipboardSnapshot> {
            Ok(ClipboardSnapshot {
                sequence: ClipboardSequence::native(self.seq.load(Ordering::SeqCst)),
                captured_at: OffsetDateTime::now_utc(),
                source: None,
                representations: vec![ClipboardRepresentation {
                    mime_type: "text/plain".to_owned(),
                    data: ClipboardData::Text(self.text.lock().unwrap().clone()),
                }],
            })
        }
        async fn current_sequence(&self) -> Result<ClipboardSequence> {
            Ok(ClipboardSequence::native(self.seq.load(Ordering::SeqCst)))
        }
    }

    struct SwitchingWindow {
        bundle: Arc<StdMutex<String>>,
        blind: Arc<AtomicBool>,
    }
    #[async_trait]
    impl WindowBehavior for SwitchingWindow {
        async fn frontmost_app(&self) -> Result<Option<FrontmostApp>> {
            Ok(Some(FrontmostApp {
                source: SourceApp {
                    bundle_id: Some(self.bundle.lock().unwrap().clone()),
                    name: None,
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
            if self.blind.load(Ordering::Relaxed) {
                Err(AppError::Platform("AX wedged".to_owned()))
            } else {
                Ok(false)
            }
        }
    }

    let seq = Arc::new(AtomicI64::new(1));
    let text = Arc::new(StdMutex::new("prime".to_owned()));
    let bundle = Arc::new(StdMutex::new("com.example.editor".to_owned()));
    let blind = Arc::new(AtomicBool::new(true));
    let store = SqliteStore::open_memory().expect("memory store");
    let mut loop_ = CaptureLoop::new(
        ScriptedReader {
            seq: seq.clone(),
            text: text.clone(),
        },
        store.clone(),
        store.clone(),
        AppSettings::default(),
    )
    .with_window(Arc::new(SwitchingWindow {
        bundle: bundle.clone(),
        blind: blind.clone(),
    }));

    let base = UNIX_EPOCH + Duration::from_secs(1_000_000);

    // Prime the AX-error counter to the threshold (benign frontmost, blind).
    for n in 0..SECURE_FOCUS_FAIL_CLOSED_THRESHOLD - 1 {
        seq.store(i64::from(n) + 1, Ordering::SeqCst);
        *text.lock().unwrap() = format!("prime-{n}");
        assert!(
            loop_
                .capture_once_at(base + Duration::from_secs(u64::from(n)))
                .await
                .unwrap()
                .is_some()
        );
    }

    // A benign clip copied while blind at sequence 100: fail-closed skip
    // remembers its (benign) source.
    seq.store(100, Ordering::SeqCst);
    *text.lock().unwrap() = "benign-clip".to_owned();
    assert!(
        loop_
            .capture_once_at(base + Duration::from_secs(10))
            .await
            .unwrap()
            .is_none()
    );

    // AX recovers; the benign clip is captured. Its recovery source lingers,
    // to be cleared on the next non-forced tick.
    blind.store(false, Ordering::Relaxed);
    assert!(
        loop_
            .capture_once_at(base + Duration::from_secs(11))
            .await
            .unwrap()
            .is_some()
    );

    // The next tick is a sleep/wake resync, and a lapped `changeCount` lands a
    // *different* clip from a denylisted app at the same sequence 100. The stale
    // benign recovery source must have been cleared, so this clip is classified
    // against 1Password and dropped — not stored under the stale benign source.
    seq.store(100, Ordering::SeqCst);
    *text.lock().unwrap() = "from-1password".to_owned();
    *bundle.lock().unwrap() = "com.agilebits.onepassword7".to_owned();
    assert!(
        loop_
            .capture_once_at(base + Duration::from_mins(1))
            .await
            .unwrap()
            .is_none(),
        "a wake-gap after a decided recovery must not carry a stale source",
    );
    let rows = store.list_recent(50).await.unwrap();
    assert!(
        !rows
            .iter()
            .any(|e| e.plain_text() == Some("from-1password")),
        "a lapped wake-gap clip must not inherit the previous clip's benign source",
    );
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn stale_recovery_cleared_even_when_wake_gap_tick_returns_early() {
    // The stale-recovery cleanup must run before the `capture_enabled` check and
    // the sequence read, so a wake-gap tick that returns early still drops a
    // now-stale source. This exercises the capture-disabled early return; a
    // failed sequence read returns even earlier but shares the same pre-gate
    // cleanup. Otherwise it survives into the next tick, where a lapped
    // `changeCount` could re-apply it to a different clip — the denylist bypass,
    // via the early-return path.
    use async_trait::async_trait;
    use nagori_core::{
        AppError, ClipboardData, ClipboardRepresentation, ClipboardSnapshot, SourceApp,
    };
    use nagori_platform::{FrontmostApp, WindowBehavior};
    use std::sync::Mutex as StdMutex;
    use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
    use std::time::{Duration, UNIX_EPOCH};

    struct ScriptedReader {
        seq: Arc<AtomicI64>,
        text: Arc<StdMutex<String>>,
    }
    #[async_trait]
    impl ClipboardReader for ScriptedReader {
        async fn current_snapshot(&self) -> Result<ClipboardSnapshot> {
            Ok(ClipboardSnapshot {
                sequence: ClipboardSequence::native(self.seq.load(Ordering::SeqCst)),
                captured_at: OffsetDateTime::now_utc(),
                source: None,
                representations: vec![ClipboardRepresentation {
                    mime_type: "text/plain".to_owned(),
                    data: ClipboardData::Text(self.text.lock().unwrap().clone()),
                }],
            })
        }
        async fn current_sequence(&self) -> Result<ClipboardSequence> {
            Ok(ClipboardSequence::native(self.seq.load(Ordering::SeqCst)))
        }
    }

    struct SwitchingWindow {
        bundle: Arc<StdMutex<String>>,
        blind: Arc<AtomicBool>,
    }
    #[async_trait]
    impl WindowBehavior for SwitchingWindow {
        async fn frontmost_app(&self) -> Result<Option<FrontmostApp>> {
            Ok(Some(FrontmostApp {
                source: SourceApp {
                    bundle_id: Some(self.bundle.lock().unwrap().clone()),
                    name: None,
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
            if self.blind.load(Ordering::Relaxed) {
                Err(AppError::Platform("AX wedged".to_owned()))
            } else {
                Ok(false)
            }
        }
    }

    let seq = Arc::new(AtomicI64::new(1));
    let text = Arc::new(StdMutex::new("prime".to_owned()));
    let bundle = Arc::new(StdMutex::new("com.example.editor".to_owned()));
    let blind = Arc::new(AtomicBool::new(true));
    let store = SqliteStore::open_memory().expect("memory store");
    let mut loop_ = CaptureLoop::new(
        ScriptedReader {
            seq: seq.clone(),
            text: text.clone(),
        },
        store.clone(),
        store.clone(),
        AppSettings::default(),
    )
    .with_window(Arc::new(SwitchingWindow {
        bundle: bundle.clone(),
        blind: blind.clone(),
    }));

    let base = UNIX_EPOCH + Duration::from_secs(1_000_000);

    // Prime the AX-error counter to the threshold (benign frontmost, blind).
    for n in 0..SECURE_FOCUS_FAIL_CLOSED_THRESHOLD - 1 {
        seq.store(i64::from(n) + 1, Ordering::SeqCst);
        *text.lock().unwrap() = format!("prime-{n}");
        assert!(
            loop_
                .capture_once_at(base + Duration::from_secs(u64::from(n)))
                .await
                .unwrap()
                .is_some()
        );
    }

    // Benign clip skipped while blind at sequence 100, then captured once AX
    // recovers. Its recovery source lingers, pending the next non-forced tick.
    seq.store(100, Ordering::SeqCst);
    *text.lock().unwrap() = "benign-clip".to_owned();
    assert!(
        loop_
            .capture_once_at(base + Duration::from_secs(10))
            .await
            .unwrap()
            .is_none()
    );
    blind.store(false, Ordering::Relaxed);
    assert!(
        loop_
            .capture_once_at(base + Duration::from_secs(11))
            .await
            .unwrap()
            .is_some()
    );

    // Capture is disabled, then a sleep/wake resync tick fires: it returns early
    // at the `capture_enabled` gate, but the stale-recovery cleanup must already
    // have run before that gate.
    let disabled = AppSettings {
        capture_enabled: false,
        ..AppSettings::default()
    };
    loop_.update_settings(disabled);
    assert!(
        loop_
            .capture_once_at(base + Duration::from_mins(1))
            .await
            .unwrap()
            .is_none()
    );

    // Capture is re-enabled and a lapped `changeCount` lands a denylisted clip
    // at the same sequence 100. The stale benign source must be gone, so this is
    // classified against 1Password and dropped.
    loop_.update_settings(AppSettings::default());
    seq.store(100, Ordering::SeqCst);
    *text.lock().unwrap() = "from-1password".to_owned();
    *bundle.lock().unwrap() = "com.agilebits.onepassword7".to_owned();
    assert!(
        loop_
            .capture_once_at(base + Duration::from_secs(62))
            .await
            .unwrap()
            .is_none(),
        "a clip after an early-returning wake-gap tick must not carry a stale source",
    );
    let rows = store.list_recent(50).await.unwrap();
    assert!(
        !rows
            .iter()
            .any(|e| e.plain_text() == Some("from-1password")),
        "an early-returning wake-gap tick must still drop the stale recovery source",
    );
}

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
    use nagori_core::ClipboardSnapshot;

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
        async fn current_snapshot_with_max(&self, _max: usize) -> Result<CapturedSnapshot> {
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

use super::super::*;
use super::loop_for;

use nagori_platform::{ClipboardWriter, MemoryClipboard};
use nagori_storage::SqliteStore;

fn paused() -> AppSettings {
    AppSettings {
        capture_enabled: false,
        ..AppSettings::default()
    }
}

#[tokio::test]
async fn resume_does_not_capture_clip_copied_while_paused() {
    // The user pauses capture, copies a secret, and resumes. The clip that is
    // still on the clipboard at resume was copied while paused, so the first
    // enabled tick must anchor it rather than record it. A copy made after
    // resume must still be captured.
    let clipboard = Arc::new(MemoryClipboard::new());
    let store = SqliteStore::open_memory().expect("memory store");
    let mut loop_ = loop_for(clipboard.clone(), store.clone(), AppSettings::default());

    clipboard
        .write_text("before pause")
        .await
        .expect("clipboard write");
    loop_
        .capture_once()
        .await
        .unwrap()
        .expect("pre-pause clip is captured");

    loop_.update_settings(paused());
    assert!(loop_.capture_once().await.unwrap().is_none());
    clipboard
        .write_text("secret copied while paused")
        .await
        .expect("clipboard write");
    assert!(loop_.capture_once().await.unwrap().is_none());

    loop_.update_settings(AppSettings::default());
    assert!(
        loop_.capture_once().await.unwrap().is_none(),
        "the first tick after resume must not capture the paused-era clip",
    );
    assert!(loop_.capture_once().await.unwrap().is_none());

    clipboard
        .write_text("after resume")
        .await
        .expect("clipboard write");
    let id = loop_
        .capture_once()
        .await
        .unwrap()
        .expect("a copy made after resume is captured");
    let stored = store.get(id).await.unwrap().expect("stored row");
    assert_eq!(stored.plain_text(), Some("after resume"));

    let texts: Vec<_> = store
        .list_recent(10)
        .await
        .unwrap()
        .iter()
        .filter_map(|e| e.plain_text().map(str::to_owned))
        .collect();
    assert_eq!(texts, vec!["after resume", "before pause"]);
}

#[tokio::test]
async fn pause_shorter_than_a_tick_still_skips_clip_copied_during_it() {
    // Pause and resume can both land between two polls, so no paused tick
    // ever runs. The clip copied inside that window must still be skipped.
    let clipboard = Arc::new(MemoryClipboard::new());
    let store = SqliteStore::open_memory().expect("memory store");
    let mut loop_ = loop_for(clipboard.clone(), store.clone(), AppSettings::default());

    loop_.update_settings(paused());
    clipboard
        .write_text("copied inside a brief pause")
        .await
        .expect("clipboard write");
    loop_.update_settings(AppSettings::default());

    assert!(loop_.capture_once().await.unwrap().is_none());
    assert!(store.list_recent(10).await.unwrap().is_empty());
}

#[tokio::test]
async fn launch_while_paused_skips_clip_on_resume_even_with_initial_capture() {
    // `capture_initial_clipboard_on_launch=true` opts into recording the
    // pre-launch clipboard, but not when the app starts paused: by the time
    // capture resumes, the clipboard holds whatever was copied during the
    // pause, which must not be recorded.
    let clipboard = Arc::new(MemoryClipboard::new());
    clipboard
        .write_text("on the clipboard at launch")
        .await
        .expect("seed clipboard");
    let store = SqliteStore::open_memory().expect("memory store");
    let settings = AppSettings {
        capture_initial_clipboard_on_launch: true,
        ..paused()
    };
    let mut loop_ = loop_for(clipboard.clone(), store.clone(), settings);

    assert!(loop_.capture_once().await.unwrap().is_none());
    loop_.update_settings(AppSettings {
        capture_initial_clipboard_on_launch: true,
        ..AppSettings::default()
    });
    assert!(loop_.capture_once().await.unwrap().is_none());
    assert!(store.list_recent(10).await.unwrap().is_empty());
}

#[tokio::test]
async fn wake_gap_after_resume_does_not_capture_paused_clip() {
    // The resume re-anchor records the paused-era clip's content hash, so a
    // later sleep/wake resync that forces a body read recognises the clip as
    // unchanged instead of capturing it.
    let clipboard = Arc::new(MemoryClipboard::new());
    let store = SqliteStore::open_memory().expect("memory store");
    let mut loop_ = loop_for(clipboard.clone(), store.clone(), AppSettings::default());

    let t0 = SystemTime::now();
    clipboard
        .write_text("before pause")
        .await
        .expect("clipboard write");
    loop_
        .capture_once_at(t0)
        .await
        .unwrap()
        .expect("pre-pause clip is captured");

    loop_.update_settings(paused());
    clipboard
        .write_text("secret copied while paused")
        .await
        .expect("clipboard write");
    assert!(
        loop_
            .capture_once_at(t0 + Duration::from_secs(1))
            .await
            .unwrap()
            .is_none()
    );

    loop_.update_settings(AppSettings::default());
    assert!(
        loop_
            .capture_once_at(t0 + Duration::from_secs(2))
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        loop_
            .capture_once_at(t0 + Duration::from_mins(1))
            .await
            .unwrap()
            .is_none(),
        "a wake-gap resync after resume must not capture the paused-era clip",
    );
    assert_eq!(store.list_recent(10).await.unwrap().len(), 1);
}

#[tokio::test]
async fn wake_gap_while_paused_does_not_capture_on_resume() {
    // A sleep/wake gap during the pause arms the one-shot content check. The
    // resume re-anchor must still win over it.
    let clipboard = Arc::new(MemoryClipboard::new());
    let store = SqliteStore::open_memory().expect("memory store");
    let mut loop_ = loop_for(clipboard.clone(), store.clone(), paused());

    let t0 = SystemTime::now();
    clipboard
        .write_text("secret copied while paused")
        .await
        .expect("clipboard write");
    assert!(loop_.capture_once_at(t0).await.unwrap().is_none());
    assert!(
        loop_
            .capture_once_at(t0 + Duration::from_mins(1))
            .await
            .unwrap()
            .is_none()
    );

    loop_.update_settings(AppSettings::default());
    assert!(
        loop_
            .capture_once_at(t0 + Duration::from_mins(2))
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        loop_
            .capture_once_at(t0 + Duration::from_secs(121))
            .await
            .unwrap()
            .is_none()
    );
    assert!(store.list_recent(10).await.unwrap().is_empty());
}

#[tokio::test]
async fn resume_reanchor_retries_on_snapshot_failure() {
    // A transient read failure on the resume tick must keep the re-anchor
    // armed, so the next tick anchors the paused-era clip instead of falling
    // through to a normal capture.
    use std::sync::Mutex;

    use async_trait::async_trait;
    use nagori_core::{
        ClipboardData, ClipboardRepresentation, ClipboardSequence, ClipboardSnapshot,
    };
    use nagori_platform::ClipboardReader;
    use time::OffsetDateTime;

    struct FlakyReader {
        snapshot_attempts: Mutex<u32>,
    }

    #[async_trait]
    impl ClipboardReader for FlakyReader {
        async fn current_snapshot(&self) -> Result<ClipboardSnapshot> {
            let attempt = {
                let mut guard = self.snapshot_attempts.lock().unwrap();
                *guard += 1;
                *guard
            };
            if attempt == 1 {
                return Err(AppError::Platform(
                    "simulated transient read failure".to_owned(),
                ));
            }
            Ok(ClipboardSnapshot {
                sequence: ClipboardSequence::content_hash("paused-seq"),
                captured_at: OffsetDateTime::now_utc(),
                source: None,
                representations: vec![ClipboardRepresentation {
                    mime_type: "text/plain".to_owned(),
                    data: ClipboardData::Text("secret copied while paused".to_owned()),
                }],
            })
        }
        async fn current_sequence(&self) -> Result<ClipboardSequence> {
            Ok(ClipboardSequence::content_hash("paused-seq"))
        }
    }

    let reader = FlakyReader {
        snapshot_attempts: Mutex::new(0),
    };
    let store = SqliteStore::open_memory().expect("memory store");
    let mut loop_ = CaptureLoop::new(reader, store.clone(), store.clone(), paused());

    assert!(loop_.capture_once().await.unwrap().is_none());
    loop_.update_settings(AppSettings::default());

    assert!(loop_.capture_once().await.is_err());
    assert!(loop_.capture_once().await.unwrap().is_none());
    assert!(loop_.capture_once().await.unwrap().is_none());
    assert!(
        store.list_recent(10).await.unwrap().is_empty(),
        "a failed resume re-anchor must retry rather than capture the paused-era clip",
    );
}

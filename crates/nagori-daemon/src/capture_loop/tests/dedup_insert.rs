use super::super::*;
use super::loop_for;

use nagori_platform::{ClipboardWriter, MemoryClipboard};
use nagori_storage::SqliteStore;

struct FlakyInsertRepo {
    inner: SqliteStore,
    fail_insert: Arc<std::sync::atomic::AtomicBool>,
}

#[async_trait::async_trait]
impl EntryRepository for FlakyInsertRepo {
    async fn insert(&self, entry: nagori_core::ClipboardEntry) -> Result<EntryId> {
        if self.fail_insert.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(AppError::storage(
                "simulated transient insert failure".to_owned(),
            ));
        }
        self.inner.insert(entry).await
    }
    async fn get(&self, id: EntryId) -> Result<Option<nagori_core::ClipboardEntry>> {
        self.inner.get(id).await
    }
    async fn update_metadata(
        &self,
        id: EntryId,
        metadata: nagori_core::EntryMetadata,
    ) -> Result<()> {
        self.inner.update_metadata(id, metadata).await
    }
    async fn mark_deleted(&self, id: EntryId) -> Result<()> {
        self.inner.mark_deleted(id).await
    }
    async fn list_recent(&self, limit: usize) -> Result<Vec<nagori_core::ClipboardEntry>> {
        self.inner.list_recent(limit).await
    }
    async fn list_pinned(&self) -> Result<Vec<nagori_core::ClipboardEntry>> {
        self.inner.list_pinned().await
    }
    async fn list_representations(
        &self,
        id: EntryId,
    ) -> Result<Vec<StoredClipboardRepresentation>> {
        self.inner.list_representations(id).await
    }
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
async fn capture_once_notifies_after_successful_insert() {
    use std::sync::Mutex;

    let clipboard = Arc::new(MemoryClipboard::new());
    let store = SqliteStore::open_memory().expect("memory store");
    let captured_ids = Arc::new(Mutex::new(Vec::new()));
    let captured_ids_for_hook = captured_ids.clone();
    let notify_capture = Arc::new(move |id| {
        captured_ids_for_hook
            .lock()
            .expect("notifier lock")
            .push(id);
    });
    let mut loop_ = loop_for(clipboard.clone(), store.clone(), AppSettings::default())
        .with_capture_notifier(notify_capture);

    clipboard
        .write_text("notify me after insert")
        .await
        .expect("clipboard write");
    let id = loop_
        .capture_once()
        .await
        .unwrap()
        .expect("capture should insert");

    assert_eq!(
        captured_ids.lock().expect("notifier lock").as_slice(),
        &[id]
    );
}

#[tokio::test]
async fn capture_once_survives_panicking_notifier() {
    let clipboard = Arc::new(MemoryClipboard::new());
    let store = SqliteStore::open_memory().expect("memory store");
    let notify_capture = Arc::new(|_id| panic!("hook torn down"));
    let mut loop_ = loop_for(clipboard.clone(), store.clone(), AppSettings::default())
        .with_capture_notifier(notify_capture);

    clipboard
        .write_text("notifier panics, insert still succeeds")
        .await
        .expect("clipboard write");
    let id = loop_
        .capture_once()
        .await
        .expect("capture_once must not propagate hook panic")
        .expect("capture should still insert despite hook panic");

    let entries = store.list_recent(10).await.unwrap();
    assert!(entries.iter().any(|e| e.id == id));
}

#[tokio::test]
async fn capture_once_retries_after_transient_insert_failure() {
    // Regression: `last_sequence` used to be anchored *before* the durable
    // insert, so a single transient failure (DB busy, disk full) stranded
    // the clip — the next tick saw the same sequence, dedup-skipped it, and
    // the content was lost forever. The anchor must now roll back on a
    // failed insert so the next tick re-reads and retries the same clip.
    use std::sync::atomic::{AtomicBool, Ordering};

    let clipboard = Arc::new(MemoryClipboard::new());
    let store = SqliteStore::open_memory().expect("memory store");
    let fail_flag = Arc::new(AtomicBool::new(true));
    let repo = FlakyInsertRepo {
        inner: store.clone(),
        fail_insert: fail_flag.clone(),
    };
    let mut loop_ = CaptureLoop::new(
        clipboard.clone(),
        repo,
        store.clone(),
        AppSettings::default(),
    );

    clipboard
        .write_text("clip that must survive a busy DB")
        .await
        .expect("clipboard write");

    // First tick: the durable insert fails and the error propagates. The
    // sequence anchor must NOT have been committed.
    assert!(
        loop_.capture_once().await.is_err(),
        "a failed insert must surface as an error",
    );
    assert_eq!(
        store.list_recent(10).await.unwrap().len(),
        0,
        "nothing should be persisted when the insert fails",
    );

    // DB recovers; the very next tick must re-read and persist the *same*
    // clip instead of dedup-skipping it on the stale sequence.
    fail_flag.store(false, Ordering::SeqCst);
    let id = loop_
        .capture_once()
        .await
        .expect("retry must not error")
        .expect("retry must re-capture the clip the failed insert dropped");

    let entries = store.list_recent(10).await.unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].id, id);
}

#[tokio::test]
async fn capture_once_retries_after_insert_failure_on_same_changecount() {
    // The hardest rollback case: an empty snapshot arms the wake-gap
    // one-shot, then the real content lands at the *same* changeCount (the
    // macOS clear-then-write single bump). If a transient insert failure
    // here rolled back only `last_sequence`, the next tick would have
    // `force_content_check = false` *and* `last_content_hash` already set
    // to the content, so both dedup gates would skip the clip and lose it.
    // The fix restores all three dedup fields, so the retry re-reads and
    // persists the same clip.
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use nagori_core::{ClipboardData, ClipboardRepresentation, ClipboardSnapshot};

    struct EmptyThenContentReader {
        // One stable changeCount for both the empty read and the content
        // read — the clear-then-write race we defend against.
        sequence: ClipboardSequence,
        snapshot_reads: AtomicUsize,
    }

    impl EmptyThenContentReader {
        fn content_snapshot(&self) -> ClipboardSnapshot {
            ClipboardSnapshot {
                sequence: self.sequence.clone(),
                captured_at: OffsetDateTime::now_utc(),
                source: None,
                representations: vec![ClipboardRepresentation {
                    mime_type: "text/plain".to_owned(),
                    data: ClipboardData::Text("content at the same changeCount".to_owned()),
                }],
            }
        }
    }

    #[async_trait::async_trait]
    impl ClipboardReader for EmptyThenContentReader {
        async fn current_snapshot(&self) -> Result<ClipboardSnapshot> {
            Ok(self.content_snapshot())
        }
        async fn current_sequence(&self) -> Result<ClipboardSequence> {
            Ok(self.sequence.clone())
        }
        async fn current_snapshot_with_max(&self, _max_bytes: usize) -> Result<CapturedSnapshot> {
            // First read: empty (mid-write). Subsequent reads: the content
            // that landed at the same changeCount.
            if self.snapshot_reads.fetch_add(1, Ordering::SeqCst) == 0 {
                Ok(CapturedSnapshot::Captured(ClipboardSnapshot {
                    sequence: self.sequence.clone(),
                    captured_at: OffsetDateTime::now_utc(),
                    source: None,
                    representations: Vec::new(),
                }))
            } else {
                Ok(CapturedSnapshot::Captured(self.content_snapshot()))
            }
        }
    }

    let store = SqliteStore::open_memory().expect("memory store");
    let fail_flag = Arc::new(AtomicBool::new(true));
    let repo = FlakyInsertRepo {
        inner: store.clone(),
        fail_insert: fail_flag.clone(),
    };
    let reader = EmptyThenContentReader {
        sequence: ClipboardSequence::content_hash("same-change-count"),
        snapshot_reads: AtomicUsize::new(0),
    };
    // Skip the "ignore pre-launch clipboard" pristine path so the first
    // tick reads the (empty) snapshot directly.
    let settings = AppSettings {
        capture_initial_clipboard_on_launch: true,
        ..AppSettings::default()
    };
    let mut loop_ = CaptureLoop::new(reader, repo, store.clone(), settings);

    // Tick 1: empty snapshot → arms the wake-gap one-shot, anchors the
    // changeCount, inserts nothing.
    assert!(loop_.capture_once().await.unwrap().is_none());

    // Tick 2: content lands at the same changeCount but the insert fails.
    assert!(
        loop_.capture_once().await.is_err(),
        "the failing insert must surface as an error",
    );
    assert_eq!(store.list_recent(10).await.unwrap().len(), 0);

    // Tick 3: DB recovers. Despite the unchanged changeCount, the retry
    // must re-read and persist the clip rather than dedup-skipping it.
    fail_flag.store(false, Ordering::SeqCst);
    let id = loop_
        .capture_once()
        .await
        .expect("retry must not error")
        .expect("retry must re-capture the clip lost to the failed insert");

    let entries = store.list_recent(10).await.unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].id, id);
}

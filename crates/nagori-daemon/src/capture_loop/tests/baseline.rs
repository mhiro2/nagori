use super::super::*;
use super::loop_for;

use nagori_platform::{ClipboardWriter, MemoryClipboard};
use nagori_storage::SqliteStore;

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
            language: None,
            image_width: None,
            image_height: None,
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
async fn pristine_baseline_skips_pre_launch_clip_over_hard_limit() {
    // capture_initial_clipboard_on_launch=false with a pre-launch clip that
    // exceeds the internal hard limit: the baseline read goes through the
    // bounded path (not the unbounded `current_snapshot`), recognises the
    // clip as oversized, anchors the sequence so it isn't re-probed, and
    // never inserts it. Such a clip can never be captured under any setting,
    // so leaving `last_content_hash` unset is correct. A later in-budget clip
    // must still flow through.
    let clipboard = Arc::new(MemoryClipboard::new());
    let oversized = "x".repeat(MAX_ENTRY_SIZE_BYTES + 1);
    clipboard
        .write_text(&oversized)
        .await
        .expect("seed clipboard");
    let store = SqliteStore::open_memory().expect("memory store");
    let settings = AppSettings {
        capture_initial_clipboard_on_launch: false,
        ..AppSettings::default()
    };
    let mut loop_ = loop_for(clipboard.clone(), store.clone(), settings);

    // First tick: the over-hard-limit pre-launch clip is the baseline. It is
    // discarded without an insert, and `last_content_hash` is left unset
    // because no body within the hard limit was read.
    assert!(loop_.capture_once().await.unwrap().is_none());
    assert!(store.list_recent(10).await.unwrap().is_empty());
    assert!(loop_.dedup.last_sequence.is_some());
    assert!(loop_.dedup.last_content_hash.is_none());

    // A small post-launch clip is within budget and must be captured.
    clipboard
        .write_text("small")
        .await
        .expect("clipboard write");
    let id = loop_
        .capture_once()
        .await
        .unwrap()
        .expect("post-launch clip should be inserted");
    let stored = store.get(id).await.unwrap().expect("stored row");
    assert_eq!(stored.plain_text(), Some("small"));
}

#[tokio::test]
async fn pristine_baseline_hashes_clip_over_setting_but_under_hard_limit() {
    // A pre-launch clip larger than the *current* `max_entry_size_bytes` but
    // within the hard limit must still have its dedup hash anchored at the
    // baseline. Otherwise, raising the setting above the clip's size and then
    // hitting a wake-gap resync would re-capture the pre-launch clip instead
    // of recognising it. This locks the baseline read to the hard limit, not
    // the live setting.
    let clipboard = Arc::new(MemoryClipboard::new());
    // 64 bytes: over the initial 16-byte budget, far under the hard limit.
    clipboard
        .write_text(&"y".repeat(64))
        .await
        .expect("seed clipboard");
    let store = SqliteStore::open_memory().expect("memory store");
    let settings = AppSettings {
        capture_initial_clipboard_on_launch: false,
        max_entry_size_bytes: 16,
        ..AppSettings::default()
    };
    let mut loop_ = loop_for(clipboard.clone(), store.clone(), settings);

    // Baseline tick: the clip is over the 16-byte setting but the bounded
    // read uses the hard limit, so the body is read and its hash anchored.
    let t0 = SystemTime::now();
    assert!(loop_.capture_once_at(t0).await.unwrap().is_none());
    assert!(loop_.dedup.last_content_hash.is_some());
    assert!(store.list_recent(10).await.unwrap().is_empty());

    // The user raises the budget so the 64-byte clip would now be capturable.
    loop_.update_settings(AppSettings {
        capture_initial_clipboard_on_launch: false,
        max_entry_size_bytes: 128,
        ..AppSettings::default()
    });

    // A wake-gap resync (>30s) forces a content-aware re-read of the same,
    // unchanged pre-launch clip. The anchored hash recognises it, so it is
    // skipped rather than promoted into history.
    assert!(
        loop_
            .capture_once_at(t0 + Duration::from_secs(31))
            .await
            .unwrap()
            .is_none(),
        "the unchanged pre-launch clip must not be captured after a setting raise + wake",
    );
    assert!(store.list_recent(10).await.unwrap().is_empty());
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
        sequence: ClipboardSequence::content_hash("colliding-seq"),
        text: Mutex::new("pre-sleep clip".to_owned()),
    };
    let store = SqliteStore::open_memory().expect("memory store");
    let mut loop_ = CaptureLoop::new(reader, store.clone(), store.clone(), AppSettings::default());

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
        ClipboardData, ClipboardRepresentation, ClipboardSequence, ClipboardSnapshot, ContentHash,
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
                sequence: ClipboardSequence::content_hash(
                    ContentHash::sha256(self.text.as_bytes()).value,
                ),
                captured_at: OffsetDateTime::now_utc(),
                source: None,
                representations: vec![ClipboardRepresentation {
                    mime_type: "text/plain".to_owned(),
                    data: ClipboardData::Text(self.text.clone()),
                }],
            })
        }
        async fn current_sequence(&self) -> Result<ClipboardSequence> {
            Ok(ClipboardSequence::content_hash(
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
        sequence: ClipboardSequence::content_hash("colliding-seq"),
        text: Mutex::new("pre-sleep clip".to_owned()),
        snapshot_attempts: Mutex::new(0),
        fail_attempt: 2,
    };
    let store = SqliteStore::open_memory().expect("memory store");
    let mut loop_ = CaptureLoop::new(reader, store.clone(), store.clone(), AppSettings::default());

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
async fn empty_midwrite_snapshot_does_not_strand_content_at_same_sequence() {
    // Regression: on macOS `clearContents()` + `writeObjects()` is a
    // *single* changeCount bump. If the capture loop polls between the two
    // — observing an empty pasteboard at the new changeCount — it anchors
    // that changeCount, and the real content then lands at the *same*
    // changeCount. Without the empty-snapshot body re-read, the next tick
    // dedup-skips it and the clip is stranded (the file-list E2E flake).
    use std::sync::Mutex;

    use async_trait::async_trait;
    use nagori_core::{
        ClipboardData, ClipboardRepresentation, ClipboardSequence, ClipboardSnapshot,
    };
    use nagori_platform::ClipboardReader;
    use time::OffsetDateTime;

    struct MidWriteReader {
        sequence: Mutex<ClipboardSequence>,
        // `None` models the empty pasteboard observed mid-write.
        text: Mutex<Option<String>>,
    }

    #[async_trait]
    impl ClipboardReader for MidWriteReader {
        async fn current_snapshot(&self) -> Result<ClipboardSnapshot> {
            let snapshot_text = self.text.lock().unwrap().clone();
            let representations = match snapshot_text {
                Some(text) => vec![ClipboardRepresentation {
                    mime_type: "text/plain".to_owned(),
                    data: ClipboardData::Text(text),
                }],
                None => Vec::new(),
            };
            Ok(ClipboardSnapshot {
                sequence: self.sequence.lock().unwrap().clone(),
                captured_at: OffsetDateTime::now_utc(),
                source: None,
                representations,
            })
        }
        async fn current_sequence(&self) -> Result<ClipboardSequence> {
            Ok(self.sequence.lock().unwrap().clone())
        }
    }

    let reader = MidWriteReader {
        sequence: Mutex::new(ClipboardSequence::content_hash("seq-baseline")),
        text: Mutex::new(Some("baseline clip".to_owned())),
    };
    let store = SqliteStore::open_memory().expect("memory store");
    let mut loop_ = CaptureLoop::new(reader, store.clone(), store.clone(), AppSettings::default());

    let t0 = SystemTime::now();
    // Tick 1: baseline clip captured, anchoring `last_sequence`.
    loop_
        .capture_once_at(t0)
        .await
        .unwrap()
        .expect("baseline capture");

    // An external writer takes pasteboard ownership: the changeCount
    // advances, but we observe the brief empty window before the content
    // lands. Small time gaps keep the wake-resync from arming
    // `force_content_check`, so the only thing that can save tick 3 is the
    // empty-snapshot re-read under test.
    *loop_.reader.sequence.lock().unwrap() = ClipboardSequence::content_hash("seq-after-clear");
    *loop_.reader.text.lock().unwrap() = None;
    assert!(
        loop_
            .capture_once_at(t0 + Duration::from_secs(1))
            .await
            .unwrap()
            .is_none(),
        "an empty mid-write snapshot inserts nothing",
    );

    // The write lands at the SAME changeCount (single bump on macOS).
    *loop_.reader.text.lock().unwrap() = Some("file-url clip".to_owned());
    let id = loop_
        .capture_once_at(t0 + Duration::from_secs(2))
        .await
        .unwrap()
        .expect("content landing at the read-empty changeCount must be captured");
    let stored = store.get(id).await.unwrap().expect("stored row");
    assert_eq!(stored.plain_text(), Some("file-url clip"));
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

use std::sync::Arc;

use async_trait::async_trait;
use nagori_core::{
    ClipboardEntry, EntryId, EntryRepository, PasteFormat, Result, SettingsRepository,
};
use nagori_ipc::{GetEntryRequest, IpcRequest, IpcResponse, ListPinnedRequest};

use super::super::*;
use super::{CountingPaste, runtime_with_memory_clipboard, runtime_with_paste};

#[tokio::test]
async fn add_text_refuses_text_that_would_not_fit_the_transport_once_escaped() {
    // Inside `max_entry_size_bytes` in raw bytes, six times that on the wire.
    // Storing it would leave a row the CLI and the desktop could never fetch.
    let (runtime, _) = runtime_with_memory_clipboard();
    let text = "\u{1}".repeat(200_000);
    assert!(text.len() < nagori_core::AppSettings::default().max_entry_size_bytes);

    let err = runtime
        .add_text(text)
        .await
        .expect_err("untransportable text must be refused");

    assert!(
        matches!(err, nagori_core::AppError::Policy(_)),
        "expected a policy refusal, got {err:?}",
    );
    assert!(
        runtime
            .list_recent(10)
            .await
            .expect("list recent")
            .is_empty()
    );
}

#[tokio::test]
async fn add_text_rejects_fully_redacted_secret_and_audits() {
    // A bare OTP body redacts to nothing but `[REDACTED]` under the default
    // `StoreRedacted`, so `add_text` must refuse it (nothing information-bearing
    // to store) with a self-contained Policy error and audit the drop as
    // `secret_redacted_dropped`.
    let (runtime, _) = runtime_with_memory_clipboard();
    let err = runtime
        .add_text("482915".to_owned())
        .await
        .expect_err("fully-redacted secret must be refused");
    assert!(
        matches!(err, nagori_core::AppError::Policy(_)),
        "expected Policy error, got {err:?}",
    );
    assert_eq!(
        runtime
            .store()
            .audit_event_count("secret_redacted_dropped")
            .await
            .unwrap(),
        1,
    );
}

#[tokio::test]
async fn paste_entry_skips_keystroke_when_auto_paste_disabled() {
    let paste = Arc::new(CountingPaste::default());
    let (runtime, clipboard) = runtime_with_paste(paste.clone());
    runtime
        .store()
        .save_settings(AppSettings {
            auto_paste_enabled: false,
            ..AppSettings::default()
        })
        .await
        .expect("save settings");
    let id = runtime
        .add_text("paste me".to_owned())
        .await
        .expect("entry should be added");

    runtime
        .paste_entry(id, None)
        .await
        .expect("paste should succeed");

    assert_eq!(clipboard.current_text().as_deref(), Some("paste me"));
    assert_eq!(paste.calls(), 0, "auto-paste must not fire by default");
}

#[tokio::test]
async fn paste_entry_pastes_when_auto_paste_enabled() {
    let paste = Arc::new(CountingPaste::default());
    let (runtime, _) = runtime_with_paste(paste.clone());
    runtime
        .store()
        .save_settings(AppSettings {
            auto_paste_enabled: true,
            ..AppSettings::default()
        })
        .await
        .expect("save settings");

    let id = runtime
        .add_text("paste me".to_owned())
        .await
        .expect("entry should be added");
    runtime
        .paste_entry(id, None)
        .await
        .expect("paste should succeed");

    assert_eq!(paste.calls(), 1);
}

#[tokio::test]
async fn copy_entry_writes_clipboard_and_increments_use_count() {
    let (runtime, clipboard) = runtime_with_memory_clipboard();
    let id = runtime
        .add_text("copy me".to_owned())
        .await
        .expect("entry should be added");

    runtime.copy_entry(id).await.expect("copy should succeed");

    assert_eq!(clipboard.current_text().as_deref(), Some("copy me"));
    let entry = runtime
        .store()
        .get(id)
        .await
        .expect("store read should succeed")
        .expect("entry should exist");
    assert_eq!(entry.metadata.use_count, 1);
    assert!(entry.metadata.last_used_at.is_some());
}

#[tokio::test]
async fn copy_entry_preserve_hydrates_stored_representations() {
    // Entries captured via the snapshot path persist every preserved
    // representation. Preserve copy-back must replay the whole set
    // through `write_representations` so a multi-rep-aware adapter can
    // re-offer the same MIME variants the source advertised. Use a
    // recording writer to lock the dispatch order: empty rep set →
    // `write_entry`; populated set → `write_representations`.
    use nagori_core::{
        ClipboardData, ClipboardRepresentation, ClipboardSequence, ClipboardSnapshot, ContentHash,
        EntryFactory, RepresentationRole, StoredClipboardRepresentation,
    };
    use time::OffsetDateTime;

    #[derive(Default)]
    struct RecordingWriter {
        entry_calls: tokio::sync::Mutex<Vec<EntryId>>,
        rep_calls: tokio::sync::Mutex<Vec<(EntryId, Vec<StoredClipboardRepresentation>)>>,
    }

    #[async_trait]
    impl ClipboardWriter for RecordingWriter {
        async fn write_entry(&self, entry: &ClipboardEntry) -> Result<()> {
            self.entry_calls.lock().await.push(entry.id);
            Ok(())
        }

        async fn write_plain(&self, _entry: &ClipboardEntry) -> Result<()> {
            Ok(())
        }

        async fn write_text(&self, _text: &str) -> Result<()> {
            Ok(())
        }

        async fn write_representations(
            &self,
            entry: &ClipboardEntry,
            representations: &[StoredClipboardRepresentation],
        ) -> Result<()> {
            self.rep_calls
                .lock()
                .await
                .push((entry.id, representations.to_vec()));
            Ok(())
        }
    }

    let snapshot = ClipboardSnapshot {
        sequence: ClipboardSequence::content_hash(ContentHash::sha256(b"preserve-hydration").value),
        captured_at: OffsetDateTime::now_utc(),
        source: None,
        representations: vec![
            ClipboardRepresentation {
                mime_type: "text/html".to_owned(),
                data: ClipboardData::Text(
                    "<p>preserve hydration <strong>html</strong></p>".to_owned(),
                ),
            },
            ClipboardRepresentation {
                mime_type: "text/plain".to_owned(),
                data: ClipboardData::Text("preserve hydration plain".to_owned()),
            },
        ],
    };
    let entry = EntryFactory::from_snapshot(snapshot)
        .expect("snapshot should yield an entry with stored representations");
    assert!(
        !entry.pending_representations.is_empty(),
        "fixture must produce a multi-rep entry",
    );

    let writer = Arc::new(RecordingWriter::default());
    let store = SqliteStore::open_memory().expect("memory store should open");
    let runtime = NagoriRuntime::builder(store)
        .clipboard(writer.clone() as Arc<dyn ClipboardWriter>)
        .build_for_test();
    let id = runtime
        .store()
        .insert(entry)
        .await
        .expect("insert snapshot-derived entry");

    runtime.copy_entry(id).await.expect("preserve copy");

    let entry_calls = writer.entry_calls.lock().await.clone();
    let rep_calls = writer.rep_calls.lock().await.clone();
    assert!(
        entry_calls.is_empty(),
        "Preserve must route through write_representations, not write_entry; saw {entry_calls:?}",
    );
    assert_eq!(rep_calls.len(), 1, "expected exactly one rep-set write");
    let (called_id, reps) = &rep_calls[0];
    assert_eq!(*called_id, id);
    assert!(
        reps.iter()
            .any(|rep| rep.role == RepresentationRole::Primary && rep.mime_type == "text/html"),
        "stored rep set must include the HTML primary, got {reps:?}",
    );
    assert!(
        reps.iter()
            .any(|rep| rep.role == RepresentationRole::PlainFallback
                && rep.mime_type == "text/plain"),
        "stored rep set must include the plain fallback, got {reps:?}",
    );
}

#[tokio::test]
async fn copy_entry_representation_publishes_only_the_selected_format() {
    // The "paste as <format>" picker resolves a chosen MIME to one stored
    // representation and publishes exactly that — never the primary, never
    // the whole set. A recording writer confirms the single rep handed to
    // `write_representation_exact`, and an absent MIME is rejected rather
    // than silently substituted.
    use nagori_core::{
        ClipboardData, ClipboardRepresentation, ClipboardSequence, ClipboardSnapshot, ContentHash,
        EntryFactory, PasteCategory, StoredClipboardRepresentation,
    };
    use time::OffsetDateTime;

    #[derive(Default)]
    struct ExactRecordingWriter {
        exact_calls: tokio::sync::Mutex<Vec<StoredClipboardRepresentation>>,
    }

    #[async_trait]
    impl ClipboardWriter for ExactRecordingWriter {
        async fn write_entry(&self, _entry: &ClipboardEntry) -> Result<()> {
            Ok(())
        }
        async fn write_plain(&self, _entry: &ClipboardEntry) -> Result<()> {
            Ok(())
        }
        async fn write_text(&self, _text: &str) -> Result<()> {
            Ok(())
        }
        async fn write_representation_exact(
            &self,
            representation: &StoredClipboardRepresentation,
        ) -> Result<()> {
            self.exact_calls.lock().await.push(representation.clone());
            Ok(())
        }
    }

    let snapshot = ClipboardSnapshot {
        sequence: ClipboardSequence::content_hash(ContentHash::sha256(b"paste-as-format").value),
        captured_at: OffsetDateTime::now_utc(),
        source: None,
        representations: vec![
            ClipboardRepresentation {
                mime_type: "text/html".to_owned(),
                data: ClipboardData::Text("<p>pick <strong>me</strong></p>".to_owned()),
            },
            ClipboardRepresentation {
                mime_type: "text/plain".to_owned(),
                data: ClipboardData::Text("pick me".to_owned()),
            },
        ],
    };
    let entry = EntryFactory::from_snapshot(snapshot).expect("multi-rep entry from snapshot");

    let writer = Arc::new(ExactRecordingWriter::default());
    let store = SqliteStore::open_memory().expect("memory store should open");
    let runtime = NagoriRuntime::builder(store)
        .clipboard(writer.clone() as Arc<dyn ClipboardWriter>)
        .build_for_test();
    let id = runtime.store().insert(entry).await.expect("insert entry");

    // The picker enumerates the HTML primary and its plain fallback in
    // canonical order.
    let options = runtime.list_paste_options(id).await.expect("list options");
    assert_eq!(
        options
            .iter()
            .map(|opt| (opt.mime.as_str(), opt.category))
            .collect::<Vec<_>>(),
        vec![
            ("text/html", PasteCategory::Html),
            ("text/plain", PasteCategory::PlainText),
        ],
    );

    // Selecting plain text publishes only the plain rep.
    runtime
        .copy_entry_representation(id, "text/plain")
        .await
        .expect("copy plain representation");
    let calls = writer.exact_calls.lock().await.clone();
    assert_eq!(calls.len(), 1, "expected exactly one exact write");
    assert_eq!(calls[0].mime_type, "text/plain");

    // A MIME the entry doesn't hold is an error, not a fallback.
    let err = runtime
        .copy_entry_representation(id, "image/png")
        .await
        .expect_err("missing format must error");
    assert!(matches!(err, AppError::InvalidInput(_)));
    assert_eq!(
        writer.exact_calls.lock().await.len(),
        1,
        "the rejected request must not publish anything",
    );
}

#[tokio::test]
async fn blocked_entry_offers_and_pastes_no_representation() {
    // `Blocked` entries can never be copied, so the picker offers nothing
    // and a direct representation paste is refused at the policy gate.
    use nagori_core::{EntryFactory, Sensitivity};

    let mut entry = EntryFactory::from_text("blocked body".to_owned());
    entry.sensitivity = Sensitivity::Blocked;

    let store = SqliteStore::open_memory().expect("memory store should open");
    let runtime = NagoriRuntime::builder(store).build_for_test();
    let id = runtime
        .store()
        .insert(entry)
        .await
        .expect("insert blocked entry");

    assert!(
        runtime
            .list_paste_options(id)
            .await
            .expect("list options")
            .is_empty(),
        "a blocked entry must offer no paste options",
    );
    let err = runtime
        .copy_entry_representation(id, "text/plain")
        .await
        .expect_err("blocked entry must refuse a representation paste");
    assert!(matches!(err, AppError::Policy(_)));
}

#[tokio::test]
async fn sensitive_entries_hide_text_until_sensitive_output_is_requested() {
    // A secret wrapped in prose classifies Secret and gets persisted with a
    // partially-redacted body under the default `StoreRedacted` (a token-only
    // body would fully redact and be dropped instead). The IPC gate still
    // applies on top of that: without `include_sensitive` the body is
    // suppressed entirely; with it the caller sees the redacted form (the raw
    // token never reached SQLite, so there is nothing else to reveal).
    let (runtime, _) = runtime_with_memory_clipboard();
    let id = runtime
        .add_text(
            "deploy with token = ghp_abcdefghijklmnopqrstuvwxyz123456 then restart".to_owned(),
        )
        .await
        .expect("secret should be stored as redacted Secret");

    let hidden = runtime
        .handle_ipc(IpcRequest::GetEntry(GetEntryRequest {
            id,
            include_sensitive: false,
        }))
        .await;
    let IpcResponse::Entry(hidden) = hidden else {
        panic!("expected hidden entry");
    };
    assert!(hidden.text.is_none());

    let visible = runtime
        .handle_ipc(IpcRequest::GetEntry(GetEntryRequest {
            id,
            include_sensitive: true,
        }))
        .await;
    let IpcResponse::Entry(visible) = visible else {
        panic!("expected visible entry");
    };
    let body = visible.text.as_deref().expect("redacted body");
    assert!(
        !body.contains("ghp_abcdefghijklmnopqrstuvwxyz123456"),
        "raw token must not be revealed: {body:?}",
    );
    assert!(body.contains("[REDACTED]"), "body: {body:?}");
}

#[tokio::test]
async fn list_pinned_honours_include_sensitive_flag() {
    // Pinned entries previously came back with `text: None` regardless
    // of sensitivity, so even Public pins lost their body and any
    // sensitive pin couldn't be opted-in to. Now the response mirrors
    // ListRecent: Public bodies are always emitted; sensitive bodies
    // require `include_sensitive: true`. The secret body is partially
    // redacted on insert (StoreRedacted), so the include_sensitive=true
    // response surfaces `[REDACTED]` rather than the raw token.
    let (runtime, _) = runtime_with_memory_clipboard();
    let public_id = runtime
        .add_text("public clipboard text".to_owned())
        .await
        .expect("public entry");
    let secret_id = runtime
        .add_text(
            "deploy with token = ghp_abcdefghijklmnopqrstuvwxyz123456 then restart".to_owned(),
        )
        .await
        .expect("secret entry");
    runtime
        .store()
        .set_pinned(public_id, true)
        .await
        .expect("pin public");
    runtime
        .store()
        .set_pinned(secret_id, true)
        .await
        .expect("pin secret");

    let hidden = runtime
        .handle_ipc(IpcRequest::ListPinned(ListPinnedRequest {
            include_sensitive: false,
        }))
        .await;
    let IpcResponse::Entries(hidden) = hidden else {
        panic!("expected entries response, got {hidden:?}");
    };
    let public = hidden.iter().find(|dto| dto.id == public_id).unwrap();
    let secret = hidden.iter().find(|dto| dto.id == secret_id).unwrap();
    assert_eq!(
        public.text.as_deref(),
        Some("public clipboard text"),
        "public pinned entry must retain body without opt-in",
    );
    assert!(
        secret.text.is_none(),
        "sensitive pinned entry must hide body without opt-in",
    );

    let visible = runtime
        .handle_ipc(IpcRequest::ListPinned(ListPinnedRequest {
            include_sensitive: true,
        }))
        .await;
    let IpcResponse::Entries(visible) = visible else {
        panic!("expected entries response");
    };
    let secret = visible.iter().find(|dto| dto.id == secret_id).unwrap();
    let body = secret.text.as_deref().expect("redacted body");
    assert!(
        !body.contains("ghp_abcdefghijklmnopqrstuvwxyz123456"),
        "raw token must not be revealed: {body:?}",
    );
    assert!(body.contains("[REDACTED]"), "body: {body:?}");
}

#[tokio::test]
async fn paste_frontmost_returns_error_when_controller_reports_pasted_false() {
    // The default `NoopPasteController` returns `PasteResult{pasted: false,
    // message: ...}`. Historically `paste_frontmost` discarded the bool
    // and returned Ok(()), so non-macOS paths and any future "tried but
    // OS blocked" outcome silently looked like success. Regression: the
    // runtime must promote `pasted=false` to a classified `Paste` error
    // (synthesis unsupported here) so the UI can warn the user instead of
    // pretending to paste.
    let store = SqliteStore::open_memory().expect("memory store");
    let runtime = NagoriRuntime::builder(store).build_for_test();
    let id = runtime
        .add_text("hello".to_owned())
        .await
        .expect("add should succeed");
    let mut lease = runtime.clipboard_lease().await;
    let publish = lease
        .copy_entry_with_format(id, PasteFormat::Preserve)
        .await
        .expect("copy should succeed");
    let err = lease
        .paste_frontmost(publish)
        .await
        .expect_err("Noop paste must surface as error");
    assert!(
        matches!(
            err,
            AppError::Paste {
                reason: nagori_core::PasteFailureReason::SynthUnsupported,
                ..
            }
        ),
        "got {err:?}"
    );
}

#[tokio::test]
async fn copy_entries_combined_joins_in_order_and_collapses_duplicates() {
    let (runtime, clipboard) = runtime_with_memory_clipboard();
    let first = runtime
        .add_text("alpha".to_owned())
        .await
        .expect("add first");
    let second = runtime
        .add_text("beta".to_owned())
        .await
        .expect("add second");

    // The same id twice must contribute one body, not two: a stale UI
    // selection set or a hand-built IPC request must not double the text (nor
    // eat the byte budget twice).
    runtime
        .copy_entries_combined(&[first, second, first])
        .await
        .expect("combined copy should succeed");

    assert_eq!(clipboard.current_text().as_deref(), Some("alpha\nbeta"));
}

#[tokio::test]
async fn copy_entries_combined_refuses_over_budget_without_touching_db_or_clipboard() {
    // The join is admitted against `max_entry_size_bytes` while the buffer is
    // being built, so an over-budget selection fails before anything is
    // stored or published — the previous shape read every body, joined them,
    // and only then hit the same limit.
    let (runtime, clipboard) = runtime_with_memory_clipboard();
    runtime
        .store()
        .save_settings(AppSettings {
            max_entry_size_bytes: 32,
            ..AppSettings::default()
        })
        .await
        .expect("save settings");
    let first = runtime.add_text("a".repeat(20)).await.expect("add first");
    let second = runtime.add_text("b".repeat(20)).await.expect("add second");
    let before = runtime.list_recent(100).await.expect("list recent").len();

    let err = runtime
        .copy_entries_combined(&[first, second])
        .await
        .expect_err("an over-budget join must be refused");

    assert!(
        matches!(err, nagori_core::AppError::InvalidInput(_)),
        "expected InvalidInput error, got {err:?}",
    );
    assert_eq!(
        clipboard.current_text(),
        None,
        "the clipboard must be untouched",
    );
    assert_eq!(
        runtime.list_recent(100).await.expect("list recent").len(),
        before,
        "no combined row may be stored",
    );
}

#[tokio::test]
async fn copy_entries_combined_refuses_a_join_that_would_not_fit_the_transport() {
    // Two escape-dense bodies, each admissible alone, whose join would exceed
    // the frame once JSON-escaped. Charging raw bytes only would store a
    // combined row no client could read back.
    let (runtime, clipboard) = runtime_with_memory_clipboard();
    // Distinct bodies: identical text would dedupe to a single row and the
    // selection would collapse to one entry.
    let first = runtime
        .add_text(format!("a{}", "\u{1}".repeat(120_000)))
        .await
        .expect("add first");
    let second = runtime
        .add_text(format!("b{}", "\u{1}".repeat(120_000)))
        .await
        .expect("add second");
    let before = runtime.list_recent(100).await.expect("list recent").len();

    let err = runtime
        .copy_entries_combined(&[first, second])
        .await
        .expect_err("a join over the transport ceiling must be refused");

    assert!(
        matches!(err, nagori_core::AppError::InvalidInput(_)),
        "expected InvalidInput error, got {err:?}",
    );
    assert_eq!(
        clipboard.current_text(),
        None,
        "the clipboard must be untouched",
    );
    assert_eq!(
        runtime.list_recent(100).await.expect("list recent").len(),
        before,
        "no combined row may be stored",
    );
}

#[tokio::test]
async fn copy_entries_combined_caps_the_selection_before_reading_bodies() {
    // The count cap exists to bound the *work*: without it a large selection
    // reads N full bodies out of storage before discovering the join cannot
    // fit. Ids beyond the cap are refused up front, so nothing is published.
    let (runtime, clipboard) = runtime_with_memory_clipboard();
    let mut ids = Vec::new();
    for index in 0..3 {
        ids.push(
            runtime
                .add_text(format!("entry {index}"))
                .await
                .expect("add entry"),
        );
    }
    // Pad with distinct ids that do not exist: the cap must trip before any
    // of them is looked up.
    while ids.len() <= nagori_core::MAX_COMBINED_COPY_ENTRIES {
        ids.push(EntryId::new());
    }

    let err = runtime
        .copy_entries_combined(&ids)
        .await
        .expect_err("an oversized selection must be refused");

    assert!(
        matches!(err, nagori_core::AppError::InvalidInput(_)),
        "expected InvalidInput error, got {err:?}",
    );
    assert_eq!(clipboard.current_text(), None);
}

#[tokio::test]
async fn copy_entries_combined_skips_sensitive_and_non_text_rows() {
    // Bulk copy must not concatenate a body the single-row path would have
    // dropped to preview-only, and an image row has no text to contribute.
    let (runtime, clipboard) = runtime_with_memory_clipboard();
    let public = runtime
        .add_text("public body".to_owned())
        .await
        .expect("add public");
    let secret = runtime
        .add_text("token ghp_abcdefghijklmnopqrstuvwxyz123456".to_owned())
        .await
        .expect("add secret");

    runtime
        .copy_entries_combined(&[public, secret])
        .await
        .expect("combined copy should succeed");

    let combined = clipboard.current_text().expect("clipboard text");
    assert_eq!(combined, "public body");
    assert!(
        !combined.contains("ghp_abcdefghijklmnopqrstuvwxyz123456"),
        "a secret body must never be joined into a bulk copy: {combined:?}",
    );
}

#[tokio::test]
async fn copy_entries_combined_refuses_a_selection_with_no_copyable_text() {
    let (runtime, clipboard) = runtime_with_memory_clipboard();
    let err = runtime
        .copy_entries_combined(&[EntryId::new()])
        .await
        .expect_err("a selection of vanished ids must be refused");

    assert!(
        matches!(err, nagori_core::AppError::InvalidInput(_)),
        "expected InvalidInput error, got {err:?}",
    );
    assert_eq!(clipboard.current_text(), None);
}

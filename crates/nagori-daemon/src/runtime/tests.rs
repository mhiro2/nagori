use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use async_trait::async_trait;
use futures::StreamExt;
use nagori_core::{
    AiActionId, AiEvent, AiOverallStatus, AiRequestOptions, ClipboardContent, ClipboardEntry,
    EntryFactory, EntryId, EntryRepository, OnboardingSettings, Result, SearchQuery,
    SettingsRepository,
};
use nagori_ipc::{
    AddEntryRequest, CopyEntryRequest, DeleteEntryRequest, EntryDto, GetEntryRequest, IpcRequest,
    IpcResponse, ListPinnedRequest, ListRecentRequest, PinEntryRequest, SearchRequest,
    SearchResponse, UpdateSettingsRequest,
};
use nagori_platform::{
    MemoryClipboard, PasteResult, PermissionCheckContext, PermissionKind, PermissionState,
    PermissionStatus,
};

use super::*;

fn runtime_with_memory_clipboard() -> (NagoriRuntime, Arc<MemoryClipboard>) {
    let store = SqliteStore::open_memory().expect("memory store should open");
    let clipboard = Arc::new(MemoryClipboard::new());
    let runtime = NagoriRuntime::builder(store)
        .clipboard(clipboard.clone())
        .build_for_test();
    (runtime, clipboard)
}

/// A runtime wired with a `MockBackend`-backed `AppleNative` engine so AI
/// action paths (gating, redaction, streaming, cancellation) are testable
/// on any host. The mock echoes the (already redaction-shaped) input back as
/// `"Summary: <first line>"`, which lets tests assert exactly what the
/// backend received.
fn runtime_with_mock_ai() -> (NagoriRuntime, Arc<MemoryClipboard>) {
    use nagori_ai::{AiEngine, MockBackend};
    use nagori_core::AiProviderKind;

    let store = SqliteStore::open_memory().expect("memory store should open");
    let clipboard = Arc::new(MemoryClipboard::new());
    let engine = AiEngine::builder(AiProviderKind::AppleNative)
        .text_generator(Arc::new(MockBackend::new()))
        .build();
    let runtime = NagoriRuntime::builder(store)
        .clipboard(clipboard.clone())
        .ai_engine(Arc::new(engine))
        .build_for_test();
    (runtime, clipboard)
}

/// A runtime whose `AppleNative` engine also wires a `MockTranslator`, so the
/// translate path (option threading, the translation semaphore, the
/// non-streaming `Done`) is testable on any host. The mock echoes
/// `"[<target>] <input>"`.
fn runtime_with_mock_translator() -> (NagoriRuntime, Arc<MemoryClipboard>) {
    use nagori_ai::{AiEngine, MockBackend, MockTranslator};
    use nagori_core::AiProviderKind;

    let store = SqliteStore::open_memory().expect("memory store should open");
    let clipboard = Arc::new(MemoryClipboard::new());
    let engine = AiEngine::builder(AiProviderKind::AppleNative)
        .text_generator(Arc::new(MockBackend::new()))
        .translator(Arc::new(MockTranslator::new()))
        .build();
    let runtime = NagoriRuntime::builder(store)
        .clipboard(clipboard.clone())
        .ai_engine(Arc::new(engine))
        .build_for_test();
    (runtime, clipboard)
}

/// Enables AI with the `AppleNative` provider plus the given extra settings,
/// so AI-action tests share one place to flip the master toggle.
fn ai_enabled_settings(extra: AppSettings) -> AppSettings {
    use nagori_core::{AiProviderKind, AiSettings};
    AppSettings {
        ai: AiSettings {
            enabled: true,
            provider: AiProviderKind::AppleNative,
            ..AiSettings::default()
        },
        ..extra
    }
}

#[derive(Default)]
struct CountingPaste {
    calls: AtomicUsize,
}

impl CountingPaste {
    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl PasteController for CountingPaste {
    async fn paste_frontmost(&self) -> Result<PasteResult> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(PasteResult {
            pasted: true,
            message: None,
        })
    }
}

fn runtime_with_paste(paste: Arc<dyn PasteController>) -> (NagoriRuntime, Arc<MemoryClipboard>) {
    let store = SqliteStore::open_memory().expect("memory store should open");
    let clipboard = Arc::new(MemoryClipboard::new());
    let runtime = NagoriRuntime::builder(store)
        .clipboard(clipboard.clone())
        .paste(paste)
        .build_for_test();
    (runtime, clipboard)
}

#[tokio::test]
async fn doctor_report_reflects_startup_health_outcome() {
    // Lock the wiring from `StartupHealth` into the `Doctor` IPC
    // handler: `nagori doctor` is the operator-facing surface where
    // a silent capture-init abort has to be visible. Without this
    // test, dropping the `startup` field from `DoctorReport` (or
    // forgetting to record it) would compile cleanly and re-introduce
    // the original "looks ready, isn't" bug.
    let (runtime, _) = runtime_with_memory_clipboard();
    let pending = runtime
        .build_doctor_report()
        .await
        .expect("doctor report builds with default startup state");
    assert!(
        !pending.startup.ready,
        "default startup state must report not-ready"
    );
    assert!(pending.startup.last_error.is_none());

    runtime
        .startup_health()
        .record_capture_failed("could not load settings");
    let failed = runtime
        .build_doctor_report()
        .await
        .expect("doctor report builds after recording a failure");
    assert!(!failed.startup.ready);
    assert_eq!(
        failed.startup.last_error.as_deref(),
        Some("could not load settings"),
    );

    // Late `record_capture_ready` must not flip a recorded failure
    // back to ready — `StartupHealth` is first-outcome-wins.
    runtime.startup_health().record_capture_ready();
    let still_failed = runtime
        .build_doctor_report()
        .await
        .expect("doctor report builds after a no-op ready record");
    assert!(!still_failed.startup.ready);
    assert!(still_failed.startup.last_error.is_some());
}

#[tokio::test]
async fn doctor_report_marks_ready_once_capture_records_success() {
    // Positive case: once the host process records readiness, the
    // doctor surface reports it without needing any additional
    // wiring. Pair with the failure test above so a future refactor
    // that hard-codes `ready: false` or `ready: true` in the
    // builder is caught.
    let (runtime, _) = runtime_with_memory_clipboard();
    runtime.startup_health().record_capture_ready();
    let report = runtime
        .build_doctor_report()
        .await
        .expect("doctor report builds after recording readiness");
    assert!(report.startup.ready);
    assert!(report.startup.last_error.is_none());
}

#[tokio::test]
async fn shutdown_ipc_is_observed_after_worker_starts_waiting() {
    let (runtime, _) = runtime_with_memory_clipboard();
    let mut shutdown = runtime.shutdown_handle();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let worker = tokio::spawn(async move {
        release_rx.await.expect("worker release should be sent");
        shutdown.cancelled().await;
    });

    let response = runtime.handle_ipc(IpcRequest::Shutdown).await;
    assert!(matches!(response, IpcResponse::Ack));

    release_tx.send(()).expect("worker should still be alive");
    tokio::time::timeout(std::time::Duration::from_millis(100), worker)
        .await
        .expect("shutdown should remain visible after the IPC request")
        .expect("worker should not panic");
}

#[tokio::test]
async fn add_entry_ipc_persists_and_searches_text() {
    let (runtime, _) = runtime_with_memory_clipboard();

    let response = runtime
        .handle_ipc(IpcRequest::AddEntry(AddEntryRequest {
            text: "Clipboard history value".to_owned(),
        }))
        .await;
    let IpcResponse::Entry(EntryDto { id, text, .. }) = response else {
        panic!("expected entry response");
    };

    assert_eq!(text.as_deref(), Some("Clipboard history value"));

    let response = runtime
        .handle_ipc(IpcRequest::Search(SearchRequest {
            query: "history".to_owned(),
            limit: 10,
        }))
        .await;
    let IpcResponse::Search(SearchResponse { results }) = response else {
        panic!("expected search response");
    };

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].id, id);
}

#[tokio::test]
async fn ipc_writes_notify_external_mutations_and_reads_do_not() {
    let (runtime, _) = runtime_with_memory_clipboard();
    let mut mutations = runtime.external_mutations_subscribe();
    let baseline = *mutations.borrow_and_update();

    // A read must not signal a corpus mutation — the desktop forwards
    // every change to a palette refresh, so false positives would re-run
    // the open query on every CLI `list` / `search`.
    let response = runtime
        .handle_ipc(IpcRequest::ListRecent(ListRecentRequest {
            limit: 10,
            include_sensitive: false,
        }))
        .await;
    assert!(matches!(response, IpcResponse::Entries(_)));
    assert!(
        !mutations.has_changed().expect("channel should be open"),
        "a read-only request must not bump the mutation counter",
    );

    let response = runtime
        .handle_ipc(IpcRequest::AddEntry(AddEntryRequest {
            text: "added over ipc".to_owned(),
        }))
        .await;
    let IpcResponse::Entry(EntryDto { id, .. }) = response else {
        panic!("expected entry response");
    };
    assert!(
        *mutations.borrow_and_update() > baseline,
        "an IPC add must bump the mutation counter",
    );

    let response = runtime
        .handle_ipc(IpcRequest::PinEntry(PinEntryRequest { id, pinned: true }))
        .await;
    assert!(matches!(response, IpcResponse::Ack));
    assert!(
        mutations.has_changed().expect("channel should be open"),
        "an IPC pin must bump the mutation counter",
    );
    let _ = mutations.borrow_and_update();

    // Copy bumps use_count / last_used_at, which reorders ranking — the
    // palette must hear about it even when the host's capture loop is
    // disabled and never sees the clipboard write.
    let response = runtime
        .handle_ipc(IpcRequest::CopyEntry(CopyEntryRequest { id }))
        .await;
    assert!(matches!(response, IpcResponse::Ack));
    assert!(
        mutations.has_changed().expect("channel should be open"),
        "an IPC copy must bump the mutation counter",
    );
    let _ = mutations.borrow_and_update();

    let response = runtime
        .handle_ipc(IpcRequest::DeleteEntry(DeleteEntryRequest { id }))
        .await;
    assert!(matches!(response, IpcResponse::Ack));
    assert!(
        mutations.has_changed().expect("channel should be open"),
        "an IPC delete must bump the mutation counter",
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
    // OTP-shaped clips classify as Secret and get persisted as
    // `[REDACTED]` under the default `StoreRedacted`. The IPC gate
    // still applies on top of that: without `include_sensitive` the
    // body is suppressed entirely; with it the caller sees the
    // redacted form (the raw OTP never reached SQLite, so there is
    // nothing else to reveal).
    let (runtime, _) = runtime_with_memory_clipboard();
    let id = runtime
        .add_text("123456".to_owned())
        .await
        .expect("OTP should be stored as redacted Secret");

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
    assert_eq!(visible.text.as_deref(), Some("[REDACTED]"));
}

#[tokio::test]
async fn list_pinned_honours_include_sensitive_flag() {
    // Pinned entries previously came back with `text: None` regardless
    // of sensitivity, so even Public pins lost their body and any
    // sensitive pin couldn't be opted-in to. Now the response mirrors
    // ListRecent: Public bodies are always emitted; sensitive bodies
    // require `include_sensitive: true`. The OTP body is redacted on
    // insert (StoreRedacted), so the include_sensitive=true response
    // surfaces `[REDACTED]` rather than the raw 6-digit code.
    let (runtime, _) = runtime_with_memory_clipboard();
    let public_id = runtime
        .add_text("public clipboard text".to_owned())
        .await
        .expect("public entry");
    let secret_id = runtime
        .add_text("123456".to_owned())
        .await
        .expect("OTP entry");
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
    assert_eq!(secret.text.as_deref(), Some("[REDACTED]"));
}

#[tokio::test]
async fn search_before_watch_seed_reads_persisted_recent_order() {
    // The settings watch starts at `AppSettings::default()` until the startup
    // refresh lands. A search racing that window must not serve the default
    // order: it refreshes the watch from the store itself, after which the
    // fast path takes over.
    let (runtime, _) = runtime_with_memory_clipboard();
    let persisted = AppSettings {
        recent_order: nagori_core::RecentOrder::ByUseCount,
        ..Default::default()
    };
    // Write straight to the store so the runtime's publish path never runs —
    // exactly the pre-seed state a freshly built runtime is in.
    runtime
        .store()
        .save_settings(persisted)
        .await
        .expect("settings should persist");
    assert!(!runtime.settings_watch_seeded());

    runtime
        .search(SearchQuery::new("", String::new(), 5))
        .await
        .expect("search should succeed");

    // The fallback read seeded the watch with the persisted value, so later
    // searches use the snapshot.
    assert!(runtime.settings_watch_seeded());
    assert_eq!(
        runtime.current_settings().recent_order,
        nagori_core::RecentOrder::ByUseCount
    );
}

#[tokio::test]
async fn update_settings_ipc_persists_and_publishes_current_settings() {
    let (runtime, _) = runtime_with_memory_clipboard();
    let settings = AppSettings {
        capture_enabled: false,
        global_hotkey: "CmdOrCtrl+Alt+V".to_owned(),
        ..Default::default()
    };
    let value = serde_json::to_value(&settings).expect("settings should serialize");

    let response = runtime
        .handle_ipc(IpcRequest::UpdateSettings(UpdateSettingsRequest { value }))
        .await;

    assert!(matches!(response, IpcResponse::Ack));
    assert_eq!(runtime.current_settings().global_hotkey, "CmdOrCtrl+Alt+V");
    assert!(!runtime.current_settings().capture_enabled);
    let persisted = runtime
        .store()
        .get_settings()
        .await
        .expect("settings should persist");
    assert_eq!(persisted, settings);
}

#[tokio::test]
async fn disabled_cli_ipc_rejects_non_control_requests() {
    let (runtime, _) = runtime_with_memory_clipboard();
    runtime
        .save_settings(AppSettings {
            cli_ipc_enabled: false,
            ..AppSettings::default()
        })
        .await
        .expect("save settings");

    let rejected = runtime
        .handle_ipc(IpcRequest::AddEntry(AddEntryRequest {
            text: "blocked".to_owned(),
        }))
        .await;
    let IpcResponse::Error(err) = rejected else {
        panic!("expected disabled IPC to reject writes");
    };
    assert_eq!(err.code, "permission_error");

    let health = runtime.handle_ipc(IpcRequest::Health).await;
    assert!(
        matches!(health, IpcResponse::Health(_)),
        "health must remain available while IPC is disabled",
    );

    // Capabilities is read-only and treated as a control request,
    // so it must also bypass the cli_ipc_enabled gate. Otherwise
    // a user disabling CLI IPC would also blind the doctor / UI
    // to the OS capability matrix.
    let capabilities = runtime.handle_ipc(IpcRequest::Capabilities).await;
    assert!(
        matches!(capabilities, IpcResponse::Capabilities(_)),
        "capabilities must remain available while IPC is disabled",
    );
}

#[tokio::test]
async fn capabilities_handler_returns_builder_value() {
    // Builder-supplied capabilities must round-trip through the
    // dispatcher — that's the contract the desktop + CLI rely on,
    // so they can render exactly what the daemon was started with
    // rather than reprobing the OS in two places. `ai_actions` is the
    // sole exception: the builder reconciles it against the wired
    // engine (none here → `Unsupported`), so `expected` matches the
    // reconciled value rather than an echoed input.
    use nagori_platform::{Capability, NO_AI_ENGINE_REASON, Platform, SupportTier};

    let store = SqliteStore::open_memory().expect("memory store should open");
    let expected = PlatformCapabilities {
        platform: Platform::MacOS,
        tier: SupportTier::Supported,
        capture_text: Capability::Available,
        capture_image: Capability::Available,
        capture_files: Capability::Available,
        write_text: Capability::Available,
        write_image: Capability::Available,
        clipboard_multi_representation_write: Capability::Available,
        auto_paste: Capability::Available,
        global_hotkey: Capability::Available,
        frontmost_app: Capability::Available,
        permissions_ui: Capability::Available,
        update_check: Capability::Available,
        preview_quick_look: Capability::Available,
        ai_actions: Capability::Unsupported {
            reason: NO_AI_ENGINE_REASON.to_owned(),
        },
    };
    let runtime = NagoriRuntime::builder(store)
        .clipboard(Arc::new(MemoryClipboard::new()))
        .capabilities(expected.clone())
        .build_for_test();

    let response = runtime.handle_ipc(IpcRequest::Capabilities).await;
    let IpcResponse::Capabilities(actual) = response else {
        panic!("expected Capabilities response");
    };
    assert_eq!(*actual, expected);
}

#[test]
fn ai_actions_capability_tracks_the_wired_engine() {
    // The desktop hides every AI surface unless this row is supported.
    // It must reflect the actually-wired engine — not a static per-OS
    // guess — so a host that gains a backend (a test-injected mock
    // today, a runtime-configured provider tomorrow) lights AI up with
    // no second edit, and a host with none never offers a dead toggle.
    let (with_engine, _) = runtime_with_mock_ai();
    assert!(
        with_engine
            .capabilities()
            .ai_actions
            .is_supported_by_platform(),
        "a wired engine must mark ai_actions supported"
    );

    let (without_engine, _) = runtime_with_memory_clipboard();
    assert!(
        matches!(
            without_engine.capabilities().ai_actions,
            Capability::Unsupported { .. }
        ),
        "no engine must mark ai_actions Unsupported"
    );
}

#[tokio::test]
async fn quick_actions_run_under_defaults() {
    use nagori_core::QuickActionId;
    // Quick actions never gate on the AI toggle — they must run under the
    // default (AI off) config or the palette's quick-action buttons would
    // be perma-broken. `FormatJson` needs valid JSON, since anything else
    // surfaces as `AppError::Ai` and would look like a gate rejection.
    let cases: &[(QuickActionId, &str)] = &[
        (QuickActionId::SummarizeFirstSentence, "hello world"),
        (QuickActionId::FormatJson, r#"{"a":1}"#),
        (QuickActionId::ExtractTasks, "TODO: ship the thing"),
        (QuickActionId::RedactSecrets, "no secrets here"),
    ];
    for (action, input) in cases {
        let (runtime, _) = runtime_with_memory_clipboard();
        let id = runtime
            .add_text((*input).to_owned())
            .await
            .expect("entry should be added");
        runtime
            .run_quick_action(id, *action)
            .await
            .unwrap_or_else(|err| panic!("{action:?} must run under defaults; got {err:?}"));
    }
}

/// Inserts a minimal image entry (no text representation) and returns its id.
async fn add_image_entry(runtime: &NagoriRuntime) -> EntryId {
    let content = ClipboardContent::Image(nagori_core::ImageContent {
        width: Some(1),
        height: Some(1),
        byte_count: 4,
        mime_type: Some("image/png".to_owned()),
        pending_bytes: Some(vec![0u8, 1, 2, 3]),
    });
    runtime
        .store
        .insert(EntryFactory::from_content(content, None, None))
        .await
        .expect("image entry should be inserted")
}

#[tokio::test]
async fn quick_action_on_image_is_invalid_input() {
    use nagori_core::QuickActionId;
    // Images have no text representation, so a quick action must refuse with
    // InvalidInput rather than silently running on an empty string.
    let (runtime, _) = runtime_with_memory_clipboard();
    let id = add_image_entry(&runtime).await;
    let err = runtime
        .run_quick_action(id, QuickActionId::SummarizeFirstSentence)
        .await
        .expect_err("quick action on an image must be refused");
    assert!(matches!(err, AppError::InvalidInput(_)), "got {err:?}");
}

#[tokio::test]
async fn ai_action_on_image_is_invalid_input() {
    // Same guard on the model-backed path: an image carries no text to shape.
    let (runtime, _) = runtime_with_mock_ai();
    runtime
        .save_settings(ai_enabled_settings(AppSettings::default()))
        .await
        .expect("save settings");
    let id = add_image_entry(&runtime).await;
    let err = runtime
        .run_ai_action(id, AiActionId::Summarize, AiRequestOptions::default())
        .await
        .expect_err("ai action on an image must be refused");
    assert!(matches!(err, AppError::InvalidInput(_)), "got {err:?}");
}

#[tokio::test]
async fn ai_action_blocked_when_disabled() {
    // With AI off (the default), a model-backed action is refused even
    // though the engine is wired.
    let (runtime, _) = runtime_with_mock_ai();
    let id = runtime
        .add_text("hello".to_owned())
        .await
        .expect("entry should be added");
    let err = runtime
        .run_ai_action(id, AiActionId::Summarize, AiRequestOptions::default())
        .await
        .expect_err("ai actions must be refused when disabled");
    assert!(matches!(err, AppError::Policy(_)), "got {err:?}");
}

#[tokio::test]
async fn ai_action_runs_when_enabled() {
    let (runtime, _) = runtime_with_mock_ai();
    runtime
        .save_settings(ai_enabled_settings(AppSettings::default()))
        .await
        .expect("save settings");
    let id = runtime
        .add_text("hello world".to_owned())
        .await
        .expect("entry should be added");
    let output = runtime
        .run_ai_action(id, AiActionId::Summarize, AiRequestOptions::default())
        .await
        .expect("summarize should succeed when enabled");
    assert!(output.text.starts_with("Summary:"), "got {}", output.text);
    // The registry handle is removed once the run completes.
    assert_eq!(runtime.ai_registry.active_count(), 0);
}

#[tokio::test]
async fn ai_action_times_out_waiting_for_a_wedged_permit() {
    use nagori_core::{AiProviderKind, AiSettings};
    // The request budget is anchored at registration, so a predecessor wedged
    // while holding the single text-generation permit must time *this* request
    // out before it ever reaches the model — not leave it queued forever.
    let (runtime, _) = runtime_with_mock_ai();
    runtime
        .save_settings(AppSettings {
            ai: AiSettings {
                enabled: true,
                provider: AiProviderKind::AppleNative,
                // Small real budget so the test bounds the permit wait without
                // sleeping long (paused time would not advance behind the
                // semaphore acquire).
                request_timeout_ms: 50,
                ..AiSettings::default()
            },
            ..AppSettings::default()
        })
        .await
        .expect("save settings");
    let id = runtime
        .add_text("hello world".to_owned())
        .await
        .expect("entry should be added");

    // Stand in for a wedged predecessor by holding the only text-generation
    // permit for the whole test.
    let held = runtime
        .ai_registry
        .semaphores()
        .text_generation
        .clone()
        .acquire_owned()
        .await
        .expect("hold the only text-generation permit");

    // `AiActionRun` isn't `Debug`, so bind the error explicitly rather than
    // `expect_err`.
    let Err(err) = runtime
        .start_ai_action(id, AiActionId::Summarize, AiRequestOptions::default())
        .await
    else {
        panic!("a wedged permit must time the request out before it starts");
    };
    assert!(
        matches!(&err, AppError::Ai(msg) if msg.contains("timed out waiting for a concurrency permit")),
        "got {err:?}"
    );
    // The timed-out request must not leak a registry slot.
    assert_eq!(runtime.ai_registry.active_count(), 0);
    drop(held);
}

#[tokio::test]
async fn ai_action_unsupported_without_engine() {
    // No engine wired (the default test builder): AI actions surface as
    // Unsupported even when enabled.
    let (runtime, _) = runtime_with_memory_clipboard();
    runtime
        .save_settings(ai_enabled_settings(AppSettings::default()))
        .await
        .expect("save settings");
    let id = runtime
        .add_text("hello".to_owned())
        .await
        .expect("entry should be added");
    let err = runtime
        .run_ai_action(id, AiActionId::Summarize, AiRequestOptions::default())
        .await
        .expect_err("no engine must surface as Unsupported");
    assert!(matches!(err, AppError::Unsupported(_)), "got {err:?}");
}

#[tokio::test]
async fn ai_action_provider_mismatch_is_unsupported() {
    use nagori_core::{AiProviderKind, AiSettings};
    // Engine is AppleNative, but settings select the (unwired)
    // OpenAI-compatible provider.
    let (runtime, _) = runtime_with_mock_ai();
    runtime
        .save_settings(AppSettings {
            ai: AiSettings {
                enabled: true,
                provider: AiProviderKind::OpenAiCompatible,
                ..AiSettings::default()
            },
            ..AppSettings::default()
        })
        .await
        .expect("save settings");
    let id = runtime
        .add_text("hello".to_owned())
        .await
        .expect("entry should be added");
    let err = runtime
        .run_ai_action(id, AiActionId::Summarize, AiRequestOptions::default())
        .await
        .expect_err("provider mismatch must surface as Unsupported");
    assert!(matches!(err, AppError::Unsupported(_)), "got {err:?}");
}

#[tokio::test]
async fn ai_action_not_in_allow_list_is_blocked() {
    use nagori_core::{AiProviderKind, AiSettings};
    // A non-empty allow-list that omits the action blocks it.
    let (runtime, _) = runtime_with_mock_ai();
    runtime
        .save_settings(AppSettings {
            ai: AiSettings {
                enabled: true,
                provider: AiProviderKind::AppleNative,
                allowed_actions: vec![AiActionId::Translate],
                ..AiSettings::default()
            },
            ..AppSettings::default()
        })
        .await
        .expect("save settings");
    let id = runtime
        .add_text("hello".to_owned())
        .await
        .expect("entry should be added");
    let err = runtime
        .run_ai_action(id, AiActionId::Summarize, AiRequestOptions::default())
        .await
        .expect_err("action outside the allow-list must be blocked");
    assert!(matches!(err, AppError::Policy(_)), "got {err:?}");
}

#[tokio::test]
async fn ai_action_applies_user_regex_to_redaction() {
    // The classifier must be settings-aware so a `regex_denylist` rule
    // redacts AI input even on an entry classified before the rule existed.
    // The mock echoes the shaped input, so we can assert what it received.
    let (runtime, _) = runtime_with_mock_ai();
    let id = runtime
        .add_text("ticket INTERNAL-42 stays".to_owned())
        .await
        .expect("public entry should be added");
    runtime
        .save_settings(ai_enabled_settings(AppSettings {
            regex_denylist: vec![r"INTERNAL-\d+".to_owned()],
            ..AppSettings::default()
        }))
        .await
        .expect("save settings");

    let output = runtime
        .run_ai_action(id, AiActionId::Summarize, AiRequestOptions::default())
        .await
        .expect("summarize should succeed");
    assert!(
        !output.text.contains("INTERNAL-42"),
        "user regex must redact AI input, got: {}",
        output.text,
    );
    assert!(
        output.text.contains("[REDACTED]"),
        "expected redaction marker, got: {}",
        output.text,
    );
}

#[tokio::test]
async fn quick_redact_secrets_applies_user_regex_on_public_entry() {
    use nagori_core::QuickActionId;
    // `RedactSecrets` routes input through the settings-aware classifier
    // before the built-in scrub, so a `regex_denylist`-only match on a
    // Public entry is still redacted.
    let (runtime, _) = runtime_with_memory_clipboard();
    let id = runtime
        .add_text("ticket INTERNAL-77 stays".to_owned())
        .await
        .expect("public entry should be added");
    runtime
        .save_settings(AppSettings {
            regex_denylist: vec![r"INTERNAL-\d+".to_owned()],
            ..AppSettings::default()
        })
        .await
        .expect("save settings");

    let output = runtime
        .run_quick_action(id, QuickActionId::RedactSecrets)
        .await
        .expect("redact-secrets should succeed");
    assert!(
        !output.text.contains("INTERNAL-77"),
        "user regex must redact RedactSecrets input, got: {}",
        output.text,
    );
    assert!(
        output.text.contains("[REDACTED]"),
        "expected redaction marker, got: {}",
        output.text,
    );
}

#[tokio::test]
async fn ai_action_blocked_when_input_exceeds_max_bytes() {
    // A body over the per-action byte cap is refused rather than truncated.
    let (runtime, _) = runtime_with_mock_ai();
    runtime
        .save_settings(ai_enabled_settings(AppSettings {
            max_entry_size_bytes: 256 * 1024,
            ..AppSettings::default()
        }))
        .await
        .expect("save settings");
    let large = "a".repeat(65 * 1024);
    let id = runtime
        .add_text(large)
        .await
        .expect("large entry should be added");
    let err = runtime
        .run_ai_action(id, AiActionId::Summarize, AiRequestOptions::default())
        .await
        .expect_err("must refuse inputs over max_bytes");
    assert!(matches!(err, AppError::Policy(_)), "got {err:?}");
}

#[tokio::test]
async fn ai_action_cancel_via_registry_yields_cancelled() {
    // Cancelling by `request_id` through the registry propagates to the
    // stream, which terminates with `Cancelled` and removes its handle.
    let (runtime, _) = runtime_with_mock_ai();
    runtime
        .save_settings(ai_enabled_settings(AppSettings::default()))
        .await
        .expect("save settings");
    let id = runtime
        .add_text("a long body to summarize repeatedly".to_owned())
        .await
        .expect("entry should be added");
    let run = runtime
        .start_ai_action(id, AiActionId::Summarize, AiRequestOptions::default())
        .await
        .expect("summarize should start");
    assert!(runtime.cancel_ai_action(run.request_id));

    let mut events = run.events;
    let mut saw_cancelled = false;
    while let Some(item) = events.next().await {
        if matches!(item, Ok(AiEvent::Cancelled)) {
            saw_cancelled = true;
        }
        assert!(
            !matches!(item, Ok(AiEvent::Done { .. })),
            "a cancelled run must not complete"
        );
    }
    assert!(saw_cancelled, "stream must terminate with Cancelled");
    drop(events);
    assert_eq!(runtime.ai_registry.active_count(), 0);
}

#[tokio::test]
async fn allow_streaming_false_suppresses_intermediate_snapshots() {
    // With the UI streaming toggle off the daemon must surface only the
    // terminal result — no `Delta` / `Replace` — while `Done.final_text` still
    // carries the full output.
    use nagori_ai::{AiEngine, MockBackend};
    use nagori_core::{AiProviderKind, AiSettings};

    let store = SqliteStore::open_memory().expect("memory store");
    let clipboard = Arc::new(MemoryClipboard::new());
    let engine = AiEngine::builder(AiProviderKind::AppleNative)
        .text_generator(Arc::new(MockBackend::with_output("hello world")))
        .build();
    let runtime = NagoriRuntime::builder(store)
        .clipboard(clipboard)
        .ai_engine(Arc::new(engine))
        .build_for_test();
    runtime
        .save_settings(AppSettings {
            ai: AiSettings {
                enabled: true,
                provider: AiProviderKind::AppleNative,
                allow_streaming: false,
                ..AiSettings::default()
            },
            ..AppSettings::default()
        })
        .await
        .expect("save settings");
    let id = runtime
        .add_text("some text".to_owned())
        .await
        .expect("entry should be added");
    let run = runtime
        .start_ai_action(id, AiActionId::Summarize, AiRequestOptions::default())
        .await
        .expect("summarize should start");

    let mut events = run.events;
    let mut final_text = None;
    while let Some(item) = events.next().await {
        match item.expect("no stream error") {
            AiEvent::Delta { .. } | AiEvent::Replace { .. } => {
                panic!("streaming is disabled; no intermediate snapshot may surface")
            }
            AiEvent::Done {
                final_text: text, ..
            } => final_text = Some(text),
            AiEvent::Cancelled => panic!("unexpected cancel"),
        }
    }
    assert_eq!(final_text.as_deref(), Some("hello world"));
    drop(events);
    assert_eq!(runtime.ai_registry.active_count(), 0);
}

#[tokio::test]
async fn request_max_input_tokens_tightens_the_input_budget() {
    // A per-request `max_input_tokens` below the input's estimate refuses the
    // run before the backend is touched, even though the model's hard cap would
    // otherwise have admitted it. No registry slot is leaked.
    let (runtime, _) = runtime_with_mock_ai();
    runtime
        .save_settings(ai_enabled_settings(AppSettings::default()))
        .await
        .expect("save settings");
    let id = runtime
        .add_text("the quick brown fox jumps over the lazy dog".to_owned())
        .await
        .expect("entry should be added");
    let options = AiRequestOptions {
        max_input_tokens: Some(1),
        ..AiRequestOptions::default()
    };
    let Err(err) = runtime
        .start_ai_action(id, AiActionId::Summarize, options)
        .await
    else {
        panic!("a 1-token budget must reject this input");
    };
    assert!(matches!(err, AppError::Policy(_)), "got {err:?}");
    assert_eq!(runtime.ai_registry.active_count(), 0);
}

#[tokio::test]
async fn translate_action_threads_target_language_to_backend() {
    // The translate option (target language) reaches the backend, the
    // translation semaphore is acquired and released, and the non-streaming
    // result arrives as a single terminal `Done`.
    let (runtime, _) = runtime_with_mock_translator();
    runtime
        .save_settings(ai_enabled_settings(AppSettings::default()))
        .await
        .expect("save settings");
    let id = runtime
        .add_text("hello world".to_owned())
        .await
        .expect("entry should be added");
    let options = AiRequestOptions {
        target_language: Some("ja".to_owned()),
        ..AiRequestOptions::default()
    };
    let run = runtime
        .start_ai_action(id, AiActionId::Translate, options)
        .await
        .expect("translate should start");
    let mut events = run.events;
    let mut final_text = None;
    while let Some(item) = events.next().await {
        if let Ok(AiEvent::Done {
            final_text: text, ..
        }) = item
        {
            final_text = Some(text);
            break;
        }
    }
    assert_eq!(final_text.as_deref(), Some("[ja] hello world"));
    drop(events);
    assert_eq!(runtime.ai_registry.active_count(), 0);
}

#[tokio::test]
async fn translate_action_without_target_language_is_unsupported() {
    // With no target language the engine refuses with a capability mismatch,
    // which surfaces as Unsupported.
    let (runtime, _) = runtime_with_mock_translator();
    runtime
        .save_settings(ai_enabled_settings(AppSettings::default()))
        .await
        .expect("save settings");
    let id = runtime
        .add_text("hello".to_owned())
        .await
        .expect("entry should be added");
    let err = runtime
        .run_ai_action(id, AiActionId::Translate, AiRequestOptions::default())
        .await
        .expect_err("translate without a target language must error");
    assert!(matches!(err, AppError::Unsupported(_)), "got {err:?}");
    assert_eq!(runtime.ai_registry.active_count(), 0);
}

#[tokio::test]
async fn run_ai_action_translate_honours_request_options_target_language() {
    // The one-shot IPC path must forward the request options to the backend, so
    // a translate with a target language succeeds rather than being run with
    // the defaults a wire request used to be reduced to.
    let (runtime, _) = runtime_with_mock_translator();
    runtime
        .save_settings(ai_enabled_settings(AppSettings::default()))
        .await
        .expect("save settings");
    let id = runtime
        .add_text("hello".to_owned())
        .await
        .expect("entry should be added");
    let options = AiRequestOptions {
        target_language: Some("ja".to_owned()),
        ..AiRequestOptions::default()
    };
    let output = runtime
        .run_ai_action(id, AiActionId::Translate, options)
        .await
        .expect("translate with a target language should succeed");
    assert_eq!(output.text, "[ja] hello");
    assert_eq!(runtime.ai_registry.active_count(), 0);
}

#[tokio::test]
async fn ai_availability_reports_disabled_by_default() {
    let (runtime, _) = runtime_with_mock_ai();
    let report = runtime.ai_availability().await.expect("availability");
    assert_eq!(report.overall_status, AiOverallStatus::Disabled);
}

#[test]
fn builder_build_errors_when_clipboard_missing() {
    // `build()` is the production entry point: a missing clipboard
    // adapter means the runtime would silently fall back to an
    // in-memory stub and the app would come up with capture
    // forever-disabled. Pin the contract that this returns
    // `AppError::Configuration` instead, so wiring drift is caught
    // at startup rather than as "clipboard quietly stopped working".
    let store = SqliteStore::open_memory().expect("memory store");
    let result = NagoriRuntime::builder(store)
        .paste(Arc::new(nagori_platform::NoopPasteController))
        .build();
    match result {
        Err(AppError::Configuration(ref msg)) if msg.contains("clipboard") => {}
        Err(err) => panic!("expected Configuration(clipboard), got {err:?}"),
        Ok(_) => panic!("expected error, builder accepted missing clipboard"),
    }
}

#[test]
fn builder_build_errors_when_paste_missing() {
    // Symmetrically, a missing paste controller means
    // `paste_frontmost` would always be a no-op success on platforms
    // that forgot to wire their adapter. Surface this as
    // `AppError::Configuration` at build time.
    let store = SqliteStore::open_memory().expect("memory store");
    let result = NagoriRuntime::builder(store)
        .clipboard(Arc::new(MemoryClipboard::new()))
        .build();
    match result {
        Err(AppError::Configuration(ref msg)) if msg.contains("paste") => {}
        Err(err) => panic!("expected Configuration(paste), got {err:?}"),
        Ok(_) => panic!("expected error, builder accepted missing paste controller"),
    }
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
    let err = runtime
        .paste_frontmost()
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
async fn search_cache_serves_repeat_empty_query_without_round_tripping_storage() {
    // Empty query is the hottest path (palette open). The runtime must
    // serve the repeat call from the in-memory cache so SQLite isn't
    // touched once per keystroke.
    let (runtime, _) = runtime_with_memory_clipboard();
    runtime
        .add_text("alpha".to_owned())
        .await
        .expect("seed entry");

    let first = runtime
        .search(SearchQuery::new("", String::new(), 10))
        .await
        .expect("first search");
    assert_eq!(first.len(), 1);
    assert_eq!(
        runtime.search_cache_handle().lock().unwrap().len(),
        1,
        "first search should populate the cache"
    );

    let second = runtime
        .search(SearchQuery::new("", String::new(), 10))
        .await
        .expect("repeat search");
    assert_eq!(second.len(), 1);
    assert_eq!(second[0].entry_id, first[0].entry_id);
}

#[tokio::test]
async fn search_cache_invalidates_after_add_text() {
    // Invariant: any insert through the runtime must drop cached hits so
    // the next search reflects the new row. Without invalidation a freshly
    // captured clip wouldn't surface in the palette until the cache
    // happened to be flushed by some other mutation.
    let (runtime, _) = runtime_with_memory_clipboard();
    runtime.add_text("alpha".to_owned()).await.expect("seed");
    let _ = runtime
        .search(SearchQuery::new("", String::new(), 10))
        .await
        .expect("warm cache");
    assert_eq!(runtime.search_cache_handle().lock().unwrap().len(), 1);

    runtime
        .add_text("beta".to_owned())
        .await
        .expect("second entry");
    assert!(
        runtime.search_cache_handle().lock().unwrap().is_empty(),
        "add_text must invalidate the search cache",
    );

    let results = runtime
        .search(SearchQuery::new("", String::new(), 10))
        .await
        .expect("post-insert search");
    assert_eq!(results.len(), 2);
}

#[tokio::test]
async fn search_cache_invalidates_after_pin_toggle() {
    // `recent_entries` hoists pinned rows above plain ones, so toggling
    // the pin bit reorders the empty-query result. Stale cache hits would
    // hide the pin until something else cleared the cache.
    let (runtime, _) = runtime_with_memory_clipboard();
    let id = runtime
        .add_text("alpha".to_owned())
        .await
        .expect("seed entry");
    let _ = runtime
        .search(SearchQuery::new("", String::new(), 10))
        .await
        .expect("warm cache");

    runtime
        .pin_entry(id, true)
        .await
        .expect("pin should succeed");
    assert!(
        runtime.search_cache_handle().lock().unwrap().is_empty(),
        "pin_entry must invalidate the search cache",
    );
}

#[derive(Debug)]
struct StubPermissionChecker {
    check_response: std::sync::Mutex<Vec<PermissionStatus>>,
    check_observed_ctx: std::sync::Mutex<Option<PermissionCheckContext>>,
    request_response: std::sync::Mutex<PermissionStatus>,
    request_observed_prompt: std::sync::Mutex<Option<bool>>,
}

impl StubPermissionChecker {
    fn new(initial: Vec<PermissionStatus>, request: PermissionStatus) -> Self {
        Self {
            check_response: std::sync::Mutex::new(initial),
            check_observed_ctx: std::sync::Mutex::new(None),
            request_response: std::sync::Mutex::new(request),
            request_observed_prompt: std::sync::Mutex::new(None),
        }
    }

    fn set_check(&self, response: Vec<PermissionStatus>) {
        *self.check_response.lock().unwrap() = response;
    }

    fn set_request(&self, status: PermissionStatus) {
        *self.request_response.lock().unwrap() = status;
    }

    fn observed_ctx(&self) -> Option<PermissionCheckContext> {
        self.check_observed_ctx.lock().unwrap().clone()
    }

    fn observed_prompt(&self) -> Option<bool> {
        *self.request_observed_prompt.lock().unwrap()
    }
}

#[async_trait]
impl PermissionChecker for StubPermissionChecker {
    async fn check(&self, ctx: &PermissionCheckContext) -> Result<Vec<PermissionStatus>> {
        *self.check_observed_ctx.lock().unwrap() = Some(ctx.clone());
        Ok(self.check_response.lock().unwrap().clone())
    }

    async fn request_accessibility(&self, prompt: bool) -> Result<PermissionStatus> {
        *self.request_observed_prompt.lock().unwrap() = Some(prompt);
        Ok(self.request_response.lock().unwrap().clone())
    }
}

fn accessibility_row(state: PermissionState) -> PermissionStatus {
    PermissionStatus {
        kind: PermissionKind::Accessibility,
        state,
        message: None,
        reason_code: None,
        setup_route: None,
        docs_url: None,
    }
}

#[tokio::test]
async fn request_accessibility_stamps_prompted_at_when_prompt_true() {
    // The "NotRequested vs PromptShownNotGranted" UI branch keys off
    // `onboarding.accessibility_prompted_at`. Verify the
    // runtime persists that timestamp the first time we ask the host
    // to surface the TCC dialog (`prompt = true`).
    let store = SqliteStore::open_memory().expect("memory store should open");
    let stub = Arc::new(StubPermissionChecker::new(
        vec![accessibility_row(PermissionState::NotDetermined)],
        accessibility_row(PermissionState::Denied),
    ));
    let runtime = NagoriRuntime::builder(store)
        .permissions(stub.clone())
        .build_for_test();
    // Pre-condition: never prompted, so the context the checker sees
    // should be empty.
    let _ = runtime.permission_check().await.expect("permission_check");
    let ctx = stub.observed_ctx().expect("check was invoked");
    assert!(ctx.accessibility_prompted_at.is_none());

    let _ = runtime
        .request_accessibility(true)
        .await
        .expect("request_accessibility");
    assert_eq!(stub.observed_prompt(), Some(true));

    // Post-condition: the runtime persisted the prompt timestamp, and a
    // follow-up check carries it through the context so the checker
    // can discriminate Denied from NotDetermined.
    let settings = runtime.current_settings();
    assert!(
        settings.onboarding.accessibility_prompted_at.is_some(),
        "prompt = true must stamp accessibility_prompted_at",
    );
    let _ = runtime.permission_check().await.expect("permission_check");
    let ctx_after = stub.observed_ctx().expect("check was invoked");
    assert!(ctx_after.accessibility_prompted_at.is_some());
}

#[tokio::test]
async fn request_accessibility_skips_prompted_at_when_prompt_false() {
    // `prompt = false` is the "just probe, don't surface UI" path
    // (`AXIsProcessTrustedWithOptions(prompt:NO)`); it must not move
    // the persisted prompt timestamp, otherwise a UI re-render that
    // calls the no-prompt probe would erroneously flip NotRequested.
    let store = SqliteStore::open_memory().expect("memory store should open");
    let stub = Arc::new(StubPermissionChecker::new(
        vec![accessibility_row(PermissionState::NotDetermined)],
        accessibility_row(PermissionState::Denied),
    ));
    let runtime = NagoriRuntime::builder(store)
        .permissions(stub.clone())
        .build_for_test();

    let _ = runtime
        .request_accessibility(false)
        .await
        .expect("request_accessibility");
    assert_eq!(stub.observed_prompt(), Some(false));

    let settings = runtime.current_settings();
    assert!(
        settings.onboarding.accessibility_prompted_at.is_none(),
        "prompt = false must leave accessibility_prompted_at untouched",
    );
}

#[tokio::test]
async fn permission_check_stamps_first_granted_once() {
    // `accessibility_first_granted_at` is a sticky onboarding marker:
    // once stamped, it must not be overwritten on subsequent grants
    // (the UI uses it for "you're set up" copy timing and onboarding
    // exit). Verify both the first-grant write and the no-op on a
    // second Granted observation.
    let store = SqliteStore::open_memory().expect("memory store should open");
    let stub = Arc::new(StubPermissionChecker::new(
        vec![accessibility_row(PermissionState::Granted)],
        accessibility_row(PermissionState::Granted),
    ));
    let runtime = NagoriRuntime::builder(store)
        .permissions(stub.clone())
        .build_for_test();

    let _ = runtime.permission_check().await.expect("first check");
    let stamped = runtime
        .current_settings()
        .onboarding
        .accessibility_first_granted_at
        .expect("first Granted observation must stamp the marker");

    // Tick the clock through a short sleep so any rewrite would
    // produce a strictly-later timestamp.
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    let _ = runtime.permission_check().await.expect("second check");
    let after = runtime
        .current_settings()
        .onboarding
        .accessibility_first_granted_at
        .expect("marker remains set on subsequent grants");
    assert_eq!(stamped, after, "first_granted_at must be sticky");
}

#[tokio::test]
async fn permission_check_does_not_stamp_when_not_granted() {
    // Symmetry with the sticky-marker test: a Denied / NotDetermined
    // observation must leave the marker absent, otherwise the Setup
    // card would skip its "Grant access" CTA and the doctor would
    // claim onboarding completed.
    let store = SqliteStore::open_memory().expect("memory store should open");
    let stub = Arc::new(StubPermissionChecker::new(
        vec![accessibility_row(PermissionState::Denied)],
        accessibility_row(PermissionState::Denied),
    ));
    let runtime = NagoriRuntime::builder(store)
        .permissions(stub.clone())
        .build_for_test();

    let _ = runtime.permission_check().await.expect("check");
    assert!(
        runtime
            .current_settings()
            .onboarding
            .accessibility_first_granted_at
            .is_none(),
    );

    // Flip to Granted and re-check; the marker should now appear.
    stub.set_check(vec![accessibility_row(PermissionState::Granted)]);
    let _ = runtime.permission_check().await.expect("check after grant");
    assert!(
        runtime
            .current_settings()
            .onboarding
            .accessibility_first_granted_at
            .is_some(),
    );
}

#[tokio::test]
async fn request_accessibility_stamps_first_granted_on_grant() {
    // The `request_accessibility` path also has to stamp the marker on
    // its own (rather than waiting for the next `permission_check`),
    // because the Setup card finishes its flow as soon as the trait
    // call resolves Granted — without this hook the marker would lag
    // by one full check cycle.
    let store = SqliteStore::open_memory().expect("memory store should open");
    let stub = Arc::new(StubPermissionChecker::new(
        vec![accessibility_row(PermissionState::NotDetermined)],
        accessibility_row(PermissionState::Granted),
    ));
    let runtime = NagoriRuntime::builder(store)
        .permissions(stub.clone())
        .build_for_test();
    let _ = runtime
        .request_accessibility(true)
        .await
        .expect("request_accessibility");
    let onboarding = runtime.current_settings().onboarding;
    assert!(
        onboarding.accessibility_first_granted_at.is_some(),
        "Granted result must stamp first_granted_at without an extra permission_check"
    );
    assert!(onboarding.accessibility_prompted_at.is_some());
    // Flip the response back to Denied and re-call: the sticky marker
    // must not regress, even though the new observation is not Granted.
    stub.set_request(accessibility_row(PermissionState::Denied));
    let before = runtime
        .current_settings()
        .onboarding
        .accessibility_first_granted_at;
    let _ = runtime
        .request_accessibility(true)
        .await
        .expect("request_accessibility");
    assert_eq!(
        runtime
            .current_settings()
            .onboarding
            .accessibility_first_granted_at,
        before,
        "first_granted_at must be sticky across later Denied results",
    );
}

#[tokio::test]
async fn save_settings_preserves_persisted_onboarding_markers() {
    // The runtime owns the `onboarding` markers; an `update_settings`
    // IPC from the desktop shell (which round-trips a possibly-stale
    // snapshot of the markers) must never overwrite a marker that
    // the daemon stamped between the frontend's get_settings and
    // its follow-up update_settings. `save_settings` re-merges the
    // persisted `onboarding` block inside the write lock to enforce
    // that invariant.
    let store = SqliteStore::open_memory().expect("memory store should open");
    let stub = Arc::new(StubPermissionChecker::new(
        vec![accessibility_row(PermissionState::Granted)],
        accessibility_row(PermissionState::Granted),
    ));
    let runtime = NagoriRuntime::builder(store)
        .permissions(stub.clone())
        .build_for_test();
    // Stamp the marker via a permission_check observation.
    let _ = runtime.permission_check().await.expect("permission_check");
    let stamped = runtime
        .current_settings()
        .onboarding
        .accessibility_first_granted_at
        .expect("first_granted_at must be set after Granted observation");
    // Simulate a stale frontend snapshot: read settings, zero the
    // onboarding markers, then write back. The persisted markers
    // must survive.
    let mut stale = runtime.current_settings();
    stale.onboarding = OnboardingSettings::default();
    runtime
        .save_settings(stale)
        .await
        .expect("save_settings round-trip");
    let after = runtime.current_settings().onboarding;
    assert_eq!(
        after.accessibility_first_granted_at,
        Some(stamped),
        "save_settings must restore onboarding markers from the store",
    );
}

#[tokio::test]
async fn set_capture_enabled_does_not_roll_back_concurrent_field_edits() {
    // The tray's pause/resume toggles only `capture_enabled`. The old
    // implementation read a full settings snapshot *outside* the write
    // lock, then saved it — so a `save_settings` (e.g. an
    // `update_settings` IPC editing `global_hotkey`) landing in between
    // got silently rolled back by the stale blob. `mutate_settings`
    // reads-modifies-writes inside the lock, so whichever op commits
    // second still observes (and re-persists) the other's change.
    let (runtime, _) = runtime_with_memory_clipboard();
    assert!(runtime.current_settings().capture_enabled);

    let mut edited = runtime.current_settings();
    edited.global_hotkey = "CmdOrCtrl+Alt+V".to_owned();

    let (toggled, saved) = tokio::join!(
        runtime.set_capture_enabled(false),
        runtime.save_settings(edited)
    );
    let toggled = toggled.expect("capture toggle should succeed");
    saved.expect("concurrent settings save should succeed");

    // The toggle's own return value must reflect the persisted state,
    // not a pre-toggle snapshot.
    assert!(!toggled.capture_enabled);

    let persisted = runtime
        .store()
        .get_settings()
        .await
        .expect("settings should persist");
    assert_eq!(
        persisted.global_hotkey, "CmdOrCtrl+Alt+V",
        "capture toggle must not roll back a concurrent global_hotkey edit",
    );
}

#[tokio::test]
async fn search_cache_skips_long_queries() {
    // Long queries turn over too quickly to be worth caching, and would
    // crowd the small LRU. Verify we don't cache anything for a query
    // longer than `CACHEABLE_QUERY_LEN`.
    let (runtime, _) = runtime_with_memory_clipboard();
    runtime
        .add_text("alphabetagamma".to_owned())
        .await
        .expect("seed");
    let long = "alphabetagamma".to_owned();
    let _ = runtime
        .search(SearchQuery::new(long.clone(), long, 10))
        .await
        .expect("search");
    assert!(
        runtime.search_cache_handle().lock().unwrap().is_empty(),
        "queries longer than the cache threshold must not populate the cache",
    );
}

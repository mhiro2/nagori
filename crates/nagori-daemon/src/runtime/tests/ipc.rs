use std::sync::Arc;

use nagori_core::SettingsRepository;
use nagori_ipc::{
    AddEntryRequest, CopyEntryRequest, DeleteEntryRequest, EntryDto, IpcRequest, IpcResponse,
    ListRecentRequest, PinEntryRequest, SearchRequest, SearchResponse, UpdateSettingsRequest,
};
use nagori_platform::MemoryClipboard;

use super::super::*;
use super::{runtime_with_memory_clipboard, runtime_with_mock_ai};

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

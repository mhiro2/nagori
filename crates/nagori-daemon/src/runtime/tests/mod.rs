//! Unit tests for [`NagoriRuntime`], grouped by concern. Shared runtime
//! builders and mock controllers live in this module; the per-concern
//! submodules hold only tests.

mod ai;
mod clipboard;
mod ipc;
mod lifecycle;
mod permissions;
mod search;
mod settings;

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use async_trait::async_trait;
use nagori_core::Result;
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

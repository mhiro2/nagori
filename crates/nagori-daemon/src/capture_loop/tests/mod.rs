//! Unit tests for [`CaptureLoop`], grouped by concern. The shared loop
//! builder lives here; the per-concern submodules hold only tests.

mod backoff;
mod baseline;
mod dedup_insert;
mod filters;
mod images;
mod secure_focus;

use std::sync::Arc;

use nagori_platform::MemoryClipboard;
use nagori_storage::SqliteStore;

use super::*;

fn loop_for(
    clipboard: Arc<MemoryClipboard>,
    store: SqliteStore,
    settings: AppSettings,
) -> CaptureLoop<Arc<MemoryClipboard>, SqliteStore, SqliteStore> {
    CaptureLoop::new(clipboard, store.clone(), store, settings)
}

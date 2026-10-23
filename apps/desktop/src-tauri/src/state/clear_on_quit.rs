//! Desktop wiring for the shared clear-on-quit lifecycle
//! (`nagori_daemon::clear_on_quit`): the marker is keyed to the DB this
//! launch opened, a pending purge is finished before the state reaches Tauri,
//! and the exit path hands the marker to the shared quit purge.

use nagori_core::Result;
use nagori_daemon::ClearOnQuitMarker;

use super::AppState;

impl AppState {
    /// The clear-on-quit purge-pending marker for this launch's DB. `None`
    /// for `build`-only callers (tests, in-memory stores), which own no
    /// on-disk directory to put it in.
    pub const fn clear_on_quit_marker(&self) -> Option<&ClearOnQuitMarker> {
        self.clear_on_quit_marker.as_ref()
    }

    /// Tell the next launch that this process is restarting, so it waits for
    /// the instance lock this process still holds. Best-effort: without the
    /// marker the restarted instance shows the "already running" fallback,
    /// which the user can dismiss and relaunch from.
    pub(crate) fn announce_restart(&self) {
        if let Some(Err(err)) = self
            .restart_handoff
            .as_ref()
            .map(super::restart_handoff::RestartHandoff::announce)
        {
            tracing::warn!(error = %err, "restart_handoff_marker_write_failed");
        }
    }

    /// Finish a clear-on-quit purge a previous owner of this DB — this app,
    /// the standalone daemon, or the CLI — could not complete. Runs during
    /// `try_new_at`, before the state is handed to Tauri and before any window
    /// can serve a row, so a failure surfaces the startup fallback window and
    /// leaves the marker for the next launch to retry rather than booting into
    /// a session that still shows the history the user asked to clear.
    pub(super) fn finish_pending_clear_on_quit(&self) -> Result<()> {
        let Some(marker) = self.clear_on_quit_marker.as_ref() else {
            return Ok(());
        };
        tauri::async_runtime::block_on(nagori_daemon::finish_pending_clear_on_quit(
            self.runtime.store(),
            marker,
        ))
        .map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use nagori_core::{EntryFactory, EntryRepository};

    use super::*;
    use crate::state::test_support::build_test_state;

    #[test]
    fn finishing_without_a_marker_path_is_a_no_op() {
        // A `build`-only state (in-memory store) has nowhere to keep a marker,
        // so nothing is ever pending for it.
        let state = build_test_state();
        assert!(state.clear_on_quit_marker().is_none());
        state
            .finish_pending_clear_on_quit()
            .expect("no marker means nothing to finish");
    }

    #[test]
    fn a_pending_marker_is_finished_before_the_state_is_used() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let marker = ClearOnQuitMarker::for_db(&tmp.path().join("nagori.sqlite"));
        let mut state = build_test_state();
        tauri::async_runtime::block_on(
            state
                .runtime
                .store()
                .insert(EntryFactory::from_text("history to clear")),
        )
        .expect("insert");
        marker.mark_pending().expect("mark");
        state.clear_on_quit_marker = Some(marker.clone());

        state.finish_pending_clear_on_quit().expect("finish");

        let left =
            tauri::async_runtime::block_on(state.runtime.store().list_recent(10)).expect("list");
        assert!(left.is_empty(), "the pending purge must run on open");
        assert!(!marker.is_pending());
    }
}

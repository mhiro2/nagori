//! Clear-on-quit purge-pending marker: write it before a shutdown purge,
//! remove it once the purge lands, and finish a purge a previous session
//! could not complete. Keeping the sentinel on the filesystem (beside the DB)
//! lets a hard kill that ran no DB write still leave a trace the next launch
//! resumes fail-closed.

use std::path::{Path, PathBuf};

use nagori_core::{AppError, Result};

use super::AppState;

impl AppState {
    /// Write the clear-on-quit purge-pending marker. Called by the shutdown
    /// path *before* it attempts the purge so a timeout, crash, or kill
    /// mid-purge is finished on the next launch instead of leaving the history
    /// the user asked to clear. No-op (`Ok`) for `build`-only callers (no
    /// marker path). The `io::Result` is surfaced rather than swallowed: a
    /// write failure means a subsequent purge timeout could not be resumed, so
    /// the caller logs it instead of silently losing the guarantee.
    pub fn mark_clear_on_quit_pending(&self) -> std::io::Result<()> {
        let Some(path) = self.clear_on_quit_marker.as_ref() else {
            return Ok(());
        };
        std::fs::write(path, b"")
    }

    /// Remove the purge-pending marker after a purge has actually completed.
    /// A missing file is success (the marker was never written or already
    /// gone). A real removal failure is returned, not swallowed: a marker left
    /// behind after a *successful* purge would make the next launch purge again
    /// — including any history captured in between — so the caller must treat
    /// it as a hard error rather than logging and moving on.
    pub fn clear_clear_on_quit_pending(&self) -> std::io::Result<()> {
        let Some(path) = self.clear_on_quit_marker.as_ref() else {
            return Ok(());
        };
        match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err),
        }
    }

    /// Whether a clear-on-quit purge from a previous session is still pending,
    /// i.e. the marker survived because the shutdown purge did not complete.
    /// An I/O error probing the marker is treated as **present** so an
    /// unreadable marker fails closed into a purge attempt rather than silently
    /// skipping the user's clear-on-quit intent.
    #[must_use]
    pub fn clear_on_quit_marker_present(&self) -> bool {
        let Some(path) = self.clear_on_quit_marker.as_ref() else {
            return false;
        };
        match path.try_exists() {
            Ok(present) => present,
            Err(err) => {
                tracing::warn!(error = %err, "clear_on_quit_marker_probe_failed");
                true
            }
        }
    }

    /// Finish a clear-on-quit purge a previous session could not complete
    /// within its shutdown budget. Runs during `try_new_at` — before the state
    /// is handed to Tauri and before any window can serve a row — so the purge
    /// is fail-closed: if the marker is present the purge must succeed (and its
    /// marker must be removed) before the app starts normally. A failure is
    /// returned as a startup error, which surfaces the fallback window and
    /// leaves the marker so the next launch retries, rather than booting into a
    /// session that still shows — or later re-purges — the history the user
    /// asked to clear.
    pub(super) fn finish_pending_clear_on_quit(&self) -> Result<()> {
        if !self.clear_on_quit_marker_present() {
            return Ok(());
        }
        tracing::warn!("clear_on_quit_resuming_pending_purge");
        tauri::async_runtime::block_on(self.runtime.clear_non_pinned()).map_err(|err| {
            AppError::storage(format!(
                "could not finish the clear-on-quit purge left pending by the previous session: \
                 {err}. The clipboard history you asked to clear on quit is still present; relaunch \
                 to retry, or move the database aside to start fresh."
            ))
        })?;
        self.clear_clear_on_quit_pending().map_err(|err| {
            AppError::storage(format!(
                "finished the clear-on-quit purge but could not remove its marker: {err}. Remove \
                 the clear-on-quit.pending file in the database directory so the next launch does \
                 not purge again."
            ))
        })?;
        Ok(())
    }
}

/// Path to the clear-on-quit purge-pending marker: a sentinel file beside the
/// DB. Kept on the filesystem rather than in the DB so it can be written /
/// checked even when the DB itself is the contended resource that made the
/// shutdown purge time out, and so a hard kill that never ran any DB write
/// still leaves a trace the next launch can act on.
///
/// Keyed to the DB *file*, not just its directory: a marker named after the
/// directory alone would let a different `NAGORI_DB_PATH` pointed at another DB
/// in the same directory inherit a stale marker and purge the wrong database's
/// history at startup. Appending the suffix to the full path yields e.g.
/// `…/nagori.sqlite.clear-on-quit.pending`.
pub(super) fn clear_on_quit_marker_path(db_path: &Path) -> PathBuf {
    let mut marker = db_path.as_os_str().to_owned();
    marker.push(".clear-on-quit.pending");
    PathBuf::from(marker)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::test_support::build_test_state;

    #[test]
    fn clear_on_quit_marker_path_is_keyed_to_the_db_file() {
        // Suffixing the full DB path (not just its directory) keeps two DBs in
        // the same directory from inheriting each other's stale marker.
        let marker = clear_on_quit_marker_path(std::path::Path::new("/data/nagori.sqlite"));
        assert_eq!(
            marker,
            std::path::PathBuf::from("/data/nagori.sqlite.clear-on-quit.pending"),
        );
    }

    #[test]
    fn clear_on_quit_marker_write_probe_remove_cycle() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let marker = tmp.path().join("nagori.sqlite.clear-on-quit.pending");
        let mut state = build_test_state();
        state.clear_on_quit_marker = Some(marker.clone());

        // Nothing pending until a shutdown purge is staged.
        assert!(!state.clear_on_quit_marker_present());

        // Marking writes the sentinel so a crash before the purge finishes is
        // resumed on the next launch.
        state
            .mark_clear_on_quit_pending()
            .expect("mark writes the file");
        assert!(state.clear_on_quit_marker_present());
        assert!(marker.exists());

        // Clearing after a completed purge removes the sentinel, and clearing
        // again (already gone) is success — a missing marker is not an error.
        state
            .clear_clear_on_quit_pending()
            .expect("clear removes the file");
        assert!(!state.clear_on_quit_marker_present());
        assert!(!marker.exists());
        state
            .clear_clear_on_quit_pending()
            .expect("clearing an absent marker is idempotent");
    }

    #[test]
    fn clear_on_quit_marker_ops_are_noops_without_a_configured_path() {
        // A disabled clear-on-quit (or in-memory store) leaves the marker path
        // `None`; every op must be a silent no-op and nothing is ever pending.
        let state = build_test_state();
        assert!(state.clear_on_quit_marker.is_none());
        assert!(!state.clear_on_quit_marker_present());
        state
            .mark_clear_on_quit_pending()
            .expect("mark is a no-op when unconfigured");
        assert!(!state.clear_on_quit_marker_present());
        state
            .clear_clear_on_quit_pending()
            .expect("clear is a no-op when unconfigured");
    }
}

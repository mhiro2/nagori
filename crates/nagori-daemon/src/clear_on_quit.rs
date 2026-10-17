//! Clear-on-quit lifecycle shared by every host that owns the store: the
//! desktop app, the standalone daemon, and the CLI's direct-store path.
//!
//! The guarantee has two halves, and every host must honour both:
//!
//! * **Quit** — when `clear_on_quit` is on, a host that is shutting down
//!   writes a purge-pending marker beside the DB, purges every non-pinned
//!   entry, and removes the marker once the purge has landed
//!   ([`clear_history_on_quit`]).
//! * **Open** — a marker that survived (the purge timed out, the process was
//!   killed mid-purge, the purge failed) means the previous owner never
//!   finished. Whoever opens the store next completes the purge *before* it
//!   serves any history or records anything new ([`finish_pending_clear_on_quit`]),
//!   so the cleared history is never shown again and nothing captured
//!   afterwards is swept by a late purge.
//!
//! The marker lives on the filesystem rather than in the DB so it can be
//! written and checked even when the DB is the contended resource that made
//! the purge time out, and so a hard kill that ran no DB write still leaves a
//! trace.

use std::path::{Path, PathBuf};
use std::time::Duration;

use nagori_core::{AppError, Result};
use nagori_storage::SqliteStore;

use crate::NagoriRuntime;

/// Purge-pending marker for one DB file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClearOnQuitMarker {
    path: PathBuf,
}

impl ClearOnQuitMarker {
    /// Marker for the DB at `db_path`: a sentinel file beside it.
    ///
    /// Keyed to the DB *file*, not just its directory: a marker named after
    /// the directory alone would let a different `NAGORI_DB_PATH` pointed at
    /// another DB in the same directory inherit a stale marker and purge the
    /// wrong database's history. Appending the suffix to the full path yields
    /// e.g. `…/nagori.sqlite.clear-on-quit.pending`.
    ///
    /// The path is resolved through symlinks first, so every host reaching the
    /// same store — one through `--db` pointed at an alias, another through
    /// the real path — agrees on a single marker. A DB that does not exist yet
    /// cannot be resolved itself, so its directory is resolved instead and the
    /// file name re-attached.
    #[must_use]
    pub fn for_db(db_path: &Path) -> Self {
        let mut marker = resolve_db_path(db_path).into_os_string();
        marker.push(".clear-on-quit.pending");
        Self {
            path: PathBuf::from(marker),
        }
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Write the marker. Called *before* a quit purge so a timeout, crash, or
    /// kill mid-purge is finished by the next owner instead of leaving the
    /// history the user asked to clear.
    pub fn mark_pending(&self) -> std::io::Result<()> {
        std::fs::write(&self.path, b"")
    }

    /// Remove the marker after a purge has actually completed. A missing file
    /// is success. A real removal failure is returned: a marker left behind
    /// after a *successful* purge would make the next owner purge again —
    /// including any history captured in between.
    pub fn clear_pending(&self) -> std::io::Result<()> {
        match std::fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err),
        }
    }

    /// Whether a quit purge is still pending. An I/O error probing the marker
    /// counts as **pending** so an unreadable marker fails closed into a purge
    /// attempt rather than silently skipping the user's clear-on-quit intent.
    #[must_use]
    pub fn is_pending(&self) -> bool {
        match self.path.try_exists() {
            Ok(present) => present,
            Err(err) => {
                tracing::warn!(error = %err, "clear_on_quit_marker_probe_failed");
                true
            }
        }
    }
}

fn resolve_db_path(db_path: &Path) -> PathBuf {
    std::fs::canonicalize(db_path).unwrap_or_else(|_| {
        let parent = match db_path.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => parent,
            _ => Path::new("."),
        };
        let resolved_parent =
            std::fs::canonicalize(parent).unwrap_or_else(|_| parent.to_path_buf());
        match db_path.file_name() {
            Some(name) => resolved_parent.join(name),
            None => resolved_parent,
        }
    })
}

/// Finish a clear-on-quit purge a previous owner of `store` could not complete.
///
/// Every host calls this right after it opens the store and before any history
/// is served or captured, while holding the store's single-instance lock: the
/// marker is checked under the lock, so a purge can never run after another
/// owner already finished it and started recording new history.
///
/// Fail-closed: when the marker is present the purge must succeed and the
/// marker must be removed, or an error is returned and the marker stays so the
/// next open retries. Returns whether a pending purge was finished.
pub async fn finish_pending_clear_on_quit(
    store: &SqliteStore,
    marker: &ClearOnQuitMarker,
) -> Result<bool> {
    if !marker.is_pending() {
        return Ok(false);
    }
    tracing::warn!("clear_on_quit_resuming_pending_purge");
    store.clear_non_pinned().await.map_err(|err| {
        AppError::storage(format!(
            "could not finish the clear-on-quit purge left pending by the previous session: \
             {err}. The clipboard history you asked to clear on quit is still present; retry, \
             or move the database aside to start fresh."
        ))
    })?;
    marker.clear_pending().map_err(|err| {
        AppError::storage(format!(
            "finished the clear-on-quit purge but could not remove its marker {}: {err}. Remove \
             it so the next start does not purge again.",
            marker.path().display()
        ))
    })?;
    Ok(true)
}

/// Honour `clear_on_quit` while a host shuts down.
///
/// Writes the marker, purges every non-pinned entry, and removes the marker
/// once the purge has landed. Call it after the capture and maintenance
/// workers have drained, so nothing is recorded behind the purge, and while
/// the single-instance lock is still held.
///
/// `timeout` bounds the wait for a host whose exit must not hang (the desktop
/// quit path); that host must exit right after, because the abandoned purge
/// keeps running on a blocking thread until the process goes away. `None`
/// waits for the purge to finish. A purge that times out, fails, or is killed
/// leaves the marker for [`finish_pending_clear_on_quit`] to complete on the
/// next open. Without a `marker` (an in-memory store) or when the marker
/// cannot be written there is nothing to resume from, so the purge is always
/// awaited to completion.
///
/// Returns whether `clear_on_quit` was enabled, so a host can extend the clean
/// slate to data it keeps outside the store.
pub async fn clear_history_on_quit(
    runtime: &NagoriRuntime,
    marker: Option<&ClearOnQuitMarker>,
    timeout: Option<Duration>,
) -> bool {
    if !runtime.current_settings().clear_on_quit {
        return false;
    }
    let resumable = match marker.map(ClearOnQuitMarker::mark_pending) {
        Some(Ok(())) => true,
        Some(Err(err)) => {
            tracing::warn!(error = %err, "clear_on_quit_marker_write_failed");
            false
        }
        None => false,
    };
    if !resumable {
        // A timed-out purge would be fail-open here (nothing finishes it on
        // the next open), so honouring clear-on-quit now is the only remaining
        // guarantee. A DELETE on the local SQLite file does not hang
        // indefinitely; a genuine failure means a broken filesystem.
        if let Err(err) = runtime.clear_non_pinned().await {
            tracing::error!(error = %err, "clear_on_quit_purge_failed_without_marker");
        }
        return true;
    }
    let purged = match timeout {
        Some(timeout) => tokio::time::timeout(timeout, runtime.clear_non_pinned())
            .await
            .unwrap_or_else(|_| {
                Err(AppError::storage(
                    "clear-on-quit purge did not finish in time".to_owned(),
                ))
            }),
        None => runtime.clear_non_pinned().await,
    };
    if purged.is_ok() {
        // Only a completed purge removes the marker. A removal failure makes
        // the next open purge again — harmless at the start of a session.
        if let Some(Err(err)) = marker.map(ClearOnQuitMarker::clear_pending) {
            tracing::warn!(error = %err, "clear_on_quit_marker_remove_failed");
        }
    } else {
        tracing::warn!("clear_on_quit_incomplete_purge_deferred_to_next_open");
    }
    true
}

#[cfg(test)]
mod tests {
    use nagori_core::{AppSettings, EntryFactory, EntryRepository};

    use super::*;

    #[test]
    fn marker_path_is_keyed_to_the_db_file() {
        // Suffixing the full DB path (not just its directory) keeps two DBs in
        // the same directory from inheriting each other's stale marker.
        let marker = ClearOnQuitMarker::for_db(Path::new("/data/nagori.sqlite"));
        assert_eq!(
            marker.path(),
            Path::new("/data/nagori.sqlite.clear-on-quit.pending")
        );
    }

    #[cfg(unix)]
    #[test]
    fn marker_resolves_a_symlinked_db_to_its_real_path() {
        // A store reached through an alias must share the real path's marker,
        // or a host opening the alias would miss a purge left pending for it.
        let tmp = tempfile::tempdir().expect("tempdir");
        let real = tmp.path().join("real.sqlite");
        std::fs::write(&real, b"").expect("db file");
        let alias = tmp.path().join("alias.sqlite");
        std::os::unix::fs::symlink(&real, &alias).expect("symlink");

        let via_real = ClearOnQuitMarker::for_db(&real);
        let via_alias = ClearOnQuitMarker::for_db(&alias);
        assert_eq!(via_real, via_alias);
        via_real.mark_pending().expect("mark");
        assert!(via_alias.is_pending());
    }

    #[test]
    fn marker_write_probe_remove_cycle() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let marker = ClearOnQuitMarker::for_db(&tmp.path().join("nagori.sqlite"));

        assert!(!marker.is_pending());
        marker.mark_pending().expect("mark writes the file");
        assert!(marker.is_pending());
        marker.clear_pending().expect("clear removes the file");
        assert!(!marker.is_pending());
        marker
            .clear_pending()
            .expect("clearing an absent marker is idempotent");
    }

    async fn store_with_history(db_path: &Path) -> SqliteStore {
        let store = SqliteStore::open(db_path).expect("store");
        let pinned = store
            .insert(EntryFactory::from_text("pinned snippet"))
            .await
            .expect("insert");
        store.set_pinned(pinned, true).await.expect("pin");
        store
            .insert(EntryFactory::from_text("history to clear"))
            .await
            .expect("insert");
        store
    }

    #[tokio::test]
    async fn finish_pending_purges_unpinned_history_and_removes_the_marker() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let db_path = tmp.path().join("nagori.sqlite");
        let store = store_with_history(&db_path).await;
        let marker = ClearOnQuitMarker::for_db(&db_path);
        marker.mark_pending().expect("mark");

        assert!(
            finish_pending_clear_on_quit(&store, &marker)
                .await
                .expect("finish")
        );

        let left = store.list_recent(10).await.expect("list");
        assert_eq!(left.len(), 1, "only the pinned entry survives");
        assert_eq!(left[0].plain_text(), Some("pinned snippet"));
        assert!(!marker.is_pending());
    }

    #[tokio::test]
    async fn finish_pending_is_a_no_op_without_a_marker() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let db_path = tmp.path().join("nagori.sqlite");
        let store = store_with_history(&db_path).await;

        assert!(
            !finish_pending_clear_on_quit(&store, &ClearOnQuitMarker::for_db(&db_path))
                .await
                .expect("finish")
        );
        assert_eq!(store.list_recent(10).await.expect("list").len(), 2);
    }

    #[tokio::test]
    async fn quit_purge_respects_the_setting_and_clears_its_marker() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let db_path = tmp.path().join("nagori.sqlite");
        let store = store_with_history(&db_path).await;
        let runtime = NagoriRuntime::builder(store.clone()).build_for_test();
        let marker = ClearOnQuitMarker::for_db(&db_path);

        assert!(
            !clear_history_on_quit(&runtime, Some(&marker), None).await,
            "clear_on_quit is off by default"
        );
        assert_eq!(store.list_recent(10).await.expect("list").len(), 2);

        runtime
            .save_settings(AppSettings {
                clear_on_quit: true,
                ..AppSettings::default()
            })
            .await
            .expect("save settings");
        assert!(clear_history_on_quit(&runtime, Some(&marker), None).await);
        assert_eq!(store.list_recent(10).await.expect("list").len(), 1);
        assert!(!marker.is_pending(), "a completed purge removes its marker");
    }
}

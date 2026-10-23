//! Hand the single-instance lock over across an app restart.
//!
//! Tauri's restart spawns the new process *before* the old one exits, and the
//! old process keeps the data-directory lock until the kernel releases it at
//! exit (in-flight commands or an abandoned clear-on-quit purge may still touch
//! the store until then). So the restarted instance usually finds the lock
//! held. The old process announces the restart with a marker file next to the
//! lock, and a launch that finds a fresh marker waits briefly for the lock
//! instead of refusing to start. Without a marker a held lock is refused
//! straight away, as for any duplicate launch.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use nagori_core::Result;
use nagori_storage::ProcessLock;

const MARKER_FILE_NAME: &str = "nagori.restart-pending";

/// A marker older than this is left over from a restart that never came up
/// (killed, crashed); it must not make an unrelated later launch wait.
const MARKER_MAX_AGE: Duration = Duration::from_mins(1);

/// How long the restarted instance waits for the old one to exit. The old
/// process has already finished its quit cleanup before Tauri spawns the new
/// one, so the gap is normally milliseconds; the ceiling only bounds a slow
/// teardown.
const LOCK_WAIT: Duration = Duration::from_secs(10);

const LOCK_POLL_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Debug, Clone)]
pub struct RestartHandoff {
    path: PathBuf,
}

impl RestartHandoff {
    pub fn for_lock_dir(lock_dir: &Path) -> Self {
        Self {
            path: lock_dir.join(MARKER_FILE_NAME),
        }
    }

    /// Record that this process is about to restart into a new instance.
    pub fn announce(&self) -> std::io::Result<()> {
        std::fs::write(&self.path, b"")
    }

    /// Consume the marker, returning whether it was written within
    /// `MARKER_MAX_AGE` of `now`. The marker is removed either way so it can
    /// only ever delay one launch.
    fn take_fresh(&self, now: SystemTime) -> bool {
        let Ok(modified) = std::fs::metadata(&self.path).and_then(|meta| meta.modified()) else {
            return false;
        };
        if let Err(err) = std::fs::remove_file(&self.path) {
            tracing::warn!(error = %err, "restart_handoff_marker_remove_failed");
        }
        now.duration_since(modified)
            .is_ok_and(|age| age <= MARKER_MAX_AGE)
    }
}

/// Take the data-directory lock, waiting up to `LOCK_WAIT` for it when a
/// fresh restart marker says the previous instance is on its way out.
/// Returns `None` when the lock stays held.
pub fn acquire_lock(lock_dir: &Path) -> Result<Option<ProcessLock>> {
    acquire_lock_with(lock_dir, LOCK_WAIT, SystemTime::now())
}

fn acquire_lock_with(
    lock_dir: &Path,
    wait: Duration,
    now: SystemTime,
) -> Result<Option<ProcessLock>> {
    if let Some(lock) = ProcessLock::try_acquire(lock_dir)? {
        // A marker left behind by a restart that found the lock free straight
        // away must not delay a later launch.
        let _ = RestartHandoff::for_lock_dir(lock_dir).take_fresh(now);
        return Ok(Some(lock));
    }
    if !RestartHandoff::for_lock_dir(lock_dir).take_fresh(now) {
        return Ok(None);
    }
    tracing::info!("restart_handoff_waiting_for_instance_lock");
    let deadline = Instant::now() + wait;
    while Instant::now() < deadline {
        std::thread::sleep(LOCK_POLL_INTERVAL);
        if let Some(lock) = ProcessLock::try_acquire(lock_dir)? {
            return Ok(Some(lock));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hold(dir: &Path) -> ProcessLock {
        ProcessLock::try_acquire(dir)
            .expect("lock io")
            .expect("lock should be free")
    }

    #[test]
    fn held_lock_without_marker_is_refused_immediately() {
        let temp = tempfile::tempdir().expect("temp dir");
        let _held = hold(temp.path());
        let started = Instant::now();
        let lock = acquire_lock_with(temp.path(), Duration::from_secs(5), SystemTime::now())
            .expect("lock io");
        assert!(lock.is_none());
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn restarted_instance_waits_for_the_old_owner_to_exit() {
        let temp = tempfile::tempdir().expect("temp dir");
        let held = hold(temp.path());
        RestartHandoff::for_lock_dir(temp.path())
            .announce()
            .expect("write marker");
        let releaser = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            drop(held);
        });

        let lock = acquire_lock_with(temp.path(), Duration::from_secs(5), SystemTime::now())
            .expect("lock io");

        releaser.join().expect("releaser thread");
        assert!(lock.is_some(), "the lock should be handed over");
        assert!(!temp.path().join(MARKER_FILE_NAME).exists());
    }

    #[test]
    fn wait_gives_up_when_the_old_owner_never_exits() {
        let temp = tempfile::tempdir().expect("temp dir");
        let _held = hold(temp.path());
        RestartHandoff::for_lock_dir(temp.path())
            .announce()
            .expect("write marker");

        let lock = acquire_lock_with(temp.path(), Duration::from_millis(250), SystemTime::now())
            .expect("lock io");

        assert!(lock.is_none());
        assert!(!temp.path().join(MARKER_FILE_NAME).exists());
    }

    #[test]
    fn stale_marker_does_not_delay_a_duplicate_launch() {
        let temp = tempfile::tempdir().expect("temp dir");
        let _held = hold(temp.path());
        RestartHandoff::for_lock_dir(temp.path())
            .announce()
            .expect("write marker");
        let later = SystemTime::now() + MARKER_MAX_AGE + Duration::from_secs(1);
        let started = Instant::now();

        let lock = acquire_lock_with(temp.path(), Duration::from_secs(5), later).expect("lock io");

        assert!(lock.is_none());
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(!temp.path().join(MARKER_FILE_NAME).exists());
    }

    #[test]
    fn free_lock_clears_a_leftover_marker() {
        let temp = tempfile::tempdir().expect("temp dir");
        RestartHandoff::for_lock_dir(temp.path())
            .announce()
            .expect("write marker");

        let lock = acquire_lock_with(temp.path(), Duration::from_secs(5), SystemTime::now())
            .expect("lock io");

        assert!(lock.is_some());
        assert!(!temp.path().join(MARKER_FILE_NAME).exists());
    }
}

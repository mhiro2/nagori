//! Cross-process exclusive lock guarding a nagori data directory.
//!
//! Both the desktop shell and the standalone daemon own an in-process capture
//! loop writing into the same `SQLite` store. Two of them running against the
//! same directory would double-capture the clipboard, race schema migrations,
//! and let one process' clear-on-quit purge data the other still considers
//! live. [`ProcessLock`] makes that mutually exclusive: a process acquires the
//! lock before it starts owning the store and holds it for its whole lifetime.
//!
//! The lock is an advisory lock on a `nagori.lock` file taken via
//! [`std::fs::File::try_lock`] (`flock(LOCK_EX)` on Unix, `LockFileEx` on
//! Windows). The kernel releases it when the underlying file handle closes,
//! which happens on process exit — including a crash or `SIGKILL` — so there
//! is no stale-lockfile problem to clean up. A leftover `nagori.lock` inode on
//! disk is therefore meaningless on its own; only a *live* handle still
//! holding the OS lock blocks a second acquirer. That is exactly the
//! "lifetime lock" property the IPC socket path lacks: a transient
//! `connect()` failure can mislead a socket-liveness probe, but a held file
//! lock cannot be faked by a dead process.

use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};

use nagori_core::{AppError, Result};

/// Filename of the lock created inside the guarded directory. Shared by the
/// desktop and daemon so that, when they point at the same data directory
/// (the default), they also exclude each other and never double-own the
/// store.
const LOCK_FILE_NAME: &str = "nagori.lock";

/// An acquired process-lifetime lock over a data directory.
///
/// Hold this value for as long as the process should own the directory;
/// dropping it (or the process exiting) releases the lock. The struct is
/// intentionally inert — its only job is to keep the locked file handle
/// alive.
#[derive(Debug)]
pub struct ProcessLock {
    // The OS lock lives exactly as long as this handle is open. It is never
    // read after construction; `#[allow(dead_code)]` documents that the field
    // exists for its drop side effect (closing the descriptor releases the
    // lock), not for its value.
    #[allow(dead_code)]
    file: File,
    path: PathBuf,
}

impl ProcessLock {
    /// Try to take the exclusive lock at `dir`/`nagori.lock`.
    ///
    /// - `Ok(Some(lock))` — acquired; the caller is the sole owner of `dir`.
    /// - `Ok(None)` — another live process already holds it; the caller should
    ///   refuse to start a second store owner.
    /// - `Err(_)` — the lock file could not be opened (missing directory, bad
    ///   permissions) or the OS lock call failed for a reason other than
    ///   contention.
    ///
    /// `dir` must already exist; callers prepare it with
    /// [`ensure_private_directory`](crate::ensure_private_directory) first.
    pub fn try_acquire(dir: &Path) -> Result<Option<Self>> {
        Self::try_acquire_at(&dir.join(LOCK_FILE_NAME))
    }

    /// Try to take the exclusive lock on the file at `path` directly.
    ///
    /// Like [`try_acquire`](Self::try_acquire) but keyed on an explicit lock
    /// file rather than the conventional `nagori.lock` inside a directory.
    /// Used for locks that guard something other than the data directory —
    /// e.g. ownership of a specific IPC endpoint, whose lock file is keyed on
    /// the endpoint path so two endpoints in the same directory don't share a
    /// lock. The semantics of the three outcomes match `try_acquire`.
    ///
    /// `path`'s parent directory must already exist.
    pub fn try_acquire_at(path: &Path) -> Result<Option<Self>> {
        let file = open_lock_file(path)?;
        match file.try_lock() {
            Ok(()) => Ok(Some(Self {
                file,
                path: path.to_path_buf(),
            })),
            Err(TryLockError::WouldBlock) => Ok(None),
            Err(TryLockError::Error(err)) => Err(AppError::Platform(format!(
                "failed to acquire process lock at {}: {err}",
                path.display()
            ))),
        }
    }

    /// Path of the lock file, for diagnostics / log lines.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(unix)]
fn open_lock_file(path: &Path) -> Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    // `0o600` mirrors the DB / socket file mode: the lock lives inside the
    // already-`0o700` data directory, but pin its own mode so a stray umask
    // can't widen it. `truncate(false)` leaves any existing file untouched —
    // the content is irrelevant (we only ever lock the inode, never read it),
    // and there is no value in rewriting it.
    OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .mode(0o600)
        .open(path)
        .map_err(|err| AppError::Platform(format!("failed to open {}: {err}", path.display())))
}

#[cfg(not(unix))]
fn open_lock_file(path: &Path) -> Result<File> {
    OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(path)
        .map_err(|err| AppError::Platform(format!("failed to open {}: {err}", path.display())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_acquire_succeeds_and_second_observes_contention() {
        let dir = tempfile::tempdir().expect("tempdir");
        let first = ProcessLock::try_acquire(dir.path())
            .expect("first acquire should not error")
            .expect("first acquire should obtain the lock");
        assert!(first.path().ends_with(LOCK_FILE_NAME));

        // A second acquirer (separate file handle / open file description)
        // must observe the held lock as contention, not as success — this is
        // the property that keeps two store owners from coexisting.
        let second = ProcessLock::try_acquire(dir.path()).expect("second acquire should not error");
        assert!(
            second.is_none(),
            "a held lock must make a second acquire return None"
        );
    }

    #[test]
    fn releasing_the_lock_frees_the_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        let first = ProcessLock::try_acquire(dir.path())
            .expect("acquire should not error")
            .expect("acquire should obtain the lock");
        drop(first);

        // After the first owner exits its lock is released, so a fresh owner
        // can claim the directory.
        let again = ProcessLock::try_acquire(dir.path())
            .expect("re-acquire should not error")
            .expect("re-acquire should obtain the lock after release");
        drop(again);
    }

    #[test]
    fn distinct_directories_do_not_contend() {
        let dir_a = tempfile::tempdir().expect("tempdir a");
        let dir_b = tempfile::tempdir().expect("tempdir b");
        let _a = ProcessLock::try_acquire(dir_a.path())
            .expect("acquire a should not error")
            .expect("acquire a should obtain the lock");
        let b = ProcessLock::try_acquire(dir_b.path()).expect("acquire b should not error");
        assert!(
            b.is_some(),
            "locks on different directories must be independent"
        );
    }

    #[test]
    fn try_acquire_at_keys_on_the_file_not_the_directory() {
        // Two distinct lock files in the *same* directory must not contend —
        // this is what lets an endpoint lock (keyed on the endpoint path)
        // coexist with the data-directory lock in the same leaf without one
        // blocking the other.
        let dir = tempfile::tempdir().expect("tempdir");
        let lock_a = dir.path().join("a.lock");
        let lock_b = dir.path().join("b.lock");
        let _a = ProcessLock::try_acquire_at(&lock_a)
            .expect("acquire a should not error")
            .expect("acquire a should obtain the lock");
        let b = ProcessLock::try_acquire_at(&lock_b).expect("acquire b should not error");
        assert!(
            b.is_some(),
            "locks on different files must be independent even in one directory"
        );

        // A second acquirer of the *same* file still observes contention.
        let a_again =
            ProcessLock::try_acquire_at(&lock_a).expect("second acquire of a should not error");
        assert!(
            a_again.is_none(),
            "a held lock file must make a second acquire return None"
        );
    }

    #[test]
    fn missing_directory_is_an_error_not_contention() {
        let dir = tempfile::tempdir().expect("tempdir");
        let missing = dir.path().join("does-not-exist");
        let result = ProcessLock::try_acquire(&missing);
        assert!(
            result.is_err(),
            "opening the lock under a missing directory should error, not return None"
        );
    }
}

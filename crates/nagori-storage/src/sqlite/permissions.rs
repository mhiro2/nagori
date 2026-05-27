use std::path::Path;

use nagori_core::{AppError, Result};

/// Names of the `SQLite` sidecar files derived from the main DB path.
/// `SQLite` creates these under the process umask once `journal_mode = WAL`
/// runs.
#[cfg(unix)]
const DB_SIDECAR_SUFFIXES: [&str; 2] = ["-wal", "-shm"];

#[cfg(unix)]
fn db_sidecar_path(path: &Path, suffix: &str) -> std::path::PathBuf {
    let mut sibling = path.as_os_str().to_owned();
    sibling.push(suffix);
    std::path::PathBuf::from(sibling)
}

/// Stat `path` without following the final component. Returns `Ok(true)` when
/// it exists as a non-symlink, `Ok(false)` when absent, and an error when it is
/// a symlink (so `set_permissions` / `SQLite` never chmod-follow or open the
/// target of a planted link) or the stat itself fails.
#[cfg(unix)]
fn reject_if_symlink(path: &Path, role: &str) -> Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) => {
            if meta.file_type().is_symlink() {
                Err(AppError::Storage(format!(
                    "{} is a symlink; refusing to use it as {role}",
                    path.display()
                )))
            } else {
                Ok(true)
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(storage_err_io(&err)),
    }
}

#[cfg(unix)]
pub(crate) fn harden_db_file_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = || std::fs::Permissions::from_mode(0o600);
    std::fs::set_permissions(path, mode()).map_err(|err| storage_err_io(&err))?;
    // WAL/SHM sidecars are created by SQLite under the process umask once
    // `PRAGMA journal_mode = WAL` runs. Tighten any that already exist, but
    // refuse to follow a symlink raced into a sidecar path.
    for suffix in DB_SIDECAR_SUFFIXES {
        let sibling = db_sidecar_path(path, suffix);
        if reject_if_symlink(&sibling, "a database sidecar")? {
            std::fs::set_permissions(&sibling, mode()).map_err(|err| storage_err_io(&err))?;
        }
    }
    Ok(())
}

#[cfg(not(unix))]
pub(crate) fn harden_db_file_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

/// Atomically create the `SQLite` main file with mode `0o600` if it does not
/// already exist, eliminating the TOCTOU window between `Connection::open`
/// and a subsequent `chmod`. If the file is already there (subsequent
/// daemon launch), enforce the mask defensively in case an earlier build
/// left it world-readable.
#[cfg(unix)]
pub(crate) fn pre_create_db_file_private(path: &Path) -> Result<()> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    // The DB directory must be writable by us alone — otherwise a co-tenant can
    // race a `-wal`/`-shm` symlink into the window before SQLite opens the
    // sidecars. Reject a shared `--db` location up front.
    ensure_db_parent_strictly_private(path)?;
    // Refuse to chmod / open a symlink planted at the DB path itself.
    if reject_if_symlink(path, "the database file")? {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(|err| storage_err_io(&err))?;
    } else {
        // `create_new` (O_EXCL) refuses to follow a symlink raced into the
        // path between the stat above and this open, so the create path needs
        // no extra guard.
        std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(path)
            .map_err(|err| storage_err_io(&err))?;
    }
    // Reject a symlink planted at the WAL/SHM sidecar paths *before*
    // `Connection::open` runs `journal_mode = WAL` — SQLite would otherwise
    // follow the link when it creates/opens the sidecar. We don't create them
    // ourselves (SQLite owns that); we only refuse a hostile pre-existing link.
    for suffix in DB_SIDECAR_SUFFIXES {
        reject_if_symlink(&db_sidecar_path(path, suffix), "a database sidecar")?;
    }
    Ok(())
}

#[cfg(not(unix))]
pub(crate) fn pre_create_db_file_private(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn storage_err_io(err: &std::io::Error) -> AppError {
    AppError::Storage(err.to_string())
}

/// Create missing directory components with `0o700` perms on Unix.
///
/// Existing directories are only validated and are never chmodded. This keeps
/// custom paths under shared parents such as `/tmp` from mutating permissions
/// outside Nagori's ownership.
pub fn ensure_private_directory(dir: &Path) -> Result<()> {
    ensure_private_directory_inner(dir).map_err(|err| AppError::Storage(err.to_string()))
}

#[cfg(unix)]
fn ensure_private_directory_inner(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;

    if dir.as_os_str().is_empty() {
        return Ok(());
    }
    if let Some(existing) = existing_path_metadata(dir)? {
        return validate_existing_directory(dir, &existing);
    }
    if let Some(parent) = dir.parent() {
        ensure_private_directory_inner(parent)?;
    }
    let mut builder = std::fs::DirBuilder::new();
    builder.mode(0o700);
    match builder.create(dir) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
            let metadata = std::fs::symlink_metadata(dir)?;
            validate_existing_directory(dir, &metadata)
        }
        Err(err) => Err(err),
    }
}

#[cfg(not(unix))]
fn ensure_private_directory_inner(dir: &Path) -> std::io::Result<()> {
    if dir.as_os_str().is_empty() {
        return Ok(());
    }
    std::fs::create_dir_all(dir)
}

#[cfg(unix)]
fn existing_path_metadata(dir: &Path) -> std::io::Result<Option<std::fs::Metadata>> {
    match std::fs::symlink_metadata(dir) {
        Ok(metadata) => Ok(Some(metadata)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err),
    }
}

/// Effective uid of the calling process.
///
/// Isolated so the `unsafe` block (the FFI call, which takes no arguments and
/// cannot fail) carries a narrow `allow(unsafe_code)` rather than relaxing the
/// workspace-wide deny over the whole validator.
#[cfg(unix)]
#[allow(unsafe_code)]
fn current_euid() -> u32 {
    // SAFETY: `geteuid` takes no arguments, never fails, and is always safe to
    // call.
    unsafe { libc::geteuid() }
}

#[cfg(unix)]
fn validate_existing_directory(dir: &Path, metadata: &std::fs::Metadata) -> std::io::Result<()> {
    // The shared helper (IPC socket / token parents) tolerates a sticky
    // world-writable root so dev endpoints under `/tmp` keep working.
    validate_existing_directory_with(dir, metadata, true)
}

/// Validate an existing directory we are about to host sensitive files in.
///
/// `allow_sticky_shared` controls whether a world-writable directory protected
/// only by the sticky bit (e.g. `/tmp`) is acceptable. The IPC helper allows it
/// because the socket/token files defend themselves (Unix-socket mode, atomic
/// symlink-replacing token writes). The database does **not**: `SQLite`
/// creates `-wal`/`-shm` sidecars whose names a co-tenant could win a race to plant as
/// symlinks, so the DB requires a directory no other user can write at all.
#[cfg(unix)]
fn validate_existing_directory_with(
    dir: &Path,
    metadata: &std::fs::Metadata,
    allow_sticky_shared: bool,
) -> std::io::Result<()> {
    use std::os::unix::fs::MetadataExt;

    if metadata.file_type().is_symlink() {
        return Err(std::io::Error::other(format!(
            "{} is a symlink",
            dir.display()
        )));
    }
    if !metadata.is_dir() {
        return Err(std::io::Error::other(format!(
            "{} is not a directory",
            dir.display()
        )));
    }
    // A directory holding the IPC socket / token / database must be owned by
    // us. A directory owned by another (non-root) user could have been planted
    // to intercept the socket or read the token. Root-owned shared roots (e.g.
    // `/tmp`, `/var`) are allowed so custom endpoints under them still work.
    let euid = current_euid();
    let owner = metadata.uid();
    if owner != euid && owner != 0 {
        return Err(std::io::Error::other(format!(
            "{} is owned by uid {owner}, expected {euid}",
            dir.display()
        )));
    }
    // Reject group/other-writable directories: anyone who can write the
    // directory can plant a malicious socket or symlink at our endpoint. The
    // sticky bit (as on `/tmp`) restricts deletion/rename to the owner, so a
    // world-writable + sticky shared root is acceptable *only* for callers that
    // opt in via `allow_sticky_shared`.
    let mode = metadata.mode();
    let group_other_writable = mode & 0o022 != 0;
    let sticky = mode & 0o1000 != 0;
    if group_other_writable && !(allow_sticky_shared && sticky) {
        return Err(std::io::Error::other(format!(
            "{} is group/other-writable (mode {:#o})",
            dir.display(),
            mode & 0o7777,
        )));
    }
    Ok(())
}

/// Reject a database path whose parent directory any other user can write.
///
/// Runs before `Connection::open`, so it closes the residual race where a
/// co-tenant in a sticky shared dir (`/tmp`) plants a `-wal`/`-shm` symlink in
/// the window between our existence check and `SQLite` opening the sidecar: if no
/// other user can write the directory, no such file can be planted at all. The
/// default DB lives in a private `0o700` data dir, so this only ever rejects a
/// deliberately shared `--db` location.
#[cfg(unix)]
fn ensure_db_parent_strictly_private(path: &Path) -> Result<()> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    if parent.as_os_str().is_empty() {
        return Ok(());
    }
    let metadata = std::fs::symlink_metadata(parent).map_err(|err| storage_err_io(&err))?;
    validate_existing_directory_with(parent, &metadata, false)
        .map_err(|err| AppError::Storage(err.to_string()))
}

#[cfg(not(unix))]
fn ensure_db_parent_strictly_private(_path: &Path) -> Result<()> {
    Ok(())
}

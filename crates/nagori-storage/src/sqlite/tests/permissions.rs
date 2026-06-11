use super::super::*;

#[cfg(unix)]
#[test]
fn ensure_private_directory_does_not_chmod_existing_directory() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().unwrap();
    let shared = temp.path().join("shared");
    std::fs::create_dir(&shared).unwrap();
    // 0o750: group-readable but not group/other-writable, so it passes
    // the privacy validation and must be left untouched (not chmodded).
    std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o750)).unwrap();

    ensure_private_directory(&shared).unwrap();

    let mode = std::fs::metadata(&shared).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o750);
}

#[cfg(unix)]
#[test]
fn ensure_private_directory_rejects_world_writable_without_sticky() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().unwrap();
    let shared = temp.path().join("shared");
    std::fs::create_dir(&shared).unwrap();
    // World-writable without the sticky bit lets a co-tenant plant a
    // socket/symlink at our endpoint — must be rejected.
    std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o777)).unwrap();

    let err = ensure_private_directory(&shared).unwrap_err();

    assert!(
        err.to_string().contains("group/other-writable"),
        "unexpected error: {err}"
    );
}

#[cfg(unix)]
#[test]
fn ensure_private_directory_allows_world_writable_with_sticky() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().unwrap();
    let shared = temp.path().join("shared");
    std::fs::create_dir(&shared).unwrap();
    // Sticky + world-writable mirrors `/tmp`: deletion/rename is restricted
    // to the owner, so a custom endpoint under it stays usable.
    std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o1777)).unwrap();

    ensure_private_directory(&shared).unwrap();
}

#[cfg(unix)]
#[test]
fn ensure_private_directory_creates_missing_leaf_private() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().unwrap();
    let leaf = temp.path().join("nagori").join("ipc");

    ensure_private_directory(&leaf).unwrap();

    let mode = std::fs::metadata(&leaf).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o700);
}

#[cfg(unix)]
#[test]
fn pre_create_db_file_private_rejects_symlinked_path() {
    let temp = tempfile::tempdir().unwrap();
    let bystander = temp.path().join("victim");
    std::fs::write(&bystander, b"do-not-touch").unwrap();
    let db_path = temp.path().join("nagori.db");
    std::os::unix::fs::symlink(&bystander, &db_path).unwrap();

    let err = pre_create_db_file_private(&db_path).unwrap_err();

    assert!(
        err.to_string().contains("is a symlink"),
        "unexpected error: {err}"
    );
    // The symlink target must be untouched (not chmodded or truncated).
    assert_eq!(std::fs::read(&bystander).unwrap(), b"do-not-touch");
}

#[cfg(unix)]
#[test]
fn pre_create_db_file_private_rejects_shared_parent_dir() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().unwrap();
    let shared = temp.path().join("shared");
    std::fs::create_dir(&shared).unwrap();
    // Sticky + world-writable (like `/tmp`): tolerated for IPC, but the DB
    // must refuse it because a co-tenant could race a sidecar symlink.
    std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o1777)).unwrap();
    let db_path = shared.join("nagori.db");

    let err = pre_create_db_file_private(&db_path).unwrap_err();

    assert!(
        err.to_string().contains("group/other-writable"),
        "unexpected error: {err}"
    );
    // Nothing was created in the rejected directory.
    assert!(!db_path.exists());
}

#[cfg(unix)]
#[test]
fn pre_create_db_file_private_rejects_symlinked_wal_sidecar() {
    // A co-tenant can't necessarily plant the main DB path, but the WAL
    // sidecar SQLite creates later is just as dangerous: rejecting it must
    // happen before `journal_mode = WAL` opens it.
    let temp = tempfile::tempdir().unwrap();
    let bystander = temp.path().join("victim");
    std::fs::write(&bystander, b"do-not-touch").unwrap();
    let db_path = temp.path().join("nagori.db");
    let wal_path = temp.path().join("nagori.db-wal");
    std::os::unix::fs::symlink(&bystander, &wal_path).unwrap();

    let err = pre_create_db_file_private(&db_path).unwrap_err();

    assert!(
        err.to_string().contains("is a symlink"),
        "unexpected error: {err}"
    );
    assert_eq!(std::fs::read(&bystander).unwrap(), b"do-not-touch");
}

#[cfg(unix)]
#[test]
fn ensure_private_directory_rejects_symlinked_directory() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("target");
    let link = temp.path().join("link");
    std::fs::create_dir(&target).unwrap();
    std::os::unix::fs::symlink(&target, &link).unwrap();

    let err = ensure_private_directory(&link).unwrap_err();

    assert!(err.to_string().contains("is a symlink"));
}

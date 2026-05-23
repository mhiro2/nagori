use std::path::{Path, PathBuf};

use nagori_core::ContentHash;
use nagori_core::{AppError, Result};

/// Per-launch authentication token. The daemon writes the hex-encoded form
/// into a 0o600 file inside the same data directory as the socket; the CLI
/// reads it back and presents it on every IPC request.
///
/// 32 random bytes -> 64 hex chars; long enough that brute-forcing across
/// the daemon's lifetime is not realistic, short enough to fit comfortably
/// in a single envelope frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthToken(String);

impl AuthToken {
    pub fn generate() -> Result<Self> {
        let mut bytes = [0_u8; 32];
        getrandom::fill(&mut bytes)
            .map_err(|err| AppError::Platform(format!("token rng failure: {err}")))?;
        Ok(Self(hex::encode(bytes)))
    }

    pub fn from_hex(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        // Reject anything that isn't pure ASCII hex of the expected length so
        // a malformed file fails loud rather than producing a token that
        // "looks like" a valid one but never matches.
        if value.len() != 64 || !value.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(AppError::InvalidInput(
                "auth token must be 64 hex characters".to_owned(),
            ));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Constant-time comparison so we don't leak the token a byte at a time
    /// to an attacker who can measure response timing.
    pub fn verify(&self, candidate: &str) -> bool {
        let actual = self.0.as_bytes();
        let candidate = candidate.as_bytes();
        if actual.len() != candidate.len() {
            return false;
        }
        let mut diff: u8 = 0;
        for (a, b) in actual.iter().zip(candidate.iter()) {
            diff |= a ^ b;
        }
        diff == 0
    }
}

/// Per-user app data directory used to host token files. The daemon is
/// expected to ensure this directory exists and is private (0o700 on Unix)
/// before any token file is written into it; see
/// `nagori_storage::ensure_private_directory`.
fn app_data_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("nagori")
}

/// Default location for the daemon's token file: same directory as the
/// default socket, named `nagori.token`. Co-locating is safe here because
/// `app_data_dir()` is the daemon's private leaf.
pub fn default_token_path() -> PathBuf {
    app_data_dir().join("nagori.token")
}

/// Sanitise an endpoint segment for use as a token-filename stem. Replaces
/// anything outside `[A-Za-z0-9._-]` with `_` so the result is filesystem-
/// safe on every supported OS.
fn sanitise_segment(segment: &str) -> String {
    segment
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Derive a token-file path for a non-default IPC endpoint.
///
/// Without this, a daemon launched with `--ipc <custom>` and the CLI that
/// reads from the same custom endpoint would both fall back to
/// `default_token_path`, trampling the token file written by any other
/// daemon already serving the default endpoint.
///
/// Token files are always placed under the per-user `app_data_dir()` (which
/// the daemon hardens to `0o700` on Unix), never next to the socket. That
/// keeps the leaf directory's symlink / ownership guarantees independent
/// of where the user chose to place the IPC endpoint — important when
/// `--ipc /tmp/dev.sock` puts the socket in a world-writable parent.
///
/// * The default endpoint keeps producing exactly `<app_data_dir>/nagori.token`
///   so existing installations don't see their token filename drift on
///   upgrade.
/// * Every other endpoint is namespaced as
///   `<app_data_dir>/<sanitised>-<hash>.token`. The 8-hex-char hash of the
///   full endpoint string disambiguates collisions where two endpoints
///   sanitise to the same visible stem (e.g. `/tmp/a:b.sock` and
///   `/tmp/a?b.sock` both reduce to `a_b`).
pub fn token_path_for_endpoint(endpoint: &Path) -> PathBuf {
    let app_dir = app_data_dir();
    #[cfg(unix)]
    {
        // Resolve the default socket path on Unix without depending on
        // nagori-daemon: it is `<app_data_dir>/nagori.sock`. Matching by
        // value (not pointer equality) keeps `--ipc <default>` and the
        // implicit default producing the same token filename.
        if endpoint == app_dir.join("nagori.sock") {
            return app_dir.join("nagori.token");
        }
    }
    #[cfg(windows)]
    {
        if endpoint.to_string_lossy() == crate::server::DEFAULT_PIPE_NAME {
            return app_dir.join("nagori.token");
        }
    }

    let raw = endpoint.to_string_lossy();
    // Pick a human-readable stem from the last filesystem / pipe segment so
    // operators can still tell which token belongs to which daemon at a
    // glance. The hash suffix below makes the filename unique regardless.
    let segment = raw
        .rsplit(['\\', '/'])
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or("nagori");
    let sanitised = sanitise_segment(segment);
    let hash = ContentHash::sha256(raw.as_bytes()).value;
    let suffix = &hash[..8];
    app_dir.join(format!("{sanitised}-{suffix}.token"))
}

/// Write the token to `path` with `0o600` permissions on Unix.
///
/// Every daemon launch produces a fresh token, so a stale file from a
/// previous run must be replaced. Replacement is done by writing a sibling
/// temp file with `O_CREAT|O_EXCL` + `mode(0o600)` and then `rename(2)` over
/// `path`. The rename guarantees:
///
/// * The pre-existing entry at `path` — including a symlink planted by a
///   co-tenant in a shared parent (e.g. `/tmp/dev.token` -> `/etc/passwd`)
///   — is **replaced**, not followed. Without this the previous
///   `OpenOptions::truncate(true)` flow would write our token bytes into
///   the symlink target.
/// * There is never a moment where the file exists with umask-derived
///   permissions: `O_EXCL` only creates the temp file, and the mode is
///   set atomically in `open(2)`.
#[cfg(unix)]
pub fn write_token_file(path: &Path, token: &AuthToken) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let parent = path.parent().ok_or_else(|| {
        AppError::Platform(format!("token path has no parent: {}", path.display()))
    })?;
    std::fs::create_dir_all(parent).map_err(|err| AppError::Platform(err.to_string()))?;
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| AppError::Platform(format!("token path has no name: {}", path.display())))?;
    // Random tail so two daemons creating their temp files at the same
    // moment can't collide. `O_EXCL` would catch the collision anyway,
    // but the random suffix avoids the failure entirely.
    let mut random = [0_u8; 8];
    getrandom::fill(&mut random)
        .map_err(|err| AppError::Platform(format!("token tmp rng failure: {err}")))?;
    let tmp_path = parent.join(format!(
        ".{file_name}.{}.{}.tmp",
        std::process::id(),
        hex::encode(random),
    ));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp_path)
        .map_err(|err| AppError::Platform(err.to_string()))?;
    let write_result = file
        .write_all(token.as_str().as_bytes())
        .and_then(|()| file.sync_all());
    if let Err(err) = write_result {
        drop(file);
        let _ = std::fs::remove_file(&tmp_path);
        return Err(AppError::Platform(err.to_string()));
    }
    drop(file);
    if let Err(err) = std::fs::rename(&tmp_path, path) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(AppError::Platform(err.to_string()));
    }
    Ok(())
}

#[cfg(not(unix))]
pub fn write_token_file(path: &Path, token: &AuthToken) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|err| AppError::Platform(err.to_string()))?;
    }
    std::fs::write(path, token.as_str()).map_err(|err| AppError::Platform(err.to_string()))
}

pub fn read_token_file(path: &Path) -> Result<AuthToken> {
    let contents =
        std::fs::read_to_string(path).map_err(|err| AppError::Platform(err.to_string()))?;
    AuthToken::from_hex(contents.trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_round_trips_through_hex() {
        let token = AuthToken::generate().unwrap();
        let parsed = AuthToken::from_hex(token.as_str()).unwrap();
        assert!(token.verify(parsed.as_str()));
    }

    #[test]
    fn token_rejects_wrong_length_or_non_hex() {
        assert!(AuthToken::from_hex("abc").is_err());
        assert!(AuthToken::from_hex("g".repeat(64)).is_err());
        assert!(AuthToken::from_hex("a".repeat(63)).is_err());
    }

    #[test]
    fn verify_returns_false_for_different_token() {
        let a = AuthToken::generate().unwrap();
        let b = AuthToken::generate().unwrap();
        assert!(!a.verify(b.as_str()));
    }

    #[cfg(unix)]
    #[test]
    fn write_token_file_creates_file_with_0600_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("nagori.token");
        let token = AuthToken::generate().unwrap();
        write_token_file(&path, &token).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        let read = read_token_file(&path).unwrap();
        assert!(token.verify(read.as_str()));
    }

    #[cfg(unix)]
    #[test]
    fn token_path_for_endpoint_namespaces_unix_endpoints_under_app_dir() {
        // Default endpoint (`<app_data_dir>/nagori.sock`) keeps producing
        // exactly `<app_data_dir>/nagori.token` so existing installs
        // don't drift on upgrade.
        let default = app_data_dir().join("nagori.sock");
        assert_eq!(token_path_for_endpoint(&default), default_token_path());

        // Custom endpoints in a shared parent (e.g. `/tmp/...`) MUST NOT
        // produce a token path in that shared parent. Otherwise a
        // co-tenant could plant a symlink at the predictable token name
        // and trick the daemon into following it. We keep the file in
        // the private app dir and disambiguate with a hash suffix.
        let custom = PathBuf::from("/tmp/other/dev.sock");
        let custom_token = token_path_for_endpoint(&custom);
        assert_eq!(custom_token.parent(), Some(app_data_dir().as_path()));
        let custom_name = custom_token
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let extension_is_token = custom_token
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("token"));
        assert!(
            custom_name.starts_with("dev.sock-") && extension_is_token,
            "unexpected filename shape: {custom_name}",
        );

        // Endpoints that sanitise to the same visible segment must still
        // produce distinct token files thanks to the hash suffix.
        let colon = PathBuf::from("/tmp/a:b.sock");
        let question = PathBuf::from("/tmp/a?b.sock");
        assert_ne!(
            token_path_for_endpoint(&colon),
            token_path_for_endpoint(&question),
            "endpoints differing only in sanitised characters must not collide",
        );
    }

    #[cfg(unix)]
    #[test]
    fn write_token_file_replaces_symlink_without_following_it() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        // Target the symlink would point at if naively followed. The
        // sentinel content must survive the daemon's write.
        let bystander = dir.path().join("sensitive");
        std::fs::write(&bystander, b"must-not-overwrite").unwrap();
        // Plant a hostile symlink at the token path.
        let token_path = dir.path().join("nagori.token");
        std::os::unix::fs::symlink(&bystander, &token_path).unwrap();

        let token = AuthToken::generate().unwrap();
        write_token_file(&token_path, &token).unwrap();

        // The symlink has been replaced by a regular file containing the
        // token, NOT the symlink's target.
        let metadata = std::fs::symlink_metadata(&token_path).unwrap();
        assert!(metadata.file_type().is_file());
        let mode = metadata.permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let read = read_token_file(&token_path).unwrap();
        assert!(token.verify(read.as_str()));
        // The bystander file was not touched.
        let bystander_bytes = std::fs::read(&bystander).unwrap();
        assert_eq!(bystander_bytes, b"must-not-overwrite");
    }

    #[cfg(windows)]
    #[test]
    fn token_path_for_endpoint_namespaces_pipe_names() {
        // The default pipe must keep producing the historic
        // `nagori.token` filename so existing installs don't lose track
        // of their token on upgrade.
        let default = PathBuf::from(crate::server::DEFAULT_PIPE_NAME);
        assert_eq!(
            token_path_for_endpoint(&default).file_name().unwrap(),
            std::ffi::OsStr::new("nagori.token"),
        );

        // Non-default endpoints get a hash suffix so the visible segment
        // can't be the only disambiguator. Don't assert the literal hash
        // (so SHA-256 isn't pinned to the test) — assert the structure:
        // `<sanitised>-<8 hex>.token`, with the visible stem preserved.
        let custom = PathBuf::from(r"\\.\pipe\nagori-dev");
        let custom_path = token_path_for_endpoint(&custom);
        let custom_name = custom_path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let extension_is_token = custom_path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("token"));
        assert!(
            custom_name.starts_with("nagori-dev-") && extension_is_token,
            "unexpected filename shape: {custom_name}"
        );
        // Two endpoints that sanitise to the same segment must produce
        // *different* token filenames; this is the bug-class we're
        // closing (`a:b` and `a?b` both sanitise to `a_b`).
        let colon = PathBuf::from(r"\\.\pipe\a:b");
        let question = PathBuf::from(r"\\.\pipe\a?b");
        let colon_name = token_path_for_endpoint(&colon);
        let question_name = token_path_for_endpoint(&question);
        assert_ne!(
            colon_name, question_name,
            "endpoints differing only in sanitised characters must not collide",
        );
    }
}

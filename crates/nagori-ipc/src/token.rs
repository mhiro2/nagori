use std::path::{Path, PathBuf};

#[cfg(windows)]
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

/// Default location for the daemon's token file: same directory as the
/// socket, named `nagori.token`. Co-locating means the umask/parent-dir
/// hardening done for the socket also covers the token.
pub fn default_token_path() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("nagori")
        .join("nagori.token")
}

/// Derive a token-file path for a non-default IPC endpoint.
///
/// Without this, a daemon launched with `--ipc <custom>` and the CLI that
/// reads from the same custom endpoint would both fall back to
/// `default_token_path`, trampling the token file written by any other
/// daemon already serving the default endpoint. Locating the token file
/// next to (or in a namespace mirrored after) the endpoint keeps the two
/// processes paired without requiring a separate flag.
///
/// * On Unix the endpoint is a socket file, so the token lives in the same
///   directory under `<stem>.token`.
/// * On Windows the endpoint is a pipe name (no filesystem parent), so the
///   token lives under `%LOCALAPPDATA%\nagori\<sanitised-pipe-name>.token`
///   to keep token files contained in the same private directory the
///   default token already uses.
pub fn token_path_for_endpoint(endpoint: &Path) -> PathBuf {
    #[cfg(unix)]
    {
        if let Some(parent) = endpoint.parent()
            && let Some(stem) = endpoint.file_stem().and_then(|s| s.to_str())
            && !stem.is_empty()
        {
            return parent.join(format!("{stem}.token"));
        }
        default_token_path()
    }
    #[cfg(windows)]
    {
        let raw = endpoint.to_string_lossy();
        // The default pipe (`\\.\pipe\nagori`) keeps producing exactly
        // `nagori.token` so existing installations don't see their token
        // filename drift on upgrade. For every other endpoint we append
        // a short hash of the *full* endpoint string so two pipe names
        // whose sanitised tail collides — e.g. `\\.\pipe\a:b` and
        // `\\.\pipe\a?b` both sanitise to `a_b` — still produce distinct
        // token files. Without the hash, two daemons on those endpoints
        // would race for the same token path and the CLI would happily
        // read the wrong daemon's token.
        if raw == crate::server::DEFAULT_PIPE_NAME {
            return dirs::data_local_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("nagori")
                .join("nagori.token");
        }
        let segment = raw
            .rsplit(['\\', '/'])
            .next()
            .filter(|s| !s.is_empty())
            .unwrap_or("nagori");
        let sanitised: String = segment
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        // First 8 hex chars of SHA-256 over the full endpoint. ~32 bits
        // of namespace is more than enough to disambiguate the handful
        // of pipes a single user might run concurrently while keeping
        // the filename short for log readability.
        let hash = ContentHash::sha256(raw.as_bytes()).value;
        let suffix = &hash[..8];
        dirs::data_local_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("nagori")
            .join(format!("{sanitised}-{suffix}.token"))
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = endpoint;
        default_token_path()
    }
}

/// Write the token to `path` with `0o600` permissions on Unix.
///
/// Overwrites any existing file — every daemon launch produces a fresh token,
/// so a stale file from a previous run must be replaced. On non-Unix the
/// permissions guarantee is best-effort.
#[cfg(unix)]
pub fn write_token_file(path: &Path, token: &AuthToken) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|err| AppError::Platform(err.to_string()))?;
    }
    // Open with O_CREAT|O_WRONLY|O_TRUNC and 0o600 in one call so there is
    // no window where the file briefly has umask-derived permissions.
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .map_err(|err| AppError::Platform(err.to_string()))?;
    file.write_all(token.as_str().as_bytes())
        .map_err(|err| AppError::Platform(err.to_string()))?;
    file.sync_all()
        .map_err(|err| AppError::Platform(err.to_string()))?;
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
    fn token_path_for_endpoint_co_locates_with_unix_socket() {
        // Default-style endpoint -> token file sits next to it with
        // matching stem. This regression-locks the rule that two daemons
        // on different sockets get different token files.
        let socket = PathBuf::from("/tmp/nagori-test/nagori.sock");
        assert_eq!(
            token_path_for_endpoint(&socket),
            PathBuf::from("/tmp/nagori-test/nagori.token"),
        );
        let custom = PathBuf::from("/tmp/other/dev.sock");
        assert_eq!(
            token_path_for_endpoint(&custom),
            PathBuf::from("/tmp/other/dev.token"),
        );
    }

    #[cfg(windows)]
    #[test]
    fn token_path_for_endpoint_namespaces_pipe_names() {
        // The default pipe must keep producing the historic
        // `nagori.token` filename so existing installs don't lose track
        // of their token on upgrade.
        let default = PathBuf::from(r"\\.\pipe\nagori");
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

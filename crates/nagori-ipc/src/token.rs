use std::path::{Path, PathBuf};

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
}

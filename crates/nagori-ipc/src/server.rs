use std::{future::Future, path::Path};

use nagori_core::{AppError, Result};
#[cfg(unix)]
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
#[cfg(unix)]
use std::sync::Arc;
#[cfg(unix)]
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixListener,
    sync::Semaphore,
    time::timeout,
};

#[cfg(unix)]
use crate::AuthToken;
use crate::{IpcEnvelope, IpcRequest, IpcResponse};

#[cfg(unix)]
const MAX_IPC_REQUEST_BYTES: usize = crate::MAX_IPC_BYTES;

/// Cap each daemon -> client response at the same byte budget the client
/// already enforces (`crate::MAX_IPC_BYTES`). The check runs *after*
/// `serde_json::to_vec`, so the handler still pays for constructing and
/// serialising the response — bounding peak daemon RSS for pathological
/// requests (e.g. `ListRecent` with `limit = usize::MAX`) requires
/// request-level limits at each handler. What this guard does buy is:
/// (a) we never write a line the client's bounded reader would reject as a
/// truncated half-JSON, and (b) we drop the oversized payload immediately
/// in favour of a small structured rejection so the connection can be
/// reused instead of stalling until timeout.
#[cfg(unix)]
const MAX_IPC_RESPONSE_BYTES: usize = crate::MAX_IPC_BYTES;

#[cfg(unix)]
const READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

/// RAII guard for the process umask. Restoring on drop is critical because
/// `umask(2)` is process-global; if we tightened it during `bind` and then
/// panicked, every other file the process creates would inherit the mask.
#[cfg(unix)]
struct UmaskGuard {
    previous: libc::mode_t,
}

#[cfg(unix)]
#[allow(unsafe_code)]
impl UmaskGuard {
    fn set(mask: libc::mode_t) -> Self {
        // SAFETY: `umask` is a thread-safe libc call returning the previous
        // mask. There is no failure mode and no aliasing concerns.
        let previous = unsafe { libc::umask(mask) };
        Self { previous }
    }
}

#[cfg(unix)]
#[allow(unsafe_code)]
impl Drop for UmaskGuard {
    fn drop(&mut self) {
        // SAFETY: see `UmaskGuard::set`.
        let _ = unsafe { libc::umask(self.previous) };
    }
}

/// Bind a `UnixListener` at `path` with `0o600` perms.
///
/// Synchronous-friendly callers (daemon startup) can `await` this and
/// propagate the failure before signalling that they are ready, which is what
/// the daemon needs to fail fast on bind errors instead of staying
/// half-alive.
#[cfg(unix)]
pub async fn bind_unix(path: impl AsRef<Path>) -> Result<UnixListener> {
    let path = path.as_ref();
    if path.exists() {
        let metadata =
            std::fs::symlink_metadata(path).map_err(|err| AppError::Platform(err.to_string()))?;
        if !metadata.file_type().is_socket() {
            return Err(AppError::Platform(format!(
                "refusing to remove non-socket IPC path: {}",
                path.display()
            )));
        }
        if tokio::net::UnixStream::connect(path).await.is_ok() {
            return Err(AppError::Platform(format!(
                "IPC socket is already in use: {}",
                path.display()
            )));
        }
        std::fs::remove_file(path).map_err(|err| AppError::Platform(err.to_string()))?;
    }
    // `bind` creates the socket inode using the process umask. Tighten the
    // mask to 0o077 around the call so the file is born `0o600` and there
    // is no window where a co-tenant on the same machine could `connect()`
    // before the explicit chmod below.
    let listener = {
        let _restore = UmaskGuard::set(0o077);
        UnixListener::bind(path).map_err(|err| AppError::Platform(err.to_string()))?
    };
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|err| AppError::Platform(err.to_string()))?;
    Ok(listener)
}

/// Accept connections on `listener` and dispatch authenticated requests to `handler`.
///
/// Validates the per-launch auth token in each `IpcEnvelope` before
/// dispatching the inner `IpcRequest`. Loops until the listener errors. Token
/// validation runs in constant time (see `AuthToken::verify`) so the response
/// time can't be used to brute the token byte-by-byte.
#[cfg(unix)]
pub async fn accept_loop<F, Fut>(
    listener: UnixListener,
    expected_token: AuthToken,
    handler: F,
) -> Result<()>
where
    F: Fn(IpcRequest) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = IpcResponse> + Send + 'static,
{
    let handler = Arc::new(handler);
    let token = Arc::new(expected_token);
    let semaphore = Arc::new(Semaphore::new(32));

    loop {
        let (stream, _) = listener
            .accept()
            .await
            .map_err(|err| AppError::Platform(err.to_string()))?;
        let permit = semaphore.clone().acquire_owned().await.map_err(|err| {
            AppError::Platform(format!("failed to acquire IPC connection permit: {err}"))
        })?;
        let handler = handler.clone();
        let token = token.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let mut stream = stream;
            // Bound the time we will hold a connection slot for a slow or
            // stalled client. Without this, an idle peer that never writes a
            // newline would pin one of the 32 semaphore permits forever.
            let line = match timeout(READ_TIMEOUT, read_bounded_line(&mut stream)).await {
                Ok(result) => result,
                Err(_) => Err("IPC request timed out".to_owned()),
            };
            let response = match line {
                Ok(line) => match serde_json::from_slice::<IpcEnvelope>(&line) {
                    Ok(envelope) => {
                        if token.verify(&envelope.token) {
                            handler(envelope.request).await
                        } else {
                            IpcResponse::Error(crate::IpcError {
                                code: "unauthorized".to_owned(),
                                message: "invalid auth token".to_owned(),
                                recoverable: false,
                            })
                        }
                    }
                    Err(err) => IpcResponse::Error(crate::IpcError {
                        code: "invalid_request".to_owned(),
                        message: err.to_string(),
                        recoverable: true,
                    }),
                },
                Err(err) => IpcResponse::Error(crate::IpcError {
                    code: "invalid_request".to_owned(),
                    message: err,
                    recoverable: true,
                }),
            };
            let payload = match serde_json::to_vec(&response) {
                Ok(payload) if payload.len() < MAX_IPC_RESPONSE_BYTES => Some(payload),
                Ok(payload) => {
                    // The daemon already paid the allocation by the time we
                    // get here, so this branch protects the *wire* and the
                    // client's bounded reader — not daemon RSS. Replace with
                    // a small error envelope so the caller sees a structured
                    // rejection it can act on (retry with a tighter limit)
                    // instead of timing out on a truncated half-JSON.
                    let oversized = IpcResponse::Error(crate::IpcError {
                        code: "response_too_large".to_owned(),
                        message: format!(
                            "response would be {} bytes, exceeds limit {}",
                            payload.len(),
                            MAX_IPC_RESPONSE_BYTES
                        ),
                        recoverable: false,
                    });
                    serde_json::to_vec(&oversized).ok()
                }
                Err(_) => None,
            };
            if let Some(payload) = payload {
                let _ = stream.write_all(&payload).await;
                let _ = stream.write_all(b"\n").await;
            }
        });
    }
}

#[cfg(unix)]
pub async fn serve_unix<F, Fut>(path: impl AsRef<Path>, token: AuthToken, handler: F) -> Result<()>
where
    F: Fn(IpcRequest) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = IpcResponse> + Send + 'static,
{
    let listener = bind_unix(path).await?;
    accept_loop(listener, token, handler).await
}

#[cfg(unix)]
async fn read_bounded_line(
    stream: &mut tokio::net::UnixStream,
) -> std::result::Result<Vec<u8>, String> {
    let mut line = Vec::new();
    let mut chunk = [0_u8; 4096];
    loop {
        let read = stream
            .read(&mut chunk)
            .await
            .map_err(|err| err.to_string())?;
        if read == 0 {
            break;
        }
        if let Some(newline) = chunk[..read].iter().position(|byte| *byte == b'\n') {
            if line.len() + newline > MAX_IPC_REQUEST_BYTES {
                return Err("IPC request is too large".to_owned());
            }
            line.extend_from_slice(&chunk[..newline]);
            break;
        }
        if line.len() + read > MAX_IPC_REQUEST_BYTES {
            return Err("IPC request is too large".to_owned());
        }
        line.extend_from_slice(&chunk[..read]);
    }
    Ok(line)
}

#[cfg(all(test, unix))]
mod tests {
    use std::time::Duration;

    use nagori_core::AppError;

    use crate::{
        AddEntryRequest, EntryDto, GetEntryRequest, HealthResponse, IpcClient, ListRecentRequest,
        SearchRequest, SearchResponse,
    };

    use super::*;

    fn test_token() -> AuthToken {
        AuthToken::generate().expect("token should generate")
    }

    #[tokio::test]
    async fn refuses_to_unlink_active_socket() {
        let dir = tempfile::tempdir().expect("tempdir should be created");
        let path = dir.path().join("nagori.sock");
        let _listener =
            tokio::net::UnixListener::bind(&path).expect("test listener should bind socket");

        let err = serve_unix(&path, test_token(), |_request| async { IpcResponse::Ack })
            .await
            .expect_err("active socket should be refused");

        assert!(matches!(err, AppError::Platform(message) if message.contains("already in use")));
        assert!(path.exists());
    }

    /// Boot a `serve_unix` server backed by a closure handler in the
    /// background and tear it down once the test scope exits. The returned
    /// `JoinHandle` is aborted in `Drop`-equivalent fashion at the call
    /// site by overwriting the variable; the listening task either yields
    /// on the next `accept` or finishes when the temp dir is removed.
    async fn spawn_handler<F, Fut>(
        path: std::path::PathBuf,
        token: AuthToken,
        handler: F,
    ) -> tokio::task::JoinHandle<()>
    where
        F: Fn(IpcRequest) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = IpcResponse> + Send + 'static,
    {
        let server_path = path.clone();
        let task = tokio::spawn(async move {
            // Errors from `serve_unix` (e.g. abort) are expected when the
            // test concludes; we ignore them.
            let _ = serve_unix(&server_path, token, handler).await;
        });
        // Wait for the socket file to appear before returning so callers
        // can connect immediately. Bounded retry — fail loudly if bind
        // never succeeds.
        for _ in 0..50 {
            if path.exists() {
                return task;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("ipc socket never appeared at {}", path.display());
    }

    #[tokio::test]
    async fn round_trip_health_request_returns_health_response() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("health.sock");
        let token = test_token();
        let server = spawn_handler(path.clone(), token.clone(), |request| async move {
            assert!(matches!(request, IpcRequest::Health));
            IpcResponse::Health(HealthResponse {
                ok: true,
                version: "test-version".to_owned(),
            })
        })
        .await;

        let client = IpcClient::new(path.to_string_lossy().to_string(), token);
        let response = client
            .send(IpcRequest::Health)
            .await
            .expect("health round-trip");
        let IpcResponse::Health(health) = response else {
            panic!("expected health response, got {response:?}");
        };
        assert!(health.ok);
        assert_eq!(health.version, "test-version");
        server.abort();
    }

    #[tokio::test]
    async fn round_trip_search_request_passes_query_and_limit() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("search.sock");
        let token = test_token();
        let server = spawn_handler(path.clone(), token.clone(), |request| async move {
            let IpcRequest::Search(SearchRequest { query, limit }) = request else {
                return IpcResponse::Error(crate::IpcError {
                    code: "test_failure".to_owned(),
                    message: "unexpected request kind".to_owned(),
                    recoverable: false,
                });
            };
            assert_eq!(query, "needle");
            assert_eq!(limit, 7);
            IpcResponse::Search(SearchResponse {
                results: Vec::new(),
            })
        })
        .await;

        let client = IpcClient::new(path.to_string_lossy().to_string(), token);
        let response = client
            .send(IpcRequest::Search(SearchRequest {
                query: "needle".to_owned(),
                limit: 7,
            }))
            .await
            .expect("search round-trip");
        assert!(
            matches!(response, IpcResponse::Search(SearchResponse { results }) if results.is_empty())
        );
        server.abort();
    }

    #[tokio::test]
    async fn rejects_request_with_wrong_token() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("auth.sock");
        let server = spawn_handler(path.clone(), test_token(), |_request| async {
            // Should never run for an unauthorized request — assert below
            // verifies the response was synthesised by the server before
            // dispatch.
            IpcResponse::Ack
        })
        .await;

        let bogus = AuthToken::generate().expect("alt token");
        let client = IpcClient::new(path.to_string_lossy().to_string(), bogus);
        let response = client
            .send(IpcRequest::Health)
            .await
            .expect("auth round-trip");
        let IpcResponse::Error(err) = response else {
            panic!("expected error response, got {response:?}");
        };
        assert_eq!(err.code, "unauthorized");
        assert!(!err.recoverable);
        server.abort();
    }

    #[tokio::test]
    async fn round_trip_invalid_request_returns_error_response() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("bad.sock");
        // The handler is unreachable for malformed payloads — the server
        // synthesises an invalid_request error before the closure runs.
        let server = spawn_handler(path.clone(), test_token(), |_request| async {
            IpcResponse::Ack
        })
        .await;

        let mut stream = tokio::net::UnixStream::connect(&path)
            .await
            .expect("connect");
        tokio::io::AsyncWriteExt::write_all(&mut stream, b"not-json-payload\n")
            .await
            .expect("write");
        let mut buf = Vec::new();
        let mut chunk = [0_u8; 1024];
        loop {
            let n = tokio::io::AsyncReadExt::read(&mut stream, &mut chunk)
                .await
                .expect("read");
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
            if buf.contains(&b'\n') {
                break;
            }
        }
        let line = buf
            .split(|byte| *byte == b'\n')
            .next()
            .expect("response line");
        let response: IpcResponse = serde_json::from_slice(line).expect("decode response");
        let IpcResponse::Error(err) = response else {
            panic!("expected error response, got {response:?}");
        };
        assert_eq!(err.code, "invalid_request");
        assert!(err.recoverable);
        server.abort();
    }

    #[tokio::test]
    async fn round_trip_handler_serialises_entry_payloads_intact() {
        use time::OffsetDateTime;
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("entry.sock");
        let token = test_token();
        let server = spawn_handler(path.clone(), token.clone(), move |request| async move {
            match request {
                IpcRequest::AddEntry(AddEntryRequest { text }) => {
                    assert_eq!(text, "ipc payload");
                    IpcResponse::Entry(EntryDto {
                        id: nagori_core::EntryId::new(),
                        kind: nagori_core::ContentKind::Text,
                        text: Some(text),
                        preview: "ipc payload".to_owned(),
                        created_at: OffsetDateTime::now_utc(),
                        updated_at: OffsetDateTime::now_utc(),
                        last_used_at: None,
                        use_count: 0,
                        pinned: false,
                        source_app_name: None,
                        sensitivity: nagori_core::Sensitivity::Public,
                    })
                }
                IpcRequest::ListRecent(ListRecentRequest { .. })
                | IpcRequest::GetEntry(GetEntryRequest { .. }) => IpcResponse::Entries(Vec::new()),
                _ => IpcResponse::Ack,
            }
        })
        .await;

        let client = IpcClient::new(path.to_string_lossy().to_string(), token);
        let response = client
            .send(IpcRequest::AddEntry(AddEntryRequest {
                text: "ipc payload".to_owned(),
            }))
            .await
            .expect("add round-trip");
        let IpcResponse::Entry(entry) = response else {
            panic!("expected entry response, got {response:?}");
        };
        assert_eq!(entry.text.as_deref(), Some("ipc payload"));
        assert_eq!(entry.preview, "ipc payload");
        server.abort();
    }
}

#[cfg(not(unix))]
pub async fn serve_unix<F, Fut>(
    _path: impl AsRef<Path>,
    _token: crate::AuthToken,
    _handler: F,
) -> Result<()>
where
    F: Fn(IpcRequest) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = IpcResponse> + Send + 'static,
{
    Err(AppError::Unsupported(
        "Unix socket IPC server is not available on this platform".to_owned(),
    ))
}

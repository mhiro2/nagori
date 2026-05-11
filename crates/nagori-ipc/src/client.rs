use std::time::Duration;

use nagori_core::{AppError, Result};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::time::timeout;

use crate::{AuthToken, IpcEnvelope, IpcRequest, IpcResponse};

const MAX_IPC_RESPONSE_BYTES: usize = crate::MAX_IPC_BYTES;

/// Total budget for connect+write+read on a single IPC round trip. Without a
/// cap, a half-alive daemon (or a malicious peer that accepts but never
/// answers) would pin the CLI forever.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

/// Windows named-pipe servers signal "all instances busy" with
/// `ERROR_PIPE_BUSY` (231). Treat it as transient and back off briefly
/// before retrying within the connect budget.
#[cfg(windows)]
const ERROR_PIPE_BUSY: i32 = 231;
#[cfg(windows)]
const PIPE_BUSY_RETRY: Duration = Duration::from_millis(50);

#[derive(Debug, Clone)]
pub struct IpcClient {
    path: String,
    token: AuthToken,
    request_timeout: Duration,
    connect_timeout: Duration,
}

impl IpcClient {
    pub fn new(path: impl Into<String>, token: AuthToken) -> Self {
        Self {
            path: path.into(),
            token,
            request_timeout: REQUEST_TIMEOUT,
            connect_timeout: CONNECT_TIMEOUT,
        }
    }

    /// Override the request timeout. Mostly for tests that need to assert the
    /// CLI gives up rather than waiting on a half-alive peer.
    #[must_use]
    pub const fn with_request_timeout(mut self, timeout: Duration) -> Self {
        self.request_timeout = timeout;
        self
    }

    /// Override the connect-only timeout. The full request also has its own
    /// budget; this is for callers that want to fail faster when the socket
    /// path exists but no one is accepting.
    #[must_use]
    pub const fn with_connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = timeout;
        self
    }

    #[cfg(any(unix, windows))]
    pub async fn send(&self, request: IpcRequest) -> Result<IpcResponse> {
        match timeout(self.request_timeout, self.send_inner(request)).await {
            Ok(result) => result,
            Err(_) => Err(AppError::Platform(format!(
                "IPC request timed out after {:?}",
                self.request_timeout
            ))),
        }
    }

    #[cfg(unix)]
    async fn send_inner(&self, request: IpcRequest) -> Result<IpcResponse> {
        let connect_fut = tokio::net::UnixStream::connect(&self.path);
        let stream = match timeout(self.connect_timeout, connect_fut).await {
            Ok(Ok(stream)) => stream,
            Ok(Err(err)) => return Err(AppError::Platform(err.to_string())),
            Err(_) => {
                return Err(AppError::Platform(format!(
                    "IPC connect timed out after {:?}",
                    self.connect_timeout
                )));
            }
        };
        exchange_envelope(stream, &self.token, request).await
    }

    #[cfg(all(windows, not(unix)))]
    async fn send_inner(&self, request: IpcRequest) -> Result<IpcResponse> {
        let stream = match timeout(self.connect_timeout, open_pipe_client(&self.path)).await {
            Ok(Ok(stream)) => stream,
            Ok(Err(err)) => return Err(AppError::Platform(err.to_string())),
            Err(_) => {
                return Err(AppError::Platform(format!(
                    "IPC connect timed out after {:?}",
                    self.connect_timeout
                )));
            }
        };
        exchange_envelope(stream, &self.token, request).await
    }

    #[cfg(not(any(unix, windows)))]
    pub async fn send(&self, _request: IpcRequest) -> Result<IpcResponse> {
        Err(AppError::Unsupported(
            "IPC client is not implemented on this platform".to_owned(),
        ))
    }
}

/// Common write-envelope-then-read-line helper used by every transport. The
/// stream type is the only thing that varies between unix-socket and
/// named-pipe paths, so isolating the wire-format work here keeps the
/// envelope shape, length-prefix, and bounded reader identical across
/// platforms (any divergence would be a wire-compat bug).
#[cfg(any(unix, windows))]
async fn exchange_envelope<S>(
    mut stream: S,
    token: &AuthToken,
    request: IpcRequest,
) -> Result<IpcResponse>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let envelope = IpcEnvelope {
        token: token.as_str().to_owned(),
        request,
    };
    let payload =
        serde_json::to_vec(&envelope).map_err(|err| AppError::InvalidInput(err.to_string()))?;
    stream
        .write_all(&payload)
        .await
        .map_err(|err| AppError::Platform(err.to_string()))?;
    stream
        .write_all(b"\n")
        .await
        .map_err(|err| AppError::Platform(err.to_string()))?;
    // Flush so the daemon sees the request promptly on transports (named
    // pipes) that buffer until shutdown; on unix sockets this is a cheap
    // no-op.
    stream
        .flush()
        .await
        .map_err(|err| AppError::Platform(err.to_string()))?;
    let response = read_bounded_line(&mut stream).await?;
    serde_json::from_slice(&response).map_err(|err| AppError::Platform(err.to_string()))
}

/// Open a Windows named-pipe client, retrying briefly on `ERROR_PIPE_BUSY`.
/// The server can only hold a single connected instance at a time per
/// `NamedPipeServer` handle; if the daemon's accept loop is between
/// `connect()` returning and re-creating the next instance, we transiently
/// see `ERROR_PIPE_BUSY` and just retry within the caller's connect budget.
#[cfg(windows)]
async fn open_pipe_client(
    path: &str,
) -> std::result::Result<tokio::net::windows::named_pipe::NamedPipeClient, std::io::Error> {
    use tokio::net::windows::named_pipe::ClientOptions;

    loop {
        match ClientOptions::new().open(path) {
            Ok(client) => return Ok(client),
            Err(err) if err.raw_os_error() == Some(ERROR_PIPE_BUSY) => {
                tokio::time::sleep(PIPE_BUSY_RETRY).await;
            }
            Err(err) => return Err(err),
        }
    }
}

async fn read_bounded_line<R>(reader: &mut R) -> Result<Vec<u8>>
where
    R: AsyncRead + Unpin,
{
    let mut line = Vec::new();
    let mut chunk = [0_u8; 4096];
    loop {
        let read = reader
            .read(&mut chunk)
            .await
            .map_err(|err| AppError::Platform(err.to_string()))?;
        if read == 0 {
            break;
        }
        if let Some(newline) = chunk[..read].iter().position(|byte| *byte == b'\n') {
            if line.len() + newline > MAX_IPC_RESPONSE_BYTES {
                return Err(AppError::Platform("IPC response is too large".to_owned()));
            }
            line.extend_from_slice(&chunk[..newline]);
            break;
        }
        if line.len() + read > MAX_IPC_RESPONSE_BYTES {
            return Err(AppError::Platform("IPC response is too large".to_owned()));
        }
        line.extend_from_slice(&chunk[..read]);
    }
    Ok(line)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn bounded_response_reader_rejects_oversized_lines() {
        let (mut client, mut server) = tokio::io::duplex(MAX_IPC_RESPONSE_BYTES + 128);
        let writer = tokio::spawn(async move {
            let payload = vec![b'a'; MAX_IPC_RESPONSE_BYTES + 1];
            server
                .write_all(&payload)
                .await
                .expect("write should succeed");
            server.write_all(b"\n").await.expect("write should succeed");
        });

        let err = read_bounded_line(&mut client)
            .await
            .expect_err("oversized response should fail");

        assert!(matches!(err, AppError::Platform(message) if message.contains("too large")));
        writer.await.expect("writer task should finish");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn send_times_out_when_peer_never_accepts() {
        // Bind a UnixListener but never accept — connect succeeds but the
        // server side never reads/responds. The client must give up rather
        // than wait forever.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("stalled.sock");
        let _listener = tokio::net::UnixListener::bind(&path).expect("listener should bind socket");

        let token = AuthToken::generate().expect("token");
        let client = IpcClient::new(path.to_string_lossy().to_string(), token)
            .with_request_timeout(Duration::from_millis(50))
            .with_connect_timeout(Duration::from_millis(50));

        let started = tokio::time::Instant::now();
        let err = client
            .send(IpcRequest::Health)
            .await
            .expect_err("stalled peer should time out");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "client must give up promptly"
        );
        assert!(matches!(err, AppError::Platform(message) if message.contains("timed out")));
    }

    #[tokio::test]
    async fn bounded_response_reader_returns_line_without_newline() {
        let (mut client, mut server) = tokio::io::duplex(64);
        let writer = tokio::spawn(async move {
            server
                .write_all(br#"{"Health":{"ok":true,"version":"test"}}"#)
                .await
                .expect("write should succeed");
            server
                .write_all(b"\nextra")
                .await
                .expect("write should succeed");
        });

        let line = read_bounded_line(&mut client)
            .await
            .expect("line should be read");

        assert_eq!(line, br#"{"Health":{"ok":true,"version":"test"}}"#);
        writer.await.expect("writer task should finish");
    }
}

use std::time::Duration;

use nagori_core::{AppError, Result};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::time::timeout;

use crate::{AuthToken, IpcEnvelope, IpcRequest, IpcResponse};

const MAX_IPC_RESPONSE_BYTES: usize = crate::MAX_IPC_BYTES;

/// Upper bound for a single outgoing request payload. The server's bounded
/// reader counts only the payload bytes (the trailing `\n` delimiter is
/// excluded) and rejects anything larger, so pre-checking against the same
/// bound here lets the CLI fail fast with a deterministic error instead of
/// writing a request the daemon will only drop.
const MAX_IPC_REQUEST_BYTES: usize = crate::MAX_IPC_BYTES;

/// Default budget for connect+write+read on a single IPC round trip. Without a
/// cap, a half-alive daemon (or a malicious peer that accepts but never
/// answers) would pin the CLI forever. Long-running requests opt into
/// [`LONG_REQUEST_TIMEOUT`] instead — see [`request_timeout`].
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

/// Budget for requests the daemon legitimately drives for minutes:
/// `RunAiAction` (model inference, bounded server-side by
/// `ai.request_timeout_ms` up to `MAX_AI_REQUEST_TIMEOUT_MS`) and `Clear`
/// (a bulk delete over a large history). The 15 s default would sever a valid
/// inference *and* — because the client closing the socket is the daemon's
/// cancel signal — abort the server-side handler mid-flight, so `nagori ai
/// --ipc` failed on every non-trivial prompt.
///
/// The 60 s grace over the server's max deadline is deliberately comfortable:
/// the client clock starts before connect+write, while the daemon's deadline
/// starts only once it receives the request (and it reaps leaked handles a
/// further `REAP_GRACE` later). The margin guarantees the client outlasts the
/// server, so a request that hits the cap returns the daemon's structured
/// `deadline_exceeded` rather than racing it to a generic client timeout.
const LONG_REQUEST_TIMEOUT: Duration =
    Duration::from_millis(nagori_core::settings::MAX_AI_REQUEST_TIMEOUT_MS + 60_000);

const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

/// Resolve the round-trip budget for `request`. An explicit
/// [`IpcClient::with_request_timeout`] override (`Some`) wins for every request
/// kind so tests can still force a fast give-up; otherwise model-backed actions
/// and bulk deletes get [`LONG_REQUEST_TIMEOUT`] and everything else the
/// default.
fn request_timeout(override_timeout: Option<Duration>, request: &IpcRequest) -> Duration {
    override_timeout.unwrap_or(match request {
        IpcRequest::RunAiAction(_) | IpcRequest::Clear(_) => LONG_REQUEST_TIMEOUT,
        _ => REQUEST_TIMEOUT,
    })
}

/// Windows named-pipe servers signal "all instances busy" with
/// `ERROR_PIPE_BUSY` (231). Treat it as transient and back off briefly
/// before retrying within the connect budget.
#[cfg(windows)]
const ERROR_PIPE_BUSY: i32 = 231;
#[cfg(windows)]
const PIPE_BUSY_RETRY: Duration = Duration::from_millis(50);

/// `ERROR_FILE_NOT_FOUND` (2): the daemon's accept loop has a window between
/// a successful `connect()` and creating the next chained pipe instance
/// during which *no* instance exists at the name, so `CreateFile` fails with
/// this code rather than `ERROR_PIPE_BUSY`. It is also the steady-state error
/// when no daemon is running at all, so it is only retried within the short
/// budget below — long enough to ride out the instance-switchover window,
/// short enough that a dead daemon still fails fast instead of burning the
/// whole connect budget.
#[cfg(windows)]
const ERROR_FILE_NOT_FOUND: i32 = 2;
#[cfg(windows)]
const PIPE_NOT_FOUND_RETRY_BUDGET: Duration = Duration::from_millis(250);

/// `SECURITY_IDENTIFICATION` impersonation level (`winbase.h`, `0x0001_0000`).
///
/// Without an explicit `QoS` level, a process that squatted `\\.\pipe\nagori`
/// before the daemon bound it could attempt to impersonate the connecting
/// client at the default (`Impersonation`) level. Identification lets the
/// server learn who connected but never act under the client's token.
/// tokio's `security_qos_flags` ORs in `SECURITY_SQOS_PRESENT` itself, so
/// only the level is specified here.
#[cfg(windows)]
const SECURITY_IDENTIFICATION: u32 = 0x0001_0000;

#[derive(Debug, Clone)]
pub struct IpcClient {
    path: String,
    token: AuthToken,
    /// `None` selects a per-request default (see [`request_timeout`]); `Some`
    /// is an explicit override that applies to every request kind.
    request_timeout: Option<Duration>,
    connect_timeout: Duration,
}

impl IpcClient {
    pub fn new(path: impl Into<String>, token: AuthToken) -> Self {
        Self {
            path: path.into(),
            token,
            request_timeout: None,
            connect_timeout: CONNECT_TIMEOUT,
        }
    }

    /// Override the per-request timeout for *every* request kind. Mostly for
    /// tests that need to assert the CLI gives up rather than waiting on a
    /// half-alive peer, and for the daemon's own fast liveness probe.
    #[must_use]
    pub const fn with_request_timeout(mut self, timeout: Duration) -> Self {
        self.request_timeout = Some(timeout);
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
        let budget = request_timeout(self.request_timeout, &request);
        match timeout(budget, self.send_inner(request)).await {
            Ok(result) => result,
            Err(_) => Err(AppError::Platform(format!(
                "IPC request timed out after {budget:?}"
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
    // The server's bounded reader caps the payload (the trailing `\n` is not
    // counted), so mirror that exact bound and reject before we touch the
    // socket — otherwise we write a request the daemon is guaranteed to drop
    // and the caller sees an opaque connection error instead of a clear size
    // violation.
    if payload.len() > MAX_IPC_REQUEST_BYTES {
        return Err(AppError::InvalidInput(format!(
            "IPC request is {} bytes, exceeds the limit of {MAX_IPC_REQUEST_BYTES} bytes",
            payload.len()
        )));
    }
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

/// Open a Windows named-pipe client, retrying briefly on the two transient
/// connect failures the daemon's accept loop can legitimately produce:
/// `ERROR_PIPE_BUSY` when every instance is mid-handshake, and
/// `ERROR_FILE_NOT_FOUND` during the window between a successful server-side
/// `connect()` and the creation of the next chained instance (when no
/// instance exists at the name at all). `ERROR_PIPE_BUSY` retries within the
/// caller's connect budget; `ERROR_FILE_NOT_FOUND` only within
/// [`PIPE_NOT_FOUND_RETRY_BUDGET`] because it is also the steady-state
/// "daemon not running" error and must keep failing fast in that case.
#[cfg(windows)]
async fn open_pipe_client(
    path: &str,
) -> std::result::Result<tokio::net::windows::named_pipe::NamedPipeClient, std::io::Error> {
    use tokio::net::windows::named_pipe::ClientOptions;

    let started = tokio::time::Instant::now();
    loop {
        match ClientOptions::new()
            .security_qos_flags(SECURITY_IDENTIFICATION)
            .open(path)
        {
            Ok(client) => return Ok(client),
            Err(err) if err.raw_os_error() == Some(ERROR_PIPE_BUSY) => {
                tokio::time::sleep(PIPE_BUSY_RETRY).await;
            }
            Err(err)
                if err.raw_os_error() == Some(ERROR_FILE_NOT_FOUND)
                    && started.elapsed() < PIPE_NOT_FOUND_RETRY_BUDGET =>
            {
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

    #[test]
    fn long_running_requests_get_extended_timeout() {
        use nagori_core::{AiActionId, AiRequestOptions, EntryId};

        let ai = IpcRequest::RunAiAction(crate::RunAiActionRequest {
            id: EntryId::new(),
            action: AiActionId::Summarize,
            options: AiRequestOptions::default(),
        });
        let clear = IpcRequest::Clear(crate::ClearRequest::All);

        // Model inference and bulk deletes get the long ceiling...
        assert_eq!(request_timeout(None, &ai), LONG_REQUEST_TIMEOUT);
        assert_eq!(request_timeout(None, &clear), LONG_REQUEST_TIMEOUT);
        // ...while ordinary requests keep the snappy default.
        assert_eq!(request_timeout(None, &IpcRequest::Health), REQUEST_TIMEOUT);

        // An explicit override wins for every request kind so tests and the
        // daemon's liveness probe can still force a fast give-up.
        let override_timeout = Duration::from_millis(5);
        assert_eq!(
            request_timeout(Some(override_timeout), &ai),
            override_timeout
        );
        assert_eq!(
            request_timeout(Some(override_timeout), &IpcRequest::Health),
            override_timeout,
        );
    }

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

    #[cfg(any(unix, windows))]
    #[tokio::test]
    async fn oversized_request_is_rejected_before_send() {
        // A request whose serialized payload exceeds the server's line cap must
        // be rejected locally — before any bytes hit the socket — so the caller
        // sees a clear size violation instead of an opaque dropped connection.
        let token = AuthToken::generate().expect("token");
        let huge = "a".repeat(MAX_IPC_REQUEST_BYTES + 1);
        let request = IpcRequest::AddEntry(crate::AddEntryRequest { text: huge });

        // The peer never reads. With a tiny duplex buffer a real `write_all`
        // would block, so resolving to the error at all proves the size
        // pre-check fired before any write was attempted.
        let (client, _server) = tokio::io::duplex(64);
        let err = exchange_envelope(client, &token, request)
            .await
            .expect_err("oversized request must be rejected");

        assert!(
            matches!(err, AppError::InvalidInput(message) if message.contains("exceeds the limit"))
        );
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

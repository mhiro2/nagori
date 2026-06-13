//! Transport-agnostic per-connection driver.
//!
//! Both the Unix-socket and Windows named-pipe servers funnel through
//! [`handle_connection`], so the bounded read, auth check, and bounded
//! write-back live here once. Compiled only on platforms that have a
//! server transport.

use std::future::Future;
use std::sync::Arc;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::time::timeout;
use tracing::warn;

use crate::AuthToken;
use crate::{IpcEnvelope, IpcRequest, IpcResponse};

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
const MAX_IPC_RESPONSE_BYTES: usize = crate::MAX_IPC_BYTES;

/// Hard ceiling on how long a single connection can block before the
/// envelope is fully read. CLI clients send one short JSON line and
/// disconnect, so a few seconds is plenty of slack for the slowest
/// realistic local round-trip. Kept tight because on Windows the named
/// pipe uses the default DACL — any local user can open a connection
/// and would otherwise park one of the 32 permits for the full window
/// without ever sending a byte, starving the legitimate CLI.
const READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// Sub-budget for the first read. If the peer hasn't sent any bytes
/// within this window we drop the connection immediately. Caps the
/// silent-peer slow-loris cost at roughly one second per parked permit,
/// while still letting a slightly stalled writer (e.g. the CLI flushing
/// stdin) complete the envelope under `READ_TIMEOUT`.
const FIRST_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);

/// Hard ceiling on how long a single connection can block while the
/// response is written back. The read side is already bounded by
/// `READ_TIMEOUT`, but `write_all` + `flush` block once the transport's
/// socket / pipe buffer fills — so a client that authenticates, triggers a
/// large response, then stops reading would otherwise pin its connection
/// permit (one of the 32) and its handler task indefinitely. Thirty-two
/// such slow-readers would starve the legitimate CLI. Sized in the same
/// few-seconds band as `READ_TIMEOUT`: ample for the slowest realistic
/// local writeback, tight enough that a wedged reader frees its permit
/// promptly.
const WRITE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// Absolute ceiling on how long a single request handler may run before the
/// connection is force-released and a structured `deadline_exceeded` rejection
/// is written back. This is a *backstop*: peer-disconnect detection
/// (`wait_for_peer_close`) already frees the connection slot — and cancels the
/// handler's in-flight work — the instant a client gives up and closes, which
/// is the common case (the CLI client's own request timeout is far shorter).
/// The deadline only matters for the degenerate case where the peer neither
/// reads the response nor closes while the handler is wedged.
///
/// Sized strictly above the longest *legitimate* handler so it never trips a
/// healthy request: a `RunAiAction` drives the model to completion under its
/// own absolute deadline, which the settings cap at
/// [`nagori_core::settings::MAX_AI_REQUEST_TIMEOUT_MS`]. The extra slack keeps
/// this from racing a legitimately slow AI action that is finishing right at
/// its own deadline.
const HANDLER_DEADLINE: std::time::Duration = std::time::Duration::from_millis(
    nagori_core::settings::MAX_AI_REQUEST_TIMEOUT_MS.saturating_add(30_000),
);

/// Bounded-read + auth-check + write-back driver shared by every
/// transport. Generic over `AsyncRead + AsyncWrite` so the Unix-socket and
/// Windows named-pipe servers reuse the exact same envelope handling.
pub(super) async fn handle_connection<S, F, Fut>(
    mut stream: S,
    permit: tokio::sync::OwnedSemaphorePermit,
    handler: Arc<F>,
    token: Arc<AuthToken>,
) where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    F: Fn(IpcRequest) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = IpcResponse> + Send + 'static,
{
    let _permit = permit;
    // Bound the time we will hold a connection slot for a slow or stalled
    // client. Without this, an idle peer that never writes a newline would
    // pin one of the 32 semaphore permits forever.
    let line = match timeout(READ_TIMEOUT, read_bounded_line(&mut stream)).await {
        Ok(result) => result,
        Err(_) => Err("IPC request timed out".to_owned()),
    };
    let response = match line {
        Ok(line) => match serde_json::from_slice::<IpcEnvelope>(&line) {
            Ok(envelope) => {
                if token.verify(&envelope.token) {
                    // Run the handler under a server-side deadline and watch
                    // the read half for a peer disconnect. `None` means the
                    // handler was cancelled — the peer went away (skip the
                    // write entirely) — so we drop the stream and the permit
                    // by returning, releasing the connection slot and (via the
                    // dropped handler future) any AI permit / DB work it held.
                    match run_handler_bounded(&mut stream, handler(envelope.request)).await {
                        Some(response) => response,
                        None => return,
                    }
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
            // The daemon already paid the allocation by the time we get
            // here, so this branch protects the *wire* and the client's
            // bounded reader — not daemon RSS. Replace with a small error
            // envelope so the caller sees a structured rejection it can
            // act on (retry with a tighter limit) instead of timing out
            // on a truncated half-JSON.
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
        // Bound the write-back the same way the read side is bounded. Once
        // the transport buffer fills, `write_all` blocks on a client that
        // has stopped reading; without a ceiling that handler — and the
        // connection permit it holds — would be pinned forever. On timeout
        // we fall through and return, dropping `stream` (closing the
        // connection) and `_permit` (freeing the slot) so a starved CLI can
        // make progress. Inner write errors stay best-effort, as before.
        let write_back = async {
            stream.write_all(&payload).await?;
            stream.write_all(b"\n").await?;
            // Best-effort flush so the client receives the response promptly
            // even on transports (named pipes) that buffer until shutdown.
            stream.flush().await
        };
        if timeout(WRITE_TIMEOUT, write_back).await.is_err() {
            warn!(
                timeout_ms = u64::try_from(WRITE_TIMEOUT.as_millis()).unwrap_or(u64::MAX),
                "ipc_write_timeout_dropping_slow_reader",
            );
        }
    }
}

/// Drive `handler` to completion while bounding it two ways:
///
/// * **Peer disconnect.** The handler never touches the stream, so we
///   concurrently watch the read half for EOF. The CLI client keeps its write
///   half open until it has read the response (it never half-closes — see
///   `exchange_envelope`), so an `Ok(0)` here means the peer truly went away
///   (gave up after its own timeout, exited, or dropped the client). Returning
///   `None` lets the caller skip the write and drop the handler future,
///   cancelling whatever in-flight work it held — the connection permit, the
///   AI concurrency permit, and any pending DB query — instead of finishing a
///   response no one will read.
/// * **Server-side deadline.** A backstop for the degenerate case where the
///   peer neither reads nor closes while the handler is wedged. On expiry we
///   return a structured `deadline_exceeded` error so a peer that *is* still
///   waiting gets a deterministic rejection (best-effort, bounded by
///   `WRITE_TIMEOUT` like every other write-back) rather than a hang.
///
/// `biased` so a handler that completes in the same poll as a coincident
/// deadline / EOF still wins — we always prefer delivering a real response.
async fn run_handler_bounded<S, Fut>(stream: &mut S, handler: Fut) -> Option<IpcResponse>
where
    S: AsyncRead + Unpin,
    Fut: Future<Output = IpcResponse>,
{
    tokio::pin!(handler);
    tokio::select! {
        biased;
        response = &mut handler => Some(response),
        () = wait_for_peer_close(stream) => {
            warn!("ipc_peer_disconnected_cancelling_handler");
            None
        }
        () = tokio::time::sleep(HANDLER_DEADLINE) => {
            warn!(
                timeout_ms = u64::try_from(HANDLER_DEADLINE.as_millis()).unwrap_or(u64::MAX),
                "ipc_handler_deadline_exceeded",
            );
            Some(IpcResponse::Error(crate::IpcError {
                code: "deadline_exceeded".to_owned(),
                message: format!(
                    "handler exceeded the {}ms server deadline",
                    HANDLER_DEADLINE.as_millis()
                ),
                recoverable: false,
            }))
        }
    }
}

/// Resolve once the peer closes its end of `stream` (or the read errors).
///
/// Never resolves while the peer is alive and waiting: the one-line
/// request/response protocol means a well-behaved peer sends nothing after its
/// request, so the read stays pending until EOF. Any stray bytes a peer
/// pipelines after the request line are ignored (we keep watching) rather than
/// mistaken for a disconnect. The read is only ever dropped while pending (when
/// another `select!` arm wins), so it cannot swallow buffered response bytes.
async fn wait_for_peer_close<S>(stream: &mut S)
where
    S: AsyncRead + Unpin,
{
    let mut scratch = [0_u8; 256];
    loop {
        match stream.read(&mut scratch).await {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
    }
}

async fn read_bounded_line<R>(stream: &mut R) -> std::result::Result<Vec<u8>, String>
where
    R: AsyncRead + Unpin,
{
    let mut line = Vec::new();
    let mut chunk = [0_u8; 4096];
    let mut first_read = true;
    loop {
        // The first read gets a tight budget so a connecting peer that
        // never writes anything (slow-loris) cannot hold a permit for
        // the full `READ_TIMEOUT`; subsequent reads inherit the
        // surrounding `READ_TIMEOUT` set in `handle_connection`.
        let read = if first_read {
            match timeout(FIRST_READ_TIMEOUT, stream.read(&mut chunk)).await {
                Ok(result) => result.map_err(|err| err.to_string())?,
                Err(_) => return Err("IPC peer sent no data".to_owned()),
            }
        } else {
            stream
                .read(&mut chunk)
                .await
                .map_err(|err| err.to_string())?
        };
        first_read = false;
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

/// Unit tests for the request framing read by [`read_bounded_line`] — the
/// server's sole entry point for an incoming request line. Exercises the
/// chunk-boundary handling, the request size ceiling, and the first-read
/// slow-loris timeout in isolation from any transport.
#[cfg(test)]
mod tests_framing {
    use std::pin::Pin;
    use std::task::{Context, Poll};

    use tokio::io::{AsyncRead, ReadBuf};

    use super::{FIRST_READ_TIMEOUT, MAX_IPC_REQUEST_BYTES, read_bounded_line};

    /// Reader that is forever `Pending`, modelling a peer that connects and
    /// then sends nothing — the slow-loris the first-read timeout defends.
    struct NeverReady;

    impl AsyncRead for NeverReady {
        fn poll_read(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            _buf: &mut ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            Poll::Pending
        }
    }

    #[tokio::test]
    async fn reads_a_short_line_and_stops_at_the_newline() {
        let mut input: &[u8] = b"hello world\ntrailing ignored";
        let line = read_bounded_line(&mut input).await.expect("line");
        assert_eq!(line, b"hello world");
    }

    #[tokio::test]
    async fn assembles_a_line_that_spans_the_4096_byte_chunk_boundary() {
        // The reader fills at most 4096 bytes per read, so a 5000-byte line
        // with its newline only in the *third* chunk must be assembled across
        // reads — a newline scan that only looked at the first chunk would
        // truncate the request.
        let mut payload = vec![b'a'; 5000];
        payload.push(b'\n');
        payload.extend_from_slice(b"after the newline");
        let mut input: &[u8] = &payload;

        let line = read_bounded_line(&mut input).await.expect("line");
        assert_eq!(line.len(), 5000);
        assert!(line.iter().all(|&b| b == b'a'));
    }

    #[tokio::test]
    async fn rejects_a_request_that_exceeds_the_size_ceiling() {
        // A newline-less stream larger than the ceiling must be refused rather
        // than buffered without bound.
        let oversized = vec![b'a'; MAX_IPC_REQUEST_BYTES + 64];
        let mut input: &[u8] = &oversized;
        let err = read_bounded_line(&mut input)
            .await
            .expect_err("oversized request must be rejected");
        assert!(err.contains("too large"), "got {err}");
    }

    #[tokio::test(start_paused = true)]
    async fn first_read_times_out_when_the_peer_sends_nothing() {
        // The first read carries a tight `FIRST_READ_TIMEOUT` budget so a peer
        // that connects and never writes cannot pin a connection permit for
        // the full read timeout. Paused time auto-advances to the timer.
        let start = tokio::time::Instant::now();
        let mut reader = NeverReady;
        let err = read_bounded_line(&mut reader)
            .await
            .expect_err("a silent peer must trip the first-read timeout");
        assert!(err.contains("no data"), "got {err}");
        assert!(
            start.elapsed() >= FIRST_READ_TIMEOUT,
            "the timeout must wait the first-read budget"
        );
    }
}

/// Transport-agnostic tests for the shared [`handle_connection`] driver.
///
/// Both the Unix-socket and named-pipe servers funnel through
/// `handle_connection`, so a `tokio::io::duplex` peer that authenticates
/// but never reads the response exercises the same slow-reader write path
/// for both. Compiled on every platform that has a server so the
/// regression runs in both the Unix-socket and named-pipe CI matrices.
#[cfg(test)]
mod tests_transport {
    use std::sync::Arc;

    use tokio::sync::Semaphore;

    use super::*;
    use crate::IpcEnvelope;

    fn test_token() -> AuthToken {
        AuthToken::generate().expect("token should generate")
    }

    #[tokio::test(start_paused = true)]
    async fn slow_reader_releases_permit_after_write_timeout() {
        // Regression: before `WRITE_TIMEOUT`, the write-back path was a
        // bare `write_all` + `flush`. A client that authenticated, drew a
        // response, then stopped reading would fill the transport buffer
        // and block the handler forever — pinning one of the 32
        // connection permits. Thirty-two such peers would starve the
        // legitimate CLI. The handler below returns a response far larger
        // than the duplex buffer, the peer never reads it, and we assert
        // the connection times out and frees its permit.
        let token = Arc::new(test_token());
        let handler = Arc::new(|_request: IpcRequest| async {
            // ~1 KiB error response, well above the 16-byte duplex buffer
            // and well under `MAX_IPC_RESPONSE_BYTES` (1 MiB) so it is
            // written rather than rejected as oversized.
            IpcResponse::Error(crate::IpcError {
                code: "x".repeat(512),
                message: "y".repeat(512),
                recoverable: false,
            })
        });

        // A single permit models the production semaphore: a permit that
        // is never returned is exactly the starvation bug.
        let semaphore = Arc::new(Semaphore::new(1));
        let permit = semaphore
            .clone()
            .acquire_owned()
            .await
            .expect("permit should be available");
        assert_eq!(semaphore.available_permits(), 0);

        let request = serde_json::to_vec(&IpcEnvelope {
            token: token.as_str().to_owned(),
            request: IpcRequest::Health,
        })
        .expect("serialise envelope");

        // Tight buffer so even the small response cannot drain in one
        // shot once the peer stops reading.
        let (server_io, mut client_io) = tokio::io::duplex(16);

        let start = tokio::time::Instant::now();
        let server = handle_connection(server_io, permit, handler, token);
        let client = async {
            client_io
                .write_all(&request)
                .await
                .expect("client should write the request envelope");
            client_io
                .write_all(b"\n")
                .await
                .expect("client should terminate the request line");
            // Deliberately never read the response, holding the
            // connection open so the server's write blocks on a full
            // buffer. `pending` parks without a timer so the only timer
            // left is the server's `WRITE_TIMEOUT`, which paused-time
            // auto-advance fires.
            std::future::pending::<()>().await;
        };

        tokio::select! {
            () = server => {}
            () = client => unreachable!("the slow reader never finishes on its own"),
        }

        let elapsed = start.elapsed();
        assert!(
            elapsed >= WRITE_TIMEOUT,
            "handle_connection must block until WRITE_TIMEOUT fires (not error out early): {elapsed:?}",
        );
        assert_eq!(
            semaphore.available_permits(),
            1,
            "the connection permit must be released once the slow reader times out",
        );
    }

    #[tokio::test]
    async fn peer_disconnect_cancels_handler_and_releases_permit() {
        use std::sync::atomic::{AtomicBool, Ordering};

        // Regression: before peer-disconnect detection, a handler kept running
        // (holding its connection permit, AI permit, and any DB work) after the
        // client gave up and closed. This handler never completes on its own but
        // flips a flag when its future is dropped, so we can prove the peer-close
        // path cancelled it rather than waiting out the deadline.
        let cancelled = Arc::new(AtomicBool::new(false));
        let handler = {
            let cancelled = cancelled.clone();
            Arc::new(move |_request: IpcRequest| {
                let cancelled = cancelled.clone();
                async move {
                    struct DropFlag(Arc<AtomicBool>);
                    impl Drop for DropFlag {
                        fn drop(&mut self) {
                            self.0.store(true, Ordering::SeqCst);
                        }
                    }
                    let _flag = DropFlag(cancelled);
                    std::future::pending::<IpcResponse>().await
                }
            })
        };

        let token = Arc::new(test_token());
        let semaphore = Arc::new(Semaphore::new(1));
        let permit = semaphore
            .clone()
            .acquire_owned()
            .await
            .expect("permit should be available");
        assert_eq!(semaphore.available_permits(), 0);

        let request = serde_json::to_vec(&IpcEnvelope {
            token: token.as_str().to_owned(),
            request: IpcRequest::Health,
        })
        .expect("serialise envelope");

        let (server_io, mut client_io) = tokio::io::duplex(256);
        let start = tokio::time::Instant::now();
        let server = handle_connection(server_io, permit, handler, token);
        let client = async move {
            client_io
                .write_all(&request)
                .await
                .expect("client should write the request envelope");
            client_io
                .write_all(b"\n")
                .await
                .expect("client should terminate the request line");
            // Give up like a real client whose own request timeout fired:
            // dropping the stream closes both halves, so the server's read
            // half sees EOF.
            drop(client_io);
        };

        // Both futures live in this task; `join!` drives them to completion.
        tokio::join!(server, client);

        assert!(
            cancelled.load(Ordering::SeqCst),
            "the handler future must be dropped (cancelled) when the peer disconnects",
        );
        assert_eq!(
            semaphore.available_permits(),
            1,
            "the connection permit must be released as soon as the peer disconnects",
        );
        assert!(
            start.elapsed() < HANDLER_DEADLINE,
            "peer disconnect must cancel immediately, not wait out the handler deadline",
        );
    }

    #[tokio::test(start_paused = true)]
    async fn wedged_handler_is_force_released_at_the_deadline() {
        // A handler that never completes, paired with a peer that neither reads
        // the response nor closes the connection (the degenerate case
        // peer-disconnect detection cannot catch). The server-side deadline is
        // the backstop: it must fire, free the permit, and emit a structured
        // rejection rather than pinning the slot forever.
        let handler = Arc::new(|_request: IpcRequest| std::future::pending::<IpcResponse>());

        let token = Arc::new(test_token());
        let semaphore = Arc::new(Semaphore::new(1));
        let permit = semaphore
            .clone()
            .acquire_owned()
            .await
            .expect("permit should be available");
        assert_eq!(semaphore.available_permits(), 0);

        let request = serde_json::to_vec(&IpcEnvelope {
            token: token.as_str().to_owned(),
            request: IpcRequest::Health,
        })
        .expect("serialise envelope");

        // Buffer large enough to absorb the small `deadline_exceeded` response
        // without blocking, so the test isolates the handler deadline from the
        // write-back timeout.
        let (server_io, mut client_io) = tokio::io::duplex(4096);
        let start = tokio::time::Instant::now();
        let server = handle_connection(server_io, permit, handler, token);
        let client = async {
            client_io
                .write_all(&request)
                .await
                .expect("client should write the request envelope");
            client_io
                .write_all(b"\n")
                .await
                .expect("client should terminate the request line");
            // Never read, never close: only the server's `HANDLER_DEADLINE`
            // timer can break the stalemate, which paused-time auto-advance
            // fires once every task is parked.
            std::future::pending::<()>().await;
        };

        tokio::select! {
            () = server => {}
            () = client => unreachable!("the wedged peer never finishes on its own"),
        }

        assert!(
            start.elapsed() >= HANDLER_DEADLINE,
            "the handler must run until HANDLER_DEADLINE fires: {:?}",
            start.elapsed(),
        );
        assert_eq!(
            semaphore.available_permits(),
            1,
            "the connection permit must be released once the handler deadline fires",
        );
    }
}

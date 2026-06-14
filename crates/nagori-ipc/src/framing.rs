//! Shared newline-delimited frame reader for the IPC wire protocol.
//!
//! Both the client (reading a response) and the per-connection server driver
//! (reading a request) need the identical bounded, chunk-spanning line read.
//! Keeping a single implementation here — rather than a near-copy on each side
//! — is what stops the two from drifting on the frame-size boundary: an
//! off-by-one between the reader's `>` and the writer's `<` once let the
//! server reject a response of exactly `MAX_IPC_BYTES` that both readers would
//! have accepted. One reader, one boundary.

use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::time::timeout;

/// Why [`read_line_bounded`] gave up before returning a complete line.
#[derive(Debug)]
pub(crate) enum FrameError {
    /// The accumulated payload would exceed `max_bytes` before a newline was
    /// seen (the trailing `\n` delimiter itself is never counted toward the
    /// bound).
    TooLarge,
    /// `first_read_timeout` was set and the first read produced no bytes within
    /// that budget — a peer that connected and never wrote (slow-loris).
    FirstReadTimeout,
    /// The underlying transport read errored.
    Io(String),
}

/// Read a single newline-delimited frame from `reader`.
///
/// The payload is capped at `max_bytes` (excluding the delimiter); anything
/// larger yields [`FrameError::TooLarge`] rather than buffering without bound.
/// A line of *exactly* `max_bytes` is accepted, so callers wanting symmetry
/// with this reader must reject only payloads strictly larger than the cap.
///
/// When `first_read_timeout` is `Some`, the *first* read carries that budget so
/// a peer that connects and never writes cannot pin the caller; subsequent
/// reads inherit whatever outer timeout the caller wraps this call in. `None`
/// applies no per-read budget (the client bounds the whole round-trip with its
/// own request timeout instead).
pub(crate) async fn read_line_bounded<R>(
    reader: &mut R,
    max_bytes: usize,
    first_read_timeout: Option<Duration>,
) -> Result<Vec<u8>, FrameError>
where
    R: AsyncRead + Unpin,
{
    let mut line = Vec::new();
    let mut chunk = [0_u8; 4096];
    let mut first_read = true;
    loop {
        let read = if first_read {
            match first_read_timeout {
                Some(budget) => match timeout(budget, reader.read(&mut chunk)).await {
                    Ok(result) => result.map_err(|err| FrameError::Io(err.to_string()))?,
                    Err(_) => return Err(FrameError::FirstReadTimeout),
                },
                None => reader
                    .read(&mut chunk)
                    .await
                    .map_err(|err| FrameError::Io(err.to_string()))?,
            }
        } else {
            reader
                .read(&mut chunk)
                .await
                .map_err(|err| FrameError::Io(err.to_string()))?
        };
        first_read = false;
        if read == 0 {
            break;
        }
        if let Some(newline) = chunk[..read].iter().position(|byte| *byte == b'\n') {
            if line.len() + newline > max_bytes {
                return Err(FrameError::TooLarge);
            }
            line.extend_from_slice(&chunk[..newline]);
            break;
        }
        if line.len() + read > max_bytes {
            return Err(FrameError::TooLarge);
        }
        line.extend_from_slice(&chunk[..read]);
    }
    Ok(line)
}

/// Unit tests for the shared frame reader — the sole entry point both
/// transports funnel an incoming line through. Exercises the chunk-boundary
/// handling, the size ceiling (including the exact-boundary acceptance), and
/// the first-read slow-loris timeout in isolation from any transport.
#[cfg(test)]
mod tests {
    use std::pin::Pin;
    use std::task::{Context, Poll};
    use std::time::Duration;

    use tokio::io::{AsyncRead, ReadBuf};

    use super::{FrameError, read_line_bounded};

    const TEST_CAP: usize = crate::MAX_IPC_BYTES;

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
        let line = read_line_bounded(&mut input, TEST_CAP, None)
            .await
            .expect("line");
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

        let line = read_line_bounded(&mut input, TEST_CAP, None)
            .await
            .expect("line");
        assert_eq!(line.len(), 5000);
        assert!(line.iter().all(|&b| b == b'a'));
    }

    #[tokio::test]
    async fn rejects_a_payload_that_exceeds_the_size_ceiling() {
        // A newline-less stream larger than the ceiling must be refused rather
        // than buffered without bound.
        let oversized = vec![b'a'; TEST_CAP + 64];
        let mut input: &[u8] = &oversized;
        let err = read_line_bounded(&mut input, TEST_CAP, None)
            .await
            .expect_err("oversized payload must be rejected");
        assert!(matches!(err, FrameError::TooLarge));
    }

    #[tokio::test]
    async fn accepts_a_payload_of_exactly_the_ceiling() {
        // The boundary is inclusive: a line of exactly `max_bytes` is accepted,
        // so the reader and any writer that caps at the same value agree on the
        // limit. This pins the boundary the server write-back path mirrors.
        let mut payload = vec![b'a'; TEST_CAP];
        payload.push(b'\n');
        let mut input: &[u8] = &payload;
        let line = read_line_bounded(&mut input, TEST_CAP, None)
            .await
            .expect("a payload of exactly the cap must be accepted");
        assert_eq!(line.len(), TEST_CAP);
    }

    #[tokio::test(start_paused = true)]
    async fn first_read_times_out_when_the_peer_sends_nothing() {
        // With a first-read budget set, a peer that connects and never writes
        // trips `FirstReadTimeout` instead of pinning the caller. Paused time
        // auto-advances to the timer.
        let budget = Duration::from_secs(1);
        let start = tokio::time::Instant::now();
        let mut reader = NeverReady;
        let err = read_line_bounded(&mut reader, TEST_CAP, Some(budget))
            .await
            .expect_err("a silent peer must trip the first-read timeout");
        assert!(matches!(err, FrameError::FirstReadTimeout));
        assert!(
            start.elapsed() >= budget,
            "the timeout must wait the first-read budget"
        );
    }

    #[tokio::test]
    async fn no_first_read_budget_never_trips_the_timeout_branch() {
        // With `None`, the first read has no per-read budget — a reader that
        // immediately hits EOF returns an empty line rather than a timeout.
        let mut input: &[u8] = b"";
        let line = read_line_bounded(&mut input, TEST_CAP, None)
            .await
            .expect("EOF with no budget yields an empty line");
        assert!(line.is_empty());
    }
}

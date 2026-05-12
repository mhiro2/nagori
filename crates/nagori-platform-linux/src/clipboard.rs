use async_trait::async_trait;
use nagori_core::{
    AppError, ClipboardContent, ClipboardEntry, ClipboardSequence, ClipboardSnapshot, Result,
};
#[cfg(target_os = "linux")]
use nagori_core::{ClipboardData, ClipboardRepresentation};
use nagori_platform::{CapturedSnapshot, ClipboardReader, ClipboardWriter};
#[cfg(target_os = "linux")]
use sha2::{Digest, Sha256};
#[cfg(target_os = "linux")]
use std::io::Read;
#[cfg(target_os = "linux")]
use time::OffsetDateTime;
#[cfg(target_os = "linux")]
use wl_clipboard_rs::{
    copy::{self, MimeType as CopyMimeType, Options, Source},
    paste::{self, ClipboardType, MimeType as PasteMimeType, Seat},
};

/// Hard ceiling for the unbounded `current_snapshot` path. The capture
/// loop's authoritative size cap is `max_entry_size_bytes` in
/// `AppSettings`, which it threads through `current_snapshot_with_max`;
/// this constant is a defence-in-depth ceiling for the rarely-hit
/// pristine-session path and any future callers that bypass the
/// bounded entry point. 256 MiB is comfortably above any realistic
/// `max_entry_size_bytes` and below the address-space pressure that
/// would put the daemon at risk on a 32-bit Linux host.
#[cfg(target_os = "linux")]
const INTERNAL_BODY_CEILING_BYTES: usize = 256 * 1024 * 1024;

/// Linux (Wayland) clipboard adapter.
///
/// Talks directly to `wl-clipboard-rs` over the Wayland
/// `wlr_data_control` / `ext_data_control` protocols so the daemon does
/// not have to run as a graphical window client. There is **no X11
/// fallback** — that is the whole point of using `wl-clipboard-rs`
/// instead of arboard, which would silently degrade to X11 when the
/// Wayland feature is missing or initialisation fails. If the
/// compositor does not expose either data-control protocol the adapter
/// refuses to start, surfacing the protocol name in the error so the
/// operator can react. GNOME currently ships neither protocol
/// unconditionally; the supported set is wlroots-based compositors and
/// KDE Plasma 5.27+.
pub struct LinuxClipboard {
    #[cfg(target_os = "linux")]
    _marker: (),
}

impl LinuxClipboard {
    #[cfg(target_os = "linux")]
    pub fn new() -> Result<Self> {
        // Eagerly probe the data-control globals so a missing
        // `wlr_data_control_manager_v1` / `ext_data_control_manager_v1`
        // surfaces at construction rather than on the first capture
        // poll. `ClipboardEmpty` / `NoSeats` are success cases — the
        // protocol is bound but the selection or seat list is empty.
        // `MissingProtocol` is what we expect on GNOME today and is
        // the error the operator needs to act on. We do **not**
        // pre-check `WAYLAND_DISPLAY`; `wl-clipboard-rs` delegates to
        // `wayland-client` which surfaces a `WaylandConnection` error
        // when no compositor is reachable, and that is the
        // authoritative signal. `WAYLAND_SOCKET` is not supported here
        // because `wayland-client` consumes the inherited fd on first
        // connect — the constructor probe would burn it before the
        // capture loop's `get_contents` call could reuse it.
        match paste::get_mime_types(ClipboardType::Regular, Seat::Unspecified) {
            Ok(_) | Err(paste::Error::ClipboardEmpty | paste::Error::NoSeats) => {
                Ok(Self { _marker: () })
            }
            Err(paste::Error::MissingProtocol { name, version }) => {
                Err(AppError::Unsupported(format!(
                    "compositor does not expose the Wayland data-control protocol ({name} v{version}). \
                     Nagori requires wlr-data-control or ext-data-control (Sway, KDE Plasma 5.27+, \
                     Hyprland, river). GNOME Wayland does not currently expose these protocols.",
                )))
            }
            Err(paste::Error::WaylandConnection(err)) => Err(AppError::Unsupported(format!(
                "could not connect to a Wayland compositor ({err}). Linux nagori requires a \
                 live Wayland session (set WAYLAND_DISPLAY); X11 is not supported.",
            ))),
            Err(err) => Err(AppError::Platform(format!(
                "could not bind Wayland clipboard: {err}",
            ))),
        }
    }

    #[cfg(not(target_os = "linux"))]
    pub fn new() -> Result<Self> {
        Err(AppError::Unsupported(
            "LinuxClipboard is only available on Linux targets".to_owned(),
        ))
    }
}

#[async_trait]
impl ClipboardReader for LinuxClipboard {
    async fn current_snapshot(&self) -> Result<ClipboardSnapshot> {
        #[cfg(target_os = "linux")]
        {
            let read = pipe_read_pass(INTERNAL_BODY_CEILING_BYTES).await?;
            let sequence = ClipboardSequence::content_hash(read.sequence);
            let text = read
                .buffered
                .map(|bytes| String::from_utf8(bytes).unwrap_or_default())
                .unwrap_or_default();
            let mut representations = Vec::new();
            if !text.is_empty() {
                representations.push(ClipboardRepresentation {
                    mime_type: "text/plain".to_owned(),
                    data: ClipboardData::Text(text),
                });
            }
            Ok(ClipboardSnapshot {
                sequence,
                captured_at: OffsetDateTime::now_utc(),
                source: None,
                representations,
            })
        }
        #[cfg(not(target_os = "linux"))]
        {
            Err(unsupported_off_target())
        }
    }

    async fn current_sequence(&self) -> Result<ClipboardSequence> {
        // Wayland has no `GetClipboardSequenceNumber` equivalent — the
        // closest the data-control protocols expose is the offer's
        // serial, but `wl-clipboard-rs` does not surface it. Stream the
        // body through SHA-256 with a small buffer so that even
        // multi-megabyte clipboards do not pin memory in the daemon
        // address space. Clips above `INTERNAL_BODY_CEILING_BYTES`
        // fall back to a ceiling/prefix-keyed sentinel and close the pipe
        // immediately so a malicious owner cannot keep a blocking
        // worker occupied by streaming forever.
        #[cfg(target_os = "linux")]
        {
            let read = pipe_read_pass_no_buffer(INTERNAL_BODY_CEILING_BYTES).await?;
            Ok(ClipboardSequence::content_hash(read.sequence))
        }
        #[cfg(not(target_os = "linux"))]
        {
            Err(unsupported_off_target())
        }
    }

    #[cfg_attr(not(target_os = "linux"), allow(unused_variables))]
    async fn current_sequence_with_max(&self, max_bytes: usize) -> Result<ClipboardSequence> {
        #[cfg(target_os = "linux")]
        {
            let read = pipe_read_pass_no_buffer(max_bytes).await?;
            Ok(ClipboardSequence::content_hash(read.sequence))
        }
        #[cfg(not(target_os = "linux"))]
        {
            Err(unsupported_off_target())
        }
    }

    #[cfg_attr(not(target_os = "linux"), allow(unused_variables))]
    async fn current_snapshot_with_max(&self, max_bytes: usize) -> Result<CapturedSnapshot> {
        // The capture loop's hot path. The pipe-read pass buffers up
        // to `max_bytes` so a malicious or runaway source app cannot
        // make the daemon allocate gigabytes. Once the stream crosses
        // the configured cap, we close the read end and return an
        // Oversized variant instead of draining the owner-controlled
        // pipe to EOF.
        #[cfg(target_os = "linux")]
        {
            let read = pipe_read_pass(max_bytes).await?;
            let sequence = ClipboardSequence::content_hash(read.sequence);
            match read.buffered {
                Some(bytes) => {
                    let text = String::from_utf8(bytes).unwrap_or_default();
                    let mut representations = Vec::new();
                    if !text.is_empty() {
                        representations.push(ClipboardRepresentation {
                            mime_type: "text/plain".to_owned(),
                            data: ClipboardData::Text(text),
                        });
                    }
                    Ok(CapturedSnapshot::Captured(ClipboardSnapshot {
                        sequence,
                        captured_at: OffsetDateTime::now_utc(),
                        source: None,
                        representations,
                    }))
                }
                None => Ok(CapturedSnapshot::Oversized {
                    sequence,
                    observed_bytes: read.observed_total,
                    limit: max_bytes,
                }),
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            Err(unsupported_off_target())
        }
    }
}

#[async_trait]
impl ClipboardWriter for LinuxClipboard {
    async fn write_entry(&self, entry: &ClipboardEntry) -> Result<()> {
        if let ClipboardContent::Image(_) = &entry.content {
            return Err(AppError::Unsupported(
                "image clipboard writes are not implemented on Linux yet".to_owned(),
            ));
        }
        let Some(text) = entry.plain_text() else {
            return Err(AppError::Unsupported(
                "clipboard entry has no representable payload".to_owned(),
            ));
        };
        self.write_text(text).await
    }

    async fn write_plain(&self, entry: &ClipboardEntry) -> Result<()> {
        let Some(text) = entry.plain_text() else {
            return Err(AppError::Unsupported(
                "clipboard entry has no plain-text payload".to_owned(),
            ));
        };
        self.write_text(text).await
    }

    async fn write_text(&self, text: &str) -> Result<()> {
        #[cfg(target_os = "linux")]
        {
            let bytes = text.as_bytes().to_vec().into_boxed_slice();
            tokio::task::spawn_blocking(move || -> Result<()> {
                // `copy::copy` spawns a background thread that holds
                // the data offer alive until the selection is
                // overwritten; when it returns Ok the offer is
                // registered with the compositor. Errors surface
                // synchronously and are mapped to `AppError::Platform`
                // because by this point we have already validated the
                // protocol is exposed.
                copy::copy(Options::new(), Source::Bytes(bytes), CopyMimeType::Text)
                    .map_err(|err| AppError::Platform(format!("wl-clipboard copy failed: {err}")))
            })
            .await
            .map_err(|err| AppError::Platform(err.to_string()))?
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = text;
            Err(unsupported_off_target())
        }
    }
}

/// Result of one bounded pass over the data-control pipe.
///
/// `sequence` is the SHA-256 of the body when it fits within the caller's
/// read ceiling, or a ceiling/prefix-keyed sentinel above it. The
/// oversized sentinel intentionally does not include the full body length
/// because the reader closes the pipe as soon as the ceiling is crossed.
///
/// `buffered` is `Some(bytes)` iff the stream reaches EOF before both
/// `buffer_cap` and the read ceiling. Pass a `buffer_cap` of `None` (via
/// `pipe_read_pass_no_buffer`) when the caller only needs the sequence —
/// the helper then skips the allocation and just streams bytes through the
/// hasher.
#[cfg(target_os = "linux")]
struct PipePass {
    buffered: Option<Vec<u8>>,
    observed_total: usize,
    sequence: String,
}

#[cfg(target_os = "linux")]
const PIPE_CHUNK: usize = 8 * 1024;

#[cfg(target_os = "linux")]
async fn pipe_read_pass(buffer_cap: usize) -> Result<PipePass> {
    pipe_read_pass_internal(Some(buffer_cap), buffer_cap).await
}

#[cfg(target_os = "linux")]
async fn pipe_read_pass_no_buffer(ceiling: usize) -> Result<PipePass> {
    pipe_read_pass_internal(None, ceiling).await
}

#[cfg(target_os = "linux")]
async fn pipe_read_pass_internal(
    buffer_cap: Option<usize>,
    read_ceiling: usize,
) -> Result<PipePass> {
    tokio::task::spawn_blocking(move || -> Result<PipePass> {
        let mut pipe = match paste::get_contents(
            ClipboardType::Regular,
            Seat::Unspecified,
            PasteMimeType::Text,
        ) {
            Ok((pipe, _mime)) => pipe,
            // Empty selection / no seats / no text mime → treat as
            // empty so the capture loop's body-empty short-circuit
            // kicks in without logging an error every poll.
            Err(
                paste::Error::ClipboardEmpty | paste::Error::NoSeats | paste::Error::NoMimeType,
            ) => {
                return Ok(PipePass {
                    buffered: buffer_cap.map(|_| Vec::new()),
                    observed_total: 0,
                    sequence: hex::encode(Sha256::new().finalize()),
                });
            }
            Err(err) => {
                return Err(AppError::Platform(format!(
                    "wl-clipboard paste failed: {err}"
                )));
            }
        };
        read_pipe_contents(&mut pipe, buffer_cap, read_ceiling)
    })
    .await
    .map_err(|err| AppError::Platform(err.to_string()))?
}

#[cfg(target_os = "linux")]
fn read_pipe_contents(
    pipe: &mut impl Read,
    buffer_cap: Option<usize>,
    read_ceiling: usize,
) -> Result<PipePass> {
    let mut buffer: Option<Vec<u8>> = buffer_cap.map(|_| Vec::new());
    let mut hasher = Sha256::new();
    let mut chunk = [0u8; PIPE_CHUNK];
    let mut observed: usize = 0;
    loop {
        let n = pipe
            .read(&mut chunk)
            .map_err(|err| AppError::Platform(format!("reading clipboard pipe failed: {err}")))?;
        if n == 0 {
            break;
        }
        let previous = observed;
        observed = observed.saturating_add(n);

        // Drop the buffer the moment we exceed `buffer_cap`. The
        // capture loop will see `buffered: None` and surface an
        // Oversized variant.
        if let (Some(cap), Some(buf)) = (buffer_cap, buffer.as_mut()) {
            if observed > cap {
                buffer = None;
            } else {
                buf.extend_from_slice(&chunk[..n]);
            }
        }

        if observed > read_ceiling {
            let prefix_remaining = read_ceiling.saturating_sub(previous).min(n);
            if prefix_remaining > 0 {
                hasher.update(&chunk[..prefix_remaining]);
            }
            let prefix_hash = hex::encode(hasher.finalize());
            return Ok(PipePass {
                buffered: None,
                observed_total: observed,
                sequence: oversized_sequence(read_ceiling, &prefix_hash),
            });
        }

        hasher.update(&chunk[..n]);
    }
    Ok(PipePass {
        buffered: buffer,
        observed_total: observed,
        sequence: hex::encode(hasher.finalize()),
    })
}

#[cfg(target_os = "linux")]
fn oversized_sequence(read_ceiling: usize, prefix_hash: &str) -> String {
    format!("oversized-over:{read_ceiling}:{prefix_hash}")
}

#[cfg(not(target_os = "linux"))]
fn unsupported_off_target() -> AppError {
    AppError::Unsupported("LinuxClipboard is only available on Linux targets".to_owned())
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use std::io::{self, Read};

    use sha2::{Digest, Sha256};

    use super::{PIPE_CHUNK, oversized_sequence, read_pipe_contents};

    struct CountingChunks {
        chunk: Vec<u8>,
        remaining_reads: usize,
        reads: usize,
    }

    impl CountingChunks {
        fn new(chunk_len: usize, remaining_reads: usize) -> Self {
            Self {
                chunk: vec![b'x'; chunk_len],
                remaining_reads,
                reads: 0,
            }
        }
    }

    impl Read for CountingChunks {
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            self.reads += 1;
            if self.remaining_reads == 0 {
                return Ok(0);
            }
            self.remaining_reads -= 1;
            let n = self.chunk.len().min(out.len());
            out[..n].copy_from_slice(&self.chunk[..n]);
            Ok(n)
        }
    }

    #[test]
    fn read_pipe_contents_closes_at_configured_ceiling() {
        let mut reader = CountingChunks::new(PIPE_CHUNK, 8);
        let pass = read_pipe_contents(&mut reader, Some(PIPE_CHUNK), PIPE_CHUNK).unwrap();

        assert_eq!(reader.reads, 2);
        assert_eq!(pass.buffered, None);
        assert_eq!(pass.observed_total, PIPE_CHUNK * 2);
        let expected_prefix = hex::encode(Sha256::digest([b'x'; PIPE_CHUNK]));
        assert_eq!(
            pass.sequence,
            oversized_sequence(PIPE_CHUNK, &expected_prefix)
        );
    }

    #[test]
    fn read_pipe_contents_buffers_within_ceiling() {
        let mut reader = io::Cursor::new(b"clipboard".to_vec());
        let pass = read_pipe_contents(&mut reader, Some(64), 64).unwrap();

        assert_eq!(pass.buffered.as_deref(), Some(&b"clipboard"[..]));
        assert_eq!(pass.observed_total, b"clipboard".len());
    }

    #[test]
    fn read_pipe_contents_uses_prefix_hash_for_oversized_sequence() {
        let mut first = io::Cursor::new([b'a'; PIPE_CHUNK + 1]);
        let mut second = io::Cursor::new([b'b'; PIPE_CHUNK + 1]);

        let first = read_pipe_contents(&mut first, Some(PIPE_CHUNK), PIPE_CHUNK).unwrap();
        let second = read_pipe_contents(&mut second, Some(PIPE_CHUNK), PIPE_CHUNK).unwrap();

        assert_ne!(first.sequence, second.sequence);
    }
}

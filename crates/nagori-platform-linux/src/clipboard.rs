use async_trait::async_trait;
use nagori_core::{
    AppError, ClipboardContent, ClipboardEntry, ClipboardSequence, ClipboardSnapshot,
    RepresentationDataRef, Result, StoredClipboardRepresentation,
};
#[cfg(target_os = "linux")]
use nagori_core::{ClipboardData, ClipboardRepresentation};
use nagori_platform::{CapturedSnapshot, ClipboardReader, ClipboardWriter};
#[cfg(target_os = "linux")]
use sha2::{Digest, Sha256};
#[cfg(target_os = "linux")]
use std::collections::HashSet;
#[cfg(target_os = "linux")]
use std::io::Read;
#[cfg(target_os = "linux")]
use time::OffsetDateTime;
#[cfg(target_os = "linux")]
use wl_clipboard_rs::{
    copy::{self, MimeSource, MimeType as CopyMimeType, Options, Source},
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

/// Image MIME types we will capture, in priority order. Mirrors the
/// `nagori-core` factory's `is_allowlisted_image_mime` allowlist
/// (PNG / JPEG / GIF / WebP / TIFF) — capturing a MIME the factory
/// would later drop wastes the publisher's send and the pipe read for
/// nothing, so the two lists must stay in lockstep. The lookup order
/// is also "first-match wins" so the storage layer sees one canonical
/// image rep per snapshot, matching the Windows adapter's
/// "publish image/png" behaviour.
#[cfg(target_os = "linux")]
const IMAGE_MIME_PRIORITY: &[&str] = &[
    "image/png",
    "image/jpeg",
    "image/gif",
    "image/webp",
    "image/tiff",
];

/// Plain-text MIME types `paste::MimeType::Text` cycles through when
/// it falls back. Mirrors the wl-clipboard-rs internal predicate so we
/// can probe "is text present at all" against the offer set up front
/// instead of always paying for an extra `get_contents` round-trip on
/// pristine sessions.
#[cfg(target_os = "linux")]
const TEXT_MIME_HINTS: &[&str] = &[
    "text/plain;charset=utf-8",
    "UTF8_STRING",
    "text/plain",
    "STRING",
    "TEXT",
];

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
            let pass = pipe_read_multi_pass(Some(INTERNAL_BODY_CEILING_BYTES)).await?;
            let representations = pass.representations.unwrap_or_default();
            Ok(ClipboardSnapshot {
                sequence: ClipboardSequence::content_hash(pass.sequence),
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
            let pass = pipe_read_multi_pass_no_buffer(INTERNAL_BODY_CEILING_BYTES).await?;
            Ok(ClipboardSequence::content_hash(pass.sequence))
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
            let pass = pipe_read_multi_pass_no_buffer(max_bytes).await?;
            Ok(ClipboardSequence::content_hash(pass.sequence))
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
            let pass = pipe_read_multi_pass(Some(max_bytes)).await?;
            let sequence = ClipboardSequence::content_hash(pass.sequence);
            match pass.representations {
                Some(representations) => Ok(CapturedSnapshot::Captured(ClipboardSnapshot {
                    sequence,
                    captured_at: OffsetDateTime::now_utc(),
                    source: None,
                    representations,
                })),
                None => Ok(CapturedSnapshot::Oversized {
                    sequence,
                    observed_bytes: pass.observed_total,
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
        if let ClipboardContent::Image(image) = &entry.content {
            #[cfg(target_os = "linux")]
            {
                let bytes = image.pending_bytes.clone().ok_or_else(|| {
                    AppError::Platform(
                        "image payload bytes were not loaded before clipboard write".to_owned(),
                    )
                })?;
                return self.write_image_bytes(bytes).await;
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = image;
                return Err(unsupported_off_target());
            }
        }
        if let ClipboardContent::FileList(files) = &entry.content {
            #[cfg(target_os = "linux")]
            {
                return self.write_files(files.paths.clone()).await;
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = files;
                return Err(unsupported_off_target());
            }
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

    async fn write_representations(
        &self,
        entry: &ClipboardEntry,
        representations: &[StoredClipboardRepresentation],
    ) -> Result<()> {
        // Pre-scan so an entry whose stored reps are all outside the
        // Wayland publisher's mapping table falls back through
        // `write_entry` instead of issuing a `copy_multi` that registers
        // an offer for no MIME the daemon actually publishes. The check
        // matches the macOS adapter's contract: only when we have at
        // least one publishable rep do we go down the multi-rep path.
        if representations.is_empty() || !has_publishable_representation(representations) {
            return self.write_entry(entry).await;
        }
        #[cfg(target_os = "linux")]
        {
            return self.publish_representations(representations.to_vec()).await;
        }
        #[cfg(not(target_os = "linux"))]
        {
            Err(unsupported_off_target())
        }
    }
}

#[cfg(target_os = "linux")]
impl LinuxClipboard {
    async fn publish_representations(
        &self,
        representations: Vec<StoredClipboardRepresentation>,
    ) -> Result<()> {
        // Map stored reps to `MimeSource` ahead of the blocking hop so a
        // bad path (e.g. relative entry in a file-list rep) surfaces as
        // an error before we spawn a worker. `copy_multi` advertises
        // every offered MIME atomically with the compositor, so a paste
        // target that wants `text/html` still sees it alongside the
        // `text/plain` fallback — matching the macOS `write_representations`
        // contract on Wayland for the first time.
        let sources = build_mime_sources(&representations)?;
        if sources.is_empty() {
            // Pre-scan in `write_representations` rules this out in
            // normal use; the only way to land here is if `build_mime_sources`
            // dropped every rep (e.g. an image rep whose bytes were empty).
            // Surface it so the daemon's `copy_entry_with_format` propagates
            // the failure instead of silently leaving the clipboard empty.
            return Err(AppError::Platform(
                "no representable bytes for Wayland multi-rep publish".to_owned(),
            ));
        }
        tokio::task::spawn_blocking(move || -> Result<()> {
            copy::copy_multi(Options::new(), sources)
                .map_err(|err| AppError::Platform(format!("wl-clipboard copy_multi failed: {err}")))
        })
        .await
        .map_err(|err| AppError::Platform(err.to_string()))?
    }

    async fn write_files(&self, paths: Vec<String>) -> Result<()> {
        // Wayland publishes file lists as `text/uri-list` (RFC 2483):
        // each line is a fully-qualified URI separated by CRLF. We refuse
        // empty lists up-front so a "copy-back" of a zero-path entry does
        // not blank the selection with an empty offer that downstream
        // readers would surface as "empty file list".
        if paths.is_empty() {
            return Err(AppError::Unsupported(
                "file-list clipboard entry has no paths".to_owned(),
            ));
        }
        let body = serialize_uri_list(&paths)?;
        let bytes = body.into_bytes().into_boxed_slice();
        tokio::task::spawn_blocking(move || -> Result<()> {
            copy::copy(
                Options::new(),
                Source::Bytes(bytes),
                CopyMimeType::Specific("text/uri-list".to_owned()),
            )
            .map_err(|err| AppError::Platform(format!("wl-clipboard file-list copy failed: {err}")))
        })
        .await
        .map_err(|err| AppError::Platform(err.to_string()))?
    }

    async fn write_image_bytes(&self, bytes: Vec<u8>) -> Result<()> {
        // Detect the MIME from the byte magic before handing the buffer
        // to `wl-clipboard-rs`. We cannot use `CopyMimeType::Autodetect`
        // — that codepath shells out to `xdg-mime` which is not always
        // installed on minimal Wayland sessions. Doing the probe here
        // also lets us refuse formats the storage pipeline never
        // produces (e.g. ICO), so we get a clear error rather than a
        // silent mismatch on copy-back.
        let mime = guess_image_mime(&bytes)?;
        let boxed = bytes.into_boxed_slice();
        tokio::task::spawn_blocking(move || -> Result<()> {
            copy::copy(
                Options::new(),
                Source::Bytes(boxed),
                CopyMimeType::Specific(mime.to_owned()),
            )
            .map_err(|err| AppError::Platform(format!("wl-clipboard image copy failed: {err}")))
        })
        .await
        .map_err(|err| AppError::Platform(err.to_string()))?
    }
}

/// True when at least one rep has a known Wayland MIME mapping.
///
/// Pre-scan used by `write_representations` so an entry whose stored
/// reps are all outside the publisher's table (e.g. only `application/json`
/// without a plain fallback) falls back to `write_entry` instead of
/// issuing a `copy_multi` for nothing. The body inspects only `nagori-core`
/// types so it stays target-independent — the workspace builds every
/// platform crate on every host and this helper has to resolve on
/// non-Linux targets too.
fn has_publishable_representation(reps: &[StoredClipboardRepresentation]) -> bool {
    reps.iter()
        .any(|rep| match (rep.mime_type.as_str(), &rep.data) {
            (
                "text/plain" | "text/html" | "application/rtf",
                RepresentationDataRef::InlineText(_),
            )
            | (
                "image/png" | "image/jpeg" | "image/gif" | "image/webp" | "image/tiff",
                RepresentationDataRef::DatabaseBlob(_),
            ) => true,
            ("text/uri-list", RepresentationDataRef::FilePaths(paths)) => !paths.is_empty(),
            _ => false,
        })
}

/// Map stored representations into a `MimeSource` batch for
/// `copy::copy_multi`.
///
/// `text/uri-list` reps are re-serialised through `serialize_uri_list`
/// so the on-wire payload matches what fresh `write_files` calls would
/// produce; an absolute-path rejection propagates as `AppError::Unsupported`
/// rather than silently dropping the file list. Unsupported (mime, payload)
/// combinations are dropped silently — the pre-scan above guarantees at
/// least one mapping exists before we get here.
#[cfg(target_os = "linux")]
fn build_mime_sources(reps: &[StoredClipboardRepresentation]) -> Result<Vec<MimeSource>> {
    let mut out = Vec::new();
    for rep in reps {
        match (rep.mime_type.as_str(), &rep.data) {
            (
                "text/plain" | "text/html" | "application/rtf",
                RepresentationDataRef::InlineText(text),
            ) => {
                out.push(MimeSource {
                    source: Source::Bytes(text.as_bytes().to_vec().into_boxed_slice()),
                    mime_type: CopyMimeType::Specific(rep.mime_type.clone()),
                });
            }
            (
                mime @ ("image/png" | "image/jpeg" | "image/gif" | "image/webp" | "image/tiff"),
                RepresentationDataRef::DatabaseBlob(bytes),
            ) => {
                if bytes.is_empty() {
                    continue;
                }
                out.push(MimeSource {
                    source: Source::Bytes(bytes.clone().into_boxed_slice()),
                    mime_type: CopyMimeType::Specific(mime.to_owned()),
                });
            }
            ("text/uri-list", RepresentationDataRef::FilePaths(paths)) if !paths.is_empty() => {
                let body = serialize_uri_list(paths)?;
                out.push(MimeSource {
                    source: Source::Bytes(body.into_bytes().into_boxed_slice()),
                    mime_type: CopyMimeType::Specific("text/uri-list".to_owned()),
                });
            }
            _ => {}
        }
    }
    Ok(out)
}

#[cfg(target_os = "linux")]
fn guess_image_mime(bytes: &[u8]) -> Result<&'static str> {
    let format = image::guess_format(bytes)
        .map_err(|err| AppError::Platform(format!("image format detection failed: {err}")))?;
    match format {
        image::ImageFormat::Png => Ok("image/png"),
        image::ImageFormat::Jpeg => Ok("image/jpeg"),
        image::ImageFormat::Gif => Ok("image/gif"),
        image::ImageFormat::WebP => Ok("image/webp"),
        image::ImageFormat::Tiff => Ok("image/tiff"),
        // BMP (and friends) are not in the factory's image allowlist, so
        // copy-back would publish bytes the daemon could never re-capture
        // cleanly. Refuse instead of silently mismatching.
        other => Err(AppError::Unsupported(format!(
            "image format {other:?} is not supported for Wayland copy-back"
        ))),
    }
}

/// Result of one multi-MIME pass over the Wayland clipboard.
///
/// `representations` is `Some(reps)` when the total payload fits within
/// the buffer cap (or there was no cap). When the total exceeds the
/// cap or the hard read ceiling, we drop the buffered bytes and return
/// `None` — the caller surfaces this as a `CapturedSnapshot::Oversized`
/// without leaking attacker-controlled allocations into the snapshot.
///
/// `sequence` is the hex SHA-256 of the concatenated rep bodies (in
/// the canonical priority order — image → uri-list → text). When the
/// read ceiling is crossed mid-stream we instead emit
/// `oversized-over:<ceiling>:<prefix-hash>` so two distinct oversized
/// clips with different prefixes still produce different sequences.
#[cfg(target_os = "linux")]
struct MultiPipePass {
    representations: Option<Vec<ClipboardRepresentation>>,
    observed_total: usize,
    sequence: String,
}

#[cfg(target_os = "linux")]
const PIPE_CHUNK: usize = 8 * 1024;

#[cfg(target_os = "linux")]
async fn pipe_read_multi_pass(buffer_cap: Option<usize>) -> Result<MultiPipePass> {
    // When the caller asks for buffering, the buffer cap also doubles as
    // the read ceiling — there is no benefit to streaming past the cap
    // since we cannot surface those bytes anyway, and reading them only
    // gives a malicious publisher more time to occupy the blocking worker.
    let read_ceiling = buffer_cap.unwrap_or(INTERNAL_BODY_CEILING_BYTES);
    pipe_read_multi_pass_internal(buffer_cap, read_ceiling).await
}

#[cfg(target_os = "linux")]
async fn pipe_read_multi_pass_no_buffer(read_ceiling: usize) -> Result<MultiPipePass> {
    pipe_read_multi_pass_internal(None, read_ceiling).await
}

#[cfg(target_os = "linux")]
async fn pipe_read_multi_pass_internal(
    buffer_cap: Option<usize>,
    read_ceiling: usize,
) -> Result<MultiPipePass> {
    tokio::task::spawn_blocking(move || -> Result<MultiPipePass> {
        let available = match paste::get_mime_types(ClipboardType::Regular, Seat::Unspecified) {
            Ok(set) => set,
            // Empty selection / no seats → treat as empty so the
            // capture loop's body-empty short-circuit kicks in without
            // logging an error every poll.
            Err(paste::Error::ClipboardEmpty | paste::Error::NoSeats) => {
                return Ok(MultiPipePass {
                    representations: buffer_cap.map(|_| Vec::new()),
                    observed_total: 0,
                    sequence: hex::encode(Sha256::new().finalize()),
                });
            }
            Err(err) => {
                return Err(AppError::Platform(format!(
                    "wl-clipboard mime enumeration failed: {err}"
                )));
            }
        };

        let mut state = MultiReadState::new(buffer_cap, read_ceiling);
        let mut representations: Vec<ClipboardRepresentation> = Vec::new();

        if let Some(image_mime) = pick_image_mime(&available)
            && !state.aborted()
            && let Some(body) = read_specific_mime(&image_mime, &mut state)?
        {
            representations.push(ClipboardRepresentation {
                mime_type: image_mime,
                data: ClipboardData::Bytes(body),
            });
        }

        if available.contains("text/uri-list")
            && !state.aborted()
            && let Some(body) = read_specific_mime("text/uri-list", &mut state)?
            && let Some(paths) = parse_uri_list(&body)
        {
            representations.push(ClipboardRepresentation {
                mime_type: "text/uri-list".to_owned(),
                data: ClipboardData::FilePaths(paths),
            });
        }

        if available
            .iter()
            .any(|m| TEXT_MIME_HINTS.contains(&m.as_str()))
            && !state.aborted()
            && let Some(body) = read_text(&mut state)?
        {
            let text = String::from_utf8(body).unwrap_or_default();
            if !text.is_empty() {
                representations.push(ClipboardRepresentation {
                    mime_type: "text/plain".to_owned(),
                    data: ClipboardData::Text(text),
                });
            }
        }

        let observed_total = state.observed_total;
        let dropped = state.buffer_overflow || state.ceiling_hit;
        let sequence = state.finalize_sequence();

        Ok(MultiPipePass {
            representations: if dropped { None } else { Some(representations) },
            observed_total,
            sequence,
        })
    })
    .await
    .map_err(|err| AppError::Platform(err.to_string()))?
}

#[cfg(target_os = "linux")]
fn pick_image_mime(available: &HashSet<String>) -> Option<String> {
    IMAGE_MIME_PRIORITY
        .iter()
        .find(|&&mime| available.contains(mime))
        .map(|&mime| mime.to_owned())
}

#[cfg(target_os = "linux")]
fn read_specific_mime(mime: &str, state: &mut MultiReadState) -> Result<Option<Vec<u8>>> {
    match paste::get_contents(
        ClipboardType::Regular,
        Seat::Unspecified,
        PasteMimeType::Specific(mime),
    ) {
        Ok((mut pipe, _mime)) => {
            state.begin_rep(mime);
            state.read_pipe(&mut pipe)
        }
        // `NoMimeType` races with a publisher that retracted between the
        // initial enumeration and the specific request — treat as absent.
        Err(paste::Error::ClipboardEmpty | paste::Error::NoSeats | paste::Error::NoMimeType) => {
            Ok(None)
        }
        Err(err) => Err(AppError::Platform(format!(
            "wl-clipboard paste {mime} failed: {err}"
        ))),
    }
}

#[cfg(target_os = "linux")]
fn read_text(state: &mut MultiReadState) -> Result<Option<Vec<u8>>> {
    // `MimeType::Text` cycles through the documented text MIME variants
    // so we do not have to second-guess which one a given source app
    // chose. If none match the offer (rare but possible: STRING-only X11
    // bridge), `NoMimeType` surfaces and we return None silently.
    match paste::get_contents(
        ClipboardType::Regular,
        Seat::Unspecified,
        PasteMimeType::Text,
    ) {
        Ok((mut pipe, _mime)) => {
            state.begin_rep("text/plain");
            state.read_pipe(&mut pipe)
        }
        Err(paste::Error::ClipboardEmpty | paste::Error::NoSeats | paste::Error::NoMimeType) => {
            Ok(None)
        }
        Err(err) => Err(AppError::Platform(format!(
            "wl-clipboard paste text failed: {err}"
        ))),
    }
}

/// Serialise filesystem paths into a `text/uri-list` payload.
///
/// Each path is converted to a `file://` URL via `url::Url::from_file_path`,
/// which percent-encodes path segments (so spaces become `%20`, etc.) and
/// rejects relative paths. RFC 2483 specifies CRLF as the line separator;
/// we follow it so receivers that parse strictly (Nautilus, Dolphin) accept
/// the offer. A trailing CRLF terminates the last entry — also per RFC.
#[cfg(target_os = "linux")]
fn serialize_uri_list(paths: &[String]) -> Result<String> {
    let mut out = String::new();
    for path in paths {
        let url = url::Url::from_file_path(path).map_err(|()| {
            AppError::Unsupported(format!(
                "cannot publish {path:?} as a Wayland file-list entry: path must be absolute",
            ))
        })?;
        out.push_str(url.as_str());
        out.push_str("\r\n");
    }
    Ok(out)
}

/// Parse a `text/uri-list` payload into raw filesystem paths.
///
/// Per RFC 2483 each line is a URI separated by CRLF; lines starting
/// with `#` are comments. We only surface `file://` URIs because the
/// rest of the pipeline models file lists as filesystem paths
/// (`ClipboardData::FilePaths`). URI decoding goes through the `url`
/// crate so percent-escaped paths (`file:///tmp/with%20space`) round-
/// trip correctly into the user-visible path.
#[cfg(target_os = "linux")]
fn parse_uri_list(bytes: &[u8]) -> Option<Vec<String>> {
    let text = std::str::from_utf8(bytes).ok()?;
    let mut paths = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let Ok(parsed) = url::Url::parse(trimmed) else {
            continue;
        };
        if parsed.scheme() != "file" {
            continue;
        }
        let Ok(path) = parsed.to_file_path() else {
            continue;
        };
        if let Some(s) = path.to_str() {
            paths.push(s.to_owned());
        }
    }
    if paths.is_empty() { None } else { Some(paths) }
}

#[cfg(target_os = "linux")]
struct MultiReadState {
    hasher: Sha256,
    observed_total: usize,
    buffer_cap: Option<usize>,
    read_ceiling: usize,
    /// Sticky once total payload exceeds `buffer_cap`; subsequent rep
    /// reads still hash bytes (so the sequence is content-stable) but
    /// drop the buffered Vec.
    buffer_overflow: bool,
    /// Sticky once total payload exceeds `read_ceiling`; once set,
    /// further reads short-circuit so a malicious owner cannot pin the
    /// blocking worker by feeding bytes indefinitely.
    ceiling_hit: bool,
}

#[cfg(target_os = "linux")]
impl MultiReadState {
    fn new(buffer_cap: Option<usize>, read_ceiling: usize) -> Self {
        Self {
            hasher: Sha256::new(),
            observed_total: 0,
            buffer_cap,
            read_ceiling,
            buffer_overflow: false,
            ceiling_hit: false,
        }
    }

    const fn aborted(&self) -> bool {
        self.ceiling_hit
    }

    /// Mix a rep boundary header (`b"\0<mime>\0"`) into the hasher.
    ///
    /// Without a boundary the multi-rep sequence is ambiguous: two
    /// different layouts whose concatenated bodies happen to coincide
    /// would hash the same and the capture loop would skip the change.
    /// A short framing prefix is enough to make the hash a function of
    /// the rep layout, not just the byte stream. We only count the
    /// hashed-but-unbuffered framing bytes against the read ceiling
    /// (not against the soft `buffer_cap`) — those bytes never end up
    /// in a stored representation so they should not push the snapshot
    /// over the user's `max_entry_size_bytes` budget.
    fn begin_rep(&mut self, mime: &str) {
        if self.ceiling_hit {
            return;
        }
        // NUL is forbidden in MIME types so the framing is unambiguous.
        self.hasher.update(b"\0");
        self.hasher.update(mime.as_bytes());
        self.hasher.update(b"\0");
    }

    fn read_pipe(&mut self, pipe: &mut impl Read) -> Result<Option<Vec<u8>>> {
        // If we already crossed the ceiling for an earlier rep, do not
        // open this one — the sequence is already locked to the oversized
        // sentinel and additional bytes would be wasted work.
        if self.ceiling_hit {
            return Ok(None);
        }
        // Allocate the per-rep buffer up front when the caller asked for
        // buffering AND we have not yet exceeded the cumulative cap.
        let mut buffer: Option<Vec<u8>> = if self.buffer_overflow {
            None
        } else {
            self.buffer_cap.map(|_| Vec::new())
        };
        let mut chunk = [0u8; PIPE_CHUNK];
        loop {
            let n = pipe.read(&mut chunk).map_err(|err| {
                AppError::Platform(format!("reading clipboard pipe failed: {err}"))
            })?;
            if n == 0 {
                break;
            }
            let previous = self.observed_total;
            self.observed_total = self.observed_total.saturating_add(n);

            // Ceiling check is the hard limit — hash whatever prefix
            // still fits, mark sticky, and bail.
            if self.observed_total > self.read_ceiling {
                let prefix_remaining = self.read_ceiling.saturating_sub(previous).min(n);
                if prefix_remaining > 0 {
                    self.hasher.update(&chunk[..prefix_remaining]);
                }
                self.ceiling_hit = true;
                return Ok(None);
            }
            self.hasher.update(&chunk[..n]);

            // Buffer check is the soft cap. We keep hashing past it so
            // a change to any rep bumps the sequence, but the bytes are
            // dropped from memory.
            if let Some(cap) = self.buffer_cap
                && self.observed_total > cap
            {
                self.buffer_overflow = true;
                buffer = None;
            } else if let Some(buf) = buffer.as_mut() {
                buf.extend_from_slice(&chunk[..n]);
            }
        }
        Ok(if self.buffer_overflow { None } else { buffer })
    }

    fn finalize_sequence(self) -> String {
        if self.ceiling_hit {
            oversized_sequence(self.read_ceiling, &hex::encode(self.hasher.finalize()))
        } else {
            hex::encode(self.hasher.finalize())
        }
    }
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

    use super::{
        IMAGE_MIME_PRIORITY, MultiReadState, PIPE_CHUNK, oversized_sequence, parse_uri_list,
        pick_image_mime, serialize_uri_list,
    };

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
    fn read_pipe_closes_at_configured_ceiling() {
        let mut reader = CountingChunks::new(PIPE_CHUNK, 8);
        let mut state = MultiReadState::new(Some(PIPE_CHUNK), PIPE_CHUNK);
        let body = state.read_pipe(&mut reader).unwrap();

        assert_eq!(reader.reads, 2);
        assert!(body.is_none());
        assert!(state.aborted());
        assert_eq!(state.observed_total, PIPE_CHUNK * 2);

        let expected_prefix = hex::encode(Sha256::digest([b'x'; PIPE_CHUNK]));
        assert_eq!(
            state.finalize_sequence(),
            oversized_sequence(PIPE_CHUNK, &expected_prefix)
        );
    }

    #[test]
    fn read_pipe_buffers_within_ceiling() {
        let mut reader = io::Cursor::new(b"clipboard".to_vec());
        let mut state = MultiReadState::new(Some(64), 64);
        let body = state.read_pipe(&mut reader).unwrap();

        assert_eq!(body.as_deref(), Some(&b"clipboard"[..]));
        assert_eq!(state.observed_total, b"clipboard".len());
        assert!(!state.aborted());
    }

    #[test]
    fn read_pipe_uses_prefix_hash_for_oversized_sequence() {
        let mut first_reader = io::Cursor::new([b'a'; PIPE_CHUNK + 1]);
        let mut second_reader = io::Cursor::new([b'b'; PIPE_CHUNK + 1]);

        let mut s1 = MultiReadState::new(Some(PIPE_CHUNK), PIPE_CHUNK);
        let _ = s1.read_pipe(&mut first_reader).unwrap();
        let mut s2 = MultiReadState::new(Some(PIPE_CHUNK), PIPE_CHUNK);
        let _ = s2.read_pipe(&mut second_reader).unwrap();

        assert_ne!(s1.finalize_sequence(), s2.finalize_sequence());
    }

    #[test]
    fn read_pipe_keeps_hashing_after_buffer_overflow() {
        // Total observed exceeds the soft buffer cap but stays under the
        // hard ceiling. The buffered Vec should drop yet the hasher must
        // continue so a downstream rep change still bumps the sequence.
        let mut first = io::Cursor::new([b'a'; PIPE_CHUNK + 1]);
        let mut second = io::Cursor::new([b'b'; PIPE_CHUNK + 1]);

        let mut state = MultiReadState::new(Some(PIPE_CHUNK), PIPE_CHUNK * 8);
        let first_body = state.read_pipe(&mut first).unwrap();
        // First rep exceeds the cap → its buffer dropped.
        assert!(first_body.is_none());
        assert!(state.buffer_overflow);
        assert!(!state.ceiling_hit);

        // Second rep is still hashed even though buffer is sticky-off.
        let prior = state.observed_total;
        let second_body = state.read_pipe(&mut second).unwrap();
        assert!(second_body.is_none());
        assert!(state.observed_total > prior);
    }

    #[test]
    fn begin_rep_disambiguates_rep_layout() {
        // Two clips with the same total bytes but different per-rep
        // boundaries must hash differently. Without `begin_rep` the two
        // sequences would collide.
        let mut s_two = MultiReadState::new(Some(64), 64);
        s_two.begin_rep("image/png");
        let _ = s_two
            .read_pipe(&mut io::Cursor::new(b"AB".to_vec()))
            .unwrap();
        s_two.begin_rep("text/plain");
        let _ = s_two
            .read_pipe(&mut io::Cursor::new(b"CD".to_vec()))
            .unwrap();

        let mut s_one = MultiReadState::new(Some(64), 64);
        s_one.begin_rep("text/plain");
        let _ = s_one
            .read_pipe(&mut io::Cursor::new(b"ABCD".to_vec()))
            .unwrap();

        assert_ne!(s_two.finalize_sequence(), s_one.finalize_sequence());
    }

    #[test]
    fn pick_image_mime_honours_priority() {
        let mut set = std::collections::HashSet::new();
        set.insert("image/jpeg".to_owned());
        set.insert("image/png".to_owned());
        // PNG wins because it sits earlier in `IMAGE_MIME_PRIORITY`,
        // independent of HashSet iteration order.
        assert_eq!(pick_image_mime(&set), Some("image/png".to_owned()));
        // And the priority list ordering matches the macOS adapter's
        // canonical-image preference (PNG first).
        assert_eq!(IMAGE_MIME_PRIORITY.first(), Some(&"image/png"));
    }

    #[test]
    fn pick_image_mime_returns_none_when_no_image_offer() {
        let set: std::collections::HashSet<String> =
            ["text/plain".to_owned(), "text/uri-list".to_owned()]
                .into_iter()
                .collect();
        assert_eq!(pick_image_mime(&set), None);
    }

    #[test]
    fn parse_uri_list_decodes_percent_escapes() {
        let body = b"file:///tmp/nagori%20alpha\r\nfile:///tmp/nagori-beta\r\n";
        let paths = parse_uri_list(body).expect("two paths");
        assert_eq!(
            paths,
            vec![
                "/tmp/nagori alpha".to_owned(),
                "/tmp/nagori-beta".to_owned(),
            ]
        );
    }

    #[test]
    fn parse_uri_list_skips_comments_and_non_file_schemes() {
        let body = b"# selection\r\nhttps://example.test/page\r\nfile:///tmp/nagori-only\r\n\r\n";
        let paths = parse_uri_list(body).expect("only the file:// row survives");
        assert_eq!(paths, vec!["/tmp/nagori-only".to_owned()]);
    }

    #[test]
    fn parse_uri_list_returns_none_when_empty() {
        assert!(parse_uri_list(b"").is_none());
        assert!(parse_uri_list(b"# only comments\n").is_none());
        assert!(parse_uri_list(b"https://example.test/no-files\n").is_none());
    }

    #[test]
    fn serialize_uri_list_percent_encodes_and_round_trips() {
        let payload = serialize_uri_list(&[
            "/tmp/nagori alpha".to_owned(),
            "/tmp/nagori-beta".to_owned(),
        ])
        .expect("absolute paths are accepted");
        assert!(
            payload.contains("file:///tmp/nagori%20alpha"),
            "space should percent-encode: {payload}",
        );
        assert!(payload.ends_with("\r\n"), "trailing CRLF: {payload:?}");
        let parsed = parse_uri_list(payload.as_bytes()).expect("non-empty parse");
        assert_eq!(
            parsed,
            vec![
                "/tmp/nagori alpha".to_owned(),
                "/tmp/nagori-beta".to_owned(),
            ],
        );
    }

    #[test]
    fn serialize_uri_list_rejects_relative_paths() {
        // `url::Url::from_file_path` only accepts absolute paths; surface
        // that as `Unsupported` so the daemon's copy-back surfaces a clear
        // error instead of publishing a malformed `text/uri-list`.
        let err = serialize_uri_list(&["relative/path".to_owned()])
            .expect_err("relative paths should be rejected");
        assert!(
            matches!(err, nagori_core::AppError::Unsupported(_)),
            "expected Unsupported, got {err:?}",
        );
    }
}

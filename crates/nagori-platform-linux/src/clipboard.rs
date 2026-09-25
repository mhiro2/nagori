use async_trait::async_trait;
use nagori_core::{
    AppError, ClipboardContent, ClipboardEntry, ClipboardSequence, ClipboardSnapshot, ReadBudget,
    Result, StoredClipboardRepresentation,
};
#[cfg(target_os = "linux")]
use nagori_core::{Bytes, ClipboardData, ClipboardRepresentation, RepresentationDataRef};
use nagori_platform::{
    CapturedSnapshot, ClipboardReader, ClipboardWriter, has_publishable_representation,
};
#[cfg(target_os = "linux")]
use nagori_platform::{ClipboardExclusionKind, SNAPSHOT_CAPTURE_MAX_RETRIES};
#[cfg(target_os = "linux")]
use std::collections::HashSet;
#[cfg(target_os = "linux")]
use std::io::{self, Read};
#[cfg(target_os = "linux")]
use std::os::fd::{AsFd, AsRawFd};
#[cfg(target_os = "linux")]
use std::sync::{Arc, Mutex, PoisonError};
#[cfg(target_os = "linux")]
use std::time::{Duration, Instant};
#[cfg(target_os = "linux")]
use time::OffsetDateTime;
#[cfg(target_os = "linux")]
use wl_clipboard_rs::copy::{self, MimeSource, MimeType as CopyMimeType, Options, Source};

#[cfg(target_os = "linux")]
use crate::selection::{Offer, SelectionState, SelectionWatcher, WatchError};

/// Per-kind budget for the unbounded `current_snapshot` path. The capture
/// loop's authoritative size caps are the per-kind budgets in `AppSettings`,
/// which it threads through `current_snapshot_with_max`; this constant is a
/// defence-in-depth ceiling for callers that bypass the bounded entry point.
/// Applied to both kinds, so one pass buffers at most three times this (one
/// image plus two text representations, see `snapshot_read_ceiling`) — 768
/// MiB, comfortably above any realistic setting.
#[cfg(target_os = "linux")]
const INTERNAL_BODY_CEILING_BYTES: usize = 256 * 1024 * 1024;

/// Cumulative read backstop for one snapshot pass, derived from the per-kind
/// [`ReadBudget`].
///
/// A Wayland clip offers at most three capturable representations: one image,
/// one `text/uri-list`, and one plain text. Each is gated individually against
/// its own kind budget while reading (see `MultiReadState::read_pipe`), so the
/// most an all-within-budget clip can total is one image budget plus two text
/// budgets. Using that sum as the cumulative ceiling keeps the buffered memory
/// bounded without ever rejecting a clip whose every representation is within
/// its own budget — the per-kind sums are then enforced authoritatively by the
/// capture loop's `admit` / `trim_alternatives_to_budget`.
#[cfg(target_os = "linux")]
const fn snapshot_read_ceiling(budget: ReadBudget) -> usize {
    budget
        .image_bytes
        .saturating_add(budget.text_bytes.saturating_mul(2))
}

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

/// Plain-text MIME types in the order we request them: the UTF-8 forms
/// first, then the legacy X11 atoms an Xwayland bridge offers. A clip that
/// offers none of these but some other `text/plain` variant (a different
/// charset spelling) still matches through `pick_text_mime`'s fallback;
/// markup such as `text/html` never stands in for plain text.
#[cfg(target_os = "linux")]
const TEXT_MIME_HINTS: &[&str] = &[
    "text/plain;charset=utf-8",
    "UTF8_STRING",
    "text/plain",
    "STRING",
    "TEXT",
];

/// KDE's owner-declared "do not record this in history" offer — the
/// cross-platform analogue of the macOS nspasteboard.org markers and the
/// Windows `Clipboard Viewer Ignore` format. Password managers (`KeePassXC`,
/// `KWallet`, …) advertise this MIME on the selection when copying a credential
/// so cooperating clipboard managers skip it.
///
/// We treat the offer's *presence* as the contract and never read its body:
/// the value is conventionally the literal `secret`, but reading it would mean
/// pulling owner-declared bytes into our address space for no benefit — the
/// marker exists precisely so a manager can skip the clip sight-unseen, exactly
/// as the macOS adapter only presence-tests `availableTypeFromArray`. Surfaces
/// as [`ClipboardExclusionKind::Concealed`] (there is no transient analogue).
#[cfg(target_os = "linux")]
const KDE_PASSWORD_MANAGER_HINT_MIME: &str = "x-kde-passwordManagerHint";

/// Minimum spacing between attempts to replace a stopped selection watcher,
/// so a compositor that keeps refusing connections is not hammered every
/// capture tick.
#[cfg(target_os = "linux")]
const WATCHER_RESTART_INTERVAL: Duration = Duration::from_secs(5);

/// Linux (Wayland) clipboard adapter.
///
/// Talks directly to the Wayland `ext_data_control_v1` /
/// `wlr_data_control_v1` protocols so the daemon does not have to run as a
/// graphical window client. There is **no X11 fallback** — that is the whole
/// point of speaking data-control instead of using arboard, which would
/// silently degrade to X11 when the Wayland feature is missing or
/// initialisation fails. If the compositor does not expose either
/// data-control protocol the adapter refuses to start, surfacing the protocol
/// name in the error so the operator can react. GNOME currently ships neither
/// protocol unconditionally; the supported set is wlroots-based compositors
/// and KDE Plasma 5.27+.
///
/// Change detection is event-driven: a [`SelectionWatcher`] keeps a
/// connection open and numbers every `selection` event, and that generation
/// is the clipboard sequence. Reads go through the offer the generation
/// names; writes use `wl-clipboard-rs`.
pub struct LinuxClipboard {
    #[cfg(target_os = "linux")]
    watcher: Mutex<WatcherSlot>,
}

/// The live selection watcher plus what is needed to replace it if its
/// connection drops (compositor restart, protocol error).
#[cfg(target_os = "linux")]
struct WatcherSlot {
    watcher: Arc<SelectionWatcher>,
    last_restart: Option<Instant>,
}

impl LinuxClipboard {
    #[cfg(target_os = "linux")]
    pub fn new() -> Result<Self> {
        // Start the selection watcher eagerly so a missing data-control
        // manager surfaces at construction rather than on the first capture
        // poll. An empty clipboard or a seat-less session is not an error —
        // the watcher simply reports no offer. We do **not** pre-check
        // `WAYLAND_DISPLAY`; `wayland-client` reports a connection error when
        // no compositor is reachable, and that is the authoritative signal.
        // `WAYLAND_SOCKET` is not supported here because `wayland-client`
        // consumes the inherited fd on first connect — the watcher would take
        // it and leave the clipboard writes, which open their own
        // connections, with nothing to connect to.
        let watcher = SelectionWatcher::spawn(0).map_err(|err| watch_error(&err))?;
        Ok(Self {
            watcher: Mutex::new(WatcherSlot {
                watcher: Arc::new(watcher),
                last_restart: None,
            }),
        })
    }

    #[cfg(not(target_os = "linux"))]
    pub fn new() -> Result<Self> {
        Err(AppError::Unsupported(
            "LinuxClipboard is only available on Linux targets".to_owned(),
        ))
    }
}

#[cfg(target_os = "linux")]
fn watch_error(err: &WatchError) -> AppError {
    match err {
        WatchError::MissingProtocol => AppError::Unsupported(format!(
            "compositor does not expose the Wayland data-control protocol ({err}). \
             Nagori requires wlr-data-control or ext-data-control (Sway, KDE Plasma 5.27+, \
             Hyprland, river). GNOME Wayland does not currently expose these protocols.",
        )),
        WatchError::Connect(_) => AppError::Unsupported(format!(
            "could not connect to a Wayland compositor ({err}). Linux nagori requires a \
             live Wayland session (set WAYLAND_DISPLAY); X11 is not supported.",
        )),
        WatchError::Communication(_) => {
            AppError::Platform(format!("could not bind Wayland clipboard: {err}"))
        }
    }
}

#[cfg(target_os = "linux")]
impl LinuxClipboard {
    /// The live selection watcher, replacing a stopped one when the restart
    /// interval allows.
    ///
    /// While the watcher is down the clipboard cannot be observed, so this
    /// errors (the capture loop backs off and retries) instead of reporting
    /// a stale generation. A replacement continues the generation count past
    /// the old watcher's last value, so its first selection event — the clip
    /// on the clipboard right now — reads as a change and is re-examined.
    async fn watcher(&self) -> Result<Arc<SelectionWatcher>> {
        let start_generation = {
            let mut slot = self.watcher.lock().unwrap_or_else(PoisonError::into_inner);
            if !slot.watcher.is_stopped() {
                return Ok(Arc::clone(&slot.watcher));
            }
            let now = Instant::now();
            if slot
                .last_restart
                .is_some_and(|at| now.duration_since(at) < WATCHER_RESTART_INTERVAL)
            {
                return Err(AppError::Platform(
                    "Wayland clipboard watcher stopped; waiting to reconnect".to_owned(),
                ));
            }
            slot.last_restart = Some(now);
            // `generation()` reports the failure now, but the last value the
            // stopped watcher issued is still readable; continue past it.
            slot.watcher.last_generation().wrapping_add(1)
        };
        let watcher =
            tokio::task::spawn_blocking(move || SelectionWatcher::spawn(start_generation))
                .await
                .map_err(|err| AppError::Platform(err.to_string()))?
                .map_err(|err| watch_error(&err))?;
        tracing::info!("clipboard_selection_watcher_restarted");
        let watcher = Arc::new(watcher);
        let mut slot = self.watcher.lock().unwrap_or_else(PoisonError::into_inner);
        slot.watcher = Arc::clone(&watcher);
        Ok(watcher)
    }
}

#[async_trait]
impl ClipboardReader for LinuxClipboard {
    async fn current_snapshot(&self) -> Result<ClipboardSnapshot> {
        #[cfg(target_os = "linux")]
        {
            let pass = read_selection(
                self.watcher().await?,
                ReadBudget::new(INTERNAL_BODY_CEILING_BYTES, INTERNAL_BODY_CEILING_BYTES),
            )
            .await?;
            // The unbounded path returns a plain snapshot, so an
            // owner-excluded clip yields an empty snapshot — its body was
            // never read (mirroring the macOS adapter).
            let representations = match pass.outcome {
                PassOutcome::Read(representations) => representations,
                PassOutcome::Excluded(_) | PassOutcome::Oversized { .. } => Vec::new(),
            };
            Ok(ClipboardSnapshot {
                sequence: pass.sequence,
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
        // The generation of the last selection event. Reading it never asks
        // the clipboard owner for data, so polling an unchanged clipboard
        // costs nothing and cannot consume a "paste once" offer.
        #[cfg(target_os = "linux")]
        {
            let generation = self
                .watcher()
                .await?
                .generation()
                .map_err(|message| watcher_stopped(&message))?;
            Ok(generation_sequence(generation))
        }
        #[cfg(not(target_os = "linux"))]
        {
            Err(unsupported_off_target())
        }
    }

    #[cfg_attr(not(target_os = "linux"), allow(unused_variables))]
    async fn current_snapshot_with_max(&self, budget: ReadBudget) -> Result<CapturedSnapshot> {
        // The capture loop's hot path. Each representation is gated against its
        // own kind budget while streaming, so a multi-megabyte screenshot is
        // captured under the image budget while a runaway text/file payload
        // still answers to the text budget. A representation that crosses its
        // budget closes the read end and returns an Oversized variant instead
        // of draining the owner-controlled pipe to EOF.
        #[cfg(target_os = "linux")]
        {
            let pass = read_selection(self.watcher().await?, budget).await?;
            let sequence = pass.sequence;
            Ok(match pass.outcome {
                // Owner exclusion is decided from the offer's types before
                // any body read, so it can be neither `Captured` nor
                // `Oversized`.
                PassOutcome::Excluded(kind) => CapturedSnapshot::Excluded { sequence, kind },
                PassOutcome::Read(representations) => {
                    CapturedSnapshot::Captured(ClipboardSnapshot {
                        sequence,
                        captured_at: OffsetDateTime::now_utc(),
                        source: None,
                        representations,
                    })
                }
                PassOutcome::Oversized {
                    observed_bytes,
                    limit,
                } => CapturedSnapshot::Oversized {
                    sequence,
                    observed_bytes,
                    // The budget of the kind whose representation tripped the
                    // ceiling (or the cumulative backstop); falls back to the
                    // larger budget when a read timeout aborted the pass.
                    limit: if limit > 0 { limit } else { budget.max() },
                },
            })
        }
        #[cfg(not(target_os = "linux"))]
        {
            Err(unsupported_off_target())
        }
    }
}

#[cfg(target_os = "linux")]
fn generation_sequence(generation: u64) -> ClipboardSequence {
    // `u64` generations start at 0 and advance once per selection event; the
    // conversion cannot saturate within any realistic session.
    ClipboardSequence::native(i64::try_from(generation).unwrap_or(i64::MAX))
}

#[cfg(target_os = "linux")]
fn watcher_stopped(message: &str) -> AppError {
    AppError::Platform(format!("Wayland clipboard watcher stopped: {message}"))
}

/// Run a *side-effecting* `wl-clipboard` write (`copy::copy` /
/// `copy::copy_multi`) on the blocking pool, awaited to completion —
/// deliberately **without** a timeout.
///
/// `copy::copy` returns once the offer is registered with the compositor;
/// under a healthy Wayland session that is near-instant. A timeout here would
/// be unsafe, though: `spawn_blocking` tasks cannot be aborted, so a timed-out
/// write would not stop — the detached worker keeps running and still
/// registers the offer once the compositor unwedges, overwriting whatever the
/// user copied in the meantime and silently clobbering newer (and possibly
/// sensitive) clipboard content. We therefore await the write to completion,
/// so the caller either learns the selection truly holds the intended content
/// or blocks until a wedged compositor recovers. This mirrors the
/// synthetic-paste contract in `nagori_platform::run_blocking_with_timeout`,
/// which awaits `Ctrl+V` synthesis without a timeout for the same reason.
///
/// Reads keep their bound: the read side uses `PIPE_READ_TIMEOUT` + `poll(2)`
/// and closes the pipe on timeout, which is a *real* cancellation (the pipe
/// read genuinely stops), so a late result is impossible there.
#[cfg(target_os = "linux")]
async fn run_clipboard_write<F>(op: &'static str, f: F) -> Result<()>
where
    F: FnOnce() -> Result<()> + Send + 'static,
{
    match tokio::task::spawn_blocking(f).await {
        Ok(inner) => inner,
        Err(join_err) => Err(AppError::Platform(format!("{op} task failed: {join_err}"))),
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
            run_clipboard_write("write_text", move || -> Result<()> {
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

    async fn write_representation_exact(
        &self,
        representation: &StoredClipboardRepresentation,
    ) -> Result<()> {
        // Strict single-representation paste: refuse a MIME this adapter
        // cannot publish rather than falling back to the primary the way
        // `write_representations` does, so the user always gets the format
        // they picked or a clear error. `publish_representations` maps the
        // rep to a `MimeSource` before touching the selection, so an
        // unrepresentable rep errors without clearing the clipboard.
        if !has_publishable_representation(std::slice::from_ref(representation)) {
            return Err(AppError::Unsupported(
                "representation cannot be published to the Wayland clipboard".to_owned(),
            ));
        }
        #[cfg(target_os = "linux")]
        {
            return self
                .publish_representations(vec![representation.clone()])
                .await;
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
        run_clipboard_write("publish_representations", move || -> Result<()> {
            copy::copy_multi(Options::new(), sources)
                .map_err(|err| AppError::Platform(format!("wl-clipboard copy_multi failed: {err}")))
        })
        .await
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
        run_clipboard_write("write_files", move || -> Result<()> {
            copy::copy(
                Options::new(),
                Source::Bytes(bytes),
                CopyMimeType::Specific("text/uri-list".to_owned()),
            )
            .map_err(|err| AppError::Platform(format!("wl-clipboard file-list copy failed: {err}")))
        })
        .await
    }

    async fn write_image_bytes(&self, bytes: Bytes) -> Result<()> {
        // Detect the MIME from the byte magic before handing the buffer
        // to `wl-clipboard-rs`. We cannot use `CopyMimeType::Autodetect`
        // — that codepath shells out to `xdg-mime` which is not always
        // installed on minimal Wayland sessions. Doing the probe here
        // also lets us refuse formats the storage pipeline never
        // produces (e.g. ICO), so we get a clear error rather than a
        // silent mismatch on copy-back.
        let mime = guess_image_mime(&bytes)?;
        // `Source::Bytes` needs an owned `Box<[u8]>`; the entry still holds
        // its `Bytes`, so this is the one copy the write has to make.
        let boxed: Box<[u8]> = Box::from(&bytes[..]);
        run_clipboard_write("write_image_bytes", move || -> Result<()> {
            copy::copy(
                Options::new(),
                Source::Bytes(boxed),
                CopyMimeType::Specific(mime.to_owned()),
            )
            .map_err(|err| AppError::Platform(format!("wl-clipboard image copy failed: {err}")))
        })
        .await
    }
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
                    source: Source::Bytes(Box::from(&bytes[..])),
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

/// Result of reading the selection once: the generation it belongs to and
/// what the read produced.
#[cfg(target_os = "linux")]
struct SelectionPass {
    sequence: ClipboardSequence,
    outcome: PassOutcome,
}

#[cfg(target_os = "linux")]
enum PassOutcome {
    /// Every capturable representation, in the canonical order image →
    /// uri-list → text. Empty for an empty clipboard.
    Read(Vec<ClipboardRepresentation>),
    /// The offer carried an owner-declared exclusion marker; no body was
    /// requested.
    Excluded(ClipboardExclusionKind),
    /// A representation crossed its kind budget (or the cumulative backstop),
    /// or the owner stopped writing before the read deadline. `limit` is the
    /// budget that tripped, `0` when it was not a budget.
    Oversized { observed_bytes: usize, limit: usize },
}

#[cfg(target_os = "linux")]
const PIPE_CHUNK: usize = 8 * 1024;

/// Upper bound on a single MIME's pipe-read time. A healthy publisher
/// streams the body in milliseconds; a hung one (compositor stuck mid-
/// transfer, source app frozen) would otherwise wedge the blocking
/// worker until the pipe is closed by the kernel — which can take
/// indefinitely long if the writer never drops its end. We cap at 3s
/// so a single misbehaving capture costs at most one blocking worker
/// for that interval, then we drop the snapshot and emit a warn.
#[cfg(target_os = "linux")]
const PIPE_READ_TIMEOUT: Duration = Duration::from_secs(3);

/// Elapsed time after which a superseded snapshot read is not retried. A
/// single attempt is bounded per MIME by `PIPE_READ_TIMEOUT` (so up to three
/// times that across image, uri-list and text); this caps how many further
/// attempts start, so an owner that streams each MIME just under its deadline
/// while the selection keeps changing cannot stack
/// `SNAPSHOT_CAPTURE_MAX_RETRIES` worst-case attempts on one blocking worker.
#[cfg(target_os = "linux")]
const SUPERSEDED_RETRY_BUDGET: Duration = Duration::from_secs(3);

/// Read the current selection's bodies through the watcher.
///
/// The pass is tied to one offer, so it can never stitch representations
/// from two clips. What can happen is the offer being *superseded* mid-read:
/// the owner of the replaced offer may stop writing early, leaving truncated
/// bodies. The generation is compared after reading and a superseded result
/// is discarded and retried against the new offer, bounded by count and by
/// [`SUPERSEDED_RETRY_BUDGET`] (checked before each retry starts). When the retries run out the pass reports an
/// empty clip at the superseded generation: nothing half-read is stored, and
/// the next capture tick sees the newer generation and reads that.
#[cfg(target_os = "linux")]
async fn read_selection(
    watcher: Arc<SelectionWatcher>,
    budget: ReadBudget,
) -> Result<SelectionPass> {
    tokio::task::spawn_blocking(move || -> Result<SelectionPass> {
        let started = Instant::now();
        let mut attempt = 0;
        loop {
            attempt += 1;
            let selection = watcher
                .current()
                .map_err(|message| watcher_stopped(&message))?;
            let outcome = read_offer(&watcher, &selection, budget)?;
            let settled = watcher
                .generation()
                .map_err(|message| watcher_stopped(&message))?
                == selection.generation;
            let sequence = generation_sequence(selection.generation);
            if settled {
                return Ok(SelectionPass { sequence, outcome });
            }
            if attempt >= SNAPSHOT_CAPTURE_MAX_RETRIES
                || started.elapsed() >= SUPERSEDED_RETRY_BUDGET
            {
                tracing::warn!("clipboard_selection_superseded_during_read");
                return Ok(SelectionPass {
                    sequence,
                    outcome: PassOutcome::Read(Vec::new()),
                });
            }
        }
    })
    .await
    .map_err(|err| AppError::Platform(err.to_string()))?
}

/// Read every capturable representation of one selection offer.
#[cfg(target_os = "linux")]
fn read_offer(
    watcher: &SelectionWatcher,
    selection: &SelectionState<Offer>,
    budget: ReadBudget,
) -> Result<PassOutcome> {
    let Some((offer, available)) = &selection.offer else {
        return Ok(PassOutcome::Read(Vec::new()));
    };

    // Owner-declared exclusion marker (KDE's password-manager hint) takes
    // precedence over reading any body, mirroring the macOS adapter: a marked
    // secret is skipped before any `receive`, so its body never enters our
    // address space. An offer's types are fixed when it is announced, so the
    // marker cannot race in after this check — a clip republished with it is
    // a new offer and a new generation.
    if let Some(kind) = offer_exclusion(available) {
        return Ok(PassOutcome::Excluded(kind));
    }

    let mut state = MultiReadState::new(snapshot_read_ceiling(budget));
    let mut representations: Vec<ClipboardRepresentation> = Vec::new();

    if let Some(image_mime) = pick_image_mime(available)
        && let Some(body) = read_mime(watcher, offer, image_mime, &mut state, budget.image_bytes)?
    {
        representations.push(ClipboardRepresentation {
            mime_type: image_mime.to_owned(),
            data: ClipboardData::Bytes(body),
        });
    }

    if available.contains("text/uri-list")
        && let Some(body) = read_mime(
            watcher,
            offer,
            "text/uri-list",
            &mut state,
            budget.text_bytes,
        )?
        && let Some(paths) = parse_uri_list(&body)
    {
        representations.push(ClipboardRepresentation {
            mime_type: "text/uri-list".to_owned(),
            data: ClipboardData::FilePaths(paths),
        });
    }

    if let Some(text_mime) = pick_text_mime(available)
        && let Some(body) = read_mime(watcher, offer, text_mime, &mut state, budget.text_bytes)?
    {
        // A text MIME promised UTF-8 but a publisher can still hand us
        // malformed bytes (truncated transfers, a broken X11 bridge, a
        // mislabelled latin-1 source). Recover lossily rather than dropping
        // the whole text representation to an empty string — the image and
        // uri-list drop paths warn instead of staying silent, so keep this
        // branch symmetric.
        let text = match String::from_utf8(body) {
            Ok(text) => text,
            Err(err) => {
                let valid_up_to = err.utf8_error().valid_up_to();
                let bytes = err.into_bytes();
                tracing::warn!(
                    valid_up_to,
                    byte_len = bytes.len(),
                    "clipboard_text_invalid_utf8_lossy"
                );
                String::from_utf8_lossy(&bytes).into_owned()
            }
        };
        if !text.is_empty() {
            representations.push(ClipboardRepresentation {
                mime_type: "text/plain".to_owned(),
                data: ClipboardData::Text(text),
            });
        }
    }

    Ok(if state.aborted() {
        PassOutcome::Oversized {
            observed_bytes: state.observed_total,
            limit: state.overflow_limit,
        }
    } else {
        PassOutcome::Read(representations)
    })
}

/// Request `mime` from `offer` and stream it through `state`. `None` when the
/// pass has already been aborted or this representation aborted it.
#[cfg(target_os = "linux")]
fn read_mime(
    watcher: &SelectionWatcher,
    offer: &Offer,
    mime: &str,
    state: &mut MultiReadState,
    rep_budget: usize,
) -> Result<Option<Vec<u8>>> {
    if state.aborted() {
        return Ok(None);
    }
    let mut pipe = watcher
        .receive(offer, mime)
        .map_err(|err| AppError::Platform(format!("requesting clipboard {mime} failed: {err}")))?;
    let mut timed = TimeoutPipeReader::new(&mut pipe, PIPE_READ_TIMEOUT);
    state.read_pipe(&mut timed, rep_budget)
}

/// Detect an owner-declared exclusion marker in the offer set.
///
/// Presence of the KDE password-manager hint is the contract; the offer's body
/// is never read (see [`KDE_PASSWORD_MANAGER_HINT_MIME`]). Returns the
/// [`ClipboardExclusionKind`] to skip on, or `None` for an ordinary clip.
#[cfg(target_os = "linux")]
fn offer_exclusion(available: &HashSet<String>) -> Option<ClipboardExclusionKind> {
    available
        .contains(KDE_PASSWORD_MANAGER_HINT_MIME)
        .then_some(ClipboardExclusionKind::Concealed)
}

#[cfg(target_os = "linux")]
fn pick_image_mime(available: &HashSet<String>) -> Option<&'static str> {
    IMAGE_MIME_PRIORITY
        .iter()
        .copied()
        .find(|&mime| available.contains(mime))
}

/// The plain-text MIME to request: the first of [`TEXT_MIME_HINTS`] the
/// offer carries, else any other `text/plain` spelling (e.g.
/// `text/plain;charset=UTF-8`). Never a markup or structured type — storing
/// `text/html` or JSON as the plain-text representation would paste raw
/// markup back into plain-text targets.
#[cfg(target_os = "linux")]
fn pick_text_mime(available: &HashSet<String>) -> Option<&str> {
    TEXT_MIME_HINTS
        .iter()
        .copied()
        .find(|&mime| available.contains(mime))
        .or_else(|| {
            let mut plain: Vec<&str> = available
                .iter()
                .map(String::as_str)
                .filter(|mime| {
                    mime.split(';')
                        .next()
                        .is_some_and(|base| base.trim().eq_ignore_ascii_case("text/plain"))
                })
                .collect();
            // `HashSet` order is arbitrary; pick deterministically.
            plain.sort_unstable();
            plain.first().copied()
        })
}

/// `Read` adapter that polls the underlying pipe fd with `poll(2)` before
/// every chunk read so a hung publisher cannot pin a blocking worker
/// indefinitely. `deadline` is the absolute moment the *current* MIME
/// read must finish by — exceeding it surfaces as
/// `io::ErrorKind::TimedOut`, which `MultiReadState::read_pipe` treats as
/// a sticky abort that drops the snapshot.
#[cfg(target_os = "linux")]
struct TimeoutPipeReader<'a, P: Read + AsFd> {
    pipe: &'a mut P,
    deadline: Instant,
}

#[cfg(target_os = "linux")]
impl<'a, P: Read + AsFd> TimeoutPipeReader<'a, P> {
    fn new(pipe: &'a mut P, timeout: Duration) -> Self {
        Self {
            pipe,
            deadline: Instant::now() + timeout,
        }
    }
}

#[cfg(target_os = "linux")]
impl<P: Read + AsFd> Read for TimeoutPipeReader<'_, P> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let now = Instant::now();
        if now >= self.deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "clipboard pipe read deadline exceeded",
            ));
        }
        let remaining = self.deadline - now;
        let fd = self.pipe.as_fd().as_raw_fd();
        if !poll_fd_readable(fd, remaining)? {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "clipboard pipe read deadline exceeded",
            ));
        }
        self.pipe.read(buf)
    }
}

/// Wait up to `timeout` for `fd` to become readable.
///
/// Returns `Ok(true)` when the kernel reports either data ready
/// (`POLLIN`) or peer hang-up (`POLLHUP`) — the latter is the kernel's
/// "writer closed; the next `read()` will see EOF" signal, so the
/// caller must still proceed to `read()` rather than treat it as a
/// timeout. `Ok(false)` is the genuine deadline-elapsed case, and
/// `POLLERR` / `POLLNVAL` are surfaced as real I/O errors. `EINTR`
/// loops within the remaining budget instead of bailing out as a
/// timeout, so a stray signal does not collapse the per-read deadline.
#[cfg(target_os = "linux")]
fn poll_fd_readable(fd: std::os::fd::RawFd, timeout: Duration) -> io::Result<bool> {
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(false);
        }
        // Clamp to `i32::MAX` ms; the deadline is bounded by `PIPE_READ_TIMEOUT`
        // so this branch is a defensive guard rather than a real ceiling.
        let timeout_ms = i32::try_from(remaining.as_millis())
            .unwrap_or(i32::MAX)
            .max(0);
        let mut pfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: `pfd` is a single, fully-initialised `pollfd` whose `fd`
        // borrow lives for the duration of the call (the borrow is rooted in
        // `pipe.as_fd()` upstream). `poll` only writes back into `revents`,
        // which we read after the call.
        let rc = unsafe { libc::poll(&raw mut pfd, 1, timeout_ms) };
        if rc < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(err);
        }
        if rc == 0 {
            return Ok(false);
        }
        // `POLLERR` and `POLLNVAL` mean the descriptor is broken (peer
        // wrote into a closed pipe / fd was already closed). Surface
        // them as I/O errors so the caller drops the snapshot rather
        // than spinning on a doomed read.
        if (pfd.revents & (libc::POLLERR | libc::POLLNVAL)) != 0 {
            return Err(io::Error::other(format!(
                "clipboard pipe poll revents=0x{:x}",
                pfd.revents,
            )));
        }
        // `POLLHUP` alone (writer closed, no pending data) is the EOF
        // case — `read()` will return 0. Treat it as "readable" so the
        // caller observes the natural end of stream.
        if (pfd.revents & (libc::POLLIN | libc::POLLHUP)) != 0 {
            return Ok(true);
        }
        // Spurious wake (no relevant revents): loop and re-arm `poll`
        // within whatever time budget remains.
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
    for (index, path) in paths.iter().enumerate() {
        // Identify the offending entry by index only — never echo the path,
        // which can be sensitive ("length only, never content").
        let url = url::Url::from_file_path(path).map_err(|()| {
            AppError::Unsupported(format!(
                "cannot publish file-list entry at index {index} as a Wayland offer: \
                 path must be absolute",
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

/// Byte accounting shared by the representation reads of one snapshot pass.
#[cfg(target_os = "linux")]
struct MultiReadState {
    observed_total: usize,
    /// Cumulative ceiling across every representation of the pass (see
    /// `snapshot_read_ceiling`).
    read_ceiling: usize,
    /// Sticky once a representation crossed its kind budget, the cumulative
    /// ceiling was crossed, or a read timed out. Later representations are
    /// not requested and the pass reports `Oversized`.
    aborted: bool,
    /// Sticky once `TimeoutPipeReader` reports a deadline miss.
    read_timeout: bool,
    /// Budget of the representation (or the cumulative ceiling) that tripped
    /// `aborted`, surfaced so the `Oversized` verdict can report the limit
    /// the overflowing content kind actually breached. `0` until a budget
    /// trips (a timeout leaves it at `0`).
    overflow_limit: usize,
}

#[cfg(target_os = "linux")]
impl MultiReadState {
    const fn new(read_ceiling: usize) -> Self {
        Self {
            observed_total: 0,
            read_ceiling,
            aborted: false,
            read_timeout: false,
            overflow_limit: 0,
        }
    }

    const fn aborted(&self) -> bool {
        self.aborted
    }

    /// Stream one representation from `pipe`, gating it against `rep_budget`
    /// (the byte budget for *this* representation's content kind).
    ///
    /// A representation larger than its own budget aborts the whole snapshot
    /// (mirroring the macOS / Windows pre-read probe, which rejects a clip when
    /// a single representation exceeds its kind's budget); the capture loop's
    /// `admit` / `trim_alternatives_to_budget` then re-applies the per-kind
    /// budgets — including the per-kind *sums* — authoritatively. Returning at
    /// the first byte over budget drops the read end, so a runaway publisher
    /// cannot keep a blocking worker occupied past it.
    fn read_pipe(&mut self, pipe: &mut impl Read, rep_budget: usize) -> Result<Option<Vec<u8>>> {
        if self.aborted {
            return Ok(None);
        }
        let mut buffer = Vec::new();
        // Bytes read for *this* representation, gated against its kind budget.
        let mut rep_observed: usize = 0;
        let mut chunk = [0u8; PIPE_CHUNK];
        loop {
            let n = match pipe.read(&mut chunk) {
                Ok(n) => n,
                Err(err) if err.kind() == io::ErrorKind::TimedOut => {
                    // A publisher (or compositor) stopped writing mid
                    // transfer. Drop the snapshot; the capture loop anchors
                    // this generation and moves on with the next copy.
                    // Logging at warn lets the doctor surface the count
                    // without failing the whole poll cycle.
                    tracing::warn!(
                        observed_total = self.observed_total,
                        "clipboard_pipe_read_timeout"
                    );
                    self.read_timeout = true;
                    self.aborted = true;
                    return Ok(None);
                }
                Err(err) => {
                    return Err(AppError::Platform(format!(
                        "reading clipboard pipe failed: {err}"
                    )));
                }
            };
            if n == 0 {
                break;
            }
            self.observed_total = self.observed_total.saturating_add(n);
            rep_observed = rep_observed.saturating_add(n);

            if rep_observed > rep_budget {
                self.aborted = true;
                self.overflow_limit = rep_budget;
                return Ok(None);
            }
            // Backstop for the buffered pass as a whole.
            if self.observed_total > self.read_ceiling {
                self.aborted = true;
                self.overflow_limit = self.read_ceiling;
                return Ok(None);
            }
            buffer.extend_from_slice(&chunk[..n]);
        }
        Ok(Some(buffer))
    }
}

#[cfg(not(target_os = "linux"))]
fn unsupported_off_target() -> AppError {
    AppError::Unsupported("LinuxClipboard is only available on Linux targets".to_owned())
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use std::collections::HashSet;
    use std::io::{self, Read};

    use nagori_platform::ClipboardExclusionKind;

    use super::{
        IMAGE_MIME_PRIORITY, KDE_PASSWORD_MANAGER_HINT_MIME, MultiReadState, PIPE_CHUNK,
        TimeoutPipeReader, offer_exclusion, parse_uri_list, pick_image_mime, pick_text_mime,
        serialize_uri_list,
    };

    /// `Read` impl that always returns `TimedOut` — lets us exercise
    /// `MultiReadState::read_pipe`'s timeout branch without spinning up
    /// a real pipe and waiting on the deadline.
    struct AlwaysTimesOut;

    impl Read for AlwaysTimesOut {
        fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::new(io::ErrorKind::TimedOut, "synthetic timeout"))
        }
    }

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

    fn offers(mimes: &[&str]) -> HashSet<String> {
        mimes.iter().map(|&mime| mime.to_owned()).collect()
    }

    #[test]
    fn read_pipe_closes_once_a_representation_crosses_its_budget() {
        let mut reader = CountingChunks::new(PIPE_CHUNK, 8);
        let mut state = MultiReadState::new(PIPE_CHUNK * 8);
        let body = state.read_pipe(&mut reader, PIPE_CHUNK).unwrap();

        // The second chunk crosses the budget; the pipe is not drained.
        assert_eq!(reader.reads, 2);
        assert!(body.is_none());
        assert!(state.aborted());
        assert_eq!(state.overflow_limit, PIPE_CHUNK);
        assert_eq!(state.observed_total, PIPE_CHUNK * 2);
    }

    #[test]
    fn read_pipe_closes_at_the_cumulative_ceiling() {
        let mut state = MultiReadState::new(PIPE_CHUNK * 3);
        let first = state
            .read_pipe(&mut CountingChunks::new(PIPE_CHUNK, 2), PIPE_CHUNK * 4)
            .unwrap();
        assert_eq!(first.map(|body| body.len()), Some(PIPE_CHUNK * 2));

        // Within its own budget, but past what the pass may buffer in total.
        let second = state
            .read_pipe(&mut CountingChunks::new(PIPE_CHUNK, 2), PIPE_CHUNK * 4)
            .unwrap();
        assert!(second.is_none());
        assert!(state.aborted());
        assert_eq!(state.overflow_limit, PIPE_CHUNK * 3);
    }

    #[test]
    fn read_pipe_buffers_within_budget() {
        let mut reader = io::Cursor::new(b"clipboard".to_vec());
        let mut state = MultiReadState::new(64);
        let body = state.read_pipe(&mut reader, 64).unwrap();

        assert_eq!(body.as_deref(), Some(&b"clipboard"[..]));
        assert_eq!(state.observed_total, b"clipboard".len());
        assert!(!state.aborted());
    }

    #[test]
    fn read_pipe_keeps_large_images_whole() {
        // Screenshots routinely exceed a megabyte; the body is buffered in
        // full under the image budget rather than judged by a prefix.
        let len = 3 * 1024 * 1024 + 17;
        let mut body = vec![0u8; len];
        body[len - 1] = 0xAB;
        let mut state = MultiReadState::new(64 * 1024 * 1024);
        let read = state
            .read_pipe(&mut io::Cursor::new(body.clone()), 16 * 1024 * 1024)
            .unwrap();
        assert_eq!(read, Some(body));
    }

    #[test]
    fn read_pipe_drops_snapshot_on_reader_timeout() {
        // A hung Wayland publisher surfaces through the wrapper as a
        // `TimedOut` error on the very first read. The state must treat
        // that as a sticky abort (no buffered body returned) so the
        // snapshot is dropped rather than left pinning a blocking worker.
        let mut state = MultiReadState::new(PIPE_CHUNK);
        let body = state.read_pipe(&mut AlwaysTimesOut, PIPE_CHUNK).unwrap();

        assert!(body.is_none());
        assert!(state.read_timeout, "read_timeout flag must latch");
        assert!(state.aborted());

        // Subsequent reads must short-circuit so the loop cannot keep
        // touching the wedged pipe across MIME types.
        let mut subsequent = io::Cursor::new(b"ignored".to_vec());
        let after = state.read_pipe(&mut subsequent, PIPE_CHUNK).unwrap();
        assert!(after.is_none());
    }

    #[test]
    fn timeout_pipe_reader_times_out_when_publisher_silent() {
        // Real pipe with no writer activity: poll(2) must fire after the
        // configured timeout and surface a `TimedOut` error rather than
        // blocking the test thread on `read(2)`. The writer end stays
        // open so the kernel does not deliver EOF instead.
        let (mut reader, _writer) = std::io::pipe().expect("pipe");
        let mut timed = TimeoutPipeReader::new(&mut reader, std::time::Duration::from_millis(50));

        let mut buf = [0u8; 16];
        let started = std::time::Instant::now();
        let err = timed.read(&mut buf).expect_err("must time out");
        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
        // Generous upper bound for the 50ms deadline so this stays
        // stable on CI scheduling variance — we just need to confirm
        // the read returned promptly rather than hanging on `read(2)`.
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
    }

    #[test]
    fn pick_image_mime_honours_priority() {
        let set = offers(&["image/jpeg", "image/png"]);
        // PNG wins because it sits earlier in `IMAGE_MIME_PRIORITY`,
        // independent of HashSet iteration order.
        assert_eq!(pick_image_mime(&set), Some("image/png"));
        // And the priority list ordering matches the macOS adapter's
        // canonical-image preference (PNG first).
        assert_eq!(IMAGE_MIME_PRIORITY.first(), Some(&"image/png"));
    }

    #[test]
    fn pick_image_mime_returns_none_when_no_image_offer() {
        assert_eq!(
            pick_image_mime(&offers(&["text/plain", "text/uri-list"])),
            None
        );
    }

    #[test]
    fn pick_text_mime_prefers_utf8_plain_text() {
        let set = offers(&[
            "STRING",
            "text/plain",
            "text/plain;charset=utf-8",
            "UTF8_STRING",
        ]);
        assert_eq!(pick_text_mime(&set), Some("text/plain;charset=utf-8"));
        assert_eq!(
            pick_text_mime(&offers(&["STRING", "UTF8_STRING"])),
            Some("UTF8_STRING")
        );
    }

    #[test]
    fn pick_text_mime_accepts_other_plain_text_spellings() {
        assert_eq!(
            pick_text_mime(&offers(&["text/html", "text/plain;charset=UTF-8"])),
            Some("text/plain;charset=UTF-8")
        );
    }

    #[test]
    fn pick_text_mime_never_substitutes_markup_for_plain_text() {
        // A browser offering only markup / structured text has no plain-text
        // representation to capture; storing the HTML source as text/plain
        // would paste raw tags back into plain-text targets.
        assert_eq!(
            pick_text_mime(&offers(&["text/html", "application/json", "text/uri-list"])),
            None
        );
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

    #[test]
    fn offer_exclusion_detects_kde_password_manager_hint() {
        // A password manager advertises the hint alongside the secret text;
        // the marker's presence is enough to skip the clip.
        let set = offers(&["text/plain;charset=utf-8", KDE_PASSWORD_MANAGER_HINT_MIME]);
        assert_eq!(
            offer_exclusion(&set),
            Some(ClipboardExclusionKind::Concealed),
        );
    }

    #[test]
    fn offer_exclusion_ignores_ordinary_offer() {
        assert_eq!(offer_exclusion(&offers(&["text/plain", "image/png"])), None);
    }
}

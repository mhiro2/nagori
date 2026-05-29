//! macOS Swift FFI boundary.
//!
//! This module is intentionally the *only* place that touches the Apple
//! frameworks. It is compiled solely on macOS (gated in `lib.rs`); every other
//! platform uses the pure-Rust mock fixtures and simulated stream driver.
//!
//! `unsafe` is inherent to a C-ABI bridge, so it is allowed module-wide here
//! and kept out of the rest of the crate. The `extern "C"` signatures mirror
//! the `@_cdecl` exports in `swift/Sources/nagori_apple/Bridge.swift`.
#![allow(unsafe_code)]

use std::ffi::{CString, c_char, c_void};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use futures::StreamExt;
use nagori_ai::{AiEventStream, BackendUnavailableReason, TranslationOutput};
use nagori_core::{AiError, AiErrorCode, AiEvent};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::availability::AppleAvailability;
use crate::event::AppleStreamEvent;
use crate::pump::SnapshotPump;
use crate::stream::StreamHandle;

unsafe extern "C" {
    fn nagori_apple_hello_c() -> i32;
    fn nagori_apple_fm_availability_c() -> i32;
    fn nagori_apple_stream_snapshots_c(
        source_ptr: *const c_char,
        ctx: *mut c_void,
        is_cancelled: extern "C" fn(*mut c_void) -> u8,
        on_snapshot: extern "C" fn(*mut c_void, *const u8, usize),
        on_done: extern "C" fn(*mut c_void, i32),
    );
    fn nagori_apple_summarize_c(
        prompt_ptr: *const c_char,
        ctx: *mut c_void,
        is_cancelled: extern "C" fn(*mut c_void) -> u8,
        on_snapshot: extern "C" fn(*mut c_void, *const u8, usize),
        on_done: extern "C" fn(*mut c_void, i32),
    );
    fn nagori_apple_translation_pair_status_c(
        source_ptr: *const c_char,
        target_ptr: *const c_char,
    ) -> i32;
    fn nagori_apple_translate_c(
        text_ptr: *const c_char,
        source_ptr: *const c_char,
        target_ptr: *const c_char,
        ctx: *mut c_void,
        on_complete: extern "C" fn(*mut c_void, i32, *const u8, usize, *const u8, usize),
    );
    fn nagori_apple_on_ac_power_c() -> i32;
    fn nagori_apple_preferred_language_c(
        ctx: *mut c_void,
        on_result: extern "C" fn(*mut c_void, *const u8, usize),
    );
    fn nagori_apple_embed_availability_c(lang_ptr: *const c_char) -> i32;
    fn nagori_apple_embed_metadata_c(
        lang_ptr: *const c_char,
        ctx: *mut c_void,
        on_complete: extern "C" fn(*mut c_void, i32, isize, isize, isize, *const u8, usize),
    );
    fn nagori_apple_embed_request_assets_c(
        lang_ptr: *const c_char,
        ctx: *mut c_void,
        on_complete: extern "C" fn(*mut c_void, i32),
    );
    fn nagori_apple_embed_c(
        lang_ptr: *const c_char,
        text_ptr: *const c_char,
        ctx: *mut c_void,
        on_complete: extern "C" fn(*mut c_void, i32, *const f32, usize),
    );
}

/// Box handed to Swift as an opaque context pointer for the duration of one
/// stream. The `cancel` clone keeps the shared flag alive while Swift may poll
/// it (until `on_done` reclaims the box) and is read atomically by
/// [`is_cancelled`].
struct BridgeCtx {
    tx: mpsc::UnboundedSender<AppleStreamEvent>,
    pump: SnapshotPump,
    cancel: Arc<AtomicBool>,
}

/// Polled by Swift before each snapshot. The atomic load happens entirely in
/// Rust, so the cancel flag's bytes are never read across the language
/// boundary (which would be a data race against the atomic store).
extern "C" fn is_cancelled(ctx: *mut c_void) -> u8 {
    if ctx.is_null() {
        // A missing context means there is nothing left to stream into; tell
        // Swift to stop.
        return 1;
    }
    // SAFETY: `ctx` is the `BridgeCtx` pointer handed to Swift. It is polled
    // sequentially from the same dispatch queue as `on_snapshot`/`on_done`, so
    // this shared borrow never overlaps the `&mut` reborrow there.
    let ctx = unsafe { &*ctx.cast::<BridgeCtx>() };
    u8::from(ctx.cancel.load(Ordering::SeqCst))
}

/// Receives one cumulative snapshot from Swift and forwards the delta.
extern "C" fn on_snapshot(ctx: *mut c_void, ptr: *const u8, len: usize) {
    if ctx.is_null() || ptr.is_null() {
        return;
    }
    // SAFETY: `ctx` is the `BridgeCtx` raw pointer we handed to Swift, which
    // invokes callbacks sequentially from a single dispatch queue, so a unique
    // `&mut` is sound. `ptr`/`len` describe a buffer Swift keeps alive for the
    // duration of the call.
    let ctx = unsafe { &mut *ctx.cast::<BridgeCtx>() };
    let bytes = unsafe { std::slice::from_raw_parts(ptr, len) };
    if let Ok(text) = std::str::from_utf8(bytes)
        && let Some(event) = ctx.pump.push(text)
    {
        let _ = ctx.tx.send(event);
    }
}

/// Final callback for a stream: emits the terminal event and reclaims the box.
extern "C" fn on_done(ctx: *mut c_void, code: i32) {
    if ctx.is_null() {
        return;
    }
    // SAFETY: `on_done` is the last callback for a stream, so reclaiming the
    // box here is sound — Swift does not touch `ctx` afterwards.
    let ctx = unsafe { Box::from_raw(ctx.cast::<BridgeCtx>()) };
    let BridgeCtx {
        tx,
        pump,
        cancel: _,
    } = *ctx;
    let cancelled = code == 1;
    let _ = tx.send(pump.finish(cancelled));
}

/// Probes live Apple Intelligence availability via the Swift bridge.
pub(crate) fn probe_real_availability() -> AppleAvailability {
    // SAFETY: a no-argument C function returning an `i32` status code.
    let code = unsafe { nagori_apple_fm_availability_c() };
    AppleAvailability::from_probe_code(code)
}

/// Calls the Swift sanity export; returns `42` when the static library is
/// linked and callable.
#[must_use]
pub fn hello() -> i32 {
    // SAFETY: a no-argument C function returning an `i32`.
    unsafe { nagori_apple_hello_c() }
}

/// Streams cumulative snapshots of `text` through the Swift bridge.
///
/// Snapshots are delta-ised by a [`SnapshotPump`]. This exercises the FFI
/// streaming and shared-`AtomicBool` cancellation paths on real hardware
/// without requiring Apple Intelligence to be enabled.
#[must_use]
pub fn bridge_snapshot_stream(text: &str) -> StreamHandle {
    spawn_bridge(text, Arc::new(AtomicBool::new(false)))
}

/// Spawns the Swift bridge producer with a caller-supplied cancel flag. Passing
/// a flag that is already `true` lets tests cancel *before the Swift loop
/// starts*: the enqueue to the dispatch queue carries the store, so the first
/// `is_cancelled` poll observes it and the terminal is deterministically
/// [`AppleStreamEvent::Cancelled`].
fn spawn_bridge(text: &str, cancel: Arc<AtomicBool>) -> StreamHandle {
    let (tx, rx) = mpsc::unbounded_channel();

    let ctx = Box::new(BridgeCtx {
        tx,
        pump: SnapshotPump::new(),
        cancel: Arc::clone(&cancel),
    });
    let ctx_ptr = Box::into_raw(ctx).cast::<c_void>();

    // Strip interior NULs so the C string survives intact.
    let source = CString::new(text.replace('\0', " ")).unwrap_or_default();

    // SAFETY: the signature matches the `@_cdecl` export; the ctx box outlives
    // the call (reclaimed in `on_done`), and the callbacks are plain `fn`
    // items that read the cancel flag through the ctx atomically.
    unsafe {
        nagori_apple_stream_snapshots_c(
            source.as_ptr(),
            ctx_ptr,
            is_cancelled,
            on_snapshot,
            on_done,
        );
    }

    StreamHandle::new(cancel, rx)
}

/// Opaque context handed to Swift for the duration of one summarize stream. It
/// owns the event sender, the snapshot-delta pump, and a clone of the
/// cancellation token Swift polls through [`summarize_is_cancelled`].
struct SummarizeCtx {
    tx: mpsc::UnboundedSender<Result<AiEvent, AiError>>,
    pump: SnapshotPump,
    cancel: CancellationToken,
}

/// Polled by Swift before forwarding each snapshot. The token's atomic load
/// happens entirely in Rust, so its bytes never cross the language boundary.
extern "C" fn summarize_is_cancelled(ctx: *mut c_void) -> u8 {
    if ctx.is_null() {
        return 1;
    }
    // SAFETY: `ctx` is the `SummarizeCtx` pointer handed to Swift; callbacks run
    // sequentially on one task, so this shared borrow never overlaps the `&mut`
    // reborrow in `summarize_on_snapshot`.
    let ctx = unsafe { &*ctx.cast::<SummarizeCtx>() };
    u8::from(ctx.cancel.is_cancelled())
}

/// Receives one cumulative snapshot from Swift and forwards the streaming delta.
extern "C" fn summarize_on_snapshot(ctx: *mut c_void, ptr: *const u8, len: usize) {
    if ctx.is_null() || ptr.is_null() {
        return;
    }
    // SAFETY: `ctx` is the `SummarizeCtx` pointer; Swift invokes callbacks
    // sequentially from a single task, so a unique `&mut` is sound. `ptr`/`len`
    // describe a buffer Swift keeps alive for the call's duration.
    let ctx = unsafe { &mut *ctx.cast::<SummarizeCtx>() };
    let bytes = unsafe { std::slice::from_raw_parts(ptr, len) };
    if let Ok(text) = std::str::from_utf8(bytes)
        && let Some(event) = ctx.pump.push(text)
        && let Some(ai_event) = streaming_event(event)
    {
        let _ = ctx.tx.send(Ok(ai_event));
    }
}

/// Final callback: maps the terminal status code onto a terminal stream item
/// and reclaims the box.
extern "C" fn summarize_on_done(ctx: *mut c_void, code: i32) {
    if ctx.is_null() {
        return;
    }
    // SAFETY: `summarize_on_done` is the last callback for a stream, so
    // reclaiming the box here is sound — Swift does not touch `ctx` afterwards.
    let ctx = unsafe { Box::from_raw(ctx.cast::<SummarizeCtx>()) };
    let SummarizeCtx {
        tx,
        pump,
        cancel: _,
    } = *ctx;
    let _ = tx.send(summarize_terminal(code, pump.current().to_owned()));
}

/// Converts a streaming [`AppleStreamEvent`] into an [`AiEvent`]. The terminal
/// variants are produced from the status code instead, so they map to `None`.
fn streaming_event(event: AppleStreamEvent) -> Option<AiEvent> {
    match event {
        AppleStreamEvent::Delta { seq, text } => Some(AiEvent::Delta { seq, text }),
        AppleStreamEvent::Replace { seq, text } => Some(AiEvent::Replace { seq, text }),
        AppleStreamEvent::Done { .. } | AppleStreamEvent::Cancelled { .. } => None,
    }
}

/// Maps the Swift terminal status code onto the stream's terminal item. Keep in
/// sync with `generationErrorCode` in `Bridge.swift`.
fn summarize_terminal(code: i32, final_text: String) -> Result<AiEvent, AiError> {
    match code {
        0 => Ok(AiEvent::Done {
            final_text,
            created_entry: None,
            warnings: Vec::new(),
        }),
        1 => Ok(AiEvent::Cancelled),
        3 => Err(AiError::new(
            AiErrorCode::InputTooLarge,
            "input exceeded the on-device model's context window",
        )),
        4 | 8 => Err(AiError::new(
            AiErrorCode::RateLimited,
            "the on-device model is busy; retry shortly",
        )),
        5 => Err(AiError::new(
            AiErrorCode::AssetMissing,
            "a required on-device asset is unavailable",
        )),
        6 => Err(AiError::new(
            AiErrorCode::BackendInternal,
            "the request was blocked by the model guardrail",
        )),
        7 => Err(AiError::new(
            AiErrorCode::BackendInternal,
            "the input language or locale is unsupported",
        )),
        _ => Err(AiError::new(
            AiErrorCode::BackendInternal,
            "the on-device model failed to generate a response",
        )),
    }
}

/// Streams a summary of `input` from the on-device language model.
///
/// Cancellation is observed by polling `cancel` (a [`CancellationToken`] the
/// caller owns) before each snapshot; the caller cancels it to stop the Swift
/// task. The returned stream ends after exactly one terminal item
/// (`Ok(Done)` / `Ok(Cancelled)` / `Err`).
pub(crate) fn summarize_stream(input: &str, cancel: CancellationToken) -> AiEventStream {
    let (tx, rx) = mpsc::unbounded_channel::<Result<AiEvent, AiError>>();
    let ctx = Box::new(SummarizeCtx {
        tx,
        pump: SnapshotPump::new(),
        cancel,
    });
    let ctx_ptr = Box::into_raw(ctx).cast::<c_void>();

    // Strip interior NULs so the C string survives intact.
    let source = CString::new(input.replace('\0', " ")).unwrap_or_default();

    // SAFETY: the signature matches the `@_cdecl` export; the ctx box outlives
    // the call (reclaimed in `summarize_on_done`), and the callbacks are plain
    // `fn` items that read the cancel token through the ctx atomically.
    unsafe {
        nagori_apple_summarize_c(
            source.as_ptr(),
            ctx_ptr,
            summarize_is_cancelled,
            summarize_on_snapshot,
            summarize_on_done,
        );
    }

    futures::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|item| (item, rx))
    })
    .boxed()
}

/// Opaque context handed to Swift for one translation. It owns the oneshot
/// sender the single `translate_on_complete` callback fulfils.
struct TranslateCtx {
    tx: oneshot::Sender<Result<TranslationOutput, AiError>>,
}

/// Single terminal callback for a translation: builds the result, sends it to
/// the waiting [`translate`] future, and reclaims the context box.
extern "C" fn translate_on_complete(
    ctx: *mut c_void,
    code: i32,
    text_ptr: *const u8,
    text_len: usize,
    src_ptr: *const u8,
    src_len: usize,
) {
    if ctx.is_null() {
        return;
    }
    // SAFETY: `translate_on_complete` is the sole, final callback for a
    // translation, so reclaiming the box here is sound — Swift does not touch
    // `ctx` afterwards.
    let ctx = unsafe { Box::from_raw(ctx.cast::<TranslateCtx>()) };
    let result = build_translation_result(code, text_ptr, text_len, src_ptr, src_len);
    // The receiver is dropped if the request was cancelled; the send then fails
    // harmlessly and the result is discarded.
    let _ = ctx.tx.send(result);
}

/// Copies a UTF-8 buffer Swift handed us into an owned `String`. Swift keeps the
/// buffer alive only for the duration of the callback, so we copy eagerly.
fn read_utf8(ptr: *const u8, len: usize) -> String {
    if ptr.is_null() || len == 0 {
        return String::new();
    }
    // SAFETY: `ptr`/`len` describe a buffer Swift keeps valid for the call.
    let bytes = unsafe { std::slice::from_raw_parts(ptr, len) };
    String::from_utf8_lossy(bytes).into_owned()
}

/// Builds the translation result from the Swift terminal callback's status code
/// and (on success) its two UTF-8 buffers.
fn build_translation_result(
    code: i32,
    text_ptr: *const u8,
    text_len: usize,
    src_ptr: *const u8,
    src_len: usize,
) -> Result<TranslationOutput, AiError> {
    if code == 0 {
        let detected = read_utf8(src_ptr, src_len);
        Ok(TranslationOutput {
            text: read_utf8(text_ptr, text_len),
            detected_source_language: (!detected.is_empty()).then_some(detected),
        })
    } else {
        Err(translate_terminal(code))
    }
}

/// Maps the Swift translation status code onto an [`AiError`]. Keep in sync with
/// `translationErrorCode` in `Bridge.swift`.
fn translate_terminal(code: i32) -> AiError {
    match code {
        // `notInstalled`: the language pack is downloadable but not present.
        // Reuse the asset-missing reason so the UI gets a download remediation.
        1 => BackendUnavailableReason::AssetMissing.into_error(),
        2 => AiError::new(
            AiErrorCode::BackendInternal,
            "the source/target language pair is not supported",
        ),
        3 => AiError::new(
            AiErrorCode::BackendInternal,
            "could not identify the source language; pass one explicitly",
        ),
        4 => AiError::new(
            AiErrorCode::BackendInternal,
            "there was nothing to translate",
        ),
        // The Swift bridge's own timeout fired (the framework wedged); see
        // `translateTimeoutNanoseconds` in `Bridge.swift`.
        6 => AiError::new(
            AiErrorCode::Timeout,
            "the translation framework did not respond in time",
        ),
        _ => AiError::new(
            AiErrorCode::BackendInternal,
            "the translation framework failed to translate the input",
        ),
    }
}

/// Reports a source/target pair's readiness via the Swift bridge.
///
/// Returns the raw status code (`0` installed, `1` downloadable, `2`
/// unsupported, `3` unknown / no source / timed out); [`crate::translate`] maps
/// it onto a `BackendAvailability`.
pub(crate) fn translation_pair_status(source: Option<&str>, target: &str) -> i32 {
    let c_source = CString::new(source.unwrap_or_default().replace('\0', " ")).unwrap_or_default();
    let c_target = CString::new(target.replace('\0', " ")).unwrap_or_default();
    // SAFETY: a C function taking two NUL-terminated strings, returning a code.
    unsafe { nagori_apple_translation_pair_status_c(c_source.as_ptr(), c_target.as_ptr()) }
}

/// Translates `input` into `target` (auto-detecting the source when `source` is
/// `None`) via the Apple Translation framework.
///
/// Cancellation is not polled here: the single async `translate` has no polling
/// point, so the engine's stream wrapper handles user cancellation by dropping
/// this future (which drops the receiver; the Swift side then sends into a
/// closed channel). To keep that from leaking native work when the framework
/// wedges, the Swift bridge enforces its own timeout and *always* calls back
/// (success, error, or a timeout sentinel), so `ctx` is reclaimed and this
/// future always resolves within a bounded time.
pub(crate) async fn translate(
    input: &str,
    source: Option<&str>,
    target: &str,
) -> Result<TranslationOutput, AiError> {
    let (tx, rx) = oneshot::channel::<Result<TranslationOutput, AiError>>();
    let ctx = Box::new(TranslateCtx { tx });
    let ctx_ptr = Box::into_raw(ctx).cast::<c_void>();

    // Strip interior NULs so the C strings survive intact.
    let c_text = CString::new(input.replace('\0', " ")).unwrap_or_default();
    let c_source = CString::new(source.unwrap_or_default().replace('\0', " ")).unwrap_or_default();
    let c_target = CString::new(target.replace('\0', " ")).unwrap_or_default();

    // SAFETY: the signature matches the `@_cdecl` export; the ctx box outlives
    // the call (reclaimed in `translate_on_complete`), and `on_complete` is a
    // plain `fn` item.
    unsafe {
        nagori_apple_translate_c(
            c_text.as_ptr(),
            c_source.as_ptr(),
            c_target.as_ptr(),
            ctx_ptr,
            translate_on_complete,
        );
    }

    match rx.await {
        Ok(result) => result,
        Err(_) => Err(AiError::new(
            AiErrorCode::BackendInternal,
            "the translation bridge closed without a result",
        )),
    }
}

/// Whether the Mac is on AC power: `Some(true)` on AC, `Some(false)` on
/// battery, `None` if the power source could not be determined.
#[must_use]
pub(crate) fn on_ac_power() -> Option<bool> {
    // SAFETY: a no-argument C function returning a status code.
    match unsafe { nagori_apple_on_ac_power_c() } {
        1 => Some(true),
        0 => Some(false),
        _ => None,
    }
}

extern "C" fn collect_language(ctx: *mut c_void, ptr: *const u8, len: usize) {
    if ctx.is_null() {
        return;
    }
    // SAFETY: `ctx` points at the caller's `Option<String>` on the stack, which
    // outlives this synchronous callback.
    let out = unsafe { &mut *ctx.cast::<Option<String>>() };
    *out = Some(read_utf8(ptr, len));
}

/// The user's preferred language code (e.g. `"en"`, `"ja"`), or `"en"` if it
/// cannot be resolved. Used to pin the embedding model to the language the
/// user's clips are most likely written in.
#[must_use]
pub(crate) fn preferred_language() -> String {
    let mut out: Option<String> = None;
    let ctx_ptr = std::ptr::from_mut(&mut out).cast::<c_void>();
    // SAFETY: the signature matches the `@_cdecl` export; `collect_language`
    // runs synchronously during the call, while `out` is still live.
    unsafe {
        nagori_apple_preferred_language_c(ctx_ptr, collect_language);
    }
    out.filter(|code| !code.is_empty())
        .unwrap_or_else(|| "en".to_owned())
}

/// Runtime metadata of the contextual-embedding model, read from the Swift
/// bridge. The backend wraps this in `nagori-ai`'s `EmbeddingModelMetadata`,
/// adding the configured language list.
pub(crate) struct RawEmbedMetadata {
    pub model_identifier: String,
    pub revision: u32,
    pub dimension: usize,
    pub max_sequence_length: usize,
}

/// Reports the embedding model's asset readiness for `language` (raw code: `0`
/// installed, `1` downloadable, `2` no model for the language).
pub(crate) fn embed_availability(language: &str) -> i32 {
    let c_lang = CString::new(language.replace('\0', " ")).unwrap_or_default();
    // SAFETY: a C function taking one NUL-terminated string, returning a code.
    unsafe { nagori_apple_embed_availability_c(c_lang.as_ptr()) }
}

/// Context for one metadata read: owns the oneshot the single callback fulfils.
struct EmbedMetaCtx {
    tx: oneshot::Sender<Result<RawEmbedMetadata, AiError>>,
}

extern "C" fn embed_metadata_on_complete(
    ctx: *mut c_void,
    code: i32,
    dimension: isize,
    revision: isize,
    max_sequence_length: isize,
    id_ptr: *const u8,
    id_len: usize,
) {
    if ctx.is_null() {
        return;
    }
    // SAFETY: the sole, final callback for this metadata read, so reclaiming the
    // box here is sound — Swift does not touch `ctx` afterwards.
    let ctx = unsafe { Box::from_raw(ctx.cast::<EmbedMetaCtx>()) };
    let result = if code == 0 {
        Ok(RawEmbedMetadata {
            model_identifier: read_utf8(id_ptr, id_len),
            revision: u32::try_from(revision).unwrap_or(0),
            dimension: usize::try_from(dimension).unwrap_or(0),
            max_sequence_length: usize::try_from(max_sequence_length).unwrap_or(0),
        })
    } else {
        Err(AiError::new(
            AiErrorCode::BackendInternal,
            "no embedding model is available for the configured language",
        ))
    };
    let _ = ctx.tx.send(result);
}

/// Reads the contextual-embedding model's runtime metadata for `language`.
pub(crate) async fn embed_metadata(language: &str) -> Result<RawEmbedMetadata, AiError> {
    let (tx, rx) = oneshot::channel::<Result<RawEmbedMetadata, AiError>>();
    let ctx = Box::new(EmbedMetaCtx { tx });
    let ctx_ptr = Box::into_raw(ctx).cast::<c_void>();
    let c_lang = CString::new(language.replace('\0', " ")).unwrap_or_default();
    // SAFETY: the signature matches the `@_cdecl` export; the ctx box is
    // reclaimed in `embed_metadata_on_complete` (invoked synchronously here).
    unsafe {
        nagori_apple_embed_metadata_c(c_lang.as_ptr(), ctx_ptr, embed_metadata_on_complete);
    }
    rx.await.unwrap_or_else(|_| {
        Err(AiError::new(
            AiErrorCode::BackendInternal,
            "the embedding bridge closed without metadata",
        ))
    })
}

/// Context for one asset-download request.
struct EmbedAssetsCtx {
    tx: oneshot::Sender<Result<(), AiError>>,
}

extern "C" fn embed_assets_on_complete(ctx: *mut c_void, code: i32) {
    if ctx.is_null() {
        return;
    }
    // SAFETY: guarded one-shot callback (Swift's `OnceFlag`), so reclaiming the
    // box here is sound.
    let ctx = unsafe { Box::from_raw(ctx.cast::<EmbedAssetsCtx>()) };
    let result = match code {
        0 => Ok(()),
        1 => Err(BackendUnavailableReason::AssetMissing.into_error()),
        6 => Err(AiError::new(
            AiErrorCode::Timeout,
            "the embedding asset download did not respond in time",
        )),
        _ => Err(AiError::new(
            AiErrorCode::BackendInternal,
            "the embedding asset download failed",
        )),
    };
    let _ = ctx.tx.send(result);
}

/// Requests download of the contextual-embedding assets for `language`.
pub(crate) async fn embed_request_assets(language: &str) -> Result<(), AiError> {
    let (tx, rx) = oneshot::channel::<Result<(), AiError>>();
    let ctx = Box::new(EmbedAssetsCtx { tx });
    let ctx_ptr = Box::into_raw(ctx).cast::<c_void>();
    let c_lang = CString::new(language.replace('\0', " ")).unwrap_or_default();
    // SAFETY: the signature matches the `@_cdecl` export; the ctx box is
    // reclaimed in `embed_assets_on_complete`, which Swift's `OnceFlag` fires
    // exactly once (success/error or the timeout sentinel).
    unsafe {
        nagori_apple_embed_request_assets_c(c_lang.as_ptr(), ctx_ptr, embed_assets_on_complete);
    }
    rx.await.unwrap_or_else(|_| {
        Err(AiError::new(
            AiErrorCode::BackendInternal,
            "the embedding bridge closed without an asset result",
        ))
    })
}

/// Context for one embedding call.
struct EmbedCtx {
    tx: oneshot::Sender<Result<Vec<f32>, AiError>>,
}

extern "C" fn embed_on_complete(ctx: *mut c_void, code: i32, ptr: *const f32, len: usize) {
    if ctx.is_null() {
        return;
    }
    // SAFETY: guarded one-shot callback (Swift's `OnceFlag`), so reclaiming the
    // box here is sound.
    let ctx = unsafe { Box::from_raw(ctx.cast::<EmbedCtx>()) };
    let result = if code == 0 {
        Ok(read_f32(ptr, len))
    } else {
        Err(embed_terminal(code))
    };
    let _ = ctx.tx.send(result);
}

/// Copies a float32 buffer Swift handed us into an owned `Vec`. Swift keeps the
/// buffer alive only for the duration of the callback, so we copy eagerly.
fn read_f32(ptr: *const f32, len: usize) -> Vec<f32> {
    if ptr.is_null() || len == 0 {
        return Vec::new();
    }
    // SAFETY: `ptr`/`len` describe a buffer Swift keeps valid for the call.
    unsafe { std::slice::from_raw_parts(ptr, len) }.to_vec()
}

/// Maps the Swift embedding status code onto an [`AiError`]. Keep in sync with
/// `nagori_apple_embed_c` in `Bridge.swift`.
fn embed_terminal(code: i32) -> AiError {
    match code {
        1 => BackendUnavailableReason::AssetMissing.into_error(),
        4 => AiError::new(AiErrorCode::BackendInternal, "there was nothing to embed"),
        6 => AiError::new(
            AiErrorCode::Timeout,
            "the embedding model did not respond in time",
        ),
        _ => AiError::new(
            AiErrorCode::BackendInternal,
            "the embedding model failed to embed the input",
        ),
    }
}

/// Embeds `text` with the contextual-embedding model for `language`, returning
/// one mean-pooled, L2-normalised document vector.
pub(crate) async fn embed_text(language: &str, text: &str) -> Result<Vec<f32>, AiError> {
    let (tx, rx) = oneshot::channel::<Result<Vec<f32>, AiError>>();
    let ctx = Box::new(EmbedCtx { tx });
    let ctx_ptr = Box::into_raw(ctx).cast::<c_void>();
    let c_lang = CString::new(language.replace('\0', " ")).unwrap_or_default();
    let c_text = CString::new(text.replace('\0', " ")).unwrap_or_default();
    // SAFETY: the signature matches the `@_cdecl` export; the ctx box is
    // reclaimed in `embed_on_complete`, which Swift's `OnceFlag` fires exactly
    // once (success, error, or the timeout sentinel).
    unsafe {
        nagori_apple_embed_c(c_lang.as_ptr(), c_text.as_ptr(), ctx_ptr, embed_on_complete);
    }
    rx.await.unwrap_or_else(|_| {
        Err(AiError::new(
            AiErrorCode::BackendInternal,
            "the embedding bridge closed without a result",
        ))
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    use super::{bridge_snapshot_stream, hello, probe_real_availability, spawn_bridge};
    use crate::availability::AppleAvailability;
    use crate::event::AppleStreamEvent;

    #[test]
    fn hello_world_bridge_is_linked() {
        assert_eq!(hello(), 42);
    }

    #[test]
    fn real_availability_returns_a_known_variant() {
        // On CI / un-enabled hosts this is `AppleIntelligenceNotEnabled`; the
        // point is that the FFI round-trips into a recognised enum value.
        let availability = probe_real_availability();
        assert!(matches!(
            availability,
            AppleAvailability::Available
                | AppleAvailability::DeviceNotEligible
                | AppleAvailability::AppleIntelligenceNotEnabled
                | AppleAvailability::ModelNotReady
                | AppleAvailability::Unknown
        ));
    }

    #[tokio::test]
    async fn bridge_streams_and_reconstructs_text() {
        let input = "swift 世界 🦀";
        let mut handle = bridge_snapshot_stream(input);
        let mut buf = String::new();
        let mut terminal = None;
        while let Some(event) = handle.recv().await {
            match event {
                AppleStreamEvent::Delta { text, .. } => buf.push_str(&text),
                AppleStreamEvent::Replace { text, .. } => buf = text,
                terminal_event => {
                    terminal = Some(terminal_event);
                    break;
                }
            }
        }
        assert_eq!(buf, input);
        assert_eq!(
            terminal,
            Some(AppleStreamEvent::Done {
                final_text: input.to_owned()
            })
        );
    }

    #[tokio::test]
    async fn bridge_cancellation_stops_stream() {
        // Start with the flag already set so the Swift loop observes it on its
        // first poll and terminates with `Cancelled`, with no scheduling race.
        let input = "x".repeat(200);
        let cancel = Arc::new(AtomicBool::new(true));
        let mut handle = spawn_bridge(&input, cancel);

        let mut last = None;
        while let Some(event) = handle.recv().await {
            last = Some(event);
        }
        match last {
            Some(AppleStreamEvent::Cancelled { final_text }) => {
                assert!(final_text.len() < input.len());
            }
            other => panic!("expected Cancelled terminal, got {other:?}"),
        }
    }

    #[test]
    fn translate_terminal_maps_not_installed_to_asset_missing() {
        use nagori_core::AiErrorCode;
        let err = super::translate_terminal(1);
        assert_eq!(err.code, AiErrorCode::AssetMissing);
        // The asset-missing reason carries a download remediation hint.
        assert!(err.remediation.is_some());
    }

    #[test]
    fn translate_terminal_maps_timeout_sentinel() {
        use nagori_core::AiErrorCode;
        assert_eq!(super::translate_terminal(6).code, AiErrorCode::Timeout);
    }

    #[test]
    fn translate_terminal_maps_other_codes_to_backend_internal() {
        use nagori_core::AiErrorCode;
        for code in [2, 3, 4, 5, 99] {
            assert_eq!(
                super::translate_terminal(code).code,
                AiErrorCode::BackendInternal
            );
        }
    }

    #[test]
    fn build_translation_result_success_copies_buffers() {
        let text = b"\xe3\x81\x93\xe3\x82\x93\xe3\x81\xab\xe3\x81\xa1\xe3\x81\xaf"; // こんにちは
        let src = b"en";
        let out =
            super::build_translation_result(0, text.as_ptr(), text.len(), src.as_ptr(), src.len())
                .expect("code 0 is success");
        assert_eq!(out.text, "こんにちは");
        assert_eq!(out.detected_source_language.as_deref(), Some("en"));
    }

    #[test]
    fn build_translation_result_success_without_detected_source() {
        let text = b"hi";
        let out =
            super::build_translation_result(0, text.as_ptr(), text.len(), std::ptr::null(), 0)
                .expect("code 0 is success");
        assert_eq!(out.text, "hi");
        assert_eq!(out.detected_source_language, None);
    }

    #[test]
    fn build_translation_result_error_code_is_err() {
        let result = super::build_translation_result(2, std::ptr::null(), 0, std::ptr::null(), 0);
        assert!(result.is_err());
    }

    #[test]
    fn embed_terminal_maps_assets_missing_with_remediation() {
        use nagori_core::AiErrorCode;
        let err = super::embed_terminal(1);
        assert_eq!(err.code, AiErrorCode::AssetMissing);
        assert!(err.remediation.is_some());
    }

    #[test]
    fn embed_terminal_maps_timeout_sentinel() {
        use nagori_core::AiErrorCode;
        assert_eq!(super::embed_terminal(6).code, AiErrorCode::Timeout);
    }

    #[test]
    fn embed_terminal_maps_other_codes_to_backend_internal() {
        use nagori_core::AiErrorCode;
        for code in [4, 5, 99] {
            assert_eq!(
                super::embed_terminal(code).code,
                AiErrorCode::BackendInternal
            );
        }
    }

    #[test]
    fn read_f32_copies_buffer() {
        let values = [0.5_f32, -1.0, 2.5];
        let copied = super::read_f32(values.as_ptr(), values.len());
        assert_eq!(copied, vec![0.5, -1.0, 2.5]);
        assert!(super::read_f32(std::ptr::null(), 0).is_empty());
    }
}

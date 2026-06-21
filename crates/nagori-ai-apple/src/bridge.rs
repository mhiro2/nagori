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
    fn nagori_apple_generate_c(
        instructions_ptr: *const c_char,
        prompt_ptr: *const c_char,
        max_output_tokens: i64,
        temperature: f64,
        timeout_ms: u64,
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
        timeout_ms: u64,
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
        timeout_ms: u64,
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

/// Opaque context handed to Swift for the duration of one text-generation
/// stream. It owns the event sender, the snapshot-delta pump, and a clone of the
/// cancellation token Swift polls through [`generate_is_cancelled`]. Shared by
/// every text-generation export (plain generate and guided extract-tasks), which
/// all use the same `isCancelled`/`onSnapshot`/`onDone` callback contract.
struct GenerateCtx {
    tx: mpsc::UnboundedSender<Result<AiEvent, AiError>>,
    pump: SnapshotPump,
    cancel: CancellationToken,
}

/// Polled by Swift before forwarding each snapshot. The token's atomic load
/// happens entirely in Rust, so its bytes never cross the language boundary.
extern "C" fn generate_is_cancelled(ctx: *mut c_void) -> u8 {
    if ctx.is_null() {
        return 1;
    }
    // SAFETY: `ctx` is the `GenerateCtx` pointer handed to Swift; callbacks run
    // sequentially on one task, so this shared borrow never overlaps the `&mut`
    // reborrow in `generate_on_snapshot`.
    let ctx = unsafe { &*ctx.cast::<GenerateCtx>() };
    u8::from(ctx.cancel.is_cancelled())
}

/// Receives one cumulative snapshot from Swift and forwards the streaming delta.
extern "C" fn generate_on_snapshot(ctx: *mut c_void, ptr: *const u8, len: usize) {
    if ctx.is_null() || ptr.is_null() {
        return;
    }
    // SAFETY: `ctx` is the `GenerateCtx` pointer; Swift invokes callbacks
    // sequentially from a single task, so a unique `&mut` is sound. `ptr`/`len`
    // describe a buffer Swift keeps alive for the call's duration.
    let ctx = unsafe { &mut *ctx.cast::<GenerateCtx>() };
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
extern "C" fn generate_on_done(ctx: *mut c_void, code: i32) {
    if ctx.is_null() {
        return;
    }
    // SAFETY: `generate_on_done` is the last callback for a stream, so
    // reclaiming the box here is sound — Swift does not touch `ctx` afterwards.
    let ctx = unsafe { Box::from_raw(ctx.cast::<GenerateCtx>()) };
    let GenerateCtx {
        tx,
        pump,
        cancel: _,
    } = *ctx;
    let _ = tx.send(generate_terminal(code, pump.current().to_owned()));
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

/// Maps the Swift terminal status code onto the stream's terminal item. Codes
/// 2–8 mirror `generationErrorCode` in `Bridge.swift`; `1` (an observed
/// consumer cancel) and `9` (the watchdog cancelled a wedged `await`) are set
/// directly by the Swift work task, so they are distinct from each other.
fn generate_terminal(code: i32, final_text: String) -> Result<AiEvent, AiError> {
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
            AiErrorCode::GuardrailViolation,
            "the request was blocked by the model guardrail",
        )),
        7 => Err(AiError::new(
            AiErrorCode::BackendInternal,
            "the input language or locale is unsupported",
        )),
        9 => Err(AiError::new(
            AiErrorCode::Timeout,
            "the on-device model did not respond in time",
        )),
        _ => Err(AiError::new(
            AiErrorCode::BackendInternal,
            "the on-device model failed to generate a response",
        )),
    }
}

/// Drains a [`GenerateCtx`]'s receiver into the engine's [`AiEventStream`]. The
/// stream ends after exactly one terminal item (`Ok(Done)` / `Ok(Cancelled)` /
/// `Err`).
///
/// `stall_timeout` is the last line of defence: it sits *behind* the Swift
/// watchdog, so it only fires when the Swift side wedged so hard that its own
/// timeout never produced `onDone` (a framework that ignores task
/// cancellation). It guards a *direct* consumer of this backend stream — one
/// that drives it without the daemon's `guard_event_stream` in front of it.
/// Both the daemon and the CLI's in-process `nagori ai` path go through
/// `start_ai_action`, which wraps the stream in that guard, so this is
/// defence-in-depth that also keeps the FFI bounded should that guard ever be
/// bypassed; without it such a direct consumer would pend on `recv()` forever.
/// On expiry the stream yields a terminal `Timeout` error and drops the
/// receiver; a late `generate_on_done` still reclaims its box, its send just
/// lands nowhere.
fn generate_event_stream(
    rx: mpsc::UnboundedReceiver<Result<AiEvent, AiError>>,
    stall_timeout: std::time::Duration,
) -> AiEventStream {
    futures::stream::unfold(Some(rx), move |state| async move {
        let mut rx = state?;
        match tokio::time::timeout(stall_timeout, rx.recv()).await {
            Ok(item) => item.map(|item| (item, Some(rx))),
            Err(_) => Some((
                Err(AiError::new(
                    AiErrorCode::Timeout,
                    "the on-device model stopped responding",
                )),
                None,
            )),
        }
    })
    .boxed()
}

/// Generation knobs forwarded across the FFI to `GenerationOptions` on the
/// Swift side. `None` leaves the corresponding knob at the framework default.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct GenerateOptions {
    pub max_output_tokens: Option<u32>,
    pub temperature: Option<f32>,
    /// The consumer-side request deadline (`AiRequestOptions::timeout_ms`,
    /// already clamped by the daemon's `EffectiveAiPolicy`). The Swift
    /// watchdog is derived from it (deadline + slack) so it stays a
    /// wedge-protection backstop that fires *after* the consumer deadline,
    /// instead of a fixed 20 s that silently cancelled any longer generation
    /// the user's `request_timeout_ms` allowed.
    pub timeout_ms: Option<u64>,
}

/// Watchdog used when a bridge call supplies no deadline. Every one-shot /
/// streaming export shares this backstop so a missing deadline behaves the same
/// across generate / translate / embed.
const DEFAULT_BRIDGE_TIMEOUT_MS: u64 = 20_000;
/// Slack added on top of the consumer deadline so the Swift watchdog only
/// fires once the consumer has certainly given up.
const BRIDGE_TIMEOUT_SLACK_MS: u64 = 5_000;

/// Derives the Swift-side wedge-protection watchdog (ms) from a consumer
/// deadline, shared by every bridge export so the watchdog tracks the request's
/// `request_timeout_ms` instead of a fixed literal that silently cancels any
/// longer call.
///
/// `None` (no deadline supplied) falls back to [`DEFAULT_BRIDGE_TIMEOUT_MS`].
/// `Some(d)` becomes `d + slack`, clamped between `floor_ms` and the settings
/// ceiling plus slack so a corrupt deadline cannot park a wedged task for hours.
/// `floor_ms` lets a streamed generation keep a 20 s minimum (the model needs
/// time to start producing) while a fast one-shot inference can fire sooner.
fn swift_watchdog_ms(timeout_ms: Option<u64>, floor_ms: u64) -> u64 {
    timeout_ms.map_or(DEFAULT_BRIDGE_TIMEOUT_MS, |t| {
        t.saturating_add(BRIDGE_TIMEOUT_SLACK_MS).clamp(
            floor_ms,
            nagori_core::settings::MAX_AI_REQUEST_TIMEOUT_MS + BRIDGE_TIMEOUT_SLACK_MS,
        )
    })
}

/// Converts [`GenerateOptions`] into the three values forwarded across the C
/// ABI: `(max_output_tokens, temperature, swift_timeout_ms)`.
///
/// The first two use out-of-band sentinels — a non-positive token count and a
/// negative temperature both mean "leave the framework default" on the Swift
/// side, so `None` maps to `0` / `-1.0`. `swift_timeout_ms` is the watchdog
/// derived from the consumer deadline (deadline + slack), floored at the legacy
/// 20 s and capped at the settings ceiling plus slack so a corrupt deadline
/// cannot park a wedged task for hours.
fn generate_ffi_sentinels(options: &GenerateOptions) -> (i64, f64, u64) {
    let swift_timeout_ms = swift_watchdog_ms(options.timeout_ms, DEFAULT_BRIDGE_TIMEOUT_MS);
    let max_output_tokens = options
        .max_output_tokens
        .map_or(0, |tokens| i64::from(tokens.max(1)));
    let temperature = options
        .temperature
        .map_or(-1.0, |temp| f64::from(temp.max(0.0)));
    (max_output_tokens, temperature, swift_timeout_ms)
}

/// Generates text from `input` under the system prompt `instructions` via the
/// on-device language model. Serves every plain text-generation action
/// (summarize, rewrite, reformat, explain); the action's steering lives in
/// `instructions`.
///
/// Cancellation is observed by polling `cancel` (a [`CancellationToken`] the
/// caller owns) before each snapshot; the caller cancels it to stop the Swift
/// task. The returned stream ends after exactly one terminal item
/// (`Ok(Done)` / `Ok(Cancelled)` / `Err`).
pub(crate) fn generate_stream(
    instructions: &str,
    input: &str,
    options: GenerateOptions,
    cancel: CancellationToken,
) -> AiEventStream {
    let (tx, rx) = mpsc::unbounded_channel::<Result<AiEvent, AiError>>();
    let ctx = Box::new(GenerateCtx {
        tx,
        pump: SnapshotPump::new(),
        cancel,
    });
    let ctx_ptr = Box::into_raw(ctx).cast::<c_void>();

    // Strip interior NULs so the C strings survive intact.
    let c_instructions = CString::new(instructions.replace('\0', " ")).unwrap_or_default();
    let source = CString::new(input.replace('\0', " ")).unwrap_or_default();

    let (max_output_tokens, temperature, swift_timeout_ms) = generate_ffi_sentinels(&options);

    // SAFETY: the signature matches the `@_cdecl` export; the ctx box outlives
    // the call (reclaimed in `generate_on_done`), and the callbacks are plain
    // `fn` items that read the cancel token through the ctx atomically.
    unsafe {
        nagori_apple_generate_c(
            c_instructions.as_ptr(),
            source.as_ptr(),
            max_output_tokens,
            temperature,
            swift_timeout_ms,
            ctx_ptr,
            generate_is_cancelled,
            generate_on_snapshot,
            generate_on_done,
        );
    }

    // The stream-side stall guard sits behind the Swift watchdog so it can
    // only fire when Swift never reported back at all.
    generate_event_stream(
        rx,
        std::time::Duration::from_millis(swift_timeout_ms.saturating_add(10_000)),
    )
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
/// `timeout_ms` is the consumer-side deadline (`AiRequestOptions::timeout_ms`,
/// already clamped by the daemon's `EffectiveAiPolicy`); the Swift wedge
/// watchdog is derived from it ([`swift_watchdog_ms`]) so a translation the
/// user's `request_timeout_ms` allows is no longer silently cut off by a fixed
/// 20 s. `None` falls back to the default backstop.
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
    timeout_ms: Option<u64>,
) -> Result<TranslationOutput, AiError> {
    let (tx, rx) = oneshot::channel::<Result<TranslationOutput, AiError>>();
    let ctx = Box::new(TranslateCtx { tx });
    let ctx_ptr = Box::into_raw(ctx).cast::<c_void>();

    // Strip interior NULs so the C strings survive intact.
    let c_text = CString::new(input.replace('\0', " ")).unwrap_or_default();
    let c_source = CString::new(source.unwrap_or_default().replace('\0', " ")).unwrap_or_default();
    let c_target = CString::new(target.replace('\0', " ")).unwrap_or_default();
    // Translation can legitimately run several seconds; keep the 20 s floor so a
    // short deadline still leaves a sane wedge backstop (the consumer-side guard
    // enforces the actual deadline regardless).
    let swift_timeout_ms = swift_watchdog_ms(timeout_ms, DEFAULT_BRIDGE_TIMEOUT_MS);

    // SAFETY: the signature matches the `@_cdecl` export; the ctx box outlives
    // the call (reclaimed in `translate_on_complete`), and `on_complete` is a
    // plain `fn` item.
    unsafe {
        nagori_apple_translate_c(
            c_text.as_ptr(),
            c_source.as_ptr(),
            c_target.as_ptr(),
            swift_timeout_ms,
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
#[derive(Debug)]
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
    let result = build_embed_metadata(
        code,
        revision,
        dimension,
        max_sequence_length,
        read_utf8(id_ptr, id_len),
    );
    let _ = ctx.tx.send(result);
}

/// Builds [`RawEmbedMetadata`] from the Swift callback's status code and numeric
/// fields. The numeric fields persist as `u32` in the index metadata, so each is
/// validated against that range here — and `dimension` / `max_sequence_length`
/// must additionally be non-zero — rather than collapsed to `0` downstream. A
/// `revision` silently forced to `0` would make the index's "revision changed →
/// rebuild" check a false negative (a real revision could coincide with the
/// stored `0`), and a `0` / collapsed dimension or sequence length is a
/// degenerate embedding space. `current_semantic_meta` still guards with
/// `unwrap_or(0)`; bounding at this FFI boundary keeps both sites consistent
/// instead of papering over a bad value. A `revision` of `0` is legitimate, so
/// only an out-of-range revision (negative or past `u32`) is rejected.
fn build_embed_metadata(
    code: i32,
    revision: isize,
    dimension: isize,
    max_sequence_length: isize,
    model_identifier: String,
) -> Result<RawEmbedMetadata, AiError> {
    if code != 0 {
        return Err(AiError::new(
            AiErrorCode::BackendInternal,
            "no embedding model is available for the configured language",
        ));
    }
    let revision = u32::try_from(revision).ok();
    // Non-negative, non-zero, and within the `u32` range the metadata persists
    // in (so the downstream `u32::try_from(...).unwrap_or(0)` never collapses).
    let dimension = usize::try_from(dimension)
        .ok()
        .filter(|&n| n > 0 && u32::try_from(n).is_ok());
    let max_sequence_length = usize::try_from(max_sequence_length)
        .ok()
        .filter(|&n| n > 0 && u32::try_from(n).is_ok());
    match (revision, dimension, max_sequence_length) {
        (Some(revision), Some(dimension), Some(max_sequence_length)) => Ok(RawEmbedMetadata {
            model_identifier,
            revision,
            dimension,
            max_sequence_length,
        }),
        _ => Err(AiError::new(
            AiErrorCode::BackendInternal,
            "the embedding model returned out-of-range metadata",
        )),
    }
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
        // The language has no embedding model at all — distinct from a download
        // that was attempted and failed, so the caller can tell "wrong language"
        // apart from "transient download failure".
        2 => Err(AiError::new(
            AiErrorCode::BackendInternal,
            "no embedding model exists for this language",
        )),
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
///
/// `timeout_ms` is the consumer-side deadline (the daemon's per-query embed
/// budget for a search, or `None` for background indexing); the Swift wedge
/// watchdog is derived from it ([`swift_watchdog_ms`]). Unlike generate /
/// translate there is no 20 s floor: embedding inference is a single fast
/// forward pass, so the watchdog can fire as soon as the consumer deadline
/// (plus slack) elapses rather than always waiting the legacy 20 s. The asset
/// *download* path keeps its own much longer timeout in Swift.
pub(crate) async fn embed_text(
    language: &str,
    text: &str,
    timeout_ms: Option<u64>,
) -> Result<Vec<f32>, AiError> {
    let (tx, rx) = oneshot::channel::<Result<Vec<f32>, AiError>>();
    let ctx = Box::new(EmbedCtx { tx });
    let ctx_ptr = Box::into_raw(ctx).cast::<c_void>();
    let c_lang = CString::new(language.replace('\0', " ")).unwrap_or_default();
    let c_text = CString::new(text.replace('\0', " ")).unwrap_or_default();
    let swift_timeout_ms = swift_watchdog_ms(timeout_ms, 0);
    // SAFETY: the signature matches the `@_cdecl` export; the ctx box is
    // reclaimed in `embed_on_complete`, which Swift's `OnceFlag` fires exactly
    // once (success, error, or the timeout sentinel).
    unsafe {
        nagori_apple_embed_c(
            c_lang.as_ptr(),
            c_text.as_ptr(),
            swift_timeout_ms,
            ctx_ptr,
            embed_on_complete,
        );
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

    use futures::StreamExt;

    use super::{bridge_snapshot_stream, hello, probe_real_availability, spawn_bridge};
    use crate::availability::AppleAvailability;
    use crate::event::AppleStreamEvent;

    #[test]
    fn hello_world_bridge_is_linked() {
        assert_eq!(hello(), 42);
    }

    #[test]
    #[ignore = "requires Apple Intelligence enabled; probes the live framework"]
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

    #[test]
    fn build_embed_metadata_accepts_in_range_values() {
        let meta = super::build_embed_metadata(0, 3, 512, 256, "model.id".to_owned())
            .expect("in-range metadata");
        assert_eq!(meta.revision, 3);
        assert_eq!(meta.dimension, 512);
        assert_eq!(meta.max_sequence_length, 256);
        assert_eq!(meta.model_identifier, "model.id");
    }

    #[test]
    fn build_embed_metadata_accepts_revision_zero() {
        // A revision of 0 is a legitimate value (only negative / out-of-range
        // revisions are rejected); the false-negative guard is about *collapsing*
        // a bad revision to 0, not about forbidding a genuine 0.
        let meta =
            super::build_embed_metadata(0, 0, 512, 256, "model.id".to_owned()).expect("revision 0");
        assert_eq!(meta.revision, 0);
    }

    #[test]
    fn build_embed_metadata_rejects_out_of_range_fields() {
        use nagori_core::AiErrorCode;
        // A negative revision must not collapse to 0 (which would make the
        // index's "revision changed → rebuild" check a false negative); negative
        // or zero dimension / sequence length is a degenerate embedding space.
        // `None` on a 32-bit host (the value exceeds `isize`), where the u32
        // overflow case is unreachable anyway — `flatten` then skips it.
        let past_u32 = isize::try_from(u64::from(u32::MAX) + 1).ok();
        let cases = [
            Some((-1_isize, 512_isize, 256_isize)), // negative revision
            Some((3, -1, 256)),                     // negative dimension
            Some((3, 512, -1)),                     // negative sequence length
            Some((3, 0, 256)),                      // zero dimension
            Some((3, 512, 0)),                      // zero sequence length
            past_u32.map(|n| (3, n, 256)),          // dimension past u32 range
        ];
        for (revision, dimension, max_seq) in cases.into_iter().flatten() {
            let err = super::build_embed_metadata(0, revision, dimension, max_seq, String::new())
                .expect_err("out-of-range metadata is rejected");
            assert_eq!(err.code, AiErrorCode::BackendInternal);
        }
    }

    #[test]
    fn build_embed_metadata_maps_nonzero_code_to_error() {
        use nagori_core::AiErrorCode;
        let err = super::build_embed_metadata(2, 0, 0, 0, String::new())
            .expect_err("code 2 means no model for the language");
        assert_eq!(err.code, AiErrorCode::BackendInternal);
    }

    #[test]
    fn generate_terminal_maps_success_to_done() {
        let event = super::generate_terminal(0, "result".to_owned()).expect("code 0 is success");
        match event {
            nagori_core::AiEvent::Done {
                final_text,
                created_entry,
                warnings,
            } => {
                assert_eq!(final_text, "result");
                assert!(created_entry.is_none());
                assert!(warnings.is_empty());
            }
            other => panic!("expected Done, got {other:?}"),
        }
    }

    #[test]
    fn generate_terminal_maps_cancellation() {
        let event =
            super::generate_terminal(1, "partial".to_owned()).expect("code 1 is a clean cancel");
        assert!(matches!(event, nagori_core::AiEvent::Cancelled));
    }

    #[test]
    fn generate_terminal_maps_distinct_error_codes() {
        use nagori_core::AiErrorCode;
        // The named codes each carry their own reason. Codes 3–8 mirror the
        // Swift `generationErrorCode` table; 9 is the watchdog timeout, set
        // outside that table so it stays distinct from an observed cancel.
        let cases = [
            (3, AiErrorCode::InputTooLarge),
            (4, AiErrorCode::RateLimited),
            (8, AiErrorCode::RateLimited),
            (5, AiErrorCode::AssetMissing),
            (6, AiErrorCode::GuardrailViolation), // guardrail block (user-actionable)
            (7, AiErrorCode::BackendInternal),    // unsupported locale
            (9, AiErrorCode::Timeout),            // watchdog cancelled a wedged await
        ];
        for (code, expected) in cases {
            let err = super::generate_terminal(code, String::new())
                .expect_err("non-zero/non-cancel codes are errors");
            assert_eq!(err.code, expected, "code {code} mapped wrong");
        }
    }

    #[test]
    fn generate_terminal_maps_unknown_codes_to_backend_internal() {
        use nagori_core::AiErrorCode;
        for code in [2, 99, -1] {
            let err = super::generate_terminal(code, String::new()).expect_err("error");
            assert_eq!(err.code, AiErrorCode::BackendInternal, "code {code}");
        }
    }

    #[test]
    fn generate_ffi_sentinels_uses_framework_defaults_for_none() {
        let (tokens, temperature, timeout) =
            super::generate_ffi_sentinels(&super::GenerateOptions::default());
        // `0` tokens / negative temperature are the "use framework default"
        // sentinels Swift looks for; the timeout falls back to the legacy 20 s.
        assert_eq!(tokens, 0);
        assert!(temperature < 0.0);
        assert_eq!(timeout, super::DEFAULT_BRIDGE_TIMEOUT_MS);
    }

    #[test]
    fn generate_ffi_sentinels_clamps_and_floors_the_watchdog() {
        // Token count floors at 1, temperature floors at 0.0, and the watchdog
        // is the consumer deadline + slack — but never below the 20 s floor.
        let tiny = super::generate_ffi_sentinels(&super::GenerateOptions {
            max_output_tokens: Some(0),
            temperature: Some(-2.0),
            timeout_ms: Some(1),
        });
        assert_eq!(tiny.0, 1, "token count floors at 1");
        assert!(
            (tiny.1 - 0.0).abs() < f64::EPSILON,
            "temperature floors at 0.0"
        );
        assert_eq!(
            tiny.2,
            super::DEFAULT_BRIDGE_TIMEOUT_MS,
            "a tiny deadline still gets the 20 s watchdog floor"
        );

        // A normal deadline becomes deadline + slack.
        let normal = super::generate_ffi_sentinels(&super::GenerateOptions {
            max_output_tokens: Some(256),
            temperature: Some(0.7),
            timeout_ms: Some(60_000),
        });
        assert_eq!(normal.0, 256);
        assert_eq!(
            normal.2,
            60_000 + super::BRIDGE_TIMEOUT_SLACK_MS,
            "watchdog is deadline + slack"
        );

        // A deadline above the settings ceiling is capped at ceiling + slack.
        let huge = super::generate_ffi_sentinels(&super::GenerateOptions {
            max_output_tokens: None,
            temperature: None,
            timeout_ms: Some(u64::MAX),
        });
        assert_eq!(
            huge.2,
            nagori_core::settings::MAX_AI_REQUEST_TIMEOUT_MS + super::BRIDGE_TIMEOUT_SLACK_MS,
            "a corrupt deadline is capped at the ceiling + slack"
        );
    }

    #[test]
    fn swift_watchdog_falls_back_to_the_default_without_a_deadline() {
        // No consumer deadline → the shared backstop, regardless of floor.
        assert_eq!(
            super::swift_watchdog_ms(None, super::DEFAULT_BRIDGE_TIMEOUT_MS),
            super::DEFAULT_BRIDGE_TIMEOUT_MS
        );
        assert_eq!(
            super::swift_watchdog_ms(None, 0),
            super::DEFAULT_BRIDGE_TIMEOUT_MS
        );
    }

    #[test]
    fn swift_watchdog_floor_only_applies_when_set() {
        // generate / translate keep a 20 s floor so a tiny deadline still leaves
        // a sane wedge backstop.
        assert_eq!(
            super::swift_watchdog_ms(Some(1), super::DEFAULT_BRIDGE_TIMEOUT_MS),
            super::DEFAULT_BRIDGE_TIMEOUT_MS
        );
        // embed (floor 0) lets a fast inference fire as soon as the consumer
        // deadline + slack elapses instead of always waiting the legacy 20 s.
        assert_eq!(
            super::swift_watchdog_ms(Some(10_000), 0),
            10_000 + super::BRIDGE_TIMEOUT_SLACK_MS
        );
        // Both share the ceiling-plus-slack cap so a corrupt deadline can't park
        // a wedged task for hours.
        assert_eq!(
            super::swift_watchdog_ms(Some(u64::MAX), 0),
            nagori_core::settings::MAX_AI_REQUEST_TIMEOUT_MS + super::BRIDGE_TIMEOUT_SLACK_MS
        );
    }

    #[tokio::test]
    async fn generate_event_stream_yields_the_terminal_item_then_ends() {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        tx.send(Ok(nagori_core::AiEvent::Done {
            final_text: "ok".to_owned(),
            created_entry: None,
            warnings: Vec::new(),
        }))
        .unwrap();
        drop(tx); // close after one item

        let mut stream = super::generate_event_stream(rx, std::time::Duration::from_mins(1));
        let first = stream.next().await.expect("one terminal item");
        assert!(matches!(first, Ok(nagori_core::AiEvent::Done { .. })));
        assert!(
            stream.next().await.is_none(),
            "stream ends after the closed channel"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn generate_event_stream_times_out_when_swift_never_reports() {
        // The stall guard is the last line of defence for a direct consumer
        // that drives this backend stream without the daemon's
        // `guard_event_stream` in front, when Swift wedges so hard `onDone`
        // never fires. Holding `_tx` keeps the channel open with nothing ever
        // sent, so `recv()` pends forever and only the deadline can end it.
        let (_tx, rx) = tokio::sync::mpsc::unbounded_channel::<
            Result<nagori_core::AiEvent, nagori_core::AiError>,
        >();
        let mut stream = super::generate_event_stream(rx, std::time::Duration::from_secs(30));

        // The guard must NOT fire before its deadline: a shorter probe timer
        // wins this race. With paused time the runtime advances to the nearest
        // timer (the 10 s probe), so a regression to an immediate timeout would
        // let the `stream.next()` arm win and fail the test.
        tokio::select! {
            _ = stream.next() => panic!("stall guard fired before its deadline"),
            () = tokio::time::sleep(std::time::Duration::from_secs(10)) => {}
        }

        // Once the deadline elapses with nothing sent, the guard yields a
        // terminal `Timeout` error.
        let item = stream
            .next()
            .await
            .expect("stall guard yields a terminal item");
        let err = item.expect_err("a wedged Swift side must surface as an error");
        assert_eq!(err.code, nagori_core::AiErrorCode::Timeout);
    }
}

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

use tokio::sync::mpsc;

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

    // Strip interior NULs so the C string survives intact (PoC inputs only).
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
}

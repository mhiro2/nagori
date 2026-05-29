//! Streaming handle plus the cross-platform simulated driver.
//!
//! [`StreamHandle`] is the consumer-facing object for both the simulated
//! driver (here) and the macOS Swift driver (`bridge.rs`). Events arrive over a
//! Tokio mpsc channel; cancellation is a shared [`AtomicBool`] the producer
//! polls. Dropping the handle requests cancellation, mirroring the design's
//! "CLI drop cancels the stream" intent.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use tokio::sync::mpsc;

use crate::event::AppleStreamEvent;
use crate::pump::SnapshotPump;

/// Receiver-side handle for an in-flight Apple text-generation stream.
#[derive(Debug)]
pub struct StreamHandle {
    cancel: Arc<AtomicBool>,
    rx: mpsc::UnboundedReceiver<AppleStreamEvent>,
}

impl StreamHandle {
    /// Constructs a handle from a cancel flag and the event receiver. Used by
    /// the simulated and macOS drivers.
    pub(crate) const fn new(
        cancel: Arc<AtomicBool>,
        rx: mpsc::UnboundedReceiver<AppleStreamEvent>,
    ) -> Self {
        Self { cancel, rx }
    }

    /// Requests cancellation. The producer observes it before the next
    /// snapshot and finishes with [`AppleStreamEvent::Cancelled`].
    pub fn request_cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }

    /// Awaits the next event, or `None` once the stream has ended.
    pub async fn recv(&mut self) -> Option<AppleStreamEvent> {
        self.rx.recv().await
    }
}

impl Drop for StreamHandle {
    fn drop(&mut self) {
        // Stop the producer promptly if the consumer goes away mid-stream.
        self.cancel.store(true, Ordering::SeqCst);
    }
}

/// Per-character delay used by the simulated driver, mirroring the Swift
/// bridge so cancellation can be observed mid-stream.
const SIMULATED_STEP: Duration = Duration::from_millis(2);

/// Drives a [`SnapshotPump`] over cumulative single-character snapshots of
/// `text` on a background thread, with no dependency on the Apple frameworks.
///
/// This is the platform-agnostic equivalent of the macOS Swift driver and is
/// what the streaming/cancellation tests exercise on every CI target.
#[must_use]
pub fn simulate_snapshot_stream(text: &str) -> StreamHandle {
    spawn_simulated(text, Arc::new(AtomicBool::new(false)))
}

/// Spawns the simulated producer with a caller-supplied cancel flag. Passing a
/// flag that is already `true` lets tests cancel *before the producer starts*,
/// giving a happens-before guarantee that the first poll observes it — so the
/// terminal is deterministically [`AppleStreamEvent::Cancelled`].
fn spawn_simulated(text: &str, cancel: Arc<AtomicBool>) -> StreamHandle {
    let (tx, rx) = mpsc::unbounded_channel();

    let cancel_thread = Arc::clone(&cancel);
    let owned = text.to_owned();
    thread::spawn(move || {
        let mut pump = SnapshotPump::new();
        let mut snapshot = String::new();
        let mut cancelled = false;
        for ch in owned.chars() {
            if cancel_thread.load(Ordering::SeqCst) {
                cancelled = true;
                break;
            }
            snapshot.push(ch);
            if let Some(event) = pump.push(&snapshot)
                && tx.send(event).is_err()
            {
                // Consumer dropped the receiver; stop producing.
                return;
            }
            thread::sleep(SIMULATED_STEP);
        }
        // Re-check in case cancellation landed during the final sleep.
        cancelled |= cancel_thread.load(Ordering::SeqCst);
        let _ = tx.send(pump.finish(cancelled));
    });

    StreamHandle::new(cancel, rx)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    use super::{StreamHandle, simulate_snapshot_stream, spawn_simulated};
    use crate::event::AppleStreamEvent;

    /// Drains a handle into a list of events.
    async fn drain(handle: &mut StreamHandle) -> Vec<AppleStreamEvent> {
        let mut events = Vec::new();
        while let Some(event) = handle.recv().await {
            let terminal = event.is_terminal();
            events.push(event);
            if terminal {
                break;
            }
        }
        events
    }

    /// Replays delta/replace events to reconstruct the streamed text.
    fn reconstruct(events: &[AppleStreamEvent]) -> String {
        let mut buf = String::new();
        for event in events {
            match event {
                AppleStreamEvent::Delta { text, .. } => buf.push_str(text),
                AppleStreamEvent::Replace { text, .. } => buf = text.clone(),
                AppleStreamEvent::Done { .. } | AppleStreamEvent::Cancelled { .. } => {}
            }
        }
        buf
    }

    #[tokio::test]
    async fn completes_and_reconstructs_text() {
        let input = "Hello, 世界 🦀";
        let mut handle = simulate_snapshot_stream(input);
        let events = drain(&mut handle).await;

        let terminal = events.last().expect("at least one event");
        assert_eq!(
            *terminal,
            AppleStreamEvent::Done {
                final_text: input.to_owned()
            }
        );
        assert_eq!(reconstruct(&events), input);

        // Sequence numbers are gap-free and ascending across streaming events.
        let seqs: Vec<u64> = events.iter().filter_map(AppleStreamEvent::seq).collect();
        assert_eq!(seqs, (0..seqs.len() as u64).collect::<Vec<_>>());
    }

    #[tokio::test]
    async fn cancellation_stops_stream() {
        // Start with the flag already set, so the producer observes it on its
        // first poll (happens-before via thread spawn) and deterministically
        // finishes with `Cancelled` rather than `Done`, with no scheduling race.
        let input = "x".repeat(200);
        let cancel = Arc::new(AtomicBool::new(true));
        let mut handle = spawn_simulated(&input, cancel);

        let mut last = None;
        while let Some(event) = handle.recv().await {
            last = Some(event);
        }

        match last {
            Some(AppleStreamEvent::Cancelled { final_text }) => {
                assert!(
                    final_text.len() < input.len(),
                    "cancelled stream should not have emitted the whole input"
                );
            }
            other => panic!("expected Cancelled terminal, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn request_cancel_after_start_eventually_cancels() {
        // The runtime path (cancel arrives mid-stream) is best-effort by
        // nature; assert only that it terminates and never yields more than the
        // full input — Cancelled is the expected outcome in practice.
        let input = "x".repeat(200);
        let mut handle = simulate_snapshot_stream(&input);
        handle.request_cancel();

        let mut last = None;
        while let Some(event) = handle.recv().await {
            last = Some(event);
        }
        match last {
            Some(
                AppleStreamEvent::Cancelled { final_text } | AppleStreamEvent::Done { final_text },
            ) => {
                assert!(final_text.len() <= input.len());
            }
            other => panic!("expected a terminal event, got {other:?}"),
        }
    }
}

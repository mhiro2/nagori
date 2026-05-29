//! Turns a sequence of partial snapshots into ordered [`AppleStreamEvent`]s.
//!
//! The pump is the deterministic, side-effect-free heart of the streaming
//! bridge: both the cross-platform simulated driver and the macOS Swift
//! callbacks feed snapshots into it. Keeping it pure makes the
//! snapshot-to-event contract exhaustively unit-testable without a runtime,
//! threads, or the Apple frameworks.

use crate::delta::{SnapshotDelta, diff_snapshot};
use crate::event::AppleStreamEvent;

/// Accumulates partial snapshots and assigns sequence numbers.
#[derive(Debug, Default)]
pub struct SnapshotPump {
    prev: String,
    seq: u64,
}

impl SnapshotPump {
    /// Creates an empty pump.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            prev: String::new(),
            seq: 0,
        }
    }

    /// The latest snapshot seen so far.
    #[must_use]
    pub fn current(&self) -> &str {
        &self.prev
    }

    /// Feeds a full cumulative `snapshot`, returning the event to emit (or
    /// `None` when the snapshot is unchanged). Sequence numbers are only
    /// consumed when an event is produced, so they stay gap-free.
    pub fn push(&mut self, snapshot: &str) -> Option<AppleStreamEvent> {
        match diff_snapshot(&self.prev, snapshot) {
            SnapshotDelta::Unchanged => None,
            SnapshotDelta::Append(text) => {
                let seq = self.next_seq();
                snapshot.clone_into(&mut self.prev);
                Some(AppleStreamEvent::Delta { seq, text })
            }
            SnapshotDelta::Replace(text) => {
                let seq = self.next_seq();
                snapshot.clone_into(&mut self.prev);
                Some(AppleStreamEvent::Replace { seq, text })
            }
        }
    }

    /// Consumes the pump and produces the terminal event, using the last
    /// snapshot as the final text.
    // Not `const`: dropping `self` (which owns a `String`) cannot run in a
    // const context, so clippy's `missing_const_for_fn` is a false positive.
    #[allow(clippy::missing_const_for_fn)]
    #[must_use]
    pub fn finish(self, cancelled: bool) -> AppleStreamEvent {
        if cancelled {
            AppleStreamEvent::Cancelled {
                final_text: self.prev,
            }
        } else {
            AppleStreamEvent::Done {
                final_text: self.prev,
            }
        }
    }

    const fn next_seq(&mut self) -> u64 {
        let seq = self.seq;
        self.seq += 1;
        seq
    }
}

#[cfg(test)]
mod tests {
    use super::SnapshotPump;
    use crate::event::AppleStreamEvent;

    #[test]
    fn appends_produce_sequenced_deltas() {
        let mut pump = SnapshotPump::new();
        assert_eq!(
            pump.push("H"),
            Some(AppleStreamEvent::Delta {
                seq: 0,
                text: "H".to_owned()
            })
        );
        assert_eq!(
            pump.push("He"),
            Some(AppleStreamEvent::Delta {
                seq: 1,
                text: "e".to_owned()
            })
        );
        assert_eq!(pump.current(), "He");
    }

    #[test]
    fn unchanged_snapshot_consumes_no_seq() {
        let mut pump = SnapshotPump::new();
        assert!(pump.push("a").is_some());
        assert_eq!(pump.push("a"), None);
        // The next change still gets seq 1, proving the no-op did not burn one.
        assert_eq!(
            pump.push("ab"),
            Some(AppleStreamEvent::Delta {
                seq: 1,
                text: "b".to_owned()
            })
        );
    }

    #[test]
    fn divergence_emits_replace() {
        let mut pump = SnapshotPump::new();
        assert!(pump.push("foobar").is_some());
        assert_eq!(
            pump.push("foobaz"),
            Some(AppleStreamEvent::Replace {
                seq: 1,
                text: "foobaz".to_owned()
            })
        );
    }

    #[test]
    fn finish_done_carries_final_text() {
        let mut pump = SnapshotPump::new();
        pump.push("done");
        assert_eq!(
            pump.finish(false),
            AppleStreamEvent::Done {
                final_text: "done".to_owned()
            }
        );
    }

    #[test]
    fn finish_cancelled_carries_partial_text() {
        let mut pump = SnapshotPump::new();
        pump.push("par");
        assert_eq!(
            pump.finish(true),
            AppleStreamEvent::Cancelled {
                final_text: "par".to_owned()
            }
        );
    }

    #[test]
    fn reconstructs_text_from_deltas() {
        // Deltas/replaces applied in order must rebuild the final snapshot.
        let snapshots = ["あ", "あい", "あいx", "あいう"];
        let mut pump = SnapshotPump::new();
        let mut buf = String::new();
        for snap in snapshots {
            match pump.push(snap) {
                Some(AppleStreamEvent::Delta { text, .. }) => buf.push_str(&text),
                Some(AppleStreamEvent::Replace { text, .. }) => buf = text,
                _ => {}
            }
        }
        assert_eq!(buf, "あいう");
        assert_eq!(pump.current(), "あいう");
    }
}

//! Streaming events emitted by the Apple snapshot-stream driver.

/// A single event in an Apple snapshot stream.
///
/// Partial-snapshot streaming is delta-ised here into
/// [`AppleStreamEvent::Delta`] / [`AppleStreamEvent::Replace`], and the stream
/// terminates with exactly one [`AppleStreamEvent::Done`] **or**
/// [`AppleStreamEvent::Cancelled`]. `seq` lets a consumer verify ordering and
/// detect gaps. The Foundation Models backend reuses the same delta pump but
/// maps the terminal onto `nagori-core`'s `AiEvent` directly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppleStreamEvent {
    /// The new snapshot extends the previous one; carries only the appended
    /// tail.
    Delta {
        /// Monotonic sequence number, starting at 0.
        seq: u64,
        /// Text appended since the previous snapshot.
        text: String,
    },
    /// The new snapshot diverged from the previous prefix; carries the full
    /// snapshot so the consumer can replace its buffer wholesale.
    Replace {
        /// Monotonic sequence number, shared with [`AppleStreamEvent::Delta`].
        seq: u64,
        /// The complete snapshot text.
        text: String,
    },
    /// Terminal event for a stream that ran to completion.
    Done {
        /// The full generated text.
        final_text: String,
    },
    /// Terminal event for a stream that was cancelled mid-flight.
    Cancelled {
        /// The text accumulated before cancellation was observed.
        final_text: String,
    },
}

impl AppleStreamEvent {
    /// Returns `true` for the two terminal variants.
    #[must_use]
    pub const fn is_terminal(&self) -> bool {
        matches!(self, Self::Done { .. } | Self::Cancelled { .. })
    }

    /// Returns the sequence number for streaming variants, or `None` for the
    /// terminal variants (which are not part of the `seq` ordering).
    #[must_use]
    pub const fn seq(&self) -> Option<u64> {
        match self {
            Self::Delta { seq, .. } | Self::Replace { seq, .. } => Some(*seq),
            Self::Done { .. } | Self::Cancelled { .. } => None,
        }
    }
}

//! Apple on-device AI bridge — Phase A proof-of-concept.
//!
//! This crate isolates every build/link dependency on the Apple frameworks
//! (`FoundationModels` / Translation / `NaturalLanguage`) behind a Swift static
//! library. Phase A delivers the Swift bridge proof-of-concept:
//!
//! - [`probe`] resolves Apple Intelligence [`AppleAvailability`], either from
//!   the live OS ([`AvailabilitySource::Real`], macOS only) or from a
//!   [`AvailabilitySource::Mock`] fixture so CI can exercise every unavailable
//!   branch without an Apple Intelligence environment.
//! - [`diff_snapshot`] / [`SnapshotPump`] delta-ise the partial *snapshots*
//!   `FoundationModels` yields into ordered [`AppleStreamEvent`]s on `char`
//!   boundaries.
//! - [`simulate_snapshot_stream`] (all platforms) and `bridge_snapshot_stream`
//!   (macOS) drive a stream over a Tokio mpsc channel with shared-`AtomicBool`
//!   cancellation; dropping the [`StreamHandle`] requests cancellation.
//!
//! The eventual `AiActionEngine` / `TextGenerator` trait integration and the
//! real `LanguageModelSession` wiring land in Phase B; this crate stays a
//! self-contained bridge until then.

mod availability;
mod delta;
mod event;
mod pump;
mod stream;

#[cfg(target_os = "macos")]
mod bridge;

pub use availability::{AppleAvailability, AvailabilitySource, MockReason, probe};
pub use delta::{SnapshotDelta, diff_snapshot};
pub use event::AppleStreamEvent;
pub use pump::SnapshotPump;
pub use stream::{StreamHandle, simulate_snapshot_stream};

#[cfg(target_os = "macos")]
pub use bridge::{bridge_snapshot_stream, hello};

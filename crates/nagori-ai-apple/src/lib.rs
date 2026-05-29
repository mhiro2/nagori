//! Apple on-device AI bridge.
//!
//! This crate isolates every build/link dependency on the Apple frameworks
//! (`FoundationModels` / Translation / `NaturalLanguage`) behind a Swift static
//! library, and exposes the result as `nagori-ai` backend implementations.
//!
//! - [`AppleFoundationBackend`] implements `nagori-ai`'s `TextGenerator` over
//!   `SystemLanguageModel`, streaming on-device summaries (macOS only).
//! - [`probe`] resolves Apple Intelligence [`AppleAvailability`], either from
//!   the live OS ([`AvailabilitySource::Real`], macOS only) or from a
//!   [`AvailabilitySource::Mock`] fixture so CI can exercise every unavailable
//!   branch without an Apple Intelligence environment.
//! - [`diff_snapshot`] / [`SnapshotPump`] delta-ise the partial *snapshots*
//!   `FoundationModels` yields into ordered events on `char` boundaries.
//! - [`simulate_snapshot_stream`] (all platforms) and `bridge_snapshot_stream`
//!   (macOS) drive a snapshot stream over a Tokio mpsc channel with
//!   shared-`AtomicBool` cancellation, exercising the streaming machinery
//!   without requiring Apple Intelligence to be enabled.

mod availability;
mod delta;
mod event;
mod pump;
mod stream;

#[cfg(target_os = "macos")]
mod bridge;
#[cfg(target_os = "macos")]
mod foundation;

pub use availability::{AppleAvailability, AvailabilitySource, MockReason, probe};
pub use delta::{SnapshotDelta, diff_snapshot};
pub use event::AppleStreamEvent;
pub use pump::SnapshotPump;
pub use stream::{StreamHandle, simulate_snapshot_stream};

#[cfg(target_os = "macos")]
pub use bridge::{bridge_snapshot_stream, hello};
#[cfg(target_os = "macos")]
pub use foundation::AppleFoundationBackend;

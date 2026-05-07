//! Domain services that orchestrate repository primitives.
//!
//! Services own no storage of their own; they sit between the pure domain
//! model in `nagori-core` and the I/O-heavy crates (`nagori-storage`,
//! `nagori-search`).

pub mod search;

pub use search::{
    FtsCandidate, NgramCandidate, Ranker, SearchCandidateProvider, SearchPlan, SearchService,
};

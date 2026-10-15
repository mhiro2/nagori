//! Cancellation handle for the palette search that is currently in flight.
//!
//! The palette runs one search at a time and queues the newest keystroke
//! behind it. Without a way to abort the running search, a slow query (a dense
//! CJK 1-gram over a large history) makes the next keystroke wait for results
//! nobody will see. The frontend tags each search with a request id and, when a
//! newer request queues behind it, asks the backend to cancel that id. The
//! storage layer interrupts the `SQLite` statement once the search future is
//! dropped, so cancelling here frees the pooled connection promptly.

use std::sync::{Mutex, MutexGuard, PoisonError};

use tokio_util::sync::CancellationToken;

/// Single slot holding the cancellation token of the registered palette
/// search. Keyed by the frontend's request id so a late cancel for a search
/// that already finished can never abort the request that replaced it.
#[derive(Debug, Default)]
pub struct InflightSearch {
    slot: Mutex<Option<(u64, CancellationToken)>>,
}

impl InflightSearch {
    fn lock(&self) -> MutexGuard<'_, Option<(u64, CancellationToken)>> {
        self.slot.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Register `request_id` as the in-flight search and return the token its
    /// command should race against. The guard clears the slot on drop, unless
    /// a newer registration already replaced it.
    pub fn register(&self, request_id: u64) -> InflightSearchGuard<'_> {
        let token = CancellationToken::new();
        *self.lock() = Some((request_id, token.clone()));
        InflightSearchGuard {
            owner: self,
            request_id,
            token,
        }
    }

    /// Cancel the search registered under `request_id`. Returns `false` when
    /// that search already finished (or never registered), in which case
    /// nothing is touched.
    pub fn cancel(&self, request_id: u64) -> bool {
        let slot = self.lock();
        match slot.as_ref() {
            Some((id, token)) if *id == request_id => {
                token.cancel();
                true
            }
            _ => false,
        }
    }
}

/// Registration of one in-flight search; see [`InflightSearch::register`].
pub struct InflightSearchGuard<'a> {
    owner: &'a InflightSearch,
    request_id: u64,
    token: CancellationToken,
}

impl InflightSearchGuard<'_> {
    pub const fn token(&self) -> &CancellationToken {
        &self.token
    }
}

impl Drop for InflightSearchGuard<'_> {
    fn drop(&mut self) {
        let mut slot = self.owner.lock();
        if slot.as_ref().is_some_and(|(id, _)| *id == self.request_id) {
            *slot = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancel_fires_only_the_matching_registration() {
        let inflight = InflightSearch::default();
        let guard = inflight.register(7);
        assert!(!inflight.cancel(6));
        assert!(!guard.token().is_cancelled());
        assert!(inflight.cancel(7));
        assert!(guard.token().is_cancelled());
    }

    #[test]
    fn late_cancel_after_completion_is_a_no_op() {
        let inflight = InflightSearch::default();
        drop(inflight.register(1));
        assert!(!inflight.cancel(1));
        let next = inflight.register(2);
        assert!(!inflight.cancel(1));
        assert!(!next.token().is_cancelled());
    }

    #[test]
    fn stale_guard_does_not_clear_a_newer_registration() {
        let inflight = InflightSearch::default();
        let first = inflight.register(1);
        let second = inflight.register(2);
        drop(first);
        assert!(inflight.cancel(2));
        assert!(second.token().is_cancelled());
    }
}

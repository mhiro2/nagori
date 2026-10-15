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

/// Cancellation state for the palette search, keyed by the frontend's request
/// id so a late cancel for a search that already finished can never abort the
/// request that replaced it.
#[derive(Debug, Default)]
pub struct InflightSearch {
    slot: Mutex<Slot>,
}

#[derive(Debug, Default)]
struct Slot {
    /// The registered search and its token.
    running: Option<(u64, CancellationToken)>,
    /// A cancel that named no registered search. `cancel_search` is a separate
    /// IPC call that can be handled before the `search_clipboard` it targets
    /// has registered; keeping its id lets that registration start out
    /// cancelled. The next registration always consumes it, so an id left
    /// over from a search that already finished never outlives the request
    /// after it.
    cancelled_early: Option<u64>,
}

impl InflightSearch {
    fn lock(&self) -> MutexGuard<'_, Slot> {
        self.slot.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Register `request_id` as the in-flight search and return the token its
    /// command should race against, already cancelled when a cancel for this
    /// id arrived first. The guard clears the slot on drop, unless a newer
    /// registration already replaced it.
    pub fn register(&self, request_id: u64) -> InflightSearchGuard<'_> {
        let token = CancellationToken::new();
        let mut slot = self.lock();
        if slot.cancelled_early.take() == Some(request_id) {
            token.cancel();
        }
        slot.running = Some((request_id, token.clone()));
        drop(slot);
        InflightSearchGuard {
            owner: self,
            request_id,
            token,
        }
    }

    /// Cancel the search registered under `request_id`. A cancel naming no
    /// registered search is held for the next registration; see
    /// [`Slot::cancelled_early`].
    pub fn cancel(&self, request_id: u64) {
        let mut slot = self.lock();
        match slot.running.as_ref() {
            Some((id, token)) if *id == request_id => token.cancel(),
            // Request ids only grow, so a held id outranks any older cancel
            // that arrives late; keeping the larger one means a delayed cancel
            // for a finished search cannot displace the one still waiting.
            _ => {
                slot.cancelled_early = slot.cancelled_early.max(Some(request_id));
            }
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
        if slot
            .running
            .as_ref()
            .is_some_and(|(id, _)| *id == self.request_id)
        {
            slot.running = None;
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
        inflight.cancel(6);
        assert!(!guard.token().is_cancelled());
        inflight.cancel(7);
        assert!(guard.token().is_cancelled());
    }

    #[test]
    fn cancel_before_registration_cancels_that_search() {
        let inflight = InflightSearch::default();
        inflight.cancel(3);
        let guard = inflight.register(3);
        assert!(guard.token().is_cancelled());
    }

    #[test]
    fn a_delayed_older_cancel_keeps_the_held_one() {
        let inflight = InflightSearch::default();
        inflight.cancel(2);
        inflight.cancel(1);
        let guard = inflight.register(2);
        assert!(guard.token().is_cancelled());
    }

    #[test]
    fn late_cancel_after_completion_never_hits_the_next_search() {
        let inflight = InflightSearch::default();
        drop(inflight.register(1));
        inflight.cancel(1);
        let next = inflight.register(2);
        assert!(!next.token().is_cancelled());
        drop(next);
        // The leftover id was consumed by the registration in between.
        let reused = inflight.register(1);
        assert!(!reused.token().is_cancelled());
    }

    #[test]
    fn stale_guard_does_not_clear_a_newer_registration() {
        let inflight = InflightSearch::default();
        let first = inflight.register(1);
        let second = inflight.register(2);
        drop(first);
        inflight.cancel(2);
        assert!(second.token().is_cancelled());
    }
}

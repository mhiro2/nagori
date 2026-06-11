//! Tracks in-flight AI actions: their cancellation tokens plus the semaphores
//! that bound how many run concurrently.
//!
//! The registry is the sole owner of each request's [`CancellationToken`], so a
//! UI / IPC caller cancels by `request_id` and a CLI caller cancels by dropping
//! the event stream (whose drop guard removes the entry). Apple's on-device
//! language model rejects concurrent generation, so the text-generation
//! semaphore starts at a single permit; translation and embedding get their own
//! permits so they can run alongside it.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use nagori_ai::BackendKind;
use nagori_core::RequestId;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;

/// Slack added to a request's absolute deadline before the watchdog reaps it.
///
/// Every registered request carries the same absolute deadline its streamed
/// generation enforces, so a *polled* run cleans up its own handle the instant
/// the budget expires. This grace lets that normal path win the common race;
/// the watchdog only acts on a handle still alive past `deadline + REAP_GRACE`,
/// which means its stream was leaked (returned but never polled or dropped) —
/// the exact case the consumer-side deadline can't catch.
const REAP_GRACE: Duration = Duration::from_secs(5);

/// How often the dedicated AI watchdog sweeps for expired request handles.
///
/// Kept well under any per-request budget so a leaked or wedged request's
/// concurrency permit is reclaimed promptly. Previously the reap rode on the
/// maintenance loop's (default 30-minute) cadence, so an orphaned permit could
/// pin the single-permit text-generation slot for half an hour.
pub const AI_WATCHDOG_INTERVAL: Duration = Duration::from_mins(1);

/// Per-capability concurrency limits.
pub struct AiSemaphores {
    pub text_generation: Arc<Semaphore>,
    pub translation: Arc<Semaphore>,
    pub embedding: Arc<Semaphore>,
}

impl Default for AiSemaphores {
    fn default() -> Self {
        Self {
            // Apple's language model rejects concurrent requests
            // (`GenerationError.concurrentRequests`), so serialise to one.
            text_generation: Arc::new(Semaphore::new(1)),
            translation: Arc::new(Semaphore::new(1)),
            embedding: Arc::new(Semaphore::new(1)),
        }
    }
}

impl AiSemaphores {
    /// The semaphore that bounds the given backend family.
    #[must_use]
    pub fn for_backend(&self, kind: BackendKind) -> Arc<Semaphore> {
        match kind {
            BackendKind::TextGeneration => Arc::clone(&self.text_generation),
            BackendKind::Translation => Arc::clone(&self.translation),
            BackendKind::Embedding => Arc::clone(&self.embedding),
        }
    }
}

/// Bookkeeping for one in-flight request.
struct RequestHandle {
    /// Absolute budget for the whole request, mirroring the deadline its
    /// pre-stream phases and streamed generation already enforce. The watchdog
    /// cancels + drops a handle still present past `deadline + REAP_GRACE`,
    /// reclaiming its permit independently of whether the stream is ever polled.
    deadline: Instant,
    cancel: CancellationToken,
    /// The backend concurrency permit, held for the request's lifetime. Owning
    /// it here (rather than in the event stream) means dropping the handle —
    /// whether on normal completion, an explicit removal, or a TTL reap —
    /// releases the permit, so a wedged or leaked stream cannot pin it forever.
    permit: Option<OwnedSemaphorePermit>,
}

/// Tracks active AI requests and owns their cancellation tokens.
pub struct AiRequestRegistry {
    handles: Mutex<HashMap<RequestId, RequestHandle>>,
    semaphores: AiSemaphores,
}

impl Default for AiRequestRegistry {
    fn default() -> Self {
        Self {
            handles: Mutex::new(HashMap::new()),
            semaphores: AiSemaphores::default(),
        }
    }
}

impl AiRequestRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The concurrency semaphores.
    #[must_use]
    pub const fn semaphores(&self) -> &AiSemaphores {
        &self.semaphores
    }

    /// Records a started request *before* its permit is acquired, so a cancel
    /// can land while the request is still queued behind the semaphore.
    ///
    /// `deadline` is the request's absolute budget (registration time plus the
    /// effective, tightened timeout), so the watchdog can reap a leaked handle
    /// without depending on the stream ever being polled.
    ///
    /// The mutex is only ever held for the map mutation — never across a
    /// semaphore acquire — so this can't deadlock against permit waiters.
    pub fn register(&self, request_id: RequestId, cancel: CancellationToken, deadline: Instant) {
        let mut handles = self.lock();
        handles.insert(
            request_id,
            RequestHandle {
                deadline,
                cancel,
                permit: None,
            },
        );
    }

    /// Attaches the acquired concurrency permit to a registered request, so the
    /// permit's lifetime is bound to the registry handle. A no-op if the request
    /// was already removed (e.g. cancelled while queued).
    pub fn attach_permit(&self, request_id: RequestId, permit: Option<OwnedSemaphorePermit>) {
        if let Some(handle) = self.lock().get_mut(&request_id) {
            handle.permit = permit;
        }
    }

    /// Removes a request's handle (called when its stream terminates or drops).
    pub fn remove(&self, request_id: RequestId) {
        self.lock().remove(&request_id);
    }

    /// Cancels a request by id, returning `true` if it was tracked.
    pub fn cancel(&self, request_id: RequestId) -> bool {
        let handles = self.lock();
        if let Some(handle) = handles.get(&request_id) {
            handle.cancel.cancel();
            true
        } else {
            false
        }
    }

    /// Number of currently tracked requests.
    #[must_use]
    pub fn active_count(&self) -> usize {
        self.lock().len()
    }

    /// Cancels and drops handles whose absolute deadline has passed (plus
    /// [`REAP_GRACE`]), returning how many were reaped. Dropping a handle
    /// releases its concurrency permit, so this is the backstop that frees a
    /// permit a leaked or wedged stream would otherwise pin until the next
    /// process restart. Reaping on the per-request deadline (rather than a flat
    /// TTL) keeps a long-but-legitimate request alive for its full budget while
    /// still reclaiming a short one promptly.
    pub fn reap_expired(&self) -> usize {
        let mut handles = self.lock();
        let now = Instant::now();
        let expired: Vec<RequestId> = handles
            .iter()
            .filter(|(_, handle)| now >= handle.deadline + REAP_GRACE)
            .map(|(id, _)| *id)
            .collect();
        for id in &expired {
            if let Some(handle) = handles.remove(id) {
                handle.cancel.cancel();
            }
        }
        expired.len()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<RequestId, RequestHandle>> {
        // A poisoned lock means a holder panicked mid-mutation; the map is a
        // simple `HashMap` with no cross-entry invariant, so recovering the
        // guard is safe and keeps cancellation working after a handler panic.
        self.handles
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn far_deadline() -> Instant {
        Instant::now() + Duration::from_hours(1)
    }

    #[test]
    fn cancel_marks_token_and_reports_tracked() {
        let registry = AiRequestRegistry::new();
        let id = RequestId::new();
        let token = CancellationToken::new();
        registry.register(id, token.clone(), far_deadline());
        assert_eq!(registry.active_count(), 1);
        assert!(registry.cancel(id));
        assert!(token.is_cancelled());
        // An unknown id reports not-tracked.
        assert!(!registry.cancel(RequestId::new()));
    }

    #[test]
    fn remove_drops_handle() {
        let registry = AiRequestRegistry::new();
        let id = RequestId::new();
        registry.register(id, CancellationToken::new(), far_deadline());
        registry.remove(id);
        assert_eq!(registry.active_count(), 0);
    }

    #[test]
    fn reap_expired_cancels_only_past_deadline_handles() {
        let registry = AiRequestRegistry::new();

        // Already past its deadline by more than the grace: must be reaped.
        let expired_id = RequestId::new();
        let expired_token = CancellationToken::new();
        registry.register(
            expired_id,
            expired_token.clone(),
            Instant::now()
                .checked_sub(REAP_GRACE + Duration::from_secs(1))
                .expect("test clock is well past the epoch"),
        );

        // Still within its budget: must survive the sweep.
        let live_id = RequestId::new();
        let live_token = CancellationToken::new();
        registry.register(live_id, live_token.clone(), far_deadline());

        assert_eq!(registry.active_count(), 2);
        assert_eq!(registry.reap_expired(), 1);
        assert_eq!(registry.active_count(), 1);
        assert!(
            expired_token.is_cancelled(),
            "reaping an expired handle cancels its token",
        );
        assert!(
            !live_token.is_cancelled(),
            "a handle still within its deadline must not be reaped",
        );
        assert!(registry.cancel(live_id), "the live handle is still tracked");
    }

    #[test]
    fn semaphores_map_each_backend() {
        let semaphores = AiSemaphores::default();
        // Text generation is serialised to a single permit.
        assert_eq!(
            semaphores
                .for_backend(BackendKind::TextGeneration)
                .available_permits(),
            1
        );
        assert_eq!(
            semaphores
                .for_backend(BackendKind::Translation)
                .available_permits(),
            1
        );
    }
}

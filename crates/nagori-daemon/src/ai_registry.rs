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
use nagori_core::{AiActionId, RequestId};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;

/// How long a handle may live before the reaper treats it as stale. A normal
/// request removes its own handle when its stream terminates or is dropped;
/// this only catches handles orphaned by a panic or a leaked stream future.
const HANDLE_TTL: Duration = Duration::from_mins(5);

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
    #[allow(dead_code)]
    action: AiActionId,
    started_at: Instant,
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
    /// The mutex is only ever held for the map mutation — never across a
    /// semaphore acquire — so this can't deadlock against permit waiters.
    pub fn register(&self, request_id: RequestId, action: AiActionId, cancel: CancellationToken) {
        let mut handles = self.lock();
        handles.insert(
            request_id,
            RequestHandle {
                action,
                started_at: Instant::now(),
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

    /// Cancels and drops handles older than [`HANDLE_TTL`], returning how many
    /// were reaped. Dropping a handle releases its concurrency permit, so this
    /// is the backstop that frees a permit a leaked or wedged stream would
    /// otherwise pin forever.
    pub fn reap_stale(&self) -> usize {
        let mut handles = self.lock();
        let now = Instant::now();
        let stale: Vec<RequestId> = handles
            .iter()
            .filter(|(_, handle)| now.duration_since(handle.started_at) > HANDLE_TTL)
            .map(|(id, _)| *id)
            .collect();
        for id in &stale {
            if let Some(handle) = handles.remove(id) {
                handle.cancel.cancel();
            }
        }
        stale.len()
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

    #[test]
    fn cancel_marks_token_and_reports_tracked() {
        let registry = AiRequestRegistry::new();
        let id = RequestId::new();
        let token = CancellationToken::new();
        registry.register(id, AiActionId::Summarize, token.clone());
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
        registry.register(id, AiActionId::Summarize, CancellationToken::new());
        registry.remove(id);
        assert_eq!(registry.active_count(), 0);
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

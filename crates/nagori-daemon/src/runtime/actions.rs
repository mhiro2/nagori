//! Quick (deterministic, always-available) actions and model-backed AI
//! actions: gating, input shaping, the cancellation/timeout guard, and the
//! availability report.

use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::StreamExt;
use nagori_ai::{AiActionRun, resolve_backend};
use nagori_core::{
    AiActionId, AiActionRequest, AiAvailabilityReport, AiError, AiErrorCode, AiEvent,
    AiInputPolicy, AiOutput, AiOverallStatus, AiRequestOptions, AppError, ClipboardEntry, EntryId,
    EntryRepository, PerActionAvailability, PerActionStatus, QuickActionId, RequestId, Result,
    SemanticIndexAvailability, Sensitivity, SensitivityClassifier, SettingsRepository,
    estimate_tokens,
};
use time::OffsetDateTime;
use tokio_util::sync::CancellationToken;

use crate::ai_registry::AiRequestRegistry;
use crate::ipc_handler::result_code;

use super::{MAX_AI_INPUT_TOKENS, NagoriRuntime, elapsed_ms};

impl NagoriRuntime {
    /// Runs a deterministic [`QuickActionId`] on-device.
    ///
    /// Quick actions never touch a language model and are always available,
    /// independent of the AI provider configuration. Input is still shaped by
    /// the settings-aware redaction classifier and the per-action size cap.
    pub async fn run_quick_action(&self, id: EntryId, action: QuickActionId) -> Result<AiOutput> {
        // Wrap the body so *every* exit — policy refusals, a missing entry, and
        // size-cap rejections, not just the runner's own result — is logged.
        // `action.slug()` is a fixed identifier and `result_code` collapses to
        // a static label, so the quick-action log never carries the entry text.
        let started = Instant::now();
        let result = self.run_quick_action_inner(id, action).await;
        tracing::debug!(
            action = action.slug(),
            result_code = result_code(&result),
            elapsed_ms = elapsed_ms(started),
            "quick_action"
        );
        result
    }

    async fn run_quick_action_inner(&self, id: EntryId, action: QuickActionId) -> Result<AiOutput> {
        let settings = self.store.get_settings().await?;
        let policy = action.input_policy();
        let entry = self.store.get(id).await?.ok_or(AppError::NotFound)?;
        let classifier = SensitivityClassifier::try_new(settings)?;
        let raw = actionable_text(&entry)?;
        // Secrets must be redacted (or refused); private entries are redacted
        // unconditionally; `require_redaction` forces redaction even on Public
        // entries. `RedactSecrets` is the one action allowed to consume a
        // secret entry, because redacting it is the whole point.
        let input = match (entry.sensitivity, action) {
            (Sensitivity::Secret | Sensitivity::Blocked, QuickActionId::RedactSecrets) => {
                classifier.redact(raw)
            }
            (Sensitivity::Secret | Sensitivity::Blocked, _) => {
                return Err(AppError::Policy(
                    "secret entries must be redacted before this quick action".to_owned(),
                ));
            }
            (Sensitivity::Private, _) => classifier.redact(raw),
            _ if policy.require_redaction => classifier.redact(raw),
            _ => raw.to_owned(),
        };
        if input.len() > policy.max_bytes {
            return Err(AppError::Policy(format!(
                "input exceeds max_bytes ({}) for action {}",
                policy.max_bytes,
                action.slug()
            )));
        }
        self.quick_runner.run(action, &input)
    }

    /// Begins a model-backed [`AiActionId`], returning its live event stream.
    ///
    /// Gates on the master toggle, the per-action allow-list, and the selected
    /// provider; shapes the input through the redaction classifier, byte cap,
    /// and token budget; acquires the appropriate concurrency permit; and
    /// registers the request so it can be cancelled by id. The returned
    /// stream's drop releases the permit and removes the registry entry, and a
    /// watchdog cancels it once the configured timeout elapses.
    pub async fn start_ai_action(
        &self,
        id: EntryId,
        action: AiActionId,
        options: AiRequestOptions,
    ) -> Result<AiActionRun> {
        let started = Instant::now();
        let settings = self.store.get_settings().await?;
        if !settings.ai.enabled {
            return Err(AppError::Policy(
                "ai actions are disabled in settings".to_owned(),
            ));
        }
        if !settings.ai.allowed_actions.is_empty() && !settings.ai.allowed_actions.contains(&action)
        {
            return Err(AppError::Policy(format!(
                "ai action {} is not in the allow-list",
                action.slug()
            )));
        }
        let engine = self.ai_engine.as_ref().ok_or_else(|| {
            AppError::Unsupported("no AI engine is available on this platform".to_owned())
        })?;
        if settings.ai.provider != engine.provider() {
            return Err(AppError::Unsupported(format!(
                "the selected AI provider ({:?}) has no backend wired in this build",
                settings.ai.provider
            )));
        }

        let policy = action.input_policy();
        let entry = self.store.get(id).await?.ok_or(AppError::NotFound)?;
        let classifier = SensitivityClassifier::try_new(settings.clone())?;
        let input = shape_ai_input(&entry, &classifier, &policy)?;

        // Steer the generated text toward the UI-language setting instead of
        // letting the on-device model default to English; a caller that set
        // this explicitly wins.
        let mut options = options;
        if options.output_language.is_none() {
            options.output_language = Some(settings.locale.as_tag().to_owned());
        }

        let request_id = RequestId::new();
        let req = AiActionRequest {
            request_id,
            action,
            input,
            policy,
            options,
        };

        // Register *before* acquiring the permit so the request is cancellable
        // while it waits behind the semaphore (Apple serialises text generation
        // to one in-flight request). The registry mutex is never held across the
        // acquire, so permit waiters can't deadlock against the map mutation.
        let cancel = CancellationToken::new();
        self.ai_registry
            .register(request_id, action, cancel.clone());

        // Acquire the backend's concurrency permit, racing it against a cancel
        // so a queued request can be aborted before it ever reaches the model.
        let permit = match resolve_backend(action, engine.provider()) {
            Some(kind) => {
                let semaphore = self.ai_registry.semaphores().for_backend(kind);
                tokio::select! {
                    acquired = semaphore.acquire_owned() => {
                        let permit = acquired.map_err(|_| {
                            self.ai_registry.remove(request_id);
                            AppError::Ai("ai concurrency semaphore closed".to_owned())
                        })?;
                        // The reaper (or an explicit cancel) may have cancelled —
                        // and removed — this request while the acquire and the
                        // cancel arms were both ready. Re-check so we don't start
                        // an untracked run that holds no registry handle; dropping
                        // `permit` here returns it to the semaphore.
                        if cancel.is_cancelled() {
                            self.ai_registry.remove(request_id);
                            return Err(AppError::Ai(
                                "ai action was cancelled while queued".to_owned(),
                            ));
                        }
                        Some(permit)
                    }
                    () = cancel.cancelled() => {
                        self.ai_registry.remove(request_id);
                        return Err(AppError::Ai(
                            "ai action was cancelled while queued".to_owned(),
                        ));
                    }
                }
            }
            // No backend wired — let the engine surface the capability mismatch.
            None => None,
        };
        // Bind the permit to the registry handle so a normal removal *and* the
        // TTL reaper both release it, even if the stream is never polled out.
        self.ai_registry.attach_permit(request_id, permit);

        let run = match engine.start(req, cancel.clone()).await {
            Ok(run) => run,
            Err(err) => {
                self.ai_registry.remove(request_id);
                return Err(ai_error_to_app(&err));
            }
        };

        let timeout = Duration::from_millis(settings.ai.request_timeout_ms);
        let guard = RequestGuard {
            registry: Arc::clone(&self.ai_registry),
            request_id,
            cancel,
        };
        // Record that a model-backed action started, with its provider, so the
        // AI path is observable end-to-end. `action.slug()` and the provider
        // enum are fixed identifiers — no prompt, input, or output text.
        tracing::debug!(
            action = action.slug(),
            provider = ?engine.provider(),
            elapsed_ms = elapsed_ms(started),
            "ai_action_started"
        );
        let events = guard_event_stream(run.events, guard, timeout);
        Ok(AiActionRun { request_id, events })
    }

    /// Runs an AI action to completion and returns the collected result.
    ///
    /// The one-shot path used by the IPC `RunAiAction` handler: it drives
    /// [`Self::start_ai_action`] and folds the stream into a single
    /// [`AiOutput`].
    pub async fn run_ai_action(&self, id: EntryId, action: AiActionId) -> Result<AiOutput> {
        let run = self
            .start_ai_action(id, action, AiRequestOptions::default())
            .await?;
        let mut events = run.events;
        let mut text = String::new();
        let mut warnings = Vec::new();
        while let Some(item) = events.next().await {
            match item {
                Ok(AiEvent::Delta { text: delta, .. }) => text.push_str(&delta),
                Ok(AiEvent::Replace { text: snapshot, .. }) => text = snapshot,
                Ok(AiEvent::Done {
                    final_text,
                    warnings: done_warnings,
                    ..
                }) => {
                    text = final_text;
                    warnings = done_warnings;
                    break;
                }
                Ok(AiEvent::Cancelled) => {
                    return Err(AppError::Ai("ai action was cancelled".to_owned()));
                }
                Err(err) => return Err(ai_error_to_app(&err)),
            }
        }
        Ok(AiOutput {
            text,
            created_entry: None,
            warnings,
        })
    }

    /// Cancels an in-flight AI action by id. Returns `true` if it was tracked.
    #[must_use]
    pub fn cancel_ai_action(&self, request_id: RequestId) -> bool {
        self.ai_registry.cancel(request_id)
    }

    /// Cancels and reaps AI request handles older than the registry TTL,
    /// returning how many were reaped. Called by the maintenance loop.
    pub fn reap_stale_ai_requests(&self) -> usize {
        self.ai_registry.reap_stale()
    }

    /// Builds a point-in-time AI availability report for the current settings.
    pub async fn ai_availability(&self) -> Result<AiAvailabilityReport> {
        let settings = self.store.get_settings().await?;
        match &self.ai_engine {
            Some(engine) => Ok(engine.availability(&settings.ai).await),
            None => Ok(no_engine_availability(&settings.ai)),
        }
    }
}

/// The entry's text for an action to operate on, or [`AppError::InvalidInput`]
/// when the content has no text representation (e.g. an image). Callers
/// previously defaulted a missing representation to an empty string, which let
/// actions run silently on nothing; surfacing the refusal lets the UI explain
/// why an action can't run.
fn actionable_text(entry: &ClipboardEntry) -> Result<&str> {
    entry
        .plain_text()
        .ok_or_else(|| AppError::InvalidInput("this entry has no text to act on".to_owned()))
}

/// Shapes an entry's text for a model-backed AI action: redacts per
/// sensitivity, enforces the byte cap, and refuses input over the token budget.
fn shape_ai_input(
    entry: &ClipboardEntry,
    classifier: &SensitivityClassifier,
    policy: &AiInputPolicy,
) -> Result<String> {
    let raw = actionable_text(entry)?;
    let input = match entry.sensitivity {
        Sensitivity::Secret | Sensitivity::Blocked => {
            return Err(AppError::Policy(
                "secret entries must be redacted before this AI action".to_owned(),
            ));
        }
        Sensitivity::Private => classifier.redact(raw),
        _ if policy.require_redaction => classifier.redact(raw),
        _ => raw.to_owned(),
    };
    if input.len() > policy.max_bytes {
        return Err(AppError::Policy(format!(
            "input exceeds max_bytes ({})",
            policy.max_bytes
        )));
    }
    let tokens = estimate_tokens(&input);
    if tokens > MAX_AI_INPUT_TOKENS {
        return Err(AppError::Policy(format!(
            "input is ~{tokens} tokens; the on-device model caps at {MAX_AI_INPUT_TOKENS}"
        )));
    }
    Ok(input)
}

/// Maps a structured [`AiError`] onto the daemon's [`AppError`].
fn ai_error_to_app(err: &AiError) -> AppError {
    match err.code {
        AiErrorCode::Unavailable | AiErrorCode::CapabilityMismatch | AiErrorCode::AssetMissing => {
            AppError::Unsupported(err.message.clone())
        }
        AiErrorCode::InputTooLarge => AppError::Policy(err.message.clone()),
        _ => AppError::Ai(err.message.clone()),
    }
}

/// Availability report for hosts with no wired AI engine.
fn no_engine_availability(settings: &nagori_core::AiSettings) -> AiAvailabilityReport {
    let status = if settings.enabled {
        PerActionStatus::OsUnavailable
    } else {
        PerActionStatus::DisabledBySettings
    };
    let per_action = AiActionId::all()
        .iter()
        .map(|&action| PerActionAvailability {
            action,
            status,
            remediation: None,
        })
        .collect();
    AiAvailabilityReport {
        generated_at: OffsetDateTime::now_utc(),
        provider: settings.provider,
        overall_status: if settings.enabled {
            AiOverallStatus::Unavailable
        } else {
            AiOverallStatus::Disabled
        },
        per_action,
        semantic_index: if settings.semantic_index_enabled {
            SemanticIndexAvailability::NotImplemented
        } else {
            SemanticIndexAvailability::Disabled
        },
    }
}

/// Cancels the request and removes its registry handle when the event stream
/// ends or is dropped.
///
/// Cancelling on drop is what makes "dropping the stream cancels the run" work
/// for the CLI: when the consumer stops polling, the backend task stops too.
/// Removing the handle releases the registry-owned concurrency permit. On a
/// clean completion the token is already past use, so the cancel is a harmless
/// no-op.
struct RequestGuard {
    registry: Arc<AiRequestRegistry>,
    request_id: RequestId,
    cancel: CancellationToken,
}

impl Drop for RequestGuard {
    fn drop(&mut self) {
        self.cancel.cancel();
        self.registry.remove(self.request_id);
    }
}

/// Internal `unfold` state for the guarded, deadline-bounded event stream.
struct GuardedStreamState {
    inner: nagori_ai::AiEventStream,
    guard: RequestGuard,
    deadline: Instant,
    ended: bool,
}

/// Wraps an engine event stream so the registry guard lives exactly as long as
/// the stream (dropping it cancels the run and releases the permit), and
/// enforces the request timeout *consumer-side*: each poll races the backend
/// against the remaining deadline, so even a backend wedged in an `await`
/// terminates with a distinct [`AiErrorCode::Timeout`] rather than hanging.
fn guard_event_stream(
    inner: nagori_ai::AiEventStream,
    guard: RequestGuard,
    timeout: Duration,
) -> nagori_ai::AiEventStream {
    let state = GuardedStreamState {
        inner,
        guard,
        deadline: Instant::now() + timeout,
        ended: false,
    };
    futures::stream::unfold(state, |mut state| async move {
        if state.ended {
            return None;
        }
        let now = Instant::now();
        let remaining = state.deadline.saturating_duration_since(now);
        if remaining.is_zero() {
            state.ended = true;
            state.guard.cancel.cancel();
            return Some((Err(timeout_error()), state));
        }
        match tokio::time::timeout(remaining, state.inner.next()).await {
            Ok(Some(item)) => Some((item, state)),
            Ok(None) => None,
            Err(_) => {
                state.ended = true;
                state.guard.cancel.cancel();
                Some((Err(timeout_error()), state))
            }
        }
    })
    .boxed()
}

fn timeout_error() -> AiError {
    AiError::new(AiErrorCode::Timeout, "ai action timed out")
}

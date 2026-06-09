//! Quick (deterministic, always-available) actions and model-backed AI
//! actions: gating, input shaping, the cancellation/timeout guard, and the
//! availability report.

use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::StreamExt;
use nagori_ai::{AiActionRun, resolve_backend};
use nagori_core::{
    AiActionId, AiActionRequest, AiAvailabilityReport, AiError, AiErrorCode, AiEvent,
    AiInputPolicy, AiOutput, AiOverallStatus, AiProviderKind, AiRequestOptions, AppError,
    ClipboardEntry, EntryId, EntryRepository, PerActionAvailability, PerActionStatus,
    QuickActionId, RequestId, Result, SemanticIndexAvailability, Sensitivity,
    SensitivityClassifier, SettingsRepository, estimate_tokens,
};
use time::OffsetDateTime;
use tokio::sync::OwnedSemaphorePermit;
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

        // Build the effective policy once by tightening the per-request options
        // against the AI settings (timeout, input/output token caps) and the
        // `allow_streaming` toggle, then enforce it everywhere downstream — the
        // input-shaping guard, the deadline, the request handed to the backend,
        // and the stream wrapper — so no limit is re-derived (and silently
        // drifts) between those sites.
        let mut options = options;
        let effective = EffectiveAiPolicy::resolve(&options, &settings.ai);
        let input = shape_ai_input(&entry, &classifier, &policy, effective.max_input_tokens)?;

        // Steer the generated text toward the UI-language setting instead of
        // letting the on-device model default to English; a caller that set
        // this explicitly wins.
        if options.output_language.is_none() {
            options.output_language = Some(settings.locale.as_tag().to_owned());
        }
        // Stamp the tightened limits onto the request so the backend sees the
        // enforced policy, not the looser values the caller supplied.
        effective.apply_to(&mut options);

        let request_id = RequestId::new();
        let req = AiActionRequest {
            request_id,
            action,
            input,
            policy,
            options,
        };

        // One absolute budget for the *whole* request, anchored at registration
        // time and bounded by the effective (tightened) timeout. The semaphore
        // wait, `engine.start`, and the streamed generation all draw down the
        // same deadline, so a wedged predecessor holding the single
        // text-generation permit — or a stalled `engine.start` — can't keep this
        // request (and its permit) alive past the budget. Previously the timeout
        // only armed *after* start, leaving the pre-stream phases unbounded.
        let deadline = Instant::now() + Duration::from_millis(effective.timeout_ms);

        // Register *before* acquiring the permit so the request is cancellable
        // while it waits behind the semaphore (Apple serialises text generation
        // to one in-flight request). The registry mutex is never held across the
        // acquire, so permit waiters can't deadlock against the map mutation.
        let cancel = CancellationToken::new();
        self.ai_registry
            .register(request_id, action, cancel.clone());

        // Acquire the backend's concurrency permit, bounded by the cancel token
        // *and* the remaining budget so a queued request is aborted before it
        // ever reaches the model — whether by an explicit cancel or by the
        // deadline expiring while a wedged predecessor holds the permit.
        let permit = self
            .acquire_ai_permit(action, engine.provider(), request_id, &cancel, deadline)
            .await?;
        // Bind the permit to the registry handle so a normal removal *and* the
        // TTL reaper both release it, even if the stream is never polled out.
        self.ai_registry.attach_permit(request_id, permit);

        // Bound `engine.start` by what's left of the budget so a model that
        // stalls during init / asset load can't pin the permit indefinitely.
        let run = match tokio::time::timeout(
            remaining_until(deadline),
            engine.start(req, cancel.clone()),
        )
        .await
        {
            Ok(Ok(run)) => run,
            Ok(Err(err)) => {
                self.ai_registry.remove(request_id);
                return Err(ai_error_to_app(&err));
            }
            Err(_elapsed) => {
                // Cancel the backend so any work `engine.start` kicked off
                // unwinds, then release the permit via the registry handle.
                cancel.cancel();
                self.ai_registry.remove(request_id);
                return Err(AppError::Ai(
                    "ai action timed out before streaming".to_owned(),
                ));
            }
        };

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
        // Enforce the streaming decision server-side: when streaming is not
        // allowed, drop intermediate snapshots so only the terminal result is
        // surfaced (the consumer still gets the full text from `Done`).
        let raw_events = if effective.streaming {
            run.events
        } else {
            coalesce_non_streaming(run.events)
        };
        // The stream shares the same absolute deadline, so the remaining budget
        // already drawn down by the acquire + start bounds the generation too.
        let events = guard_event_stream(raw_events, guard, deadline);
        Ok(AiActionRun { request_id, events })
    }

    /// Acquires the backend concurrency permit for a registered request,
    /// bounded by both `cancel` and the request `deadline`.
    ///
    /// Returns `Ok(None)` when no backend is wired for the action (the engine
    /// surfaces the capability mismatch later). On a cancel or timeout it
    /// removes the registry handle and returns the matching [`AppError::Ai`], so
    /// the caller never holds a slot for a run that never started.
    async fn acquire_ai_permit(
        &self,
        action: AiActionId,
        provider: AiProviderKind,
        request_id: RequestId,
        cancel: &CancellationToken,
        deadline: Instant,
    ) -> Result<Option<OwnedSemaphorePermit>> {
        let Some(kind) = resolve_backend(action, provider) else {
            // No backend wired — let the engine surface the capability mismatch.
            return Ok(None);
        };
        let semaphore = self.ai_registry.semaphores().for_backend(kind);
        tokio::select! {
            acquired = tokio::time::timeout(remaining_until(deadline), semaphore.acquire_owned()) => {
                match acquired {
                    Ok(acquired) => {
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
                        Ok(Some(permit))
                    }
                    Err(_elapsed) => {
                        // The budget expired while we waited for the permit — a
                        // previous request is wedged. Drop the registry slot so we
                        // don't hold it for a run that never started.
                        self.ai_registry.remove(request_id);
                        Err(AppError::Ai(
                            "ai action timed out waiting for a concurrency permit".to_owned(),
                        ))
                    }
                }
            }
            () = cancel.cancelled() => {
                self.ai_registry.remove(request_id);
                Err(AppError::Ai("ai action was cancelled while queued".to_owned()))
            }
        }
    }

    /// Runs an AI action to completion and returns the collected result.
    ///
    /// The one-shot path used by the IPC `RunAiAction` handler: it drives
    /// [`Self::start_ai_action`] and folds the stream into a single
    /// [`AiOutput`]. The caller-supplied `options` carry the wire request's
    /// per-request overrides (translate languages, tightening caps), so a CLI
    /// `ai translate --from/--to` over IPC reaches the backend instead of being
    /// replaced with defaults here.
    pub async fn run_ai_action(
        &self,
        id: EntryId,
        action: AiActionId,
        options: AiRequestOptions,
    ) -> Result<AiOutput> {
        let run = self.start_ai_action(id, action, options).await?;
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
/// sensitivity, enforces the byte cap, and refuses input over `max_input_tokens`
/// (the effective token budget — the model's hard cap tightened by any
/// per-request override).
fn shape_ai_input(
    entry: &ClipboardEntry,
    classifier: &SensitivityClassifier,
    policy: &AiInputPolicy,
    max_input_tokens: usize,
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
    if tokens > max_input_tokens {
        return Err(AppError::Policy(format!(
            "input is ~{tokens} tokens; the budget for this request is {max_input_tokens}"
        )));
    }
    Ok(input)
}

/// A request's limits after tightening the per-request [`AiRequestOptions`]
/// against the daemon-wide [`AiSettings`] and the model's hard input cap.
///
/// Built once at registration so the deadline math, the input-shaping guard,
/// the stream wrapper, and the options handed to the backend all read the same
/// enforced numbers rather than each re-deriving them (and silently drifting).
/// Every field is "tightening only": a per-request override can make a request
/// *more* restrictive than the settings / model defaults but never looser.
#[derive(Debug, Clone, Copy)]
struct EffectiveAiPolicy {
    /// Absolute per-request budget in ms: `min(settings, request override)`,
    /// floored at 1 so a zero override can't make the deadline already-expired.
    timeout_ms: u64,
    /// Input token ceiling: the model's hard cap tightened by any override.
    max_input_tokens: usize,
    /// Output token ceiling forwarded to the backend, if the request set one.
    /// No settings counterpart today, so it passes through unchanged. Output
    /// length is only knowable mid-generation, so this can't be enforced
    /// pre-call: the daemon hands it to the backend, which caps on it where it
    /// supports a max-output control. The on-device Apple text generator does
    /// not yet wire one, so the value is carried but not honoured there.
    max_output_tokens: Option<u32>,
    /// Whether intermediate snapshots may be surfaced: the UI-level
    /// `allow_streaming` toggle combined (logical AND) with the request's
    /// preference, which defaults to on.
    streaming: bool,
}

impl EffectiveAiPolicy {
    fn resolve(options: &AiRequestOptions, settings: &nagori_core::AiSettings) -> Self {
        let timeout_ms = options
            .timeout_ms
            .map_or(settings.request_timeout_ms, |req| {
                req.min(settings.request_timeout_ms)
            })
            .max(1);
        let max_input_tokens = options.max_input_tokens.map_or(MAX_AI_INPUT_TOKENS, |req| {
            (req as usize).min(MAX_AI_INPUT_TOKENS)
        });
        let streaming = settings.allow_streaming && options.streaming.unwrap_or(true);
        Self {
            timeout_ms,
            max_input_tokens,
            max_output_tokens: options.max_output_tokens,
            streaming,
        }
    }

    /// Writes the tightened values back onto `options` so the request handed to
    /// the backend carries the enforced policy — a backend that honours
    /// `streaming` / `max_output_tokens` / the caps sees the tightened figures,
    /// not the looser ones the caller supplied.
    fn apply_to(&self, options: &mut AiRequestOptions) {
        options.timeout_ms = Some(self.timeout_ms);
        options.max_input_tokens = Some(u32::try_from(self.max_input_tokens).unwrap_or(u32::MAX));
        options.max_output_tokens = self.max_output_tokens;
        options.streaming = Some(self.streaming);
    }
}

/// Suppresses intermediate `Delta` / `Replace` snapshots when streaming is not
/// allowed, leaving only terminal events (`Done` carries the authoritative
/// `final_text`, so no output is lost). Enforced server-side so the toggle
/// holds regardless of whether the backend itself streams.
fn coalesce_non_streaming(events: nagori_ai::AiEventStream) -> nagori_ai::AiEventStream {
    events
        .filter(|item| {
            let keep = !matches!(item, Ok(AiEvent::Delta { .. } | AiEvent::Replace { .. }));
            async move { keep }
        })
        .boxed()
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
/// enforces the request `deadline` *consumer-side*: each poll races the backend
/// against the time remaining until the absolute deadline, so even a backend
/// wedged in an `await` terminates with a distinct [`AiErrorCode::Timeout`]
/// rather than hanging. The deadline is the request-wide budget anchored at
/// registration, so time already spent waiting for the permit and starting the
/// engine has shortened what the stream gets.
fn guard_event_stream(
    inner: nagori_ai::AiEventStream,
    guard: RequestGuard,
    deadline: Instant,
) -> nagori_ai::AiEventStream {
    let state = GuardedStreamState {
        inner,
        guard,
        deadline,
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

/// Time left until `deadline`, saturating at zero once it has passed. A zero
/// duration handed to `tokio::time::timeout` elapses on the first poll, so a
/// blown budget surfaces as a timeout rather than an unbounded wait.
fn remaining_until(deadline: Instant) -> Duration {
    deadline.saturating_duration_since(Instant::now())
}

#[cfg(test)]
mod tests {
    use super::*;
    use nagori_core::AiSettings;

    fn settings_with(request_timeout_ms: u64, allow_streaming: bool) -> AiSettings {
        AiSettings {
            request_timeout_ms,
            allow_streaming,
            ..AiSettings::default()
        }
    }

    #[test]
    fn timeout_tightens_to_the_smaller_of_settings_and_request() {
        let settings = settings_with(30_000, true);
        // A shorter per-request override wins.
        let shorter = AiRequestOptions {
            timeout_ms: Some(5_000),
            ..AiRequestOptions::default()
        };
        assert_eq!(
            EffectiveAiPolicy::resolve(&shorter, &settings).timeout_ms,
            5_000
        );
        // A longer override cannot loosen the settings ceiling.
        let longer = AiRequestOptions {
            timeout_ms: Some(120_000),
            ..AiRequestOptions::default()
        };
        assert_eq!(
            EffectiveAiPolicy::resolve(&longer, &settings).timeout_ms,
            30_000
        );
        // No override falls back to settings.
        assert_eq!(
            EffectiveAiPolicy::resolve(&AiRequestOptions::default(), &settings).timeout_ms,
            30_000
        );
    }

    #[test]
    fn timeout_is_floored_at_one_ms() {
        // A zero override must not produce an already-expired deadline.
        let settings = settings_with(30_000, true);
        let zero = AiRequestOptions {
            timeout_ms: Some(0),
            ..AiRequestOptions::default()
        };
        assert_eq!(EffectiveAiPolicy::resolve(&zero, &settings).timeout_ms, 1);
    }

    #[test]
    fn input_tokens_tighten_against_the_model_hard_cap() {
        let settings = settings_with(30_000, true);
        // A request can only lower the cap.
        let lower = AiRequestOptions {
            max_input_tokens: Some(100),
            ..AiRequestOptions::default()
        };
        assert_eq!(
            EffectiveAiPolicy::resolve(&lower, &settings).max_input_tokens,
            100
        );
        // A request above the hard cap is clamped down to it.
        let above = AiRequestOptions {
            max_input_tokens: Some(u32::MAX),
            ..AiRequestOptions::default()
        };
        assert_eq!(
            EffectiveAiPolicy::resolve(&above, &settings).max_input_tokens,
            MAX_AI_INPUT_TOKENS
        );
        assert_eq!(
            EffectiveAiPolicy::resolve(&AiRequestOptions::default(), &settings).max_input_tokens,
            MAX_AI_INPUT_TOKENS
        );
    }

    #[test]
    fn streaming_requires_both_the_settings_toggle_and_the_request() {
        // Settings off → never streams, whatever the request asks.
        let off = settings_with(30_000, false);
        let asks = AiRequestOptions {
            streaming: Some(true),
            ..AiRequestOptions::default()
        };
        assert!(!EffectiveAiPolicy::resolve(&asks, &off).streaming);
        // Settings on, request silent → defaults to streaming.
        let on = settings_with(30_000, true);
        assert!(EffectiveAiPolicy::resolve(&AiRequestOptions::default(), &on).streaming);
        // Settings on, request opts out → no streaming.
        let opts_out = AiRequestOptions {
            streaming: Some(false),
            ..AiRequestOptions::default()
        };
        assert!(!EffectiveAiPolicy::resolve(&opts_out, &on).streaming);
    }

    #[test]
    fn apply_to_stamps_the_tightened_values_onto_the_request_options() {
        let settings = settings_with(10_000, false);
        let mut options = AiRequestOptions {
            timeout_ms: Some(60_000),
            max_input_tokens: Some(50),
            max_output_tokens: Some(256),
            streaming: Some(true),
            ..AiRequestOptions::default()
        };
        let effective = EffectiveAiPolicy::resolve(&options, &settings);
        effective.apply_to(&mut options);
        assert_eq!(options.timeout_ms, Some(10_000));
        assert_eq!(options.max_input_tokens, Some(50));
        assert_eq!(options.max_output_tokens, Some(256));
        assert_eq!(options.streaming, Some(false));
    }
}

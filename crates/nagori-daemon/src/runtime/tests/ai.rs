use std::sync::Arc;

use futures::StreamExt;
use nagori_core::{
    AiActionId, AiEvent, AiOverallStatus, AiRequestOptions, ClipboardContent, EntryFactory,
    EntryId, EntryRepository,
};
use nagori_platform::MemoryClipboard;

use super::super::*;
use super::{
    ai_enabled_settings, runtime_with_memory_clipboard, runtime_with_mock_ai,
    runtime_with_mock_translator,
};

#[tokio::test]
async fn quick_actions_run_under_defaults() {
    use nagori_core::QuickActionId;
    // Quick actions never gate on the AI toggle — they must run under the
    // default (AI off) config or the palette's quick-action buttons would
    // be perma-broken. `FormatJson` needs valid JSON, since anything else
    // surfaces as `AppError::Ai` and would look like a gate rejection.
    let cases: &[(QuickActionId, &str)] = &[
        (QuickActionId::SummarizeFirstSentence, "hello world"),
        (QuickActionId::FormatJson, r#"{"a":1}"#),
        (QuickActionId::ExtractTasks, "TODO: ship the thing"),
        (QuickActionId::RedactSecrets, "no secrets here"),
    ];
    for (action, input) in cases {
        let (runtime, _) = runtime_with_memory_clipboard();
        let id = runtime
            .add_text((*input).to_owned())
            .await
            .expect("entry should be added");
        runtime
            .run_quick_action(id, *action)
            .await
            .unwrap_or_else(|err| panic!("{action:?} must run under defaults; got {err:?}"));
    }
}

/// Inserts a minimal image entry (no text representation) and returns its id.
async fn add_image_entry(runtime: &NagoriRuntime) -> EntryId {
    let content = ClipboardContent::Image(nagori_core::ImageContent {
        width: Some(1),
        height: Some(1),
        byte_count: 4,
        mime_type: Some("image/png".to_owned()),
        pending_bytes: Some(vec![0u8, 1, 2, 3]),
    });
    runtime
        .store
        .insert(EntryFactory::from_content(content, None, None))
        .await
        .expect("image entry should be inserted")
}

#[tokio::test]
async fn quick_action_on_image_is_invalid_input() {
    use nagori_core::QuickActionId;
    // Images have no text representation, so a quick action must refuse with
    // InvalidInput rather than silently running on an empty string.
    let (runtime, _) = runtime_with_memory_clipboard();
    let id = add_image_entry(&runtime).await;
    let err = runtime
        .run_quick_action(id, QuickActionId::SummarizeFirstSentence)
        .await
        .expect_err("quick action on an image must be refused");
    assert!(matches!(err, AppError::InvalidInput(_)), "got {err:?}");
}

#[tokio::test]
async fn ai_action_on_image_is_invalid_input() {
    // Same guard on the model-backed path: an image carries no text to shape.
    let (runtime, _) = runtime_with_mock_ai();
    runtime
        .save_settings(ai_enabled_settings(AppSettings::default()))
        .await
        .expect("save settings");
    let id = add_image_entry(&runtime).await;
    let err = runtime
        .run_ai_action(id, AiActionId::Summarize, AiRequestOptions::default())
        .await
        .expect_err("ai action on an image must be refused");
    assert!(matches!(err, AppError::InvalidInput(_)), "got {err:?}");
}

#[tokio::test]
async fn ai_action_blocked_when_disabled() {
    // With AI off (the default), a model-backed action is refused even
    // though the engine is wired.
    let (runtime, _) = runtime_with_mock_ai();
    let id = runtime
        .add_text("hello".to_owned())
        .await
        .expect("entry should be added");
    let err = runtime
        .run_ai_action(id, AiActionId::Summarize, AiRequestOptions::default())
        .await
        .expect_err("ai actions must be refused when disabled");
    assert!(matches!(err, AppError::Policy(_)), "got {err:?}");
}

#[tokio::test]
async fn ai_action_runs_when_enabled() {
    let (runtime, _) = runtime_with_mock_ai();
    runtime
        .save_settings(ai_enabled_settings(AppSettings::default()))
        .await
        .expect("save settings");
    let id = runtime
        .add_text("hello world".to_owned())
        .await
        .expect("entry should be added");
    let output = runtime
        .run_ai_action(id, AiActionId::Summarize, AiRequestOptions::default())
        .await
        .expect("summarize should succeed when enabled");
    assert!(output.text.starts_with("Summary:"), "got {}", output.text);
    // The registry handle is removed once the run completes.
    assert_eq!(runtime.ai_registry.active_count(), 0);
}

#[tokio::test]
async fn ai_action_times_out_waiting_for_a_wedged_permit() {
    use nagori_core::{AiProviderKind, AiSettings};
    // The request budget is anchored at registration, so a predecessor wedged
    // while holding the single text-generation permit must time *this* request
    // out before it ever reaches the model — not leave it queued forever.
    let (runtime, _) = runtime_with_mock_ai();
    runtime
        .save_settings(AppSettings {
            ai: AiSettings {
                enabled: true,
                provider: AiProviderKind::AppleNative,
                // Small real budget so the test bounds the permit wait without
                // sleeping long (paused time would not advance behind the
                // semaphore acquire).
                request_timeout_ms: 50,
                ..AiSettings::default()
            },
            ..AppSettings::default()
        })
        .await
        .expect("save settings");
    let id = runtime
        .add_text("hello world".to_owned())
        .await
        .expect("entry should be added");

    // Stand in for a wedged predecessor by holding the only text-generation
    // permit for the whole test.
    let held = runtime
        .ai_registry
        .semaphores()
        .text_generation
        .clone()
        .acquire_owned()
        .await
        .expect("hold the only text-generation permit");

    // `AiActionRun` isn't `Debug`, so bind the error explicitly rather than
    // `expect_err`.
    let Err(err) = runtime
        .start_ai_action(id, AiActionId::Summarize, AiRequestOptions::default())
        .await
    else {
        panic!("a wedged permit must time the request out before it starts");
    };
    assert!(
        matches!(&err, AppError::Ai(msg) if msg.contains("timed out waiting for a concurrency permit")),
        "got {err:?}"
    );
    // The timed-out request must not leak a registry slot.
    assert_eq!(runtime.ai_registry.active_count(), 0);
    drop(held);
}

#[tokio::test]
async fn ai_action_unsupported_without_engine() {
    // No engine wired (the default test builder): AI actions surface as
    // Unsupported even when enabled.
    let (runtime, _) = runtime_with_memory_clipboard();
    runtime
        .save_settings(ai_enabled_settings(AppSettings::default()))
        .await
        .expect("save settings");
    let id = runtime
        .add_text("hello".to_owned())
        .await
        .expect("entry should be added");
    let err = runtime
        .run_ai_action(id, AiActionId::Summarize, AiRequestOptions::default())
        .await
        .expect_err("no engine must surface as Unsupported");
    assert!(matches!(err, AppError::Unsupported(_)), "got {err:?}");
}

#[tokio::test]
async fn ai_action_provider_mismatch_is_unsupported() {
    use nagori_core::{AiProviderKind, AiSettings};
    // Engine is AppleNative, but settings select the (unwired)
    // OpenAI-compatible provider.
    let (runtime, _) = runtime_with_mock_ai();
    runtime
        .save_settings(AppSettings {
            ai: AiSettings {
                enabled: true,
                provider: AiProviderKind::OpenAiCompatible,
                ..AiSettings::default()
            },
            ..AppSettings::default()
        })
        .await
        .expect("save settings");
    let id = runtime
        .add_text("hello".to_owned())
        .await
        .expect("entry should be added");
    let err = runtime
        .run_ai_action(id, AiActionId::Summarize, AiRequestOptions::default())
        .await
        .expect_err("provider mismatch must surface as Unsupported");
    assert!(matches!(err, AppError::Unsupported(_)), "got {err:?}");
}

#[tokio::test]
async fn ai_action_not_in_allow_list_is_blocked() {
    use nagori_core::{AiProviderKind, AiSettings};
    // A non-empty allow-list that omits the action blocks it.
    let (runtime, _) = runtime_with_mock_ai();
    runtime
        .save_settings(AppSettings {
            ai: AiSettings {
                enabled: true,
                provider: AiProviderKind::AppleNative,
                allowed_actions: vec![AiActionId::Translate],
                ..AiSettings::default()
            },
            ..AppSettings::default()
        })
        .await
        .expect("save settings");
    let id = runtime
        .add_text("hello".to_owned())
        .await
        .expect("entry should be added");
    let err = runtime
        .run_ai_action(id, AiActionId::Summarize, AiRequestOptions::default())
        .await
        .expect_err("action outside the allow-list must be blocked");
    assert!(matches!(err, AppError::Policy(_)), "got {err:?}");
}

#[tokio::test]
async fn ai_action_applies_user_regex_to_redaction() {
    // The classifier must be settings-aware so a `regex_denylist` rule
    // redacts AI input even on an entry classified before the rule existed.
    // The mock echoes the shaped input, so we can assert what it received.
    let (runtime, _) = runtime_with_mock_ai();
    let id = runtime
        .add_text("ticket INTERNAL-42 stays".to_owned())
        .await
        .expect("public entry should be added");
    runtime
        .save_settings(ai_enabled_settings(AppSettings {
            regex_denylist: vec![r"INTERNAL-\d+".to_owned()],
            ..AppSettings::default()
        }))
        .await
        .expect("save settings");

    let output = runtime
        .run_ai_action(id, AiActionId::Summarize, AiRequestOptions::default())
        .await
        .expect("summarize should succeed");
    assert!(
        !output.text.contains("INTERNAL-42"),
        "user regex must redact AI input, got: {}",
        output.text,
    );
    assert!(
        output.text.contains("[REDACTED]"),
        "expected redaction marker, got: {}",
        output.text,
    );
}

#[tokio::test]
async fn quick_redact_secrets_applies_user_regex_on_public_entry() {
    use nagori_core::QuickActionId;
    // `RedactSecrets` routes input through the settings-aware classifier
    // before the built-in scrub, so a `regex_denylist`-only match on a
    // Public entry is still redacted.
    let (runtime, _) = runtime_with_memory_clipboard();
    let id = runtime
        .add_text("ticket INTERNAL-77 stays".to_owned())
        .await
        .expect("public entry should be added");
    runtime
        .save_settings(AppSettings {
            regex_denylist: vec![r"INTERNAL-\d+".to_owned()],
            ..AppSettings::default()
        })
        .await
        .expect("save settings");

    let output = runtime
        .run_quick_action(id, QuickActionId::RedactSecrets)
        .await
        .expect("redact-secrets should succeed");
    assert!(
        !output.text.contains("INTERNAL-77"),
        "user regex must redact RedactSecrets input, got: {}",
        output.text,
    );
    assert!(
        output.text.contains("[REDACTED]"),
        "expected redaction marker, got: {}",
        output.text,
    );
}

#[tokio::test]
async fn ai_action_blocked_when_input_exceeds_max_bytes() {
    // A body over the per-action byte cap is refused rather than truncated.
    let (runtime, _) = runtime_with_mock_ai();
    runtime
        .save_settings(ai_enabled_settings(AppSettings {
            max_entry_size_bytes: 256 * 1024,
            ..AppSettings::default()
        }))
        .await
        .expect("save settings");
    let large = "a".repeat(65 * 1024);
    let id = runtime
        .add_text(large)
        .await
        .expect("large entry should be added");
    let err = runtime
        .run_ai_action(id, AiActionId::Summarize, AiRequestOptions::default())
        .await
        .expect_err("must refuse inputs over max_bytes");
    assert!(matches!(err, AppError::Policy(_)), "got {err:?}");
}

#[tokio::test]
async fn ai_action_cancel_via_registry_yields_cancelled() {
    // Cancelling by `request_id` through the registry propagates to the
    // stream, which terminates with `Cancelled` and removes its handle.
    let (runtime, _) = runtime_with_mock_ai();
    runtime
        .save_settings(ai_enabled_settings(AppSettings::default()))
        .await
        .expect("save settings");
    let id = runtime
        .add_text("a long body to summarize repeatedly".to_owned())
        .await
        .expect("entry should be added");
    let run = runtime
        .start_ai_action(id, AiActionId::Summarize, AiRequestOptions::default())
        .await
        .expect("summarize should start");
    assert!(runtime.cancel_ai_action(run.request_id));

    let mut events = run.events;
    let mut saw_cancelled = false;
    while let Some(item) = events.next().await {
        if matches!(item, Ok(AiEvent::Cancelled)) {
            saw_cancelled = true;
        }
        assert!(
            !matches!(item, Ok(AiEvent::Done { .. })),
            "a cancelled run must not complete"
        );
    }
    assert!(saw_cancelled, "stream must terminate with Cancelled");
    drop(events);
    assert_eq!(runtime.ai_registry.active_count(), 0);
}

#[tokio::test]
async fn allow_streaming_false_suppresses_intermediate_snapshots() {
    // With the UI streaming toggle off the daemon must surface only the
    // terminal result — no `Delta` / `Replace` — while `Done.final_text` still
    // carries the full output.
    use nagori_ai::{AiEngine, MockBackend};
    use nagori_core::{AiProviderKind, AiSettings};

    let store = SqliteStore::open_memory().expect("memory store");
    let clipboard = Arc::new(MemoryClipboard::new());
    let engine = AiEngine::builder(AiProviderKind::AppleNative)
        .text_generator(Arc::new(MockBackend::with_output("hello world")))
        .build();
    let runtime = NagoriRuntime::builder(store)
        .clipboard(clipboard)
        .ai_engine(Arc::new(engine))
        .build_for_test();
    runtime
        .save_settings(AppSettings {
            ai: AiSettings {
                enabled: true,
                provider: AiProviderKind::AppleNative,
                allow_streaming: false,
                ..AiSettings::default()
            },
            ..AppSettings::default()
        })
        .await
        .expect("save settings");
    let id = runtime
        .add_text("some text".to_owned())
        .await
        .expect("entry should be added");
    let run = runtime
        .start_ai_action(id, AiActionId::Summarize, AiRequestOptions::default())
        .await
        .expect("summarize should start");

    let mut events = run.events;
    let mut final_text = None;
    while let Some(item) = events.next().await {
        match item.expect("no stream error") {
            AiEvent::Delta { .. } | AiEvent::Replace { .. } => {
                panic!("streaming is disabled; no intermediate snapshot may surface")
            }
            AiEvent::Done {
                final_text: text, ..
            } => final_text = Some(text),
            AiEvent::Cancelled => panic!("unexpected cancel"),
        }
    }
    assert_eq!(final_text.as_deref(), Some("hello world"));
    drop(events);
    assert_eq!(runtime.ai_registry.active_count(), 0);
}

#[tokio::test]
async fn request_max_input_tokens_tightens_the_input_budget() {
    // A per-request `max_input_tokens` below the input's estimate refuses the
    // run before the backend is touched, even though the model's hard cap would
    // otherwise have admitted it. No registry slot is leaked.
    let (runtime, _) = runtime_with_mock_ai();
    runtime
        .save_settings(ai_enabled_settings(AppSettings::default()))
        .await
        .expect("save settings");
    let id = runtime
        .add_text("the quick brown fox jumps over the lazy dog".to_owned())
        .await
        .expect("entry should be added");
    let options = AiRequestOptions {
        max_input_tokens: Some(1),
        ..AiRequestOptions::default()
    };
    let Err(err) = runtime
        .start_ai_action(id, AiActionId::Summarize, options)
        .await
    else {
        panic!("a 1-token budget must reject this input");
    };
    assert!(matches!(err, AppError::Policy(_)), "got {err:?}");
    assert_eq!(runtime.ai_registry.active_count(), 0);
}

#[tokio::test]
async fn translate_action_threads_target_language_to_backend() {
    // The translate option (target language) reaches the backend, the
    // translation semaphore is acquired and released, and the non-streaming
    // result arrives as a single terminal `Done`.
    let (runtime, _) = runtime_with_mock_translator();
    runtime
        .save_settings(ai_enabled_settings(AppSettings::default()))
        .await
        .expect("save settings");
    let id = runtime
        .add_text("hello world".to_owned())
        .await
        .expect("entry should be added");
    let options = AiRequestOptions {
        target_language: Some("ja".to_owned()),
        ..AiRequestOptions::default()
    };
    let run = runtime
        .start_ai_action(id, AiActionId::Translate, options)
        .await
        .expect("translate should start");
    let mut events = run.events;
    let mut final_text = None;
    while let Some(item) = events.next().await {
        if let Ok(AiEvent::Done {
            final_text: text, ..
        }) = item
        {
            final_text = Some(text);
            break;
        }
    }
    assert_eq!(final_text.as_deref(), Some("[ja] hello world"));
    drop(events);
    assert_eq!(runtime.ai_registry.active_count(), 0);
}

#[tokio::test]
async fn translate_action_without_target_language_is_unsupported() {
    // With no target language the engine refuses with a capability mismatch,
    // which surfaces as Unsupported.
    let (runtime, _) = runtime_with_mock_translator();
    runtime
        .save_settings(ai_enabled_settings(AppSettings::default()))
        .await
        .expect("save settings");
    let id = runtime
        .add_text("hello".to_owned())
        .await
        .expect("entry should be added");
    let err = runtime
        .run_ai_action(id, AiActionId::Translate, AiRequestOptions::default())
        .await
        .expect_err("translate without a target language must error");
    assert!(matches!(err, AppError::Unsupported(_)), "got {err:?}");
    assert_eq!(runtime.ai_registry.active_count(), 0);
}

#[tokio::test]
async fn run_ai_action_translate_honours_request_options_target_language() {
    // The one-shot IPC path must forward the request options to the backend, so
    // a translate with a target language succeeds rather than being run with
    // the defaults a wire request used to be reduced to.
    let (runtime, _) = runtime_with_mock_translator();
    runtime
        .save_settings(ai_enabled_settings(AppSettings::default()))
        .await
        .expect("save settings");
    let id = runtime
        .add_text("hello".to_owned())
        .await
        .expect("entry should be added");
    let options = AiRequestOptions {
        target_language: Some("ja".to_owned()),
        ..AiRequestOptions::default()
    };
    let output = runtime
        .run_ai_action(id, AiActionId::Translate, options)
        .await
        .expect("translate with a target language should succeed");
    assert_eq!(output.text, "[ja] hello");
    assert_eq!(runtime.ai_registry.active_count(), 0);
}

#[tokio::test]
async fn ai_availability_reports_disabled_by_default() {
    let (runtime, _) = runtime_with_mock_ai();
    let report = runtime.ai_availability().await.expect("availability");
    assert_eq!(report.overall_status, AiOverallStatus::Disabled);
}

/// A backend that streams a `Delta` and then closes the stream without ever
/// emitting the terminal `Done` — a crashed generation or a truncated FFI
/// bridge. `run_ai_action` must surface this as an error rather than returning
/// the partial accumulation as a successful (silently truncated) result.
#[derive(Debug)]
struct NoDoneBackend;

#[async_trait::async_trait]
impl nagori_ai::TextGenerator for NoDoneBackend {
    fn capabilities(&self) -> nagori_ai::TextGenerationCapabilities {
        nagori_ai::TextGenerationCapabilities {
            streaming: true,
            guided_generation: false,
            on_device: true,
        }
    }

    async fn availability(&self) -> nagori_ai::BackendAvailability {
        nagori_ai::BackendAvailability::Available
    }

    async fn stream_text(
        &self,
        _req: nagori_ai::TextGenerationRequest,
        _cancel: tokio_util::sync::CancellationToken,
    ) -> std::result::Result<nagori_ai::AiEventStream, nagori_core::AiError> {
        // One Delta, then the stream ends — no `Done`.
        let events = futures::stream::iter(vec![Ok(AiEvent::Delta {
            seq: 0,
            text: "partial".to_owned(),
        })]);
        Ok(events.boxed())
    }
}

#[tokio::test]
async fn run_ai_action_errors_when_stream_ends_without_done() {
    use nagori_ai::AiEngine;
    use nagori_core::AiProviderKind;

    let store = SqliteStore::open_memory().expect("memory store should open");
    let clipboard = Arc::new(MemoryClipboard::new());
    let engine = AiEngine::builder(AiProviderKind::AppleNative)
        .text_generator(Arc::new(NoDoneBackend))
        .build();
    let runtime = NagoriRuntime::builder(store)
        .clipboard(clipboard)
        .ai_engine(Arc::new(engine))
        .build_for_test();
    runtime
        .save_settings(ai_enabled_settings(AppSettings::default()))
        .await
        .expect("save settings");
    let id = runtime
        .add_text("summarise me".to_owned())
        .await
        .expect("entry should be added");

    let err = runtime
        .run_ai_action(id, AiActionId::Summarize, AiRequestOptions::default())
        .await
        .expect_err("a stream that never emits Done must not be reported as success");
    assert!(matches!(err, AppError::Ai(_)), "got {err:?}");
    // The registry handle is still released even on the no-Done path.
    assert_eq!(runtime.ai_registry.active_count(), 0);
}

//! AI action commands (quick actions, streaming actions, availability),
//! the semantic-index status/rebuild, and saving an AI result as an entry.

use futures::StreamExt;
use nagori_core::{
    AiActionId, AiEvent, AiRequestOptions, AppError, EntryRepository, QuickActionId, RequestId,
    is_text_safe_for_default_output,
};
use tauri::{AppHandle, Emitter, State};

use crate::dto::{AiActionResultDto, AiAvailabilityDto, EntryDto, SemanticIndexStatusDto};
use crate::error::{CommandError, CommandResult};
use crate::state::AppState;

use super::parse_entry_id;

#[tauri::command]
pub async fn run_quick_action(
    state: State<'_, AppState>,
    action: QuickActionId,
    entry_id: String,
) -> CommandResult<AiActionResultDto> {
    let id = parse_entry_id(&entry_id)?;
    let output = state.runtime.run_quick_action(id, action).await?;
    Ok(output.into())
}

/// Point-in-time AI availability so the palette can gate the AI actions.
#[tauri::command]
pub async fn get_ai_availability(state: State<'_, AppState>) -> CommandResult<AiAvailabilityDto> {
    let report = state.runtime.ai_availability().await?;
    Ok(report.into())
}

/// Current state of the on-device semantic index (state + indexed/pending/total
/// counts) for the AI settings tab.
#[tauri::command]
pub async fn get_semantic_index_status(
    state: State<'_, AppState>,
) -> CommandResult<SemanticIndexStatusDto> {
    let status = state.runtime.semantic_index_status().await?;
    Ok(status.into())
}

/// Requests a full rebuild of the semantic index: the background worker clears
/// the stored vectors and re-embeds the whole corpus on its next pass.
#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
pub fn rebuild_semantic_index(state: State<'_, AppState>) {
    state.runtime.rebuild_semantic_index();
}

/// Starts a streaming AI action. Returns the request id immediately; events
/// arrive on the `nagori://ai/*` channel, and the run can be cancelled with
/// [`cancel_ai_action`].
#[tauri::command]
pub async fn start_ai_action(
    app: AppHandle,
    state: State<'_, AppState>,
    action: AiActionId,
    entry_id: String,
) -> CommandResult<String> {
    let id = parse_entry_id(&entry_id)?;
    let run = state
        .runtime
        .start_ai_action(id, action, AiRequestOptions::default())
        .await?;
    let request_id = run.request_id;
    let request_id_str = request_id.to_string();
    let _ = app.emit(
        "nagori://ai/started",
        serde_json::json!({ "requestId": request_id_str }),
    );
    let app_for_task = app.clone();
    let id_for_task = request_id_str.clone();
    tauri::async_runtime::spawn(async move {
        drive_ai_stream(app_for_task, id_for_task, run.events).await;
    });
    Ok(request_id_str)
}

/// Cancels an in-flight streaming AI action by request id.
#[tauri::command]
pub async fn cancel_ai_action(state: State<'_, AppState>, request_id: String) -> CommandResult<()> {
    let parsed = request_id
        .parse::<RequestId>()
        .map_err(|err| AppError::InvalidInput(format!("invalid request id: {err}")))?;
    let _ = state.runtime.cancel_ai_action(parsed);
    Ok(())
}

/// Flush threshold for coalesced deltas: emit once the pending buffer reaches
/// this many characters (or hits a boundary / the 50 ms timer below).
const AI_DELTA_FLUSH_CHARS: usize = 64;
/// Maximum time a delta is held before flushing, so slow streams still feel live.
const AI_DELTA_FLUSH_MS: u64 = 50;

/// Drives one AI event stream, coalescing deltas and re-emitting every event on
/// the `nagori://ai/*` channel for the renderer. Dropping the stream (on a
/// terminal event) releases the runtime's request guard, which cancels the run.
async fn drive_ai_stream(app: AppHandle, request_id: String, mut events: nagori_ai::AiEventStream) {
    let mut pending = String::new();
    let mut pending_seq: Option<u64> = None;

    let flush = |app: &AppHandle, seq: Option<u64>, text: &str| {
        if let Some(seq) = seq {
            let _ = app.emit(
                "nagori://ai/delta",
                serde_json::json!({ "requestId": request_id, "seq": seq, "text": text }),
            );
        }
    };

    loop {
        // Hold a pending delta no longer than the flush timer so slow streams
        // still render incrementally; flush immediately once it's non-empty.
        let next = if pending.is_empty() {
            events.next().await
        } else {
            let timer = std::time::Duration::from_millis(AI_DELTA_FLUSH_MS);
            let Ok(item) = tokio::time::timeout(timer, events.next()).await else {
                // Timer elapsed: flush the buffered delta and keep reading.
                flush(&app, pending_seq.take(), &pending);
                pending.clear();
                continue;
            };
            item
        };

        match next {
            None => break,
            Some(Ok(AiEvent::Delta { seq, text })) => {
                if pending_seq.is_none() {
                    pending_seq = Some(seq);
                }
                pending.push_str(&text);
                let boundary = pending
                    .chars()
                    .next_back()
                    .is_some_and(|ch| matches!(ch, '\n' | '。' | '.' | '!' | '?' | '！' | '？'));
                if pending.chars().count() >= AI_DELTA_FLUSH_CHARS || boundary {
                    flush(&app, pending_seq.take(), &pending);
                    pending.clear();
                }
            }
            Some(Ok(AiEvent::Replace { seq, text })) => {
                // A snapshot reset supersedes any buffered append.
                pending.clear();
                pending_seq = None;
                let _ = app.emit(
                    "nagori://ai/replace",
                    serde_json::json!({ "requestId": request_id, "seq": seq, "text": text }),
                );
            }
            Some(Ok(AiEvent::Done {
                final_text,
                created_entry,
                warnings,
            })) => {
                if !pending.is_empty() {
                    flush(&app, pending_seq.take(), &pending);
                    pending.clear();
                }
                let _ = app.emit(
                    "nagori://ai/done",
                    serde_json::json!({
                        "requestId": request_id,
                        "finalText": final_text,
                        "createdEntryId": created_entry.map(|id| id.to_string()),
                        "warnings": warnings,
                    }),
                );
                break;
            }
            Some(Ok(AiEvent::Cancelled)) => {
                let _ = app.emit(
                    "nagori://ai/cancelled",
                    serde_json::json!({ "requestId": request_id }),
                );
                break;
            }
            Some(Err(err)) => {
                let _ = app.emit(
                    "nagori://ai/error",
                    serde_json::json!({
                        "requestId": request_id,
                        "code": err.code,
                        "message": err.message,
                        "remediation": err.remediation.map(|rem| rem.i18n_key),
                    }),
                );
                break;
            }
        }
    }
}

/// Save AI action output as a brand-new clipboard entry. The action menu
/// uses this so users can promote a generated draft into the history.
#[tauri::command]
pub async fn save_ai_result(state: State<'_, AppState>, text: String) -> CommandResult<EntryDto> {
    if text.is_empty() {
        return Err(CommandError::invalid_input("empty AI result"));
    }
    let id = state.runtime.add_text(text).await?;
    let entry = state
        .runtime
        .get_entry(id)
        .await?
        .ok_or(AppError::NotFound)?;
    let include_text = is_text_safe_for_default_output(entry.sensitivity);
    let entry_id = entry.id;
    let summaries = state
        .runtime
        .store()
        .list_representation_summaries(&[entry_id])
        .await?;
    let reps = summaries.get(&entry_id).map_or(&[][..], Vec::as_slice);
    Ok(EntryDto::from_entry(entry, include_text).with_representation_summaries(reps))
}

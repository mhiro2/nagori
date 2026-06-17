//! Entry browsing and mutation commands: search, recent/pinned listing,
//! get/add/copy, delete/purge, clear-history, and pin.

use std::time::{Duration, Instant};

use nagori_core::{
    AppError, EntryRepository, SearchQuery, build_file_summary, is_text_safe_for_default_output,
};
use nagori_search::normalize_text;
use tauri::State;

use crate::dto::{EntryDto, FileSummaryDto, SearchRequestDto, SearchResponseDto, SearchResultDto};
use crate::error::CommandResult;
use crate::state::AppState;

use super::{parse_entry_id, purge_preview_temp_dir, remove_preview_temp_files_for};

const DEFAULT_SEARCH_LIMIT: usize = 50;
const DEFAULT_RECENT_LIMIT: usize = 50;
const MAX_COMMAND_LIMIT: usize = 200;

#[tauri::command]
pub async fn search_clipboard(
    state: State<'_, AppState>,
    request: SearchRequestDto,
) -> CommandResult<SearchResponseDto> {
    let limit = request
        .limit
        .unwrap_or(DEFAULT_SEARCH_LIMIT)
        .clamp(1, MAX_COMMAND_LIMIT);
    let mut query = SearchQuery::new(&request.query, normalize_text(&request.query), limit);
    if let Some(mode) = request.mode {
        query.mode = mode;
    }
    if let Some(filters) = request.filters {
        query.filters = filters.into();
    }

    let started = Instant::now();
    let results = state.runtime.search(query).await?;
    let search_elapsed = started.elapsed();
    let total_candidates = results.len();
    let ids: Vec<_> = results.iter().map(|r| r.entry_id).collect();

    // Hydrate both projections the result rows need (representation summaries
    // and basename-first file summaries) under one timer. `list_file_path_sets`
    // gates on the canonical row sensitivity and only returns `FileList` paths,
    // so the home-folding builder below never sees a sensitive path.
    let summary_started = Instant::now();
    let store = state.runtime.store();
    let summaries = store.list_representation_summaries(&ids).await?;
    let file_path_sets = store.list_file_path_sets(&ids).await?;
    let summary_elapsed = summary_started.elapsed();

    // The current user's home directory folds to `~` in location labels. Pure
    // display shortening (resolved once for the whole batch), never a privacy
    // boundary — the gate above already kept sensitive paths out entirely.
    let home = dirs::home_dir().map(|path| path.to_string_lossy().into_owned());

    let dto_results: Vec<SearchResultDto> = results
        .into_iter()
        .map(|result| {
            let entry_id = result.entry_id;
            let reps = summaries.get(&entry_id).map_or(&[][..], Vec::as_slice);
            let file_summary = file_path_sets
                .get(&entry_id)
                .and_then(|paths| build_file_summary(paths, home.as_deref()))
                .map(FileSummaryDto::from);
            SearchResultDto::from(result)
                .with_representation_summaries(reps)
                .with_file_summary(file_summary)
        })
        .collect();

    let search_elapsed_ms = millis_u64(search_elapsed);
    let summary_elapsed_ms = millis_u64(summary_elapsed);
    let total_elapsed_ms = millis_u64(started.elapsed());
    // Breakdown for diagnosing whether the search pipeline or summary
    // hydration dominates a slow query (vs. the single total the UI shows).
    tracing::debug!(
        search_ms = search_elapsed_ms,
        summary_ms = summary_elapsed_ms,
        total_ms = total_elapsed_ms,
        candidates = total_candidates,
        "search_clipboard_elapsed"
    );

    Ok(SearchResponseDto {
        results: dto_results,
        total_candidates,
        search_elapsed_ms,
        summary_elapsed_ms,
        total_elapsed_ms,
    })
}

/// Saturating conversion of a `Duration` to whole milliseconds, so a wildly
/// long search can't panic the command on the `u128 -> u64` narrowing.
fn millis_u64(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

#[tauri::command]
pub async fn list_recent_entries(
    state: State<'_, AppState>,
    limit: Option<usize>,
) -> CommandResult<Vec<EntryDto>> {
    let limit = limit
        .unwrap_or(DEFAULT_RECENT_LIMIT)
        .clamp(1, MAX_COMMAND_LIMIT);
    let entries = state.runtime.list_recent(limit).await?;
    let ids: Vec<_> = entries.iter().map(|e| e.id).collect();
    let summaries = state
        .runtime
        .store()
        .list_representation_summaries(&ids)
        .await?;
    let dtos = entries
        .into_iter()
        .map(|entry| {
            let entry_id = entry.id;
            let reps = summaries.get(&entry_id).map_or(&[][..], Vec::as_slice);
            EntryDto::from_entry(entry, false).with_representation_summaries(reps)
        })
        .collect();
    Ok(dtos)
}

#[tauri::command]
pub async fn list_pinned_entries(state: State<'_, AppState>) -> CommandResult<Vec<EntryDto>> {
    let entries = state.runtime.list_pinned().await?;
    let ids: Vec<_> = entries.iter().map(|e| e.id).collect();
    let summaries = state
        .runtime
        .store()
        .list_representation_summaries(&ids)
        .await?;
    let dtos = entries
        .into_iter()
        .map(|entry| {
            let entry_id = entry.id;
            let reps = summaries.get(&entry_id).map_or(&[][..], Vec::as_slice);
            EntryDto::from_entry(entry, false).with_representation_summaries(reps)
        })
        .collect();
    Ok(dtos)
}

#[tauri::command]
pub async fn get_entry(state: State<'_, AppState>, id: String) -> CommandResult<Option<EntryDto>> {
    let entry_id = parse_entry_id(&id)?;
    let entry = state.runtime.get_entry(entry_id).await?;
    let Some(entry) = entry else {
        return Ok(None);
    };
    let include_text = is_text_safe_for_default_output(entry.sensitivity);
    let entry_id = entry.id;
    let summaries = state
        .runtime
        .store()
        .list_representation_summaries(&[entry_id])
        .await?;
    let reps = summaries.get(&entry_id).map_or(&[][..], Vec::as_slice);
    Ok(Some(
        EntryDto::from_entry(entry, include_text).with_representation_summaries(reps),
    ))
}

#[tauri::command]
pub async fn copy_entry(state: State<'_, AppState>, id: String) -> CommandResult<()> {
    let entry_id = parse_entry_id(&id)?;
    state.runtime.copy_entry(entry_id).await?;
    Ok(())
}

#[tauri::command]
pub async fn add_entry(state: State<'_, AppState>, text: String) -> CommandResult<EntryDto> {
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

#[tauri::command]
pub async fn delete_entry(state: State<'_, AppState>, id: String) -> CommandResult<()> {
    let entry_id = parse_entry_id(&id)?;
    match state.runtime.delete_entry(entry_id).await {
        Ok(()) => {
            state.clear_last_pasted_if(entry_id);
            // Drop the plaintext Quick Look temp file (if the entry was
            // previewed) so it does not outlive the now-deleted history row.
            remove_preview_temp_files_for(entry_id);
            Ok(())
        }
        // The row was already gone (retention / another delete raced us), but
        // its plaintext preview temp file may still be on disk — drop it for
        // the same reason as the Ok path, matching `delete_entries`' NotFound
        // handling. Still surface NotFound so the frontend reconciles its list.
        Err(AppError::NotFound) => {
            state.clear_last_pasted_if(entry_id);
            remove_preview_temp_files_for(entry_id);
            Err(AppError::NotFound.into())
        }
        Err(err) => Err(err.into()),
    }
}

/// Bulk-delete a list of entries. Used by the palette's multi-select mode
/// so users can select rows with Shift/Cmd-click and discard them in one
/// sweep instead of issuing N round-trips.
///
/// Per-id `NotFound` is swallowed (the entry was concurrently swept by
/// retention or another delete path — the user's intent of "make this
/// gone" is already satisfied) so a single stale id can't abort the
/// whole batch and leave the earlier deletes committed without telling
/// the frontend. Other failures propagate and the frontend reconciles
/// against `list_recent` after the call.
#[tauri::command]
pub async fn delete_entries(state: State<'_, AppState>, ids: Vec<String>) -> CommandResult<usize> {
    let mut purged = 0_usize;
    for id in ids {
        let entry_id = parse_entry_id(&id)?;
        match state.runtime.delete_entry(entry_id).await {
            Ok(()) => {
                state.clear_last_pasted_if(entry_id);
                remove_preview_temp_files_for(entry_id);
                purged += 1;
            }
            Err(AppError::NotFound) => {
                state.clear_last_pasted_if(entry_id);
                remove_preview_temp_files_for(entry_id);
            }
            Err(err) => return Err(err.into()),
        }
    }
    Ok(purged)
}

/// Physically purge rows that were previously hidden by per-entry delete.
/// Normal delete may leave a tombstone until maintenance; this command gives
/// the Settings window an explicit "reclaim now" control.
#[tauri::command]
pub async fn purge_deleted_entries(state: State<'_, AppState>) -> CommandResult<usize> {
    let purged = state.runtime.purge_deleted_entries().await?;
    purge_preview_temp_dir();
    Ok(purged)
}

/// Soft-delete every non-pinned entry. Surfaced through both the secondary
/// "Clear history" hotkey and the palette's multi-select bulk-clear. Pinned
/// entries are intentionally preserved.
#[tauri::command]
pub async fn clear_history(state: State<'_, AppState>) -> CommandResult<usize> {
    Ok(clear_non_pinned_and_previews(&state).await?)
}

/// Soft-delete every non-pinned entry, drop the tracked last-pasted pointer,
/// and wipe the plaintext Quick Look preview cache. Returns the purged count.
///
/// This is the single clear-history primitive shared by the `clear_history`
/// Tauri command, the tray "Clear History" item, and the `ClearHistory`
/// secondary hotkey, so the three surfaces cannot drift on which cleanup steps
/// they run — in particular so none of them can skip the preview-cache purge
/// and leave a cleared `Public` body in `/tmp`. Dropping the last-pasted
/// pointer keeps a later repaste from resolving an evicted id; the cache purge
/// may also drop a pinned entry's temp file, but that file regenerates on the
/// next preview, so the purge is lossless.
pub(crate) async fn clear_non_pinned_and_previews(state: &AppState) -> Result<usize, AppError> {
    let purged = state.runtime.clear_non_pinned().await?;
    state.clear_last_pasted();
    purge_preview_temp_dir();
    Ok(purged)
}

#[tauri::command]
pub async fn pin_entry(state: State<'_, AppState>, id: String, pinned: bool) -> CommandResult<()> {
    let entry_id = parse_entry_id(&id)?;
    state.runtime.pin_entry(entry_id, pinned).await?;
    Ok(())
}

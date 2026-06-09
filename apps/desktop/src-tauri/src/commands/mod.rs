use std::time::{Duration, Instant};

use futures::StreamExt;
use nagori_core::{
    AiActionId, AiEvent, AiRequestOptions, AppError, EntryId, EntryRepository, MAX_PASTE_DELAY_MS,
    PasteFailureReason, PasteFormat, QuickActionId, RequestId, SearchQuery, Sensitivity,
    build_file_summary, is_text_safe_for_default_output,
};
use nagori_search::normalize_text;
use tauri::{AppHandle, Emitter, Manager, State, WebviewWindow};

use crate::dto::{
    AiActionResultDto, AiAvailabilityDto, AppDenyRuleDto, AppSettingsDto, EntryDto, FileSummaryDto,
    HotkeyFailureDto, PasteFormatDto, PasteOptionDto, PermissionStatusDto, PlatformCapabilitiesDto,
    SearchRequestDto, SearchResponseDto, SearchResultDto, SemanticIndexStatusDto,
};
use crate::error::{CommandError, CommandResult};
use crate::state::AppState;

pub mod installer;
pub mod preview;
pub mod updater;

// Preview temp-file helpers live in `preview` now; re-export them so the
// call sites that stayed here (clear-history / delete) and `lib.rs` (startup
// wipe, exit cleanup) keep referring to them by their original paths.
pub(crate) use self::preview::{purge_preview_temp_dir, remove_preview_temp_files_for};

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
pub async fn paste_entry(
    state: State<'_, AppState>,
    window: WebviewWindow,
    id: String,
    format: Option<PasteFormatDto>,
) -> CommandResult<()> {
    let entry_id = parse_entry_id(&id)?;
    // Self-paste guard: hide the palette and re-activate the user's previous
    // frontmost app *before* we send the paste keystroke. Without this the
    // synthesised keystroke lands on Nagori's webview because its window
    // still owns focus, and we paste straight into our own search field.
    //
    // On Linux Wayland `previous_frontmost` is always `None` (the compositor
    // refuses to expose a portable foreground-surface query), so we hide
    // our window and let `wtype` target whatever the compositor considers
    // focused afterwards. On Windows the snapshot now carries the HWND in
    // `native_handle`, so `activate_restore_target` re-foregrounds the
    // exact window the user came from via `SetForegroundWindow` instead
    // of relying on the OS to guess.
    let app = window.app_handle().clone();
    if let Some(target) = app.get_webview_window("main") {
        let _ = target.hide();
    }
    if let Some(prev) = state.take_previous_frontmost() {
        let _ = state.window.activate_restore_target(&prev).await;
    }
    // Give the OS a tick to re-focus the target app before we send the
    // synthesised paste. 60ms is the empirical sweet spot reported by
    // the Maccy / Paste community on macOS; on Windows the same value
    // covers the SetForegroundWindow → IME settle path without making
    // the keystroke feel laggy. Linux Wayland still skips: `wtype`
    // targets whatever the compositor considers focused at send time
    // and the compositor's focus handoff is already synchronous.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    tokio::time::sleep(std::time::Duration::from_millis(60)).await;
    // The palette window was hidden above so a returned `Err` would
    // strand the error inside the now-invisible `searchState.errorMessage`
    // — emit `nagori://paste_failed` first so the App-level toast can
    // surface it on the next open / Settings window, matching the
    // palette path's behaviour.
    //
    // `runtime.paste_entry` collapses copy + synthesis into one call,
    // which makes a `NotFound` / blocked / clipboard-write failure look
    // identical to a synthesis failure to the caller. Inline the two
    // steps here so we can scope the "auto-paste failed — paste
    // manually" hint to genuine synthesis failures: a copy failure
    // means the clipboard never received the entry, so telling the
    // user to "paste manually" would just paste whatever was there
    // before.
    // `get_settings` runs after the palette is hidden, so a settings-load
    // failure would otherwise strand inside the invisible webview. Emit
    // `nagori://paste_failed` first so the App-level toast (Settings
    // window or palette on re-open) still surfaces the failure.
    let settings = match state.runtime.get_settings().await {
        Ok(s) => s,
        Err(err) => {
            tracing::warn!(error = %err, "paste_entry_settings_failed");
            // Compose the toast from the *sanitized* `cmd_err.message`, not the
            // raw `{err}` Display: an `AppError::Storage`/`Search`/`Platform`
            // detail can carry DB paths, SQL fragments, or OS diagnostics, and
            // the bare interpolation would leak them straight into the UI toast.
            let cmd_err: CommandError = err.into();
            let message = format!(
                "paste failed: could not load settings — {}",
                cmd_err.message
            );
            emit_paste_failed(&app, &message);
            return Err(CommandError { message, ..cmd_err });
        }
    };
    let paste_format = format.map_or(settings.paste_format_default, Into::into);
    if let Err(err) = state
        .runtime
        .copy_entry_with_format(entry_id, paste_format)
        .await
    {
        tracing::warn!(error = %err, "paste_entry_copy_failed");
        let cmd_err: CommandError = err.into();
        let message = format!("copy failed: {}", cmd_err.message);
        emit_paste_failed(&app, &message);
        return Err(CommandError { message, ..cmd_err });
    }
    if settings.auto_paste_enabled
        && let Err(err) = state.runtime.paste_frontmost().await
    {
        tracing::warn!(error = %err, "paste_entry_synth_failed");
        let reason = paste_failure_reason(&err);
        let cmd_err: CommandError = err.into();
        let message = format!(
            "auto-paste failed — copy succeeded, paste manually. Underlying error: {}",
            cmd_err.message
        );
        emit_paste_failed_with_reason(&app, &message, &reason);
        return Err(CommandError { message, ..cmd_err });
    }
    state.record_last_pasted(entry_id);
    Ok(())
}

/// Emit the `nagori://paste_failed` toast event with a curated message.
///
/// Both `paste_entry` and `paste_entry_from_palette` hide the originating
/// window before performing the paste, so a returned `Err` alone strands
/// the message inside an invisible store. The repaste-last secondary
/// hotkey (`dispatch_secondary_hotkey` in `lib.rs`) takes the same path.
///
/// Toasts are palette-only: the Settings window surfaces permission and
/// error state through its own inline surfaces (the Setup tab, the
/// Capability table) and deliberately renders no toast stack. So route
/// the emit to the main palette webview unconditionally — it is hidden
/// between sessions but never destroyed, so the toast surfaces on the
/// next palette open. Targeting "settings" here would strand the message
/// in a window that no longer subscribes to the event.
///
/// Emits a toast-only failure with **no** `reason`, for paste-command errors
/// that are not auto-paste-synthesis failures — a clipboard-write (copy)
/// failure or a settings-load failure, where the clipboard was never updated
/// so "copy succeeded — paste manually" would be wrong. The renderer toasts
/// these but does not leave a `StatusBar` diagnostic chip. Genuine synthesis
/// failures use [`emit_paste_failed_with_reason`] instead.
pub(crate) fn emit_paste_failed(app: &AppHandle, message: &str) {
    let _ = app.emit_to(
        "main",
        crate::PASTE_FAILED_EVENT,
        serde_json::json!({ "error": message }),
    );
}

/// Like [`emit_paste_failed`] but carries the classified failure `reason` so
/// the renderer can show a per-reason hint (and the `StatusBar` can leave a
/// persistent diagnostic chip) instead of only a generic toast. The payload
/// is `{ error, reason, tool? }`: `reason` is the stable machine token,
/// `tool` is present only for `ToolMissing` (e.g. `wtype`).
pub(crate) fn emit_paste_failed_with_reason(
    app: &AppHandle,
    message: &str,
    reason: &PasteFailureReason,
) {
    let mut payload = serde_json::json!({ "error": message, "reason": reason.token() });
    if let PasteFailureReason::ToolMissing { tool } = reason {
        payload["tool"] = serde_json::Value::String(tool.clone());
    }
    let _ = app.emit_to("main", crate::PASTE_FAILED_EVENT, payload);
}

/// Pull the classified [`PasteFailureReason`] out of an auto-paste failure.
/// The platform adapters raise `AppError::Paste { reason, .. }`; anything else
/// reaching a paste site is unexpected, so it falls back to `Unknown`.
pub(crate) fn paste_failure_reason(err: &AppError) -> PasteFailureReason {
    match err {
        AppError::Paste { reason, .. } => reason.clone(),
        _ => PasteFailureReason::Unknown,
    }
}

#[allow(clippy::needless_pass_by_value)]
#[tauri::command]
pub fn open_palette(app: AppHandle, state: State<'_, AppState>) -> CommandResult<()> {
    state.remember_previous_frontmost();
    show_main_palette(&app)
}

#[allow(clippy::needless_pass_by_value)]
#[tauri::command]
pub fn close_palette(app: AppHandle, state: State<'_, AppState>) -> CommandResult<()> {
    state.clear_previous_frontmost();
    hide_main_palette(&app)
}

/// Which representation a palette paste should write back to the clipboard.
///
/// Both palette paste commands share the same hide → restore-focus →
/// auto-paste → diagnostics orchestration ([`run_palette_paste`]); they differ
/// only in this copy step. `Format` is the default Enter / alternate-format
/// paste (Preserve copy-back of the publishable set, or plain text); the
/// `Option` resolves to the user's `paste_format_default` when absent.
/// `Representation` is the "paste as <format>" picker, publishing exactly the
/// chosen MIME.
enum PaletteCopyTarget {
    Format(Option<PasteFormat>),
    Representation(String),
}

/// Whether a palette paste synthesises the ⌘/Ctrl+V keystroke after copying.
///
/// Plain Enter uses [`PasteSynthesis::RespectSetting`]: browsing history into
/// the clipboard shouldn't type into the focused app unless the user opted into
/// `auto_paste_enabled`. The explicit "paste as <format>" chord
/// (`Cmd/Ctrl+Shift+Enter`, whether it opens the picker or directly pastes the
/// alternate format) is a deliberate paste, so it uses [`PasteSynthesis::Force`]
/// — the keystroke fires regardless of the setting, and a synthesis failure is
/// surfaced (clipboard + focus are still restored) rather than silently
/// degrading to a copy.
enum PasteSynthesis {
    RespectSetting,
    Force,
}

#[tauri::command]
pub async fn paste_entry_from_palette(
    app: AppHandle,
    state: State<'_, AppState>,
    entry_id: String,
    format: Option<PasteFormatDto>,
    // `true` for the alternate-format chord's direct-paste fallback, which is a
    // deliberate paste and should fire ⌘V even when `auto_paste_enabled` is off;
    // absent/`false` for plain Enter, which honours the setting.
    force_paste: Option<bool>,
) -> CommandResult<()> {
    let entry_id = parse_entry_id(&entry_id)?;
    let synthesis = if force_paste == Some(true) {
        PasteSynthesis::Force
    } else {
        PasteSynthesis::RespectSetting
    };
    run_palette_paste(
        &app,
        &state,
        entry_id,
        PaletteCopyTarget::Format(format.map(Into::into)),
        synthesis,
    )
    .await
}

/// Paste a single chosen representation of an entry from the palette (the
/// "paste as PNG / plain text / files" picker). Shares the focus-restore /
/// auto-paste / diagnostics path with [`paste_entry_from_palette`]; only the
/// clipboard write differs (exactly the picked MIME, never the primary).
#[tauri::command]
pub async fn paste_entry_representation_from_palette(
    app: AppHandle,
    state: State<'_, AppState>,
    entry_id: String,
    mime: String,
) -> CommandResult<()> {
    let entry_id = parse_entry_id(&entry_id)?;
    // The picker is an explicit paste action, so it forces synthesis regardless
    // of `auto_paste_enabled` — selecting "paste as PNG" and getting only a
    // copy would contradict the action.
    run_palette_paste(
        &app,
        &state,
        entry_id,
        PaletteCopyTarget::Representation(mime),
        PasteSynthesis::Force,
    )
    .await
}

/// List the distinct representations the selected entry can be pasted as,
/// driving the desktop "paste as <format>" picker. Returns an empty list for
/// entries with nothing extra to offer (or `Blocked` rows), so the caller can
/// fall back to the plain alternate-format paste.
#[tauri::command]
pub async fn list_paste_options(
    state: State<'_, AppState>,
    entry_id: String,
) -> CommandResult<Vec<PasteOptionDto>> {
    let entry_id = parse_entry_id(&entry_id)?;
    let options = state.runtime.list_paste_options(entry_id).await?;
    Ok(options.iter().map(PasteOptionDto::from_option).collect())
}

/// The shared palette-paste flow: copy the chosen representation, hide the
/// palette, restore the source app's focus, then — when auto-paste is on *or*
/// `synthesis` is [`PasteSynthesis::Force`] (the explicit "paste as" chord) —
/// synthesise the keystroke, emitting `nagori://paste_failed` for the failures
/// that strand behind the now-hidden window. The copy runs *before* the palette
/// hides so a copy error still surfaces in the visible palette.
async fn run_palette_paste(
    app: &AppHandle,
    state: &AppState,
    entry_id: EntryId,
    copy: PaletteCopyTarget,
    synthesis: PasteSynthesis,
) -> CommandResult<()> {
    let settings = match state.runtime.get_settings().await {
        Ok(s) => s,
        Err(err) => {
            state.clear_previous_frontmost();
            return Err(err.into());
        }
    };
    match copy {
        PaletteCopyTarget::Format(format) => {
            state
                .runtime
                .copy_entry_with_format(entry_id, format.unwrap_or(settings.paste_format_default))
                .await?;
        }
        PaletteCopyTarget::Representation(mime) => {
            state
                .runtime
                .copy_entry_representation(entry_id, &mime)
                .await?;
        }
    }
    hide_main_palette(app)?;

    // Re-focus the app the user came from *regardless* of the auto-paste
    // setting. The snapshot was taken at `open_palette` time, so it is the
    // source they copied from / want to paste back into.
    //   * auto-paste ON  — focus must be back on the target before we
    //     synthesise ⌘V, or the keystroke lands in Nagori's own webview.
    //   * auto-paste OFF — the user pastes manually, so handing focus back
    //     means their next ⌘V lands in the right window without first
    //     clicking to re-activate it. Skipping the restore here (the
    //     previous behaviour) left the user's source window in the
    //     background, so ⌘V did nothing until they clicked it — the poor
    //     UX this restore fixes.
    // macOS dispatches on `bundle_id`; Windows re-foregrounds the HWND
    // captured in `native_handle` via `SetForegroundWindow`; Linux Wayland
    // recorded `None`, so this is a no-op and the compositor's own
    // post-hide focus handoff already returns focus to the source surface.
    let restored = match state.take_previous_frontmost() {
        Some(prev) => state.window.activate_restore_target(&prev).await,
        None => Ok(()),
    };

    // Plain Enter honours `auto_paste_enabled` (the user's switch for the
    // synthesised keystroke); the explicit "paste as" chord forces it. Focus is
    // already handed back above, so a manual ⌘V works either way when we skip.
    let synthesise = matches!(synthesis, PasteSynthesis::Force) || settings.auto_paste_enabled;
    if !synthesise {
        // Copy succeeded and we tried to restore focus. A restore failure
        // here only costs the user one manual click, so log it but do not
        // raise a hard error — there is no auto-paste step to "skip".
        if let Err(err) = restored {
            tracing::warn!(error = %err, "palette_previous_app_restore_failed");
        }
        return Ok(());
    }

    // Synthesising: a restore failure means the synthesised paste would
    // land in nagori itself, so surface it. The palette window is already
    // hidden above, so a returned `Err` only reaches the now-invisible
    // `searchState.errorMessage` — emit `nagori://paste_failed` so the
    // App-level toast (Settings window or palette on re-open) shows the
    // failure with the "copy succeeded" framing.
    if let Err(err) = restored {
        tracing::warn!(error = %err, "palette_previous_app_restore_failed");
        // Sanitize before surfacing: the bare `{err}` Display can leak
        // platform diagnostics into the toast. Convert through `CommandError`
        // and reuse its curated `message`, preserving the underlying
        // `code`/`recoverable` so the frontend's i18n + retry policy still
        // see the real cause (same contract as the auto-paste block below).
        // The failure mode here is specifically "couldn't re-focus the source
        // app", so tag it `PreviousAppLost` regardless of the underlying
        // adapter error. (The `None` branch above is the normal Wayland path
        // and never reaches here.)
        let cmd_err: CommandError = err.into();
        let message = format!(
            "auto-paste skipped: failed to restore frontmost app — copy succeeded, paste manually. Underlying error: {}",
            cmd_err.message
        );
        emit_paste_failed_with_reason(app, &message, &PasteFailureReason::PreviousAppLost);
        return Err(CommandError { message, ..cmd_err });
    }

    // Defensive clamp at the use site: `save_settings` already rejects values
    // above `MAX_PASTE_DELAY_MS`, but a stale settings row written by an
    // older daemon, a hand-edited DB, or a future field-rename refactor
    // could still surface `u64::MAX` here. Clamping locally keeps the
    // palette responsive even when the persistence-layer guard is bypassed.
    let delay_ms = settings.paste_delay_ms.min(MAX_PASTE_DELAY_MS);
    tokio::time::sleep(Duration::from_millis(delay_ms)).await;

    // Surface paste failures (Accessibility revoked, Noop controller on
    // unsupported platforms, etc.) — the palette previously rendered them
    // as silent successes which made "auto-paste did nothing" undebuggable
    // for users. The clipboard write itself already succeeded above, so
    // the user can still ⌘V manually after dismissing the error toast.
    // The palette window is hidden by this point, so a returned `Err`
    // alone strands the error inside the now-invisible
    // `searchState.errorMessage`. Emit `nagori://paste_failed` so the
    // App-level toast surfaces it on re-open (or in the open Settings
    // window) with the "copy succeeded" framing intact.
    if let Err(err) = state.runtime.paste_frontmost().await {
        tracing::warn!(error = %err, "palette_auto_paste_failed");
        // Preserve the original `code`/`recoverable` so the frontend's
        // i18n routing and retry policy still see the underlying cause,
        // but build the user-facing message from the *sanitized*
        // `cmd_err.message` (not the bare `AppError` Display, which can
        // leak storage/platform diagnostics) and keep the "copy succeeded"
        // framing so the user knows the clipboard write already landed.
        let reason = paste_failure_reason(&err);
        let cmd_err: CommandError = err.into();
        let message = format!(
            "auto-paste failed — copy succeeded, paste manually. Underlying error: {}",
            cmd_err.message
        );
        emit_paste_failed_with_reason(app, &message, &reason);
        return Err(CommandError { message, ..cmd_err });
    }
    state.record_last_pasted(entry_id);
    Ok(())
}

#[tauri::command]
pub async fn copy_entry_from_palette(
    app: AppHandle,
    state: State<'_, AppState>,
    entry_id: String,
) -> CommandResult<()> {
    let entry_id = parse_entry_id(&entry_id)?;
    state.runtime.copy_entry(entry_id).await?;
    state.clear_previous_frontmost();
    hide_main_palette(&app)?;
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

/// Concatenate the text of multiple entries with newline separators and
/// write the result to the system clipboard. Image / file-list entries and
/// any non-`Public`/`Unknown` (Private / Secret / Blocked) rows are silently
/// skipped — the multi-select UI surfaces the count of skipped entries to the
/// user. Used by the palette's bulk copy action.
#[tauri::command]
pub async fn copy_entries_combined(
    state: State<'_, AppState>,
    ids: Vec<String>,
) -> CommandResult<()> {
    if ids.is_empty() {
        return Err(CommandError::invalid_input("no entries selected"));
    }
    let mut chunks: Vec<String> = Vec::with_capacity(ids.len());
    for id in ids {
        let entry_id = parse_entry_id(&id)?;
        // Skip ids that were concurrently swept by retention / another
        // delete path. Aborting the whole copy because one row of a
        // multi-selection raced with the maintenance loop would be
        // worse than producing a slightly shorter joined string.
        let Some(entry) = state.runtime.get_entry(entry_id).await? else {
            continue;
        };
        // Only `Public` / `Unknown` text is safe to combine into the clipboard
        // without an explicit opt-in. Skipping `Private` here (alongside
        // `Secret` / `Blocked`) keeps bulk copy from silently concatenating
        // sensitive bodies the single-row path would have dropped to
        // preview-only — see `is_text_safe_for_default_output`.
        if !is_text_safe_for_default_output(entry.sensitivity) {
            continue;
        }
        let text = match &entry.content {
            nagori_core::ClipboardContent::Text(t) => Some(t.text.clone()),
            nagori_core::ClipboardContent::Url(u) => Some(u.raw.clone()),
            nagori_core::ClipboardContent::Code(c) => Some(c.text.clone()),
            nagori_core::ClipboardContent::RichText(r) => Some(r.plain_text.clone()),
            _ => None,
        };
        if let Some(text) = text {
            chunks.push(text);
        }
    }
    if chunks.is_empty() {
        return Err(CommandError::invalid_input("no copyable text in selection"));
    }
    let combined = chunks.join("\n");
    // `add_text` only inserts a row; the bulk-copy intent is for the joined
    // text to land on the OS clipboard so the user can ⌘V it elsewhere.
    // Round-trip through `copy_entry` so the clipboard write happens via the
    // same path the palette uses for single-row copies.
    //
    // A retention sweep or IPC clear can race between `add_text` and
    // `copy_entry` and remove the just-inserted row. Retry once before
    // giving up — the user pressed bulk-copy expecting the OS clipboard
    // to actually contain the combined text.
    let id = state.runtime.add_text(combined.clone()).await?;
    match state.runtime.copy_entry(id).await {
        Ok(()) => Ok(()),
        Err(AppError::NotFound) => {
            let id = state.runtime.add_text(combined).await?;
            state.runtime.copy_entry(id).await?;
            Ok(())
        }
        Err(err) => Err(err.into()),
    }
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

/// Re-paste the entry the user most recently pasted via the palette,
/// falling back to the recency-list head when no paste has happened yet
/// or the tracked id has been retention-swept. The shared `AppState`
/// helper backs both this command and the `RepasteLast` secondary hotkey.
#[tauri::command]
pub async fn repaste_last(state: State<'_, AppState>) -> CommandResult<()> {
    match state.repaste_last_or_recency().await {
        Ok(()) => Ok(()),
        Err(AppError::NotFound) => Err(CommandError::invalid_input("no recent entry to re-paste")),
        Err(err) => Err(err.into()),
    }
}

#[tauri::command]
pub async fn pin_entry(state: State<'_, AppState>, id: String, pinned: bool) -> CommandResult<()> {
    let entry_id = parse_entry_id(&id)?;
    state.runtime.pin_entry(entry_id, pinned).await?;
    Ok(())
}

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

#[tauri::command]
pub async fn get_settings(state: State<'_, AppState>) -> CommandResult<AppSettingsDto> {
    // Read the body and its revision as one consistent pair. Two separate reads
    // could return body N with revision N+1 if a write landed in between, and
    // that stale body would then pass the compare-and-swap in `update_settings`
    // and revert the concurrent change.
    let (settings, revision) = state.runtime.get_settings_with_revision().await?;
    let mut dto: AppSettingsDto = settings.into();
    // Stamp the live optimistic-concurrency token so the window can echo it
    // back on the next `update_settings` as the compare-and-swap base.
    dto.revision = revision;
    Ok(dto)
}

/// Canonical password-manager preset bundled with the daemon. The
/// Settings UI calls this to learn what rules its "Block password
/// managers" toggle should add back when the user re-enables the
/// preset after disabling it — without a round trip the frontend
/// would have to keep its own copy of the preset list in sync with
/// `nagori-core`. Pure read; never touches state.
#[tauri::command]
pub fn password_manager_preset() -> Vec<AppDenyRuleDto> {
    nagori_core::password_manager_preset_rules()
        .into_iter()
        .map(Into::into)
        .collect()
}

#[tauri::command]
pub async fn update_settings(
    state: State<'_, AppState>,
    settings: AppSettingsDto,
) -> CommandResult<u64> {
    // The DTO's `revision` is the compare-and-swap base: the revision the
    // window last read via `get_settings` (or learned from a `settings_changed`
    // broadcast). The runtime rejects the write with `settings_conflict` if the
    // stored revision moved since then, so a stale full-blob snapshot cannot
    // revert a concurrent change (e.g. the tray's pause/resume) made in the
    // meantime. It is carried on the DTO rather than as a separate argument so
    // the wire shape stays a single settings object.
    let expected_revision = settings.revision;
    let value: nagori_core::AppSettings = settings.into();
    // Runtime persists the settings *and* re-publishes them on the watch
    // channel so the capture loop, maintenance task, and other subscribers
    // pick up the change without a second round-trip here. The returned
    // revision is the post-write token (callers may ignore it and rely on the
    // broadcast echo to refresh their baseline).
    let revision = state
        .runtime
        .save_settings_checked(value, expected_revision)
        .await?;
    Ok(revision)
}

fn parse_entry_id(value: &str) -> Result<EntryId, CommandError> {
    value
        .parse::<EntryId>()
        .map_err(|err| CommandError::invalid_input(format!("invalid entry id: {err}")))
}

// Tauri injects `WebviewWindow` by value into command parameters, so the
// pedantic `needless_pass_by_value` lint does not apply here.
#[allow(clippy::needless_pass_by_value)]
#[tauri::command]
pub fn toggle_palette(state: State<'_, AppState>, window: WebviewWindow) -> CommandResult<()> {
    let app = window.app_handle();
    let Some(target) = app.get_webview_window("main") else {
        return Ok(());
    };
    if target.is_visible().unwrap_or(false) {
        state.clear_previous_frontmost();
        hide_main_palette(app)
    } else {
        // Capture frontmost before we steal focus — see
        // `AppState::remember_previous_frontmost`.
        state.remember_previous_frontmost();
        show_main_palette(app)
    }
}

#[allow(clippy::needless_pass_by_value)]
#[tauri::command]
pub fn hide_palette(window: WebviewWindow, state: State<'_, AppState>) -> CommandResult<()> {
    // Mirror `close_palette` / `toggle_palette`: dropping the palette also
    // discards the captured frontmost snapshot so a later open re-captures
    // from scratch rather than restoring stale focus.
    state.clear_previous_frontmost();
    let app = window.app_handle();
    hide_main_palette(app)
}

fn show_main_palette(app: &AppHandle) -> CommandResult<()> {
    if let Some(target) = app.get_webview_window("main") {
        recenter_palette_on_cursor_monitor(&target);
        target
            .show()
            .and_then(|()| target.set_focus())
            .map_err(|err| CommandError::internal(err.to_string()))?;
    }
    Ok(())
}

/// Re-center the palette on whichever monitor currently holds the mouse
/// cursor, leaving it ready for `show()`.
///
/// `tauri.conf.json` declares the `main` window with `"center": true`, but
/// Tauri only honours that on the *primary* monitor at creation time and the
/// window keeps its position across hide/show. On a multi-monitor setup the
/// palette would therefore always reappear on the primary display rather than
/// the screen the user is working on, so we recompute the centered position
/// from the cursor's monitor on every open. Cursor — rather than the focused
/// app window — because it is the only signal Tauri exposes portably: Wayland
/// structurally withholds other surfaces' geometry from non-compositor
/// clients (see `nagori-platform`'s `frontmost_app` notes).
///
/// Coordinate spaces differ by platform and we have to honour each toolkit's
/// native expectations, otherwise the palette lands on the wrong monitor or
/// off-center under mixed-DPI:
/// - **macOS and Linux/GTK** position windows in a unified *logical points*
///   space. `cursor_position()` reports physical pixels (logical × scale), but
///   `monitor_from_point` hit-tests in logical units (`CGDisplayBounds` on
///   macOS, `gdk_display_get_monitor_at_point` on GTK), so we scale the cursor
///   back to points before the lookup and center in logical units, handing
///   `set_position` a `LogicalPosition`. That sidesteps the toolkit's
///   physical→logical round-trip, which divides by the window's *current*
///   monitor scale and would mis-center when the target monitor differs.
///   (macOS scales the cursor by the *primary* monitor; X11/GTK applies one
///   global `GDK_SCALE` across monitors — so the primary monitor's scale is
///   the right divisor on both.)
/// - **Windows** uses a unified *physical pixel* space end to end
///   (`MonitorFromPoint` + `SetWindowPos`), so cursor, monitor geometry, and
///   `set_position` all stay in physical pixels.
///
/// Best-effort: any probe failure leaves the window where it was so the
/// palette still opens. Falls back from the cursor's monitor to the window's
/// current monitor and finally the primary monitor. On Wayland `cursor_position`
/// is unavailable and `set_position` is a no-op, so the compositor keeps owning
/// placement regardless.
pub(crate) fn recenter_palette_on_cursor_monitor(window: &WebviewWindow) {
    let Ok(cursor) = window.cursor_position() else {
        return;
    };

    // Translate the physical cursor into the space `monitor_from_point`
    // expects on this platform (see the doc comment): logical points on
    // macOS/GTK, physical pixels on Windows.
    #[cfg(not(target_os = "windows"))]
    let (cursor_x, cursor_y) = {
        let primary_scale = window
            .primary_monitor()
            .ok()
            .flatten()
            .map_or(1.0, |monitor| monitor.scale_factor());
        (cursor.x / primary_scale, cursor.y / primary_scale)
    };
    #[cfg(target_os = "windows")]
    let (cursor_x, cursor_y) = (cursor.x, cursor.y);

    let monitor = window
        .monitor_from_point(cursor_x, cursor_y)
        .ok()
        .flatten()
        .or_else(|| window.current_monitor().ok().flatten())
        .or_else(|| window.primary_monitor().ok().flatten());
    let Some(monitor) = monitor else {
        return;
    };
    let Ok(window_size) = window.outer_size() else {
        return;
    };
    if window_size.width == 0 || window_size.height == 0 {
        // A window that hasn't been realized yet can report a degenerate size
        // (notably GTK before the first map). Centering off that would scatter
        // the palette, so leave it at its current position for this open rather
        // than computing from garbage.
        return;
    }

    #[cfg(not(target_os = "windows"))]
    {
        // Center in logical points. The window's logical size is invariant
        // across monitors, so derive it from its current physical size and
        // scale; the monitor's logical bounds come from its own scale. A
        // negative offset (window larger than the monitor) still yields a true
        // center rather than pinning a corner.
        let monitor_scale = monitor.scale_factor();
        let window_scale = window.scale_factor().unwrap_or(monitor_scale);
        let mon_left = f64::from(monitor.position().x) / monitor_scale;
        let mon_top = f64::from(monitor.position().y) / monitor_scale;
        let mon_width = f64::from(monitor.size().width) / monitor_scale;
        let mon_height = f64::from(monitor.size().height) / monitor_scale;
        let win_width = f64::from(window_size.width) / window_scale;
        let win_height = f64::from(window_size.height) / window_scale;
        let _ = window.set_position(tauri::LogicalPosition::new(
            mon_left + (mon_width - win_width) / 2.0,
            mon_top + (mon_height - win_height) / 2.0,
        ));
    }
    #[cfg(target_os = "windows")]
    {
        // Center in physical pixels. Signed math keeps the window centered
        // (equal overflow on each edge) even when it is larger than the
        // monitor; `try_from`/`saturating_add` keep the offsets free of `as`
        // casts so the pedantic cast lints stay quiet.
        let position = monitor.position();
        let monitor_size = monitor.size();
        let monitor_width = i32::try_from(monitor_size.width).unwrap_or(i32::MAX);
        let monitor_height = i32::try_from(monitor_size.height).unwrap_or(i32::MAX);
        let window_width = i32::try_from(window_size.width).unwrap_or(0);
        let window_height = i32::try_from(window_size.height).unwrap_or(0);
        let _ = window.set_position(tauri::PhysicalPosition::new(
            position
                .x
                .saturating_add((monitor_width - window_width) / 2),
            position
                .y
                .saturating_add((monitor_height - window_height) / 2),
        ));
    }
}

fn hide_main_palette(app: &AppHandle) -> CommandResult<()> {
    if let Some(target) = app.get_webview_window("main") {
        target
            .hide()
            .map_err(|err| CommandError::internal(err.to_string()))?;
    }
    Ok(())
}

/// Show + focus the standalone Settings window. The window is declared in
/// `tauri.conf.json` with native decorations, so it gets an OS title bar
/// (drag, close button, no always-on-top) — this command only flips its
/// visibility. The palette is hidden as a side effect so the two windows
/// don't fight over focus on hotkey-driven open paths.
pub(crate) fn show_settings_window(app: &AppHandle) -> CommandResult<()> {
    let target = app.get_webview_window("settings").ok_or_else(|| {
        CommandError::internal("settings window is not registered in tauri.conf.json".to_string())
    })?;
    target
        .show()
        .and_then(|()| target.unminimize())
        .and_then(|()| target.set_focus())
        .map_err(|err| CommandError::internal(err.to_string()))?;
    if let Some(palette) = app.get_webview_window("main") {
        let _ = palette.hide();
    }
    Ok(())
}

fn hide_settings_window(app: &AppHandle) -> CommandResult<()> {
    let target = app.get_webview_window("settings").ok_or_else(|| {
        CommandError::internal("settings window is not registered in tauri.conf.json".to_string())
    })?;
    target
        .hide()
        .map_err(|err| CommandError::internal(err.to_string()))?;
    Ok(())
}

#[allow(clippy::needless_pass_by_value)]
#[tauri::command]
pub fn open_settings(window: WebviewWindow, route: Option<String>) -> CommandResult<()> {
    let app = window.app_handle();
    show_settings_window(app)?;
    // Emit *after* the window is shown so the Settings webview is mounted
    // and its `nagori://navigate` listener is attached. `emit_to` scopes
    // the broadcast to the Settings window only — the palette's own
    // navigate handler (App.svelte) would otherwise interpret a tab name
    // as a view name and ignore it, but routing keeps the wire clean.
    if let Some(route) = route {
        let _ = app.emit_to("settings", crate::NAVIGATE_EVENT, route);
    }
    Ok(())
}

#[allow(clippy::needless_pass_by_value)]
#[tauri::command]
pub fn close_settings(window: WebviewWindow) -> CommandResult<()> {
    hide_settings_window(window.app_handle())
}

/// Returns the current OS-level permission status. Used by the onboarding
/// view to surface "auto-paste OFF because Accessibility is missing" hints
/// without requiring the user to dive into the diagnostics CLI.
#[tauri::command]
pub async fn get_permissions(
    state: State<'_, AppState>,
) -> CommandResult<Vec<PermissionStatusDto>> {
    let statuses = state.runtime.permission_check().await?;
    Ok(statuses.into_iter().map(Into::into).collect())
}

/// Static, OS-level capability matrix wired in at runtime startup. Surfaced
/// to the frontend so the Settings → Status view can render "what could
/// work on this OS" alongside the dynamic `get_permissions` snapshot of
/// what currently *does* work. The runtime caches the matrix so this is a
/// cheap clone — safe to call on every Settings open.
#[tauri::command]
pub async fn get_capabilities(
    state: State<'_, AppState>,
) -> CommandResult<PlatformCapabilitiesDto> {
    Ok(state.runtime.capabilities().into())
}

/// Latest global-hotkey registration failure cached by the backend, or
/// `None` if the most recent (re-)registration succeeded. Used by the
/// always-on App-level subscriber to re-hydrate after a startup-race
/// emit: if the listener attached after the emit fired, the live event
/// is lost but the cached state survives. `nagori://hotkey_register_failed`
/// still fires for live updates.
#[allow(clippy::unused_async)]
#[tauri::command]
pub async fn last_hotkey_failure(
    state: State<'_, AppState>,
) -> CommandResult<Option<HotkeyFailureDto>> {
    Ok(state.current_hotkey_failure().map(Into::into))
}

/// Toggle `capture_enabled` without round-tripping the entire settings
/// blob — used by the tray menu's pause/resume entry.
#[tauri::command]
pub async fn set_capture_enabled(
    state: State<'_, AppState>,
    enabled: bool,
) -> CommandResult<AppSettingsDto> {
    let settings = state.runtime.set_capture_enabled(enabled).await?;
    Ok(settings.into())
}

/// Trigger the host's accessibility prompt and return the resulting
/// permission status. Wired to the Setup tab's `[ Grant Accessibility… ]`
/// button.
///
/// macOS: invokes `AXIsProcessTrustedWithOptions(prompt: true)` which
/// surfaces the TCC dialog asynchronously. The runtime stamps
/// `onboarding.accessibility_prompted_at` so the next
/// `permission_check` discriminates `Denied` from `NotDetermined`.
///
/// When `prompt = true` and TCC has *already* been asked previously
/// (i.e. `accessibility_prompted_at` is already `Some` before this
/// call), the OS suppresses the inline dialog. Falling back to `open(1)`
/// on the Privacy pane in that case gives the user a one-click route
/// to flip the toggle manually. On the very first prompt the inline
/// dialog is shown by the OS itself, so we deliberately skip the
/// fallback to avoid stacking a System Settings window on top of the
/// TCC dialog.
///
/// Windows / Linux: no analogous user-toggleable permission exists, so
/// the command returns the same curated status the doctor / Capability
/// table renders (granted-with-caveat for Windows UIPI, requires-wtype
/// for Linux). Returning a structured status rather than an
/// `Unsupported` error keeps the frontend code path symmetrical with
/// macOS.
#[cfg(target_os = "macos")]
#[tauri::command]
pub async fn request_accessibility(
    state: State<'_, AppState>,
    prompt: bool,
) -> CommandResult<PermissionStatusDto> {
    // Snapshot prompt history BEFORE the runtime call. The runtime
    // unconditionally stamps `accessibility_prompted_at` on
    // `prompt = true`, so reading after the call would always see the
    // fresh timestamp and never trigger the System Settings fallback.
    let previously_prompted = state
        .runtime
        .current_settings()
        .onboarding
        .accessibility_prompted_at
        .is_some();
    let status = state.runtime.request_accessibility(prompt).await?;
    if prompt && previously_prompted && status.state != nagori_platform::PermissionState::Granted {
        // TCC suppressed the dialog because it remembers a prior
        // Deny / dismiss; the Privacy pane is the user's only
        // remaining route. Surface a failed `open(1)` as a command
        // error so the Setup card can render it inline (§3.4) instead
        // of silently dropping the user's only escape hatch.
        let open_status = std::process::Command::new("open")
            .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility")
            .status()
            .map_err(|err| {
                CommandError::internal(format!("failed to open the Accessibility pane: {err}"))
            })?;
        if !open_status.success() {
            return Err(CommandError::internal(format!(
                "the Accessibility pane failed to open ({open_status})"
            )));
        }
    }
    Ok(status.into())
}

#[cfg(target_os = "windows")]
#[tauri::command]
pub async fn request_accessibility(
    state: State<'_, AppState>,
    prompt: bool,
) -> CommandResult<PermissionStatusDto> {
    let status = state.runtime.request_accessibility(prompt).await?;
    Ok(status.into())
}

#[cfg(target_os = "linux")]
#[tauri::command]
pub async fn request_accessibility(
    state: State<'_, AppState>,
    prompt: bool,
) -> CommandResult<PermissionStatusDto> {
    let status = state.runtime.request_accessibility(prompt).await?;
    Ok(status.into())
}

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
#[tauri::command]
pub async fn request_accessibility(
    _state: State<'_, AppState>,
    _prompt: bool,
) -> CommandResult<PermissionStatusDto> {
    Err(CommandError::unsupported("request_accessibility"))
}

/// Allowlisted URL schemes for the `open_url_external` external-open
/// gate. The renderer hides the "Enter to open" hint for everything else,
/// but we duplicate the check here so a forged invoke can't escape via a
/// custom scheme handler the user wouldn't expect. `mailto:` was on the
/// original plan but the core URL classifier only tags `http(s)` clips
/// as `ClipboardContent::Url`, so a `mailto:` body never reaches this
/// code path today — re-add it here alongside the classifier change.
const URL_SCHEME_ALLOWLIST: &[&str] = &["https", "http"];

/// Open a URL belonging to a Public entry in the user's default browser.
/// The renderer pre-confirms the host so the user has explicit intent;
/// this handler re-verifies sensitivity and scheme server-side so a
/// forged invoke cannot ferry a Secret body out via the system handler.
/// The `url` argument must match the entry's stored URL — we re-fetch
/// the entry and compare to defend against a compromised renderer
/// claiming any arbitrary target.
#[tauri::command]
pub async fn open_url_external(
    state: State<'_, AppState>,
    entry_id: String,
    url: String,
) -> CommandResult<()> {
    let entry_id = parse_entry_id(&entry_id)?;
    let entry = state
        .runtime
        .get_entry(entry_id)
        .await?
        .ok_or(AppError::NotFound)?;
    // Sensitivity gate first — never reach the OS handler for a
    // Private/Secret/Blocked clip even if the renderer asks. The preview
    // pane already hides the Enter hint for these, but a forged invoke
    // could still arrive (e.g. via DevTools in a debug build).
    let canonical = validate_external_open(&entry, &url)?;
    open_external_url(&canonical)?;
    Ok(())
}

/// Pure gate for `open_url_external` — extracted so the
/// sensitivity / kind / URL-match / scheme-allowlist checks can be
/// exercised without spinning up a runtime. Returns the canonical
/// (parsed-and-re-serialised) URL that should be handed to the platform
/// opener so the rest of the command never re-uses the raw renderer
/// string.
fn validate_external_open(
    entry: &nagori_core::ClipboardEntry,
    requested_url: &str,
) -> Result<String, CommandError> {
    if !matches!(entry.sensitivity, Sensitivity::Public) {
        return Err(CommandError::forbidden(
            "external open is only available for Public entries",
        ));
    }
    let nagori_core::ClipboardContent::Url(stored) = &entry.content else {
        return Err(CommandError::invalid_input("entry is not a URL clip"));
    };
    // Compare against the entry's stored URL so a compromised renderer
    // can't redirect to an attacker-controlled URL while presenting the
    // user's confirm dialog with the legitimate host.
    if requested_url.trim() != stored.raw.trim() {
        return Err(CommandError::invalid_input(
            "url does not match the stored entry",
        ));
    }
    let parsed = url::Url::parse(requested_url.trim())
        .map_err(|err| CommandError::invalid_input(format!("invalid url: {err}")))?;
    let scheme = parsed.scheme();
    if !URL_SCHEME_ALLOWLIST.contains(&scheme) {
        return Err(CommandError::invalid_input(format!(
            "scheme `{scheme}` is not allowed for external open"
        )));
    }
    Ok(parsed.as_str().to_owned())
}

/// Hand the URL to the platform's default URL handler. We shell out
/// directly (mirroring `request_accessibility`'s open-settings fallback) rather than wiring
/// `tauri-plugin-shell`'s JS surface — every call site here is already
/// inside a Rust command that has run the full sensitivity / allowlist
/// gate, so the plugin's capability layer would be redundant overhead.
///
/// Windows uses `ShellExecuteW` directly instead of `cmd /c start`: the
/// canonical URL has already been validated against the entry's stored
/// claim, but `cmd.exe` interprets `&`, `^`, `|`, etc. on its argument
/// strings before invoking `start`, so a future allowlist relaxation
/// (or a URL whose query contains those characters) could turn a benign
/// argument into a shell metacharacter. `ShellExecuteW` skips the shell
/// parser entirely.
fn open_external_url(url: &str) -> CommandResult<()> {
    #[cfg(target_os = "macos")]
    {
        use std::process::Command;
        let mut command = Command::new("open");
        // `--` stops `open(1)` from interpreting a URL beginning with a
        // dash as one of its own flags, even though the upstream parser
        // is currently strict about that — keeps us safe across releases.
        command.arg("--").arg(url);
        let status = command
            .status()
            .map_err(|err| CommandError::internal(err.to_string()))?;
        if !status.success() {
            // Mirror the Windows branch and the paste-failure UX: surface the
            // failure to the WebView so the user sees a toast instead of a
            // silent no-op when LaunchServices refuses the URL.
            return Err(CommandError::internal(format!(
                "open(1) exited with {status}"
            )));
        }
        Ok(())
    }
    #[cfg(target_os = "linux")]
    {
        use std::process::Command;
        let mut command = Command::new("xdg-open");
        command.arg(url);
        let status = command
            .status()
            .map_err(|err| CommandError::internal(err.to_string()))?;
        if !status.success() {
            // `xdg-open` exits non-zero for missing handlers, syntax errors,
            // and tool-not-found shims — none of which should look like
            // success to the WebView.
            return Err(CommandError::internal(format!(
                "xdg-open exited with {status}"
            )));
        }
        Ok(())
    }
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::ffi::OsStrExt;
        use std::ptr::null;
        use windows_sys::Win32::UI::Shell::ShellExecuteW;
        use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
        let operation: Vec<u16> = std::ffi::OsStr::new("open")
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let file: Vec<u16> = std::ffi::OsStr::new(url)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        // SAFETY: `operation` and `file` are NUL-terminated UTF-16 strings
        // that outlive the call. `ShellExecuteW` returns an HINSTANCE that
        // is `> 32` on success per the documented contract. The desktop
        // crate inherits the workspace `unsafe_code = "deny"` lint, so the
        // single Win32 FFI here is opted in locally rather than relaxing
        // the whole crate; the call site is otherwise pure Rust glue.
        #[allow(unsafe_code)]
        let hinstance = unsafe {
            ShellExecuteW(
                null::<core::ffi::c_void>() as _,
                operation.as_ptr(),
                file.as_ptr(),
                null(),
                null(),
                SW_SHOWNORMAL,
            )
        };
        if (hinstance as isize) <= 32 {
            return Err(CommandError::internal(format!(
                "ShellExecuteW failed with code {}",
                hinstance as isize
            )));
        }
        Ok(())
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        let _ = url;
        Err(CommandError::unsupported(
            "external URL open is not supported on this platform",
        ))
    }
}

#[cfg(test)]
mod helper_tests {
    use super::*;

    #[test]
    fn parse_entry_id_accepts_canonical_uuid_form() {
        // EntryId is a thin newtype around UUID; the command layer must
        // round-trip its `Display` form so the WebView can persist an id and
        // hand it back later (e.g. between palette open / preview hover).
        let original = EntryId::new();
        let parsed = parse_entry_id(&original.to_string()).expect("uuid parses");
        assert_eq!(parsed, original);
    }

    fn url_entry(raw: &str) -> nagori_core::ClipboardEntry {
        use nagori_core::{
            ClipboardData, ClipboardRepresentation, ClipboardSequence, ClipboardSnapshot,
            ContentHash, EntryFactory,
        };
        use time::OffsetDateTime;
        let snapshot = ClipboardSnapshot {
            sequence: ClipboardSequence::content_hash(ContentHash::sha256(raw.as_bytes()).value),
            captured_at: OffsetDateTime::now_utc(),
            source: None,
            representations: vec![ClipboardRepresentation {
                mime_type: "text/plain".to_owned(),
                data: ClipboardData::Text(raw.to_owned()),
            }],
        };
        let mut entry = EntryFactory::from_snapshot(snapshot).expect("url snapshot");
        // `from_snapshot` defaults `sensitivity` to `Unknown`; the gate
        // requires an explicit `Public` clip, so set it here for the
        // accept/match/scheme paths and let the negative test override it.
        entry.sensitivity = Sensitivity::Public;
        entry
    }

    #[test]
    fn validate_external_open_accepts_public_https_match() {
        let entry = url_entry("https://example.com/foo?bar=1");
        let canonical = validate_external_open(&entry, "https://example.com/foo?bar=1")
            .expect("public https url is accepted");
        assert!(canonical.starts_with("https://example.com/"));
    }

    #[test]
    fn validate_external_open_rejects_non_public_entries_with_forbidden() {
        // Sensitivity gate must trip before the URL-match check so a
        // forged invoke against a Secret entry can never reach the OS
        // handler — even if the renderer happened to know the URL.
        let mut entry = url_entry("https://example.com/foo");
        entry.sensitivity = Sensitivity::Secret;
        let err = validate_external_open(&entry, "https://example.com/foo")
            .expect_err("secret entries are blocked");
        assert_eq!(err.code, "forbidden");
        assert!(!err.recoverable);
    }

    #[test]
    fn validate_external_open_rejects_url_mismatch() {
        // The renderer-supplied URL must equal the stored one byte-for-byte
        // (after trim) so a compromised webview can't redirect to an
        // attacker-controlled host while displaying the legitimate confirm.
        let entry = url_entry("https://example.com/foo");
        let err = validate_external_open(&entry, "https://attacker.test/foo")
            .expect_err("mismatched url is rejected");
        assert_eq!(err.code, "invalid_input");
        assert!(err.message.contains("does not match"));
    }

    #[test]
    fn validate_external_open_rejects_disallowed_schemes() {
        // `file://`, `javascript:`, etc. must never reach the platform
        // handler — even on a Public clip with a matching URL string —
        // because the system handler interprets them in ways the user
        // does not expect from a clipboard preview.
        // Constructing a `file://` ClipboardEntry isn't possible (the
        // core URL parser only accepts http/https), so we exercise the
        // scheme gate through a hand-rolled mismatch fixture:
        let mut entry = url_entry("https://example.com/foo");
        // Swap the stored URL to a file:// scheme so the scheme gate
        // becomes the failure surface, not the URL-match check.
        if let nagori_core::ClipboardContent::Url(stored) = &mut entry.content {
            stored.raw = "file:///etc/passwd".to_owned();
        }
        let err = validate_external_open(&entry, "file:///etc/passwd")
            .expect_err("file scheme is rejected");
        assert_eq!(err.code, "invalid_input");
        assert!(
            err.message.contains("not allowed"),
            "expected scheme-allowlist message, got {:?}",
            err.message,
        );
    }

    #[test]
    fn parse_entry_id_rejects_garbage_with_invalid_input_error() {
        // Surface the parse failure as `invalid_input` so the WebView can
        // localise the toast and keep the row interactive (recoverable).
        let err = parse_entry_id("not-a-uuid").expect_err("garbage rejected");
        assert_eq!(err.code, "invalid_input");
        assert!(err.recoverable);
        assert!(
            err.message.contains("invalid entry id"),
            "expected curated message, got {:?}",
            err.message,
        );
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

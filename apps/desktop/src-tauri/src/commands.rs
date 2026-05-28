use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use nagori_core::{
    AiActionId, AppError, EntryId, EntryRepository, MAX_PASTE_DELAY_MS, SearchQuery, Sensitivity,
    is_text_safe_for_default_output,
};
use nagori_platform::PreviewItem;
use nagori_search::normalize_text;
use tauri::{AppHandle, Emitter, Manager, State, WebviewWindow};

use crate::dto::{
    AiActionResultDto, AppSettingsDto, CliInstallResultDto, CliInstallStatusDto, EntryDto,
    EntryPreviewDto, HotkeyFailureDto, PasteFormatDto, PermissionStatusDto,
    PlatformCapabilitiesDto, SearchRequestDto, SearchResponseDto, SearchResultDto,
};
use crate::error::{CommandError, CommandResult};
use crate::state::AppState;

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
    let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let total_candidates = results.len();
    let ids: Vec<_> = results.iter().map(|r| r.entry_id).collect();
    let summaries = state
        .runtime
        .store()
        .list_representation_summaries(&ids)
        .await?;
    let dto_results: Vec<SearchResultDto> = results
        .into_iter()
        .map(|result| {
            let entry_id = result.entry_id;
            let reps = summaries.get(&entry_id).map_or(&[][..], Vec::as_slice);
            SearchResultDto::from(result).with_representation_summaries(reps)
        })
        .collect();

    Ok(SearchResponseDto {
        results: dto_results,
        total_candidates,
        elapsed_ms,
    })
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
            let message = format!("paste failed: could not load settings — {err}");
            emit_paste_failed(&app, &message);
            let cmd_err: CommandError = err.into();
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
        let message = format!("copy failed: {err}");
        emit_paste_failed(&app, &message);
        let cmd_err: CommandError = err.into();
        return Err(CommandError { message, ..cmd_err });
    }
    if settings.auto_paste_enabled
        && let Err(err) = state.runtime.paste_frontmost().await
    {
        tracing::warn!(error = %err, "paste_entry_synth_failed");
        let message =
            format!("auto-paste failed — copy succeeded, paste manually. Underlying error: {err}");
        emit_paste_failed(&app, &message);
        let cmd_err: CommandError = err.into();
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
pub(crate) fn emit_paste_failed(app: &AppHandle, message: &str) {
    let _ = app.emit_to(
        "main",
        crate::PASTE_FAILED_EVENT,
        serde_json::json!({ "error": message }),
    );
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

#[tauri::command]
pub async fn paste_entry_from_palette(
    app: AppHandle,
    state: State<'_, AppState>,
    entry_id: String,
    format: Option<PasteFormatDto>,
) -> CommandResult<()> {
    let entry_id = parse_entry_id(&entry_id)?;
    let settings = match state.runtime.get_settings().await {
        Ok(s) => s,
        Err(err) => {
            state.clear_previous_frontmost();
            return Err(err.into());
        }
    };
    state
        .runtime
        .copy_entry_with_format(
            entry_id,
            format.map_or(settings.paste_format_default, Into::into),
        )
        .await?;
    hide_main_palette(&app)?;

    // Settings load is the user's chance to disable auto-paste. If we can't
    // read it, propagate the error rather than guessing — the copy still
    // succeeded, and the palette UI can show "copied, but auto-paste status
    // unknown" with the recoverable error.
    if !settings.auto_paste_enabled {
        state.clear_previous_frontmost();
        return Ok(());
    }

    // Re-focus the previously frontmost app before synthesising the paste
    // keystroke. macOS dispatches via `bundle_id`; Windows now uses the
    // HWND captured in `native_handle` to call `SetForegroundWindow`
    // directly. Linux Wayland records `None` for `previous_frontmost`
    // entirely, so the call is a no-op and `wtype` targets whatever the
    // compositor considers focused.
    if let Some(prev) = state.take_previous_frontmost()
        && let Err(err) = state.window.activate_restore_target(&prev).await
    {
        // Surface restore failure to the UI: the entry was copied but we
        // never refocused the originating app, so the synthesised paste
        // would land in nagori itself. The palette window is already
        // hidden above, so a returned `Err` only reaches the now-invisible
        // `searchState.errorMessage` — emit `nagori://paste_failed` so
        // the App-level toast (Settings window or palette on re-open)
        // shows the failure with the "copy succeeded" framing.
        tracing::warn!(error = %err, "palette_previous_app_restore_failed");
        let message = format!(
            "auto-paste skipped: failed to restore frontmost app — copy succeeded, paste manually. Underlying error: {err}"
        );
        emit_paste_failed(&app, &message);
        return Err(CommandError::internal(message));
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
        let message =
            format!("auto-paste failed — copy succeeded, paste manually. Underlying error: {err}");
        emit_paste_failed(&app, &message);
        // Preserve the original `code`/`recoverable` so the frontend's
        // i18n routing and retry policy still see the underlying cause,
        // but swap the user-facing message in for the "copy succeeded"
        // framing — the bare `AppError` text strands the user without
        // hint that the clipboard write already landed.
        let cmd_err: CommandError = err.into();
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

/// Build the standard 128 KiB preview body for an entry. The optional
/// `query` is best-effort: when truncation kicks in, the DTO flags
/// whether the query substring lives in the elided middle so the UI can
/// warn that a search hit is hidden. Empty / whitespace queries are
/// ignored so a pristine palette never sees a spurious warning.
#[tauri::command]
pub async fn get_entry_preview(
    state: State<'_, AppState>,
    entry_id: String,
    query: Option<String>,
) -> CommandResult<EntryPreviewDto> {
    let entry_id = parse_entry_id(&entry_id)?;
    let entry = state
        .runtime
        .get_entry(entry_id)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(EntryPreviewDto::from_entry_with_query(
        &entry,
        query.as_deref(),
    ))
}

/// Larger 1 MiB preview body for the expanded preview pane. Sensitivity
/// gating mirrors `get_entry_preview` but is enforced explicitly: any
/// entry that is not `Public` is rejected with a `forbidden` code so the
/// frontend can render a curated message rather than re-fetching with
/// the standard cap.
#[tauri::command]
pub async fn get_entry_preview_full(
    state: State<'_, AppState>,
    entry_id: String,
) -> CommandResult<EntryPreviewDto> {
    let entry_id = parse_entry_id(&entry_id)?;
    let entry = state
        .runtime
        .get_entry(entry_id)
        .await?
        .ok_or(AppError::NotFound)?;
    // Only Public bodies may flow through the full-content path. The
    // standard preview already redacts Private / Secret / Blocked bodies
    // to a placeholder, but the expanded pane hands the entire window
    // over to the body, so the gate is enforced explicitly here.
    if !matches!(entry.sensitivity, Sensitivity::Public) {
        return Err(CommandError::forbidden(
            "expanded preview is only available for Public entries",
        ));
    }
    Ok(EntryPreviewDto::from_entry_full(&entry))
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
    state.runtime.delete_entry(entry_id).await?;
    state.clear_last_pasted_if(entry_id);
    Ok(())
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
                purged += 1;
            }
            Err(AppError::NotFound) => {
                state.clear_last_pasted_if(entry_id);
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
    let purged = state.runtime.clear_non_pinned().await?;
    // Bulk-clear may have removed the tracked last-pasted entry. Drop the
    // pointer so the next repaste falls through to the recency fallback
    // instead of returning NotFound for an evicted id.
    state.clear_last_pasted();
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
pub async fn run_ai_action(
    state: State<'_, AppState>,
    action: AiActionId,
    entry_id: String,
) -> CommandResult<AiActionResultDto> {
    let id = parse_entry_id(&entry_id)?;
    let output = state.runtime.run_ai_action(id, action).await?;
    Ok(output.into())
}

#[tauri::command]
pub async fn get_settings(state: State<'_, AppState>) -> CommandResult<AppSettingsDto> {
    let settings = state.runtime.get_settings().await?;
    Ok(settings.into())
}

#[tauri::command]
pub async fn update_settings(
    state: State<'_, AppState>,
    settings: AppSettingsDto,
) -> CommandResult<()> {
    let value: nagori_core::AppSettings = settings.into();
    // Runtime persists the settings *and* re-publishes them on the watch
    // channel so the capture loop, maintenance task, and other subscribers
    // pick up the change without a second round-trip here.
    state.runtime.save_settings(value).await?;
    Ok(())
}

fn parse_entry_id(value: &str) -> Result<EntryId, CommandError> {
    value
        .parse::<EntryId>()
        .map_err(|err| CommandError::invalid_input(format!("invalid entry id: {err}")))
}

/// Open an OS-native preview overlay for the entry (Quick Look on
/// macOS).
///
/// The palette binds Cmd+Y to this command. The body is gated to
/// `Public` entries because Quick Look materialises content into a
/// temp file readable by any process running as the user — surfacing
/// a `Private` / `Secret` body through the cross-process preview path
/// would silently undo the sensitivity classifier's job. `Blocked`
/// content can never reach this command because it is dropped at
/// capture time.
///
/// `FileList` entries hand the stored paths to the preview API
/// directly; image entries write the payload bytes to a temp file
/// keyed on the entry id; text-flavoured entries render through a
/// `.txt` temp file so Quick Look's text preview can syntax-highlight.
/// Empty / unrecoverable payloads return `InvalidInput` so the palette
/// can fall back to its in-line preview pane.
///
/// On Windows / Linux the preview controller is the
/// [`UnsupportedPreviewController`] stub. The command short-circuits
/// on the capability row *before* materialising any temp file so a
/// forged invoke from a non-macOS host cannot leave preview artefacts
/// in `/tmp` even though the call ultimately fails.
#[tauri::command]
pub async fn preview_entry(state: State<'_, AppState>, entry_id: String) -> CommandResult<()> {
    if !state.runtime.capabilities().preview_quick_look.is_usable() {
        return Err(
            AppError::Unsupported("preview is not available on this platform".to_owned()).into(),
        );
    }
    let entry_id = parse_entry_id(&entry_id)?;
    let entry = state
        .runtime
        .get_entry(entry_id)
        .await?
        .ok_or(AppError::NotFound)?;
    if !matches!(entry.sensitivity, Sensitivity::Public) {
        return Err(CommandError::forbidden(
            "preview is only available for Public entries",
        ));
    }
    let items: Vec<PreviewItem> = match &entry.content {
        nagori_core::ClipboardContent::FileList(file_list) => file_list
            .paths
            .iter()
            .map(|path| PreviewItem::new(PathBuf::from(path)))
            .collect(),
        nagori_core::ClipboardContent::Image(_) => {
            let Some((bytes, mime)) = state.runtime.get_payload(entry_id).await? else {
                return Err(CommandError::invalid_input(
                    "image payload is no longer stored",
                ));
            };
            let path = write_preview_temp_file(entry_id, &bytes, extension_for_image_mime(&mime))?;
            vec![PreviewItem::new(path)]
        }
        nagori_core::ClipboardContent::Text(_)
        | nagori_core::ClipboardContent::Url(_)
        | nagori_core::ClipboardContent::Code(_)
        | nagori_core::ClipboardContent::RichText(_)
        | nagori_core::ClipboardContent::Unknown(_) => {
            let Some(text) = entry.content.plain_text() else {
                return Err(CommandError::invalid_input(
                    "entry has no previewable plain text",
                ));
            };
            let path = write_preview_temp_file(entry_id, text.as_bytes(), "txt")?;
            vec![PreviewItem::new(path)]
        }
    };
    if items.is_empty() {
        return Err(CommandError::invalid_input(
            "entry has no content to preview",
        ));
    }
    state.preview.preview(&items).await?;
    Ok(())
}

/// Map a stored image mime to a Quick-Look-friendly extension. Falling
/// back to `png` is deliberate — every macOS Quick Look generator we
/// care about handles PNG, and the extension is only a hint (Quick
/// Look re-sniffs the bytes anyway).
fn extension_for_image_mime(mime: &str) -> &'static str {
    match mime.to_ascii_lowercase().as_str() {
        "image/jpeg" | "image/jpg" => "jpg",
        "image/gif" => "gif",
        "image/tiff" => "tiff",
        "image/bmp" => "bmp",
        "image/heic" | "image/heif" => "heic",
        "image/webp" => "webp",
        _ => "png",
    }
}

/// Write `bytes` to `~/.../nagori-preview/<entry>.<ext>` and return the
/// path. The directory is created with the same private-mode helper the
/// `SQLite` store uses so the temp payload is not world-readable; reusing
/// the entry id as the filename means repeated previews of the same
/// entry overwrite a single file rather than littering the directory.
fn write_preview_temp_file(
    entry_id: EntryId,
    bytes: &[u8],
    extension: &str,
) -> Result<PathBuf, CommandError> {
    let dir = std::env::temp_dir().join("nagori-preview");
    nagori_storage::ensure_private_directory(&dir).map_err(|err| {
        CommandError::internal(format!(
            "could not prepare preview temp dir {}: {err}",
            dir.display()
        ))
    })?;
    let file = dir.join(format!("{entry_id}.{extension}"));
    std::fs::write(&file, bytes).map_err(|err| {
        CommandError::internal(format!(
            "could not write preview temp file {}: {err}",
            file.display()
        ))
    })?;
    Ok(file)
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
        target
            .show()
            .and_then(|()| target.set_focus())
            .map_err(|err| CommandError::internal(err.to_string()))?;
    }
    Ok(())
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

/// Name of the bundled CLI binary as it ships beside the desktop executable.
/// Tauri strips the target triple from the `bundle.externalBin` entry when it
/// copies the sidecar into the app, so the on-disk name is just `nagori`
/// (`nagori.exe` on Windows).
#[cfg(windows)]
const BUNDLED_CLI_NAME: &str = "nagori.exe";
#[cfg(not(windows))]
const BUNDLED_CLI_NAME: &str = "nagori";

/// Absolute path to the bundled `nagori` CLI that rides next to the desktop
/// executable (declared via `bundle.externalBin`), or `None` when it is
/// missing — most notably under `tauri dev`, where sidecars are not staged
/// beside the dev binary.
fn bundled_cli_path() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let candidate = exe.parent()?.join(BUNDLED_CLI_NAME);
    candidate.is_file().then_some(candidate)
}

/// Per-user `bin` directory the in-app installer links into. `~/.local/bin`
/// is writable without elevation; the user may still need to add it to `PATH`
/// (surfaced via `on_path`).
fn cli_bin_dir() -> Option<PathBuf> {
    Some(dirs::home_dir()?.join(".local").join("bin"))
}

/// Canonicalise `path`, falling back to the path itself when it cannot be
/// resolved (e.g. a dangling link) so comparisons stay total.
fn canonical_or_self(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// The directories on the user's *shell* `PATH`. A GUI app launched from
/// Finder inherits launchd's minimal `PATH`, not the login shell's, so we ask
/// the login+interactive shell for its `PATH` and fall back to the process
/// environment only when that probe fails. Resolved once per status probe so
/// the (possibly slow) shell spawn happens at most once.
fn shell_path_dirs() -> Vec<PathBuf> {
    let raw = user_shell_path()
        .or_else(|| std::env::var("PATH").ok())
        .unwrap_or_default();
    std::env::split_paths(&raw).collect()
}

/// Whether `dir` appears among `path_dirs` (compared canonically).
fn dir_in(dir: &Path, path_dirs: &[PathBuf]) -> bool {
    let target = canonical_or_self(dir);
    path_dirs
        .iter()
        .any(|entry| canonical_or_self(entry) == target)
}

/// Best-effort check that `dir` is on the user's shell `PATH`.
#[cfg(unix)]
fn dir_on_path(dir: &Path) -> bool {
    dir_in(dir, &shell_path_dirs())
}

/// Find an already-installed `nagori` that resolves to `source`. Considers the
/// in-app installer's own `~/.local/bin` target and every directory on the
/// user's shell `PATH` — so a Homebrew-cask link (which lives in Homebrew's
/// `bin`, not `~/.local/bin`) counts as installed and the UI does not nag the
/// user to create a redundant second link. A match whose directory is on
/// `PATH` is preferred over one that is not, so `on_path` (derived from the
/// returned link) reflects whether `nagori` is actually reachable.
fn find_linked_cli(
    source: &Path,
    bin_dir: Option<&Path>,
    path_dirs: &[PathBuf],
) -> Option<PathBuf> {
    let source = std::fs::canonicalize(source).ok()?;
    let candidates: Vec<PathBuf> = bin_dir
        .map(|dir| dir.join("nagori"))
        .into_iter()
        .chain(path_dirs.iter().map(|dir| dir.join(BUNDLED_CLI_NAME)))
        .filter(|cand| std::fs::canonicalize(cand).is_ok_and(|resolved| resolved == source))
        .collect();
    candidates
        .iter()
        .find(|cand| cand.parent().is_some_and(|dir| dir_in(dir, path_dirs)))
        .or_else(|| candidates.first())
        .cloned()
}

/// Whether the bundled binary lives somewhere stable enough to symlink
/// against. macOS App Translocation and `.dmg`-mounted launches, and Linux
/// `AppImage` mounts, expose the executable from an ephemeral path that
/// vanishes when the app quits — a symlink into one of those would dangle. We
/// refuse to link in those cases rather than create a link that silently breaks.
#[cfg(unix)]
fn cli_source_is_stable(path: &Path) -> bool {
    let shown = path.to_string_lossy();
    // macOS: Gatekeeper runs quarantined apps from a randomised read-only
    // `AppTranslocation` copy; `/Volumes/...` means we're running from the
    // still-mounted disk image.
    if shown.contains("/AppTranslocation/") || shown.starts_with("/Volumes/") {
        return false;
    }
    // Linux AppImage: the runtime fuse-mounts the bundle under `/tmp/.mount_*`
    // (exported as `$APPDIR`).
    if shown.contains("/.mount_") {
        return false;
    }
    if std::env::var("APPDIR")
        .is_ok_and(|appdir| !appdir.is_empty() && shown.starts_with(appdir.as_str()))
    {
        return false;
    }
    true
}

/// Ask the user's login shell for its effective `PATH`. Runs
/// `$SHELL -lic 'printf %s "$PATH"'` so both login (`.zprofile`,
/// `.bash_profile`) and interactive (`.zshrc`, `.bashrc`) edits — where
/// `~/.local/bin` additions usually live — are reflected. `stdin` is closed so
/// an interactive shell never blocks on a read. Returns `None` on any failure
/// so the caller can fall back to the process `PATH`.
#[cfg(unix)]
fn user_shell_path() -> Option<String> {
    use std::io::Read;
    let shell = std::env::var("SHELL").ok()?;
    let mut child = std::process::Command::new(shell)
        .args(["-lic", r#"printf %s "$PATH""#])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    // Bound the probe: a slow `.zshrc` / `.bashrc` (network calls, version
    // managers, prompts that wait on a subcommand) must not wedge the Settings
    // CLI tab. Poll for exit and kill the shell if it overruns, then fall back
    // to the process `PATH`.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Ok(Some(_)) | Err(_) => return None,
        }
    }
    let mut path = String::new();
    child.stdout.take()?.read_to_string(&mut path).ok()?;
    (!path.trim().is_empty()).then_some(path)
}

#[cfg(not(unix))]
fn user_shell_path() -> Option<String> {
    None
}

/// Report whether the bundled `nagori` CLI is reachable and whether it has
/// been linked onto the user's `PATH`. Drives the Settings → CLI install
/// affordance. `supported` is `false` on Windows, where the one-click
/// installer is not wired yet and the UI shows manual guidance instead.
///
/// Async because it shells out to the user's login shell to read `PATH`
/// (potentially slow); `spawn_blocking` keeps that work off the main thread so
/// the UI never freezes while the CLI tab loads.
#[tauri::command]
pub async fn cli_install_status() -> CliInstallStatusDto {
    tauri::async_runtime::spawn_blocking(cli_install_status_blocking)
        .await
        .unwrap_or_else(|_| CliInstallStatusDto {
            supported: cfg!(unix),
            bundled: false,
            installed: false,
            installed_path: String::new(),
            bin_dir: String::new(),
            on_path: false,
        })
}

fn cli_install_status_blocking() -> CliInstallStatusDto {
    let bundled = bundled_cli_path();
    let bin_dir = cli_bin_dir();
    let path_dirs = shell_path_dirs();
    // An existing link counts whether the user installed via this button
    // (`~/.local/bin`) or via the Homebrew cask (Homebrew's bin), so the UI
    // does not show "not installed" to cask users.
    let linked = bundled
        .as_deref()
        .and_then(|source| find_linked_cli(source, bin_dir.as_deref(), &path_dirs));
    // When linked, report whether *that* link is reachable (its directory on
    // PATH) — a Homebrew link lives outside `~/.local/bin`. When not linked,
    // fall back to whether the install target dir is on PATH so the pre-install
    // UI can warn up front.
    let on_path = match linked.as_deref().and_then(Path::parent) {
        Some(dir) => dir_in(dir, &path_dirs),
        None => bin_dir
            .as_deref()
            .is_some_and(|dir| dir_in(dir, &path_dirs)),
    };
    // Report the actual link location when found; otherwise the path this
    // installer *would* use, so the UI can name it before installing.
    let installed_path = linked
        .clone()
        .or_else(|| bin_dir.as_ref().map(|dir| dir.join("nagori")))
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_default();
    CliInstallStatusDto {
        supported: cfg!(unix),
        bundled: bundled.is_some(),
        installed: linked.is_some(),
        installed_path,
        bin_dir: bin_dir
            .map(|dir| dir.to_string_lossy().into_owned())
            .unwrap_or_default(),
        on_path,
    }
}

/// Symlink the bundled `nagori` CLI into `~/.local/bin` so it is callable from
/// a terminal. Idempotent: re-running repoints an existing link (e.g. after
/// the app moves). Returns where the link landed and whether the directory is
/// on `PATH` so the UI can prompt the user to extend it.
///
/// macOS / Linux only — `~/.local/bin` is user-writable, so no elevation is
/// needed. The link targets the binary inside the installed app bundle, so it
/// keeps working across in-place updates that replace the app at the same
/// path.
#[cfg(unix)]
#[tauri::command]
pub fn install_cli() -> CommandResult<CliInstallResultDto> {
    let source = bundled_cli_path().ok_or_else(|| {
        CommandError::internal(
            "the bundled nagori CLI was not found beside the app — install the packaged \
             app first (this action is unavailable under `tauri dev`)",
        )
    })?;
    // Refuse to link against an ephemeral copy of the app — the link would
    // dangle once the disk image is ejected or the translocated copy is reaped.
    if !cli_source_is_stable(&source) {
        return Err(CommandError::invalid_input(
            "Nagori is running from a temporary location (a disk image or a translocated \
             copy). Move Nagori to your Applications folder and relaunch it, then install \
             the CLI.",
        ));
    }
    let source_canonical = canonical_or_self(&source);
    let bin_dir = cli_bin_dir()
        .ok_or_else(|| CommandError::internal("could not resolve the home directory"))?;
    std::fs::create_dir_all(&bin_dir).map_err(|err| {
        CommandError::internal(format!("failed to create {}: {err}", bin_dir.display()))
    })?;
    let dest = bin_dir.join("nagori");
    // Idempotently repoint a link we created (handles the app moving between
    // versions), but never clobber a regular file or a foreign symlink the
    // user placed there themselves.
    match std::fs::symlink_metadata(&dest) {
        Ok(meta) => {
            // Repoint only a link we plausibly created: one that already
            // resolves to the current source, or whose target has the exact
            // bundled-CLI shape (`…/Nagori.app/Contents/MacOS/nagori`) so an
            // older app location still counts. A regular file or a foreign
            // symlink is left untouched.
            let ours = meta.file_type().is_symlink()
                && (canonical_or_self(&dest) == source_canonical
                    || std::fs::read_link(&dest)
                        .is_ok_and(|target| target.ends_with("Nagori.app/Contents/MacOS/nagori")));
            if !ours {
                return Err(CommandError::invalid_input(format!(
                    "{} already exists and was not created by Nagori. Remove it manually \
                     and retry.",
                    dest.display()
                )));
            }
            std::fs::remove_file(&dest).map_err(|err| {
                CommandError::internal(format!("failed to replace {}: {err}", dest.display()))
            })?;
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => {
            return Err(CommandError::internal(format!(
                "failed to inspect {}: {err}",
                dest.display()
            )));
        }
    }
    std::os::unix::fs::symlink(&source, &dest).map_err(|err| {
        CommandError::internal(format!(
            "failed to link {} -> {}: {err}",
            dest.display(),
            source.display()
        ))
    })?;
    Ok(CliInstallResultDto {
        installed_path: dest.to_string_lossy().into_owned(),
        bin_dir: bin_dir.to_string_lossy().into_owned(),
        source_path: source.to_string_lossy().into_owned(),
        on_path: dir_on_path(&bin_dir),
    })
}

#[cfg(not(unix))]
#[tauri::command]
pub fn install_cli() -> CommandResult<CliInstallResultDto> {
    Err(CommandError::unsupported("install_cli"))
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

/// Manual "Check for updates now" probe surfaced in Settings → Advanced.
///
/// Returns the discovered release version when an update is available,
/// `None` when the bundled build is already current, and a friendly
/// error otherwise (network down, signature mismatch, malformed
/// updater JSON). MVP behaviour is read-only — we expose the
/// *availability* and the frontend renders the GitHub release link;
/// `download_and_install` is intentionally not wired up yet, so we
/// never silently install.
///
/// Runs on every OS: `release.yaml` ships signed bundles for macOS
/// (`.app`/`.dmg`), Windows (NSIS) and Linux (`deb` + `AppImage`),
/// and `latest.json` lists them all. Whether the discovered update
/// can be installed in place depends on the install medium —
/// reported via `UpdateInfoDto::download_supported` so the UI can
/// fall back to the GitHub release link when self-replacement is
/// not possible (e.g. a `deb`-installed binary, where dpkg would
/// need root).
#[tauri::command]
pub async fn check_for_updates(app: AppHandle) -> CommandResult<Option<UpdateInfoDto>> {
    use tauri_plugin_updater::UpdaterExt;

    let updater = app
        .updater()
        .map_err(|err| CommandError::internal(format!("updater unavailable: {err}")))?;
    match updater.check().await {
        Ok(Some(update)) => Ok(Some(UpdateInfoDto {
            version: update.version.clone(),
            current_version: update.current_version.clone(),
            release_notes: update.body,
            download_supported: in_place_update_supported(),
        })),
        Ok(None) => Ok(None),
        Err(err) => Err(CommandError::internal(format!(
            "update check failed: {err}"
        ))),
    }
}

/// Whether the bundle running on the current host can be replaced in
/// place by `update.download_and_install()`.
///
/// Delegates to `tauri::utils::platform::bundle_type()` — the same
/// signal `tauri-plugin-updater` itself uses to pick a manifest entry,
/// so the UI advertisement and the updater's actual in-place path
/// stay in lock-step. `.app` / `.dmg` and the Windows NSIS bundle run
/// as the user that launched them and can rewrite the install root
/// without a privilege prompt; `AppImage` behaves the same. `deb`
/// installs land under `/usr` and would need `dpkg` + root to
/// replace, so the UI links to the GitHub release instead. When the
/// bundle type is unknown (e.g. `cargo run` during development), the
/// safe default is "no in-place update".
fn in_place_update_supported() -> bool {
    use tauri::utils::{config::BundleType, platform::bundle_type};
    matches!(
        bundle_type(),
        Some(BundleType::App | BundleType::Dmg | BundleType::Nsis | BundleType::AppImage),
    )
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInfoDto {
    pub version: String,
    pub current_version: String,
    pub release_notes: Option<String>,
    pub download_supported: bool,
}

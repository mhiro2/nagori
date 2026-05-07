use std::time::{Duration, Instant};

use nagori_core::{AiActionId, AppError, EntryId, SearchQuery, Sensitivity};
use nagori_search::normalize_text;
use tauri::{AppHandle, Manager, State, WebviewWindow};

use crate::dto::{
    AiActionResultDto, AppSettingsDto, EntryDto, EntryPreviewDto, PasteFormatDto,
    PermissionStatusDto, SearchRequestDto, SearchResponseDto, SearchResultDto,
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
    let dto_results: Vec<SearchResultDto> = results.into_iter().map(Into::into).collect();

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
    Ok(entries
        .into_iter()
        .map(|entry| EntryDto::from_entry(entry, false))
        .collect())
}

#[tauri::command]
pub async fn list_pinned_entries(state: State<'_, AppState>) -> CommandResult<Vec<EntryDto>> {
    let entries = state.runtime.list_pinned().await?;
    Ok(entries
        .into_iter()
        .map(|entry| EntryDto::from_entry(entry, false))
        .collect())
}

#[tauri::command]
pub async fn get_entry(state: State<'_, AppState>, id: String) -> CommandResult<Option<EntryDto>> {
    let entry_id = parse_entry_id(&id)?;
    let entry = state.runtime.get_entry(entry_id).await?;
    Ok(entry.map(|entry| {
        let include_text = is_text_safe_for_default_output(entry.sensitivity);
        EntryDto::from_entry(entry, include_text)
    }))
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
    // frontmost app *before* we send ⌘V. Without this, the synthesised
    // keystroke lands on Nagori's webview because its window still owns
    // focus, and we paste straight into our own search field.
    #[cfg(target_os = "macos")]
    {
        if let Some(target) = window.app_handle().get_webview_window("main") {
            let _ = target.hide();
        }
        if let Some(prev) = state.take_previous_frontmost()
            && let Some(bundle_id) = prev.bundle_id.as_deref()
        {
            let _ = state.window.activate_app(bundle_id).await;
        }
        // Give AppKit a tick to re-focus the target app. 60ms is the
        // empirical sweet spot reported by the Maccy / Paste community —
        // anything <30ms races against the focus restoration.
        tokio::time::sleep(std::time::Duration::from_millis(60)).await;
    }
    #[cfg(not(target_os = "macos"))]
    let _ = window;
    state
        .runtime
        .paste_entry(entry_id, format.map(Into::into))
        .await?;
    Ok(())
}

#[allow(clippy::needless_pass_by_value)]
#[tauri::command]
pub fn open_palette(app: AppHandle, state: State<'_, AppState>) -> CommandResult<()> {
    #[cfg(target_os = "macos")]
    state.remember_previous_frontmost();
    #[cfg(not(target_os = "macos"))]
    let _ = state;
    show_main_palette(&app)
}

#[allow(clippy::needless_pass_by_value)]
#[tauri::command]
pub fn close_palette(app: AppHandle, state: State<'_, AppState>) -> CommandResult<()> {
    #[cfg(target_os = "macos")]
    state.clear_previous_frontmost();
    #[cfg(not(target_os = "macos"))]
    let _ = state;
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
            #[cfg(target_os = "macos")]
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
        #[cfg(target_os = "macos")]
        state.clear_previous_frontmost();
        return Ok(());
    }

    #[cfg(target_os = "macos")]
    {
        if let Some(prev) = state.take_previous_frontmost()
            && let Some(bundle_id) = prev.bundle_id.as_deref()
            && let Err(err) = state.window.activate_app(bundle_id).await
        {
            // Surface restore failure to the UI: the entry was copied but
            // we never refocused the originating app, so the synthesised
            // ⌘V would land in nagori itself. Returning an error lets the
            // palette toast tell the user "copied, please paste manually".
            tracing::warn!(error = %err, "palette_previous_app_restore_failed");
            return Err(CommandError::internal(format!(
                "auto-paste skipped: failed to restore frontmost app — copy succeeded, paste manually. Underlying error: {err}"
            )));
        }
    }

    tokio::time::sleep(Duration::from_millis(settings.paste_delay_ms)).await;

    // Surface paste failures (Accessibility revoked, Noop controller on
    // unsupported platforms, etc.) — the palette previously rendered them
    // as silent successes which made "auto-paste did nothing" undebuggable
    // for users. The clipboard write itself already succeeded above, so
    // the user can still ⌘V manually after dismissing the error toast.
    if let Err(err) = state.runtime.paste_frontmost().await {
        tracing::warn!(error = %err, "palette_auto_paste_failed");
        return Err(err.into());
    }
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
    #[cfg(target_os = "macos")]
    state.clear_previous_frontmost();
    hide_main_palette(&app)?;
    Ok(())
}

#[tauri::command]
pub async fn get_entry_preview(
    state: State<'_, AppState>,
    entry_id: String,
) -> CommandResult<EntryPreviewDto> {
    let entry_id = parse_entry_id(&entry_id)?;
    let entry = state
        .runtime
        .get_entry(entry_id)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(EntryPreviewDto::from_entry(&entry))
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
    Ok(EntryDto::from_entry(entry, include_text))
}

#[tauri::command]
pub async fn delete_entry(state: State<'_, AppState>, id: String) -> CommandResult<()> {
    let entry_id = parse_entry_id(&id)?;
    state.runtime.delete_entry(entry_id).await?;
    Ok(())
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

/// Whether an entry's raw text may ride along on the default DTO without
/// the caller opting in to sensitive output. We accept the most permissive
/// answer only for `Public` / `Unknown` — `Private` and `Secret` always
/// drop to preview-only, and `Blocked` joins them defensively. The capture
/// loop refuses to persist `Blocked` rows today, but a stale row from an
/// older daemon, a future import path, or a corrupted DB could still surface
/// here, so the helper fails closed rather than trusting the upstream gate.
const fn is_text_safe_for_default_output(sensitivity: Sensitivity) -> bool {
    matches!(sensitivity, Sensitivity::Public | Sensitivity::Unknown)
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
        #[cfg(target_os = "macos")]
        state.clear_previous_frontmost();
        hide_main_palette(app)
    } else {
        // Capture frontmost before we steal focus — see
        // `AppState::remember_previous_frontmost`.
        #[cfg(target_os = "macos")]
        state.remember_previous_frontmost();
        #[cfg(not(target_os = "macos"))]
        let _ = state;
        show_main_palette(app)
    }
}

#[allow(clippy::needless_pass_by_value)]
#[tauri::command]
pub fn hide_palette(window: WebviewWindow) -> CommandResult<()> {
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

/// Open the macOS Accessibility privacy pane. Used by the onboarding banner
/// to deep-link the user into the right place — `x-apple.systempreferences:`
/// URLs are intercepted by `open(1)` but not by webview navigation.
#[cfg(target_os = "macos")]
#[tauri::command]
pub async fn open_accessibility_settings() -> CommandResult<()> {
    use std::process::Command;
    Command::new("open")
        .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility")
        .status()
        .map_err(|err| CommandError::internal(err.to_string()))?;
    Ok(())
}

#[cfg(not(target_os = "macos"))]
#[tauri::command]
pub async fn open_accessibility_settings() -> CommandResult<()> {
    Err(CommandError::unsupported("open_accessibility_settings"))
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

    #[test]
    fn is_text_safe_for_default_output_only_admits_public_and_unknown() {
        // Mirrors the gate in `add_entry` / `save_ai_result` / `get_entry`:
        // only Public / Unknown text is safe to ship verbatim. Private and
        // Secret must drop back to preview-only on the default DTO, and
        // Blocked is treated the same — a row that managed to bypass the
        // capture-time block (stale DB, future import path) must not be
        // allowed to surface its raw text just because callers usually
        // never see one.
        assert!(is_text_safe_for_default_output(Sensitivity::Public));
        assert!(is_text_safe_for_default_output(Sensitivity::Unknown));
        assert!(!is_text_safe_for_default_output(Sensitivity::Blocked));
        assert!(!is_text_safe_for_default_output(Sensitivity::Private));
        assert!(!is_text_safe_for_default_output(Sensitivity::Secret));
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
    Ok(EntryDto::from_entry(entry, include_text))
}

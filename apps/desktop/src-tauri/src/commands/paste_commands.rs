//! Paste and palette-copy commands plus the shared auto-paste orchestration.

use std::collections::HashSet;
use std::time::Duration;

use nagori_core::{
    AppError, EntryId, MAX_COMBINED_COPY_ENTRIES, MAX_PASTE_DELAY_MS, PasteFailureReason,
    PasteFormat,
};
use tauri::{AppHandle, Emitter, Manager, State, WebviewWindow};

use crate::dto::{PasteFormatDto, PasteOptionDto};
use crate::error::{CommandError, CommandResult};
use crate::state::AppState;

use super::parse_entry_id;
use super::window_commands::hide_main_palette;

#[tauri::command]
pub async fn paste_entry(
    state: State<'_, AppState>,
    window: WebviewWindow,
    id: String,
    format: Option<PasteFormatDto>,
) -> CommandResult<()> {
    let entry_id = parse_entry_id(&id)?;
    // Serialize the entire focus handoff with clipboard publication. Acquiring
    // the lease before hiding the palette prevents a concurrent paste from
    // changing focus while this command waits to publish its entry.
    let mut lease = state.runtime.clipboard_lease().await;
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
    // Capture the restore result rather than discarding it: if re-focusing the
    // source window fails (no native handle, stale HWND, `SetForegroundWindow`
    // denied) we must not synthesise the paste below, or the keystroke lands in
    // whatever window currently holds focus — the self-paste accident. Mirrors
    // `run_palette_paste`; checked at the auto-paste gate so the clipboard write
    // still happens (the user can paste manually).
    let restored = match state.take_previous_frontmost() {
        Some(prev) => state.window.activate_restore_target(&prev).await,
        None => Ok(()),
    };
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
    // before. Splitting them is only safe under one `ClipboardLease`
    // held across both: it locks out any other publish, and the
    // `ClipboardPublish` token it mints is what authorises the paste
    // below to act on *this* clip.
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
    let publish = match lease.copy_entry_with_format(entry_id, paste_format).await {
        Ok(publish) => publish,
        Err(err) => {
            tracing::warn!(error = %err, "paste_entry_copy_failed");
            let cmd_err: CommandError = err.into();
            let message = format!("copy failed: {}", cmd_err.message);
            emit_paste_failed(&app, &message);
            return Err(CommandError { message, ..cmd_err });
        }
    };
    if settings.auto_paste_enabled {
        // A restore failure means the synthesised paste would land in the
        // wrong window, so surface it and skip synthesis. The clipboard write
        // already succeeded, so the user can still paste manually — same
        // "copy succeeded" framing as `run_palette_paste`.
        if let Err(err) = restored {
            tracing::warn!(error = %err, "paste_entry_previous_app_restore_failed");
            let cmd_err: CommandError = err.into();
            let message = format!(
                "auto-paste skipped: failed to restore frontmost app — copy succeeded, paste manually. Underlying error: {}",
                cmd_err.message
            );
            emit_paste_failed_with_reason(&app, &message, &PasteFailureReason::PreviousAppLost);
            return Err(CommandError { message, ..cmd_err });
        }
        if let Err(err) = lease.paste_frontmost(publish).await {
            tracing::warn!(error = %err, "paste_entry_synth_failed");
            let reason = paste_failure_reason(&err);
            let cmd_err: CommandError = err.into();
            let message = synthesis_failure_message(&reason, &cmd_err.message);
            emit_paste_failed_with_reason(&app, &message, &reason);
            return Err(CommandError { message, ..cmd_err });
        }
    } else if let Err(err) = restored {
        // No synthesis step to abort; a restore failure only costs the user one
        // manual click, so log it but do not raise a hard error.
        tracing::warn!(error = %err, "paste_entry_previous_app_restore_failed");
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

/// Longest notification title / body the palette may raise. Generous for any
/// localized paste-failure wording, small enough that a misbehaving webview
/// cannot push a wall of text into the OS notification centre.
const MAX_NOTICE_TITLE_CHARS: usize = 120;
const MAX_NOTICE_BODY_CHARS: usize = 500;

/// Raise an OS notification for an auto-paste failure that happened while the
/// palette was hidden. The palette composes the localized wording (it already
/// owns the per-reason hints); notifications never take focus from the app the
/// user is in. Best-effort like every other notification here: without
/// permission (or a Linux notification daemon) it silently does nothing.
// Tauri injects `AppHandle` by value into command parameters.
#[allow(clippy::needless_pass_by_value)]
#[tauri::command]
pub fn notify_paste_failure(app: AppHandle, title: String, body: String) -> CommandResult<()> {
    use tauri_plugin_notification::NotificationExt;

    if title.trim().is_empty()
        || title.chars().count() > MAX_NOTICE_TITLE_CHARS
        || body.chars().count() > MAX_NOTICE_BODY_CHARS
    {
        return Err(CommandError::invalid_input(
            "paste failure notice must have a short, non-empty title and body",
        ));
    }
    let _ = app.notification().builder().title(title).body(body).show();
    Ok(())
}

/// The user-facing message for a failed paste synthesis.
///
/// Every reason but one shares the "copy succeeded, paste manually" framing —
/// the clipboard write landed, so a manual ⌘V still does what the user wanted.
/// [`PasteFailureReason::ClipboardChanged`] is the exception: the clipboard no
/// longer holds the entry, so telling the user to paste manually would tell
/// them to paste someone else's content. That case says what happened and
/// leaves the next step to them.
fn synthesis_failure_message(reason: &PasteFailureReason, detail: &str) -> String {
    if matches!(reason, PasteFailureReason::ClipboardChanged) {
        format!(
            "auto-paste cancelled: the clipboard changed after the copy, so nothing was pasted. \
             Underlying error: {detail}"
        )
    } else {
        format!("auto-paste failed — copy succeeded, paste manually. Underlying error: {detail}")
    }
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

/// Which representation a palette paste should write back to the clipboard.
///
/// Both palette paste commands share the same hide → restore-focus →
/// auto-paste → diagnostics orchestration ([`run_palette_paste`]); they differ
/// only in this copy step. `Format` is the default Enter / alternate-format
/// paste (Preserve copy-back of the publishable set, or plain text); the
/// `Option` resolves to the user's `paste_format_default` when absent.
/// `Representation` is the "paste as <format>" picker, publishing exactly the
/// chosen MIME. `Text` is a quick-action result: published as plain text and
/// never stored, so it has no entry to record as the last paste.
enum PaletteCopyTarget {
    Format(EntryId, Option<PasteFormat>),
    Representation(EntryId, String),
    Text(String),
}

#[tauri::command]
pub async fn paste_entry_from_palette(
    app: AppHandle,
    state: State<'_, AppState>,
    entry_id: String,
    format: Option<PasteFormatDto>,
) -> CommandResult<()> {
    let entry_id = parse_entry_id(&entry_id)?;
    run_palette_paste(
        &app,
        &state,
        PaletteCopyTarget::Format(entry_id, format.map(Into::into)),
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
    run_palette_paste(
        &app,
        &state,
        PaletteCopyTarget::Representation(entry_id, mime),
    )
    .await
}

/// Paste a quick-action result into the app the palette was opened from,
/// through the same hide → restore-focus → auto-paste path as an entry. The
/// result is not added to the history; keeping it stays the explicit
/// "save" action.
#[tauri::command]
pub async fn paste_text_from_palette(
    app: AppHandle,
    state: State<'_, AppState>,
    text: String,
) -> CommandResult<()> {
    run_palette_paste(&app, &state, PaletteCopyTarget::Text(text)).await
}

/// Copy a quick-action result to the clipboard without adding it to the
/// history. The palette stays open so the result can still be saved.
#[tauri::command]
pub async fn copy_text_from_palette(state: State<'_, AppState>, text: String) -> CommandResult<()> {
    state.runtime.copy_text(text).await?;
    Ok(())
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
/// palette, restore the source app's focus, then — when auto-paste is on —
/// synthesise the keystroke, emitting `nagori://paste_failed` for the failures
/// that strand behind the now-hidden window. The copy runs *before* the palette
/// hides so a copy error still surfaces in the visible palette.
async fn run_palette_paste(
    app: &AppHandle,
    state: &AppState,
    copy: PaletteCopyTarget,
) -> CommandResult<()> {
    let settings = match state.runtime.get_settings().await {
        Ok(s) => s,
        Err(err) => {
            state.clear_previous_frontmost();
            return Err(err.into());
        }
    };
    // One lease spans the copy, the window hide, the focus restore, the paste
    // delay, and the synthesis. Nothing else in the process can publish to the
    // clipboard inside that window, and the `ClipboardPublish` it mints is what
    // authorises the synthesis below to act on this clip — see
    // `nagori_daemon::ClipboardLease`.
    let mut lease = state.runtime.clipboard_lease().await;
    let (publish, entry_id) = match copy {
        PaletteCopyTarget::Format(entry_id, format) => (
            lease
                .copy_entry_with_format(entry_id, format.unwrap_or(settings.paste_format_default))
                .await?,
            Some(entry_id),
        ),
        PaletteCopyTarget::Representation(entry_id, mime) => (
            lease.copy_entry_representation(entry_id, &mime).await?,
            Some(entry_id),
        ),
        PaletteCopyTarget::Text(text) => (lease.publish_text(text).await?, None),
    };
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

    // Default, alternate, and explicit representation output share this policy.
    // Focus is already handed back, so a manual ⌘V works when auto-paste is off.
    if !settings.auto_paste_enabled {
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
    if let Err(err) = lease.paste_frontmost(publish).await {
        tracing::warn!(error = %err, "palette_auto_paste_failed");
        // Preserve the original `code`/`recoverable` so the frontend's
        // i18n routing and retry policy still see the underlying cause,
        // but build the user-facing message from the *sanitized*
        // `cmd_err.message` (not the bare `AppError` Display, which can
        // leak storage/platform diagnostics) and keep the "copy succeeded"
        // framing so the user knows the clipboard write already landed.
        let reason = paste_failure_reason(&err);
        let cmd_err: CommandError = err.into();
        let message = synthesis_failure_message(&reason, &cmd_err.message);
        emit_paste_failed_with_reason(app, &message, &reason);
        return Err(CommandError { message, ..cmd_err });
    }
    if let Some(entry_id) = entry_id {
        state.record_last_pasted(entry_id);
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
    state.clear_previous_frontmost();
    hide_main_palette(&app)?;
    Ok(())
}

/// Concatenate the text of the selected entries with newline separators, store
/// the result as one entry, and publish it to the system clipboard. Used by the
/// palette's bulk copy action.
///
/// The admission rules — duplicate ids collapsed, the selection capped at
/// `nagori_core::MAX_COMBINED_COPY_ENTRIES`, the join refused once it would
/// exceed `max_entry_size_bytes`, image / file-list and non-`Public`/`Unknown`
/// rows skipped — live in `NagoriRuntime::copy_entries_combined` so the join is
/// bounded before any body is read. Every id is parsed up front, so a malformed
/// one fails the whole call without half-copying the selection.
#[tauri::command]
pub async fn copy_entries_combined(
    state: State<'_, AppState>,
    ids: Vec<String>,
) -> CommandResult<()> {
    // De-duplicate and cap while parsing, so neither the id vector nor the set
    // can grow past the ceiling the runtime enforces — a caller must not be
    // able to make the command allocate for a selection that is about to be
    // refused. The runtime repeats both rules for its other callers (IPC,
    // in-process) and remains the authority on them.
    let capacity = ids.len().min(MAX_COMBINED_COPY_ENTRIES + 1);
    let mut seen = HashSet::with_capacity(capacity);
    let mut entry_ids = Vec::with_capacity(capacity);
    for id in &ids {
        let entry_id = parse_entry_id(id)?;
        if !seen.insert(entry_id) {
            continue;
        }
        entry_ids.push(entry_id);
        if entry_ids.len() > MAX_COMBINED_COPY_ENTRIES {
            return Err(CommandError::invalid_input(format!(
                "combined copy accepts at most {MAX_COMBINED_COPY_ENTRIES} entries"
            )));
        }
    }
    state.runtime.copy_entries_combined(&entry_ids).await?;
    Ok(())
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

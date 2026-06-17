//! Palette and settings window control: show/hide/toggle the palette,
//! open/close the settings window, and the cursor-monitor recentre.

use tauri::{AppHandle, Emitter, Manager, State, WebviewWindow};

use crate::error::{CommandError, CommandResult};
use crate::state::AppState;

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

pub(super) fn hide_main_palette(app: &AppHandle) -> CommandResult<()> {
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

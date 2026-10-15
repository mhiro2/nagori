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
///
/// The cursor's monitor rather than the active window's: the active window's
/// geometry needs per-platform window-list access (and Wayland withholds it
/// outright), while the cursor is where the user's attention already is when
/// they press the hotkey, and the two agree in the common single-app-per-screen
/// case.
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
    if let Some(monitor) = monitor {
        place_in_work_area(window, &monitor, None);
    }
}

/// Size the palette to `height` logical pixels — the height its content asks
/// for at the configured number of visible rows — within the work area of the
/// monitor it is on, then re-centre it there. The width is left as is.
#[allow(clippy::needless_pass_by_value)]
#[tauri::command]
pub fn fit_palette_height(window: WebviewWindow, height: f64) -> CommandResult<()> {
    if !height.is_finite() || height <= 0.0 {
        return Err(CommandError::invalid_input(
            "palette height must be a positive number",
        ));
    }
    let monitor = window
        .current_monitor()
        .ok()
        .flatten()
        .or_else(|| window.primary_monitor().ok().flatten());
    if let Some(monitor) = monitor {
        place_in_work_area(&window, &monitor, Some(height));
    }
    Ok(())
}

/// An axis-aligned rectangle in one coordinate space (logical points on
/// macOS / GTK, physical pixels on Windows — see
/// [`recenter_palette_on_cursor_monitor`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// Centre a window of `width` x `height` in `area`, shrinking it to fit. The
/// area is the monitor's *work area*, so the palette never opens under the
/// menu bar, the Dock or the taskbar, and a palette sized for a larger screen
/// (or for more rows than this one has room for) still fits whole.
pub(crate) fn fit_centered(area: Rect, width: f64, height: f64) -> Rect {
    let width = width.min(area.width).max(0.0);
    let height = height.min(area.height).max(0.0);
    Rect {
        x: area.x + (area.width - width) / 2.0,
        y: area.y + (area.height - height) / 2.0,
        width,
        height,
    }
}

/// Fit and centre `window` in `monitor`'s work area. `height` (logical
/// pixels) replaces the window's current height when given. Best-effort, like
/// the callers: a failed probe leaves the window as it was.
fn place_in_work_area(window: &WebviewWindow, monitor: &tauri::Monitor, height: Option<f64>) {
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
    let work_area = monitor.work_area();

    #[cfg(not(target_os = "windows"))]
    {
        // Logical points. The window's logical size is invariant across
        // monitors, so derive it from its current physical size and scale; the
        // monitor's logical bounds come from its own scale.
        let monitor_scale = monitor.scale_factor();
        let window_scale = window.scale_factor().unwrap_or(monitor_scale);
        let area = Rect {
            x: f64::from(work_area.position.x) / monitor_scale,
            y: f64::from(work_area.position.y) / monitor_scale,
            width: f64::from(work_area.size.width) / monitor_scale,
            height: f64::from(work_area.size.height) / monitor_scale,
        };
        let current_width = f64::from(window_size.width) / window_scale;
        let current_height = f64::from(window_size.height) / window_scale;
        let target = fit_centered(area, current_width, height.unwrap_or(current_height));
        if target.width < current_width || (target.height - current_height).abs() >= 1.0 {
            let _ = window.set_size(tauri::LogicalSize::new(target.width, target.height));
        }
        let _ = window.set_position(tauri::LogicalPosition::new(target.x, target.y));
    }
    #[cfg(target_os = "windows")]
    {
        // Physical pixels end to end; the requested height arrives in logical
        // pixels and is scaled by the window's factor.
        let window_scale = window.scale_factor().unwrap_or(1.0);
        let area = Rect {
            x: f64::from(work_area.position.x),
            y: f64::from(work_area.position.y),
            width: f64::from(work_area.size.width),
            height: f64::from(work_area.size.height),
        };
        let current_width = f64::from(window_size.width);
        let current_height = f64::from(window_size.height);
        let wanted_height = height.map_or(current_height, |h| h * window_scale);
        let target = fit_centered(area, current_width, wanted_height);
        if target.width < current_width || (target.height - current_height).abs() >= 1.0 {
            let _ = window.set_size(tauri::PhysicalSize::new(
                target.width.round(),
                target.height.round(),
            ));
        }
        let _ = window.set_position(tauri::PhysicalPosition::new(
            target.x.round(),
            target.y.round(),
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

#[cfg(test)]
mod placement_tests {
    use super::{Rect, fit_centered};

    const AREA: Rect = Rect {
        x: 0.0,
        y: 25.0,
        width: 1440.0,
        height: 875.0,
    };

    #[test]
    fn centres_inside_the_work_area_not_the_whole_screen() {
        // A 25pt menu bar at the top: the palette centres below it.
        let placed = fit_centered(AREA, 720.0, 480.0);
        assert_eq!(
            placed,
            Rect {
                x: 360.0,
                y: 222.5,
                width: 720.0,
                height: 480.0,
            }
        );
    }

    #[test]
    fn shrinks_a_palette_larger_than_the_work_area() {
        // Twenty rows asked for more height than a small screen has.
        let placed = fit_centered(AREA, 720.0, 1200.0);
        assert!((placed.height - AREA.height).abs() < f64::EPSILON);
        assert!((placed.y - AREA.y).abs() < f64::EPSILON);
        let narrow = fit_centered(AREA, 2000.0, 480.0);
        assert!((narrow.width - AREA.width).abs() < f64::EPSILON);
        assert!(narrow.x.abs() < f64::EPSILON);
    }

    #[test]
    fn follows_a_work_area_offset_on_a_secondary_monitor() {
        let secondary = Rect {
            x: 1440.0,
            y: -200.0,
            width: 1920.0,
            height: 1040.0,
        };
        let placed = fit_centered(secondary, 720.0, 480.0);
        assert!((placed.x - 2040.0).abs() < f64::EPSILON);
        assert!((placed.y - 80.0).abs() < f64::EPSILON);
    }
}

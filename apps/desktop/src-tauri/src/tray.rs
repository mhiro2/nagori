//! System tray icon for the desktop shell.
//!
//! The tray exposes the same actions a power user would otherwise reach
//! through the global hotkey or a CLI invocation: show the palette, pause
//! or resume capture, open settings, and quit. We rebuild the menu when
//! capture state changes so the toggle label tracks reality.
//!
//! The same module powers the macOS menu bar item, the Windows system
//! notification area icon, and the Linux `StatusNotifierItem` /
//! app-indicator entry. Tauri 2's `TrayIconBuilder` is cross-platform, so
//! this file contains no per-OS code paths; environments without
//! `StatusNotifierItem` support (some minimal Linux DEs) simply fail at
//! `install()` time, which the caller logs and degrades to in-app
//! controls.

use std::sync::Mutex;
use tauri::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tauri::tray::{TrayIcon, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, Wry};

use crate::commands::show_settings_window;
use crate::state::AppState;
use crate::toggle_main_palette;

pub(crate) const TRAY_ID: &str = "nagori-main";
const ID_TOGGLE_PALETTE: &str = "tray.toggle_palette";
const ID_TOGGLE_CAPTURE: &str = "tray.toggle_capture";
const ID_OPEN_SETTINGS: &str = "tray.open_settings";
const ID_QUIT: &str = "tray.quit";

/// Cache of the menu items we need to rebuild labels on. Stored in
/// app state so `refresh()` can find them without re-walking the menu.
struct TrayHandles {
    capture_item: MenuItem<Wry>,
}

impl TrayHandles {
    fn set_capture_label(&self, capture_enabled: bool) {
        let label = if capture_enabled {
            "Pause Capture"
        } else {
            "Resume Capture"
        };
        let _ = self.capture_item.set_text(label);
    }
}

pub fn install(app: &AppHandle) -> tauri::Result<()> {
    let toggle_palette_item =
        MenuItem::with_id(app, ID_TOGGLE_PALETTE, "Show Palette", true, None::<&str>)?;
    let toggle_capture_item =
        MenuItem::with_id(app, ID_TOGGLE_CAPTURE, "Pause Capture", true, None::<&str>)?;
    let settings_item = MenuItem::with_id(app, ID_OPEN_SETTINGS, "Settings…", true, None::<&str>)?;
    let separator = PredefinedMenuItem::separator(app)?;
    let quit_item = MenuItem::with_id(app, ID_QUIT, "Quit Nagori", true, None::<&str>)?;

    let menu = Menu::with_items(
        app,
        &[
            &toggle_palette_item,
            &toggle_capture_item,
            &settings_item,
            &separator,
            &quit_item,
        ],
    )?;

    // Tauri loads the bundle icon from `tauri.conf.json`'s `bundle.icon`
    // list as the default window icon, which we reuse here. macOS would
    // render the tray entry from `title` alone, but Windows' notification
    // area and Linux' `StatusNotifierItem` need an actual image — without
    // it the icon is invisible (or a placeholder), so we set it on every
    // platform.
    let mut builder = TrayIconBuilder::with_id(TRAY_ID)
        .title("Nagori")
        .tooltip("Nagori clipboard history")
        .menu(&menu)
        .show_menu_on_left_click(true)
        .on_menu_event(|app, event| handle_menu_event(app, &event))
        .on_tray_icon_event(handle_tray_icon_event);
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    let _tray: TrayIcon = builder.build(app)?;

    app.manage(Mutex::new(TrayHandles {
        capture_item: toggle_capture_item,
    }));

    // Sync the initial label asynchronously so it reflects the persisted
    // `capture_enabled` value rather than the hard-coded "Pause" default.
    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        let runtime = handle.state::<AppState>().runtime.clone();
        let settings = match runtime.get_settings().await {
            Ok(s) => s,
            Err(err) => {
                // Tray label sync only — log and skip rather than fall back
                // to defaults that could mis-state capture status.
                tracing::warn!(error = %err, "tray_initial_label_sync_failed");
                return;
            }
        };
        refresh(&handle, settings.capture_enabled);
    });

    Ok(())
}

pub fn refresh(app: &AppHandle, capture_enabled: bool) {
    let Some(handles) = app.try_state::<Mutex<TrayHandles>>() else {
        return;
    };
    let Ok(handles) = handles.lock() else {
        return;
    };
    handles.set_capture_label(capture_enabled);
}

/// Refresh the tray tooltip from the live health snapshots.
///
/// The default tooltip ("Nagori clipboard history") makes the tray entry
/// recognisable when nothing is wrong, but it does not surface the
/// silent-data-loss case we care about most: capture is still polling,
/// startup said "ready", and yet every clip is being silently dropped
/// because the adapter keeps erroring or the user's denylist matches
/// everything. Reading `CaptureHealth` / `MaintenanceHealth` here lets
/// the tray reflect the same source of truth as `nagori doctor` and the
/// gated startup notification — at the cost of one mutex lock per poll
/// tick, which is negligible.
pub fn refresh_tooltip(app: &AppHandle) {
    let Some(tray) = app.tray_by_id(TRAY_ID) else {
        return;
    };
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    let capture = state.runtime.capture_health().report();
    let maintenance = state.runtime.maintenance_health().report();
    let tooltip = build_tray_tooltip(capture.degraded, maintenance.degraded);
    if let Err(err) = tray.set_tooltip(Some(tooltip)) {
        // Failing to set the tooltip is non-fatal — the tray icon still
        // works, and `nagori doctor` will still surface the underlying
        // condition. Log so the failure is not invisible.
        tracing::warn!(error = %err, "tray_set_tooltip_failed");
    }
}

/// Choose the tray tooltip body for the current capture / maintenance
/// health. Extracted so the wording can be unit-tested without spinning
/// up a Tauri runtime. The "degraded" suffix is appended only when
/// `nagori doctor` would also flag the row as degraded, so the tray
/// and CLI never disagree on whether anything is wrong.
pub(crate) fn build_tray_tooltip(capture_degraded: bool, maintenance_degraded: bool) -> String {
    match (capture_degraded, maintenance_degraded) {
        (false, false) => "Nagori clipboard history".to_owned(),
        (true, false) => "Nagori — clipboard capture degraded (run `nagori doctor`)".to_owned(),
        (false, true) => "Nagori — retention paused (run `nagori doctor`)".to_owned(),
        (true, true) => {
            "Nagori — clipboard capture and retention degraded (run `nagori doctor`)".to_owned()
        }
    }
}

/// Toggle whether the tray icon is currently shown in the OS tray
/// surface (macOS menu bar, Windows notification area, Linux
/// `StatusNotifierItem`). Idempotent: calling repeatedly with the same
/// `visible` is a no-op.
pub fn set_visible(app: &AppHandle, visible: bool) {
    let Some(tray) = app.tray_by_id(TRAY_ID) else {
        return;
    };
    if let Err(err) = tray.set_visible(visible) {
        tracing::warn!(error = %err, visible, "tray_set_visible_failed");
    }
}

fn handle_menu_event(app: &AppHandle, event: &MenuEvent) {
    match event.id.as_ref() {
        ID_TOGGLE_PALETTE => toggle_main_palette(app),
        ID_TOGGLE_CAPTURE => toggle_capture(app),
        ID_OPEN_SETTINGS => open_settings(app),
        ID_QUIT => app.exit(0),
        _ => {}
    }
}

fn handle_tray_icon_event(_tray: &TrayIcon, _event: TrayIconEvent) {
    // Left-click is configured to open the menu via
    // `show_menu_on_left_click(true)`, so we don't need to act here.
}

fn toggle_capture(app: &AppHandle) {
    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        let runtime = handle.state::<AppState>().runtime.clone();
        let current = match runtime.get_settings().await {
            Ok(s) => s.capture_enabled,
            Err(err) => {
                tracing::warn!(error = %err, "tray_toggle_capture_load_failed");
                return;
            }
        };
        if let Err(err) = runtime.set_capture_enabled(!current).await {
            tracing::warn!(error = %err, "tray_toggle_capture_save_failed");
        }
    });
}

fn open_settings(app: &AppHandle) {
    // Settings lives in its own native window (declared in
    // `tauri.conf.json` with `decorations: true` and `alwaysOnTop: false`)
    // so the OS supplies a close button, drag-by-titlebar, and standard
    // Cmd+Tab / Alt+Tab membership. The tray entry just brings that
    // window forward.
    if let Err(err) = show_settings_window(app) {
        tracing::warn!(error = %err.message, "tray_open_settings_failed");
    }
}

#[cfg(test)]
mod tests {
    use super::build_tray_tooltip;

    #[test]
    fn tooltip_reads_clean_when_nothing_is_degraded() {
        let tip = build_tray_tooltip(false, false);
        assert!(tip.contains("Nagori"));
        assert!(!tip.contains("degraded"));
        assert!(!tip.contains("paused"));
    }

    #[test]
    fn tooltip_flags_capture_degraded() {
        // Surfaced first because capture-loss is the silent-data-loss
        // failure mode the user most needs to see; the body must point
        // them at `nagori doctor` for the recorded category.
        let tip = build_tray_tooltip(true, false);
        assert!(tip.contains("capture"));
        assert!(tip.contains("degraded"));
        assert!(tip.contains("nagori doctor"));
    }

    #[test]
    fn tooltip_flags_maintenance_only() {
        // Retention paused but capture still healthy: existing history
        // is being kept around past `max_entries` / retention age, but
        // new clips still land. The body has to say *retention* so the
        // user does not think their clipboard is broken.
        let tip = build_tray_tooltip(false, true);
        assert!(tip.contains("retention"));
        assert!(!tip.contains("capture"));
        assert!(tip.contains("nagori doctor"));
    }

    #[test]
    fn tooltip_flags_both_when_both_degraded() {
        let tip = build_tray_tooltip(true, true);
        assert!(tip.contains("capture"));
        assert!(tip.contains("retention"));
        assert!(tip.contains("nagori doctor"));
    }
}

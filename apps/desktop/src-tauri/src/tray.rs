//! Menu-bar tray icon for the desktop shell.
//!
//! The tray exposes the same actions a power user would otherwise reach
//! through the global hotkey or a CLI invocation: show the palette, pause
//! or resume capture, open settings, and quit. We rebuild the menu when
//! capture state changes so the toggle label tracks reality.

use std::sync::Mutex;
use tauri::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tauri::tray::{TrayIcon, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, Wry};

use crate::state::AppState;
use crate::toggle_main_palette;

const TRAY_ID: &str = "nagori-main";
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

    let _tray: TrayIcon = TrayIconBuilder::with_id(TRAY_ID)
        .title("Nagori")
        .tooltip("Nagori clipboard history")
        .menu(&menu)
        .show_menu_on_left_click(true)
        .on_menu_event(|app, event| handle_menu_event(app, &event))
        .on_tray_icon_event(handle_tray_icon_event)
        .build(app)?;

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
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.set_focus();
        // The frontend listens for this event and switches the route.
        let _ = app.emit("nagori://navigate", "settings");
    }
}

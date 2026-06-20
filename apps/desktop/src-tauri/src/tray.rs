//! System tray icon for the desktop shell.
//!
//! The tray exposes the same actions a power user would otherwise reach
//! through the global hotkey or a CLI invocation: show the palette, toggle
//! clipboard capture, open settings, and quit. Capture is a checkable menu
//! item, and the tray icon dims while capture is paused, so its on/off
//! state is legible both in the open menu and at a glance in the menu bar.
//!
//! The same module powers the macOS menu bar item, the Windows system
//! notification area icon, and the Linux `StatusNotifierItem` /
//! app-indicator entry. Tauri 2's `TrayIconBuilder` is cross-platform, so
//! this file contains no per-OS code paths; environments without
//! `StatusNotifierItem` support (some minimal Linux DEs) simply fail at
//! `install()` time, which the caller logs and degrades to in-app
//! controls.

use std::io::Cursor;
use std::sync::Mutex;
use tauri::image::Image;
use tauri::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tauri::tray::{TrayIcon, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, Wry};

use crate::commands::show_settings_window;
use crate::state::AppState;
use crate::toggle_main_palette;

// PNG bytes for the menu-bar icon, embedded at compile time. We decode
// once in `install()` and hand the RGBA buffer to Tauri — `tauri::Image`
// only accepts pre-decoded pixels, and the `png` crate is a much
// smaller dependency than `image` for the single format we need here.
const TRAY_ICON_PNG: &[u8] = include_bytes!("../icons/tray.png");

fn decode_tray_icon() -> (Vec<u8>, u32, u32) {
    decode_icon_rgba(TRAY_ICON_PNG)
}

fn decode_icon_rgba(png_bytes: &[u8]) -> (Vec<u8>, u32, u32) {
    // png 0.18 requires `BufRead + Seek` on `Decoder::new`; wrap the
    // embedded byte slice in a `Cursor` to satisfy both bounds.
    let mut decoder = png::Decoder::new(Cursor::new(png_bytes));
    // Normalise away the encodings a lossy optimiser (e.g. `pngquant`,
    // which emits an *indexed* PNG) might apply to the bundled asset:
    // `EXPAND` unpacks a palette into RGB(A) and promotes a `tRNS` chunk
    // to a real alpha channel, and `STRIP_16` collapses 16-bit channels
    // to 8-bit. After this the frame is always 8-bit RGB(A) or grayscale,
    // so re-running the icon pipeline can never reintroduce the
    // colour-type panic that crashed the app at launch.
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder
        .read_info()
        .expect("embedded tray PNG header must be valid");
    // png 0.18 returns `Option<usize>` to support progressive decoding;
    // for a fully-buffered header we always have a concrete size.
    let mut buf = vec![
        0;
        reader
            .output_buffer_size()
            .expect("png header parsed → buffer size known")
    ];
    let info = reader
        .next_frame(&mut buf)
        .expect("embedded tray PNG frame must decode");
    buf.truncate(info.buffer_size());

    let rgba = match info.color_type {
        png::ColorType::Rgba => buf,
        png::ColorType::Rgb => {
            let mut out = Vec::with_capacity(buf.len() / 3 * 4);
            for px in buf.chunks_exact(3) {
                out.extend_from_slice(px);
                out.push(0xFF);
            }
            out
        }
        png::ColorType::GrayscaleAlpha => {
            let mut out = Vec::with_capacity(buf.len() * 2);
            for ga in buf.chunks_exact(2) {
                out.extend_from_slice(&[ga[0], ga[0], ga[0], ga[1]]);
            }
            out
        }
        png::ColorType::Grayscale => {
            let mut out = Vec::with_capacity(buf.len() * 4);
            for &g in &buf {
                out.extend_from_slice(&[g, g, g, 0xFF]);
            }
            out
        }
        // `EXPAND` turns palette PNGs into RGB(A), so `Indexed` cannot
        // reach this arm; keep it explicit rather than a catch-all so a
        // future `png` change can't silently hand Tauri raw indices.
        png::ColorType::Indexed => unreachable!("EXPAND removes the palette"),
    };
    (rgba, info.width, info.height)
}

// Alpha multiplier applied to the active icon to render the "capture
// paused" variant. macOS treats `tray.png` as a template image and tints
// it with the menu-bar colour modulated by this alpha, so scaling every
// pixel's alpha down renders a fainter, visibly inactive glyph; on the
// Windows / Linux notification surfaces (no template tinting) it simply
// fades the monochrome glyph. 40% is faint enough to read as "off" yet
// still recognisably the same icon.
const PAUSED_ICON_ALPHA_NUMERATOR: u16 = 2;
const PAUSED_ICON_ALPHA_DENOMINATOR: u16 = 5;

/// Derive the "capture paused" icon from the active RGBA buffer by
/// scaling every pixel's alpha channel. Pure pixel math so it can be
/// unit-tested without a Tauri runtime.
fn dim_rgba_alpha(rgba: &[u8]) -> Vec<u8> {
    let mut out = rgba.to_vec();
    for px in out.chunks_exact_mut(4) {
        // Always ≤ 255 * 2 / 5 = 102, so it fits a u8; `try_from` keeps
        // the cast checked (clippy rejects a bare `as u8`) without an
        // allow attribute, and the saturating fallback can never fire.
        let scaled = u16::from(px[3]) * PAUSED_ICON_ALPHA_NUMERATOR / PAUSED_ICON_ALPHA_DENOMINATOR;
        px[3] = u8::try_from(scaled).unwrap_or(u8::MAX);
    }
    out
}

pub(crate) const TRAY_ID: &str = "nagori-main";
const ID_TOGGLE_PALETTE: &str = "tray.toggle_palette";
const ID_TOGGLE_CAPTURE: &str = "tray.toggle_capture";
const ID_OPEN_SETTINGS: &str = "tray.open_settings";
const ID_CLEAR_HISTORY: &str = "tray.clear_history";
const ID_QUIT: &str = "tray.quit";

/// Cached tray state needed to reflect capture on/off after install:
/// the checkable capture item (whose checkmark we toggle) and the two
/// pre-computed icon variants (active and dimmed-paused) we swap the
/// menu-bar glyph between. Stored in app state so `refresh()` can reach
/// them without re-walking the menu or re-decoding the PNG.
struct TrayHandles {
    capture_item: CheckMenuItem<Wry>,
    icon_width: u32,
    icon_height: u32,
    active_rgba: Vec<u8>,
    paused_rgba: Vec<u8>,
}

pub fn install(app: &AppHandle) -> tauri::Result<()> {
    let toggle_palette_item =
        MenuItem::with_id(app, ID_TOGGLE_PALETTE, "Show Palette", true, None::<&str>)?;
    // Checkable: the checkmark shows whether capture is currently on.
    // Starts checked and is corrected to the persisted value by the
    // async `refresh()` at the end of `install()`.
    let toggle_capture_item = CheckMenuItem::with_id(
        app,
        ID_TOGGLE_CAPTURE,
        "Capture Clipboard",
        true,
        true,
        None::<&str>,
    )?;
    let settings_item = MenuItem::with_id(app, ID_OPEN_SETTINGS, "Settings…", true, None::<&str>)?;
    let separator = PredefinedMenuItem::separator(app)?;
    let clear_history_item =
        MenuItem::with_id(app, ID_CLEAR_HISTORY, "Clear History", true, None::<&str>)?;
    let quit_item = MenuItem::with_id(app, ID_QUIT, "Quit Nagori", true, None::<&str>)?;

    // The separator sets the destructive "Clear History" + "Quit" group off
    // from the everyday Show Palette / Capture / Settings entries above.
    let menu = Menu::with_items(
        app,
        &[
            &toggle_palette_item,
            &toggle_capture_item,
            &settings_item,
            &separator,
            &clear_history_item,
            &quit_item,
        ],
    )?;

    // The colourful bundle icon (used as the window/Dock icon) does not
    // suit menu-bar surfaces: macOS expects a monochrome template image
    // so the menubar tint applies in both light and dark mode, and the
    // Windows / Linux notification surfaces are pixel-tight enough that
    // detail in a full-colour icon is lost — so we use a dedicated
    // monochrome asset here.
    let (active_rgba, width, height) = decode_tray_icon();
    let paused_rgba = dim_rgba_alpha(&active_rgba);
    // Hand a clone to the builder and keep the originals in `TrayHandles`
    // so `refresh()` can swap between the active and paused variants
    // without re-decoding the PNG on every capture toggle.
    let icon = Image::new_owned(active_rgba.clone(), width, height);
    let builder = TrayIconBuilder::with_id(TRAY_ID)
        .icon(icon)
        // No-op on Windows / Linux; on macOS this flags the image as a
        // template so the system renders it with the menubar tint.
        .icon_as_template(true)
        // Intentionally no `.title(...)` — on macOS that would render a
        // text label to the right of the icon in the menu bar, which
        // we don't want. Windows' notification area does not surface a
        // tray title at all and most Linux `StatusNotifierItem` hosts
        // only show the tooltip, so dropping the title costs nothing
        // on the other platforms.
        .tooltip("Nagori clipboard history")
        .menu(&menu)
        .show_menu_on_left_click(true)
        .on_menu_event(|app, event| handle_menu_event(app, &event))
        .on_tray_icon_event(handle_tray_icon_event);
    let _tray: TrayIcon = builder.build(app)?;

    app.manage(Mutex::new(TrayHandles {
        capture_item: toggle_capture_item,
        icon_width: width,
        icon_height: height,
        active_rgba,
        paused_rgba,
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
    // Tick the checkmark so the open menu shows capture on/off directly.
    let _ = handles.capture_item.set_checked(capture_enabled);

    // Swap the menu-bar glyph so the state is legible without opening the
    // menu: the active icon while capturing, the dimmed variant while
    // paused. `set_icon_with_as_template` re-asserts the macOS template
    // flag atomically (a bare `set_icon` would drop it, rendering the
    // swapped glyph as an untinted image), and falls back to `set_icon`
    // on Windows / Linux.
    let Some(tray) = app.tray_by_id(TRAY_ID) else {
        return;
    };
    let rgba = if capture_enabled {
        &handles.active_rgba
    } else {
        &handles.paused_rgba
    };
    let icon = Image::new(rgba, handles.icon_width, handles.icon_height);
    if let Err(err) = tray.set_icon_with_as_template(Some(icon), true) {
        tracing::warn!(error = %err, "tray_set_icon_failed");
    }
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
        ID_CLEAR_HISTORY => clear_history(app),
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
        // The native `CheckMenuItem` flips its checkmark on click *before*
        // this handler runs (true on macOS / Windows / GTK alike). On any
        // failure path below the persisted `capture_enabled` is unchanged,
        // so we re-assert the tray from the last known-good value to undo
        // that optimistic flip — otherwise the checkmark (and the glyph)
        // would lie about whether capture is on. A successful toggle is
        // reconciled by the settings-watch loop instead, which also swaps
        // the icon to match.
        let current = match runtime.get_settings().await {
            Ok(s) => s.capture_enabled,
            Err(err) => {
                tracing::warn!(error = %err, "tray_toggle_capture_load_failed");
                refresh(&handle, runtime.current_settings().capture_enabled);
                return;
            }
        };
        if let Err(err) = runtime.set_capture_enabled(!current).await {
            tracing::warn!(error = %err, "tray_toggle_capture_save_failed");
            refresh(&handle, current);
        }
    });
}

fn clear_history(app: &AppHandle) {
    use tauri::Emitter;
    use tauri_plugin_notification::NotificationExt;

    // Mirrors the `SecondaryHotkeyAction::ClearHistory` flow in `hotkey.rs`:
    // both route through `commands::clear_non_pinned_and_previews` so the
    // soft-delete, the last-pasted reset, and the plaintext preview-cache
    // purge always run together (a tray-only path skipping the purge would
    // leave cleared `Public` bodies in `/tmp`). We additionally emit
    // `CLIPBOARD_CHANGED_EVENT` so an open palette re-runs its query and the
    // cleared rows disappear live rather than lingering until the next open,
    // and confirm with a notification since the tray click gives no other
    // feedback.
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let state = app.state::<AppState>();
        match crate::commands::clear_non_pinned_and_previews(&state).await {
            Ok(purged) => {
                let _ = app.emit(crate::CLIPBOARD_CHANGED_EVENT, serde_json::json!({}));
                let _ = app
                    .notification()
                    .builder()
                    .title("Nagori")
                    .body(format!("Cleared {purged} non-pinned entries."))
                    .show();
            }
            Err(err) => {
                tracing::warn!(error = %err, "tray_clear_history_failed");
            }
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
    use super::{build_tray_tooltip, decode_icon_rgba, decode_tray_icon, dim_rgba_alpha};

    #[test]
    fn embedded_tray_icon_decodes_to_rgba() {
        // Guards against a lossy icon re-optimisation (the `pngquant`
        // indexed-PNG regression that aborted the app at launch in
        // 0.0.2): the bundled `tray.png` must always decode into a
        // width*height*4 RGBA buffer for `Image::new_owned`.
        let (rgba, width, height) = decode_tray_icon();
        assert!(width > 0 && height > 0);
        assert_eq!(rgba.len(), (width * height * 4) as usize);
    }

    #[test]
    fn indexed_png_with_transparency_decodes_to_rgba() {
        // Exercises the hardened path directly: `pngquant` emits exactly
        // this shape (a palette plus a `tRNS` chunk), which is what broke
        // 0.0.2. The bundled asset is RGBA again, so without a synthetic
        // fixture the indexed/EXPAND logic would never be covered.
        let mut bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut bytes, 2, 1);
            encoder.set_color(png::ColorType::Indexed);
            encoder.set_depth(png::BitDepth::Eight);
            encoder.set_palette(vec![0, 0, 0, 255, 255, 255]);
            // Index 0 fully transparent, index 1 fully opaque.
            encoder.set_trns(vec![0, 255]);
            let mut writer = encoder.write_header().expect("write indexed header");
            writer
                .write_image_data(&[0, 1])
                .expect("write indexed pixels");
        }

        let (rgba, width, height) = decode_icon_rgba(&bytes);
        assert_eq!((width, height), (2, 1));
        assert_eq!(rgba, vec![0, 0, 0, 0, 255, 255, 255, 255]);
    }

    #[test]
    fn dim_rgba_alpha_scales_only_alpha() {
        // Colour channels are preserved and only alpha is scaled by the
        // paused ratio (40%), rounding toward zero so the menu-bar glyph
        // only ever reads *fainter*, never brighter.
        let src = vec![255, 255, 255, 255, 10, 20, 30, 100];
        let dimmed = dim_rgba_alpha(&src);
        assert_eq!(dimmed.len(), src.len());
        assert_eq!(&dimmed[0..3], &[255, 255, 255]);
        assert_eq!(dimmed[3], 102); // 255 * 2 / 5
        assert_eq!(&dimmed[4..7], &[10, 20, 30]);
        assert_eq!(dimmed[7], 40); // 100 * 2 / 5
    }

    #[test]
    fn paused_icon_preserves_dimensions_and_fades_alpha() {
        // The runtime swap reuses the decoded buffer, so the paused
        // variant must keep the exact width*height*4 shape and be
        // strictly fainter overall (otherwise "paused" would not read).
        let (active, width, height) = decode_tray_icon();
        let paused = dim_rgba_alpha(&active);
        assert_eq!(paused.len(), active.len());
        assert_eq!(paused.len(), (width * height * 4) as usize);
        let total_alpha = |b: &[u8]| b.chunks_exact(4).map(|px| u32::from(px[3])).sum::<u32>();
        assert!(total_alpha(&paused) < total_alpha(&active));
    }

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

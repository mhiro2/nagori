mod commands;
mod dto;
mod error;
mod fallback;
mod hotkey;
mod image_scheme;
mod startup;
mod state;
mod tray;

use hotkey::{
    HotkeyFailureKind, clear_and_notify_hotkey_failure, reconcile_cached_hotkey_failures,
    record_and_emit_hotkey_failure, register_primary_hotkey, register_secondary_hotkeys,
};
use nagori_core::SecondaryHotkeyAction;
use state::AppState;
use tauri::Manager;

/// Event name emitted when an auto-paste path fails after the originating
/// window has already been hidden. The frontend subscribes via
/// `TAURI_EVENTS.pasteFailed` (App.svelte renders a toast). Keep in
/// lockstep with the frontend constant in `lib/tauri.ts`.
pub(crate) const PASTE_FAILED_EVENT: &str = "nagori://paste_failed";

/// Event emitted after the capture loop stores a new clipboard entry.
/// The palette subscribes via `TAURI_EVENTS.clipboardChanged` and refreshes
/// the active result set so a visible window does not wait for another
/// user-driven query/filter change before showing the newest row.
pub(crate) const CLIPBOARD_CHANGED_EVENT: &str = "nagori://clipboard_changed";

/// Event name emitted after every persisted settings change. The payload
/// is the full `AppSettingsDto`. The Settings view subscribes via
/// `TAURI_EVENTS.settingsChanged` so an external mutation (the tray's
/// "Pause capture" toggle, another window, an IPC client) merges into the
/// in-memory view instead of being silently clobbered by the next
/// full-snapshot autosave. Keep the literal in lockstep with
/// `TAURI_EVENTS.settingsChanged` in `lib/tauri.ts`.
const SETTINGS_CHANGED_EVENT: &str = "nagori://settings_changed";

/// Event used to hand the Settings webview an initial tab / route hint
/// after `open_settings` shows the window. Payload is the desired
/// `SettingsView` tab name (currently `"setup"`); the `SettingsView`
/// subscribes via `TAURI_EVENTS.navigate` and swaps `activeTab` so a
/// caller that already knows where the user needs to land (e.g. the
/// Palette accessibility indicator) can jump straight to that tab
/// instead of relying on the first-launch onboarding heuristic. Keep
/// the literal in lockstep with `TAURI_EVENTS.navigate` in
/// `lib/tauri.ts`.
pub(crate) const NAVIGATE_EVENT: &str = "nagori://navigate";

#[cfg_attr(mobile, tauri::mobile_entry_point)]
#[allow(clippy::too_many_lines)]
// `generate_context!` embeds the (now per-command) ACL manifest and inlines a
// large initializer closure; with the app-command permission table declared in
// `build.rs` it trips `large_stack_frames`. The closure runs once at startup on
// the main thread, so the one-time frame size is not a concern.
#[allow(clippy::large_stack_frames)]
pub fn run() {
    let builder = tauri::Builder::default()
        // The sole log sink for this binary: it captures `log`-crate records,
        // and `tracing` events reach it via the `tracing/log` bridge (enabled
        // in Cargo.toml) since no `tracing` subscriber is installed here. That
        // is what keeps the desktop and embedded-daemon `tracing` diagnostics
        // (capture_skipped, command_error, …) out of the void.
        //
        // Keep our own `nagori_*` crates at Debug so the capture / maintenance
        // / command diagnostics stay captured, while dropping everything else
        // below Info: dependency crates (wry / hyper / reqwest / …) reach `log`
        // through the same bridge and would otherwise spill their trace/debug
        // into the bounded log file (the builder defaults to Trace for all
        // targets). `level_for` can't express this — fern matches module
        // targets on `::` boundaries, so `"nagori"` would not match the
        // underscore-named `nagori_core` / `nagori_desktop` / `nagori_daemon`
        // targets — so a target-prefix filter does the gating. The default
        // level stays at Debug so our crates' debug records clear the level
        // gate before the filter runs.
        .plugin(
            tauri_plugin_log::Builder::default()
                .level(tauri_plugin_log::log::LevelFilter::Debug)
                .filter(|metadata| {
                    metadata.target().starts_with("nagori")
                        || metadata.level() <= tauri_plugin_log::log::Level::Info
                })
                .build(),
        )
        .plugin(tauri_plugin_notification::init())
        // Per-shortcut handlers attach in `spawn_settings_subscribers`
        // (primary palette toggle and `register_secondary_hotkeys`) so
        // each accelerator only fires its own callback. Registering a
        // global `with_handler` here would additionally run the palette
        // toggle for *every* shortcut, hijacking secondary hotkeys.
        // The plugin uses `RegisterHotKey` on Windows and the X11
        // `XGrabKey` backend on Linux (via the upstream `global-hotkey`
        // crate). There is no XDG global-shortcut portal path in the
        // current upstream, so Wayland-only sessions register against
        // XWayland if present and fail outright otherwise. Failed
        // registrations surface via `nagori://hotkey_register_failed`
        // so the UI can prompt the user to fall back to the in-app
        // open button rather than leaving the feature silently
        // disabled.
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .plugin(autostart_plugin())
        // Updater plugin reads `plugins.updater.endpoints` and the
        // bundled signing pubkey from `tauri.conf.json`. `release.yaml`
        // builds `.app`/`.dmg` (macOS), NSIS `.exe` (Windows) and `deb`
        // + `AppImage` (Linux) bundles, and `bundle.createUpdaterArtifacts`
        // emits the signed `.tar.gz`/`.zip`/`AppImage.tar.gz` siblings
        // that `latest.json` references. The startup probe and the
        // manual `commands::check_for_updates` run on every OS;
        // whether the discovered update can be installed in place is
        // reported via `UpdateInfoDto::download_supported` so the UI
        // can fall back to the GitHub release link for `deb` users.
        .plugin(tauri_plugin_updater::Builder::new().build())
        .register_asynchronous_uri_scheme_protocol(
            "nagori-image",
            image_scheme::dispatch_image_request,
        );

    builder
        .setup(|app| {
            // Wipe any plaintext Quick Look preview temp files left by a
            // previous run before anything else. These are an ephemeral cache
            // (`preview_entry` regenerates them on demand), so clearing them
            // unconditionally at launch means a crashed / force-quit session
            // never leaves a previewed `Public` body lingering in `/tmp`.
            commands::purge_preview_temp_dir();

            let state = match AppState::try_new() {
                Ok(state) => state,
                Err(err) => {
                    // The stderr/log lines stay so launchd / login items
                    // and `journalctl` keep seeing the failure; the
                    // fallback window then surfaces the same message in
                    // a GUI surface for users who never see the
                    // terminal output (the only path a clipboard
                    // manager normally has to its operator).
                    tracing::error!(error = %err, "startup_failed");
                    eprintln!("nagori: failed to start: {err}");
                    match fallback::show_startup_fallback_window(app.handle(), &err.to_string()) {
                        Ok(()) => {
                            // Keep the app alive so the event loop can
                            // render the fallback window. `AppState` is
                            // intentionally left unmanaged: every
                            // command's `State<'_, AppState>` extractor
                            // will reject, and `on_run_event` exits the
                            // process when the user closes the fallback
                            // window. Skipping the rest of setup keeps
                            // tray / background tasks / shortcuts from
                            // panicking against the missing state.
                            return Ok(());
                        }
                        Err(win_err) => {
                            tracing::warn!(
                                error = %win_err,
                                "startup_fallback_window_failed",
                            );
                            return Err(Box::new(err));
                        }
                    }
                }
            };
            app.manage(state);
            app.state::<AppState>()
                .spawn_background_tasks(app.handle().clone());

            // Tray icon is installed on every platform. macOS exposes it in
            // the menu bar, Windows in the system notification area, and
            // Linux through StatusNotifierItem / `libayatana-appindicator`.
            // The menu items themselves (Show Palette / Pause Capture /
            // Settings / Quit) are platform-agnostic. If creation fails
            // (e.g. Linux session without StatusNotifierItem support) we
            // log and continue so the rest of the app stays usable.
            let tray_install_result = tray::install(app.handle());
            if let Err(err) = &tray_install_result {
                tracing::warn!(error = %err, "tray_install_failed");
            }

            // macOS: switch to the `Accessory` activation policy so no Dock
            // icon ever appears, matching the per-window `skipTaskbar: true`
            // intent and the tray-only UX (the menu-bar tray is the
            // primary entry point). The Dock icon is controlled per-process
            // by NSApp's activation policy, not per-window — without this,
            // the icon flickers in/out of the Dock every time the palette
            // is shown/hidden, and the app shows up in Cmd+Tab. Windows
            // and Linux honour `skipTaskbar` directly, so this is macOS-only.
            // Applied only on the success path: the fallback branch
            // returns early above, so a startup-error session keeps the
            // default `Regular` policy and stays Dock/Cmd+Tab-visible —
            // important because fallback mode skips tray install, leaving
            // the Dock as the sole way back to the error window. We also
            // only flip the policy when tray install actually succeeded:
            // a fresh macOS session with no tray *and* no Dock icon would
            // leave the user with the palette hotkey as the sole way to
            // reach the (hidden) main window, which is a poor recovery
            // path if the hotkey itself failed to register.
            #[cfg(target_os = "macos")]
            if tray_install_result.is_ok() {
                app.set_activation_policy(tauri::ActivationPolicy::Accessory);
            }
            #[cfg(not(target_os = "macos"))]
            let _ = tray_install_result;

            spawn_settings_subscribers(app.handle());
            // Periodically refresh the tray tooltip from `CaptureHealth`
            // / `MaintenanceHealth` so a degraded loop visibly surfaces
            // without the user having to re-open `nagori doctor`. The
            // 5 s cadence is well under the capture loop's degraded
            // threshold latency (3 ticks * default 500 ms = 1.5 s) but
            // generous enough that the cost — two mutex locks plus a
            // tooltip write per tick — is negligible.
            spawn_tray_health_refresher(app.handle());

            // Defer the startup notification until the capture loop has
            // either entered polling or aborted, so the body matches the
            // truth instead of always claiming "Clipboard history is
            // ready." (the prior wording fired even when settings load
            // silently aborted the capture task). The probe polls
            // `runtime.startup_health()` rather than threading a oneshot
            // through every call site: the snapshot is sticky after the
            // first outcome, and `nagori doctor` reads from the same
            // surface so the two never disagree.
            startup::spawn_startup_ready_notification(app.handle());
            surface_first_launch_setup(app.handle());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::search_clipboard,
            commands::list_recent_entries,
            commands::list_pinned_entries,
            commands::get_entry,
            commands::copy_entry,
            commands::paste_entry,
            commands::open_palette,
            commands::close_palette,
            commands::paste_entry_from_palette,
            commands::paste_entry_representation_from_palette,
            commands::list_paste_options,
            commands::copy_entry_from_palette,
            commands::preview::get_entry_preview,
            commands::preview::get_entry_preview_full,
            commands::preview::preview_entry,
            commands::add_entry,
            commands::delete_entry,
            commands::delete_entries,
            commands::copy_entries_combined,
            commands::clear_history,
            commands::repaste_last,
            commands::pin_entry,
            commands::run_quick_action,
            commands::start_ai_action,
            commands::cancel_ai_action,
            commands::get_ai_availability,
            commands::get_semantic_index_status,
            commands::rebuild_semantic_index,
            commands::save_ai_result,
            commands::get_settings,
            commands::password_manager_preset,
            commands::update_settings,
            commands::set_capture_enabled,
            commands::get_permissions,
            commands::get_capabilities,
            commands::last_hotkey_failure,
            commands::request_accessibility,
            commands::open_url_external,
            commands::toggle_palette,
            commands::hide_palette,
            commands::open_settings,
            commands::close_settings,
            commands::updater::check_for_updates,
            commands::installer::cli_install_status,
            commands::installer::install_cli,
        ])
        .build(tauri::generate_context!())
        .unwrap_or_else(|err| {
            // Replacing the previous `expect` so the user sees the
            // underlying error (DB path, permission, etc.) instead of
            // only the generic panic banner. Exit non-zero so launchd /
            // login items can detect the failure.
            tracing::error!(error = %err, "tauri_build_failed");
            eprintln!("nagori: tauri runtime failed: {err}");
            std::process::exit(1);
        })
        .run(|app_handle, event| {
            on_run_event(app_handle, &event);
        });
}

/// Build the autostart plugin with the launcher backend appropriate for
/// the current OS. macOS uses a `LaunchAgent` (the plugin generates a
/// `~/Library/LaunchAgents/<bundle>.plist`); Windows writes a registry
/// key under `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`; Linux
/// drops a `~/.config/autostart/<bundle>.desktop` file. The plugin
/// internally dispatches on OS, but it requires the `MacosLauncher`
/// argument regardless of the target so the surrounding wiring stays
/// uniform — passing `LaunchAgent` is the documented default and is
/// ignored on Windows/Linux builds.
fn autostart_plugin() -> tauri::plugin::TauriPlugin<tauri::Wry> {
    use tauri_plugin_autostart::MacosLauncher;
    tauri_plugin_autostart::init(MacosLauncher::LaunchAgent, None)
}

/// One-shot guard so multiple `RunEvent::ExitRequested` deliveries cannot
/// run shutdown cleanup twice. A second pass would race the tokio runtime
/// teardown that the first pass started.
static EXIT_CLEANUP_FIRED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

const BACKGROUND_TASK_SHUTDOWN_GRACE: std::time::Duration = std::time::Duration::from_secs(5);

/// Cross-platform run-event hook. Handles two distinct shutdown surfaces:
///
/// * `RunEvent::ExitRequested` fires for tray "Quit", `Cmd`/`Ctrl+Q`, and
///   dock/menu Quit — all of which actually tear the process down — so
///   it's the single right place to honour `clear_on_quit`.
/// * `WindowEvent::CloseRequested` on the main window is intercepted and
///   converted into a hide. Without this, pressing `Cmd+W` on macOS or
///   `Alt+F4` on Windows/Linux would destroy the (sole) webview window —
///   the next palette toggle would then resolve `get_webview_window("main")`
///   to `None` and silently no-op. We deliberately do **not** trigger any
///   soft-delete here: a previous version ran `clear_on_quit` from this
///   hook via `block_on` on the UI thread, freezing the user's desktop
///   for up to a second every time they closed the palette. Hiding is
///   strictly synchronous and safe to run inline.
fn on_run_event(handle: &tauri::AppHandle, event: &tauri::RunEvent) {
    match event {
        tauri::RunEvent::ExitRequested { .. } => perform_exit_cleanup(handle),
        tauri::RunEvent::WindowEvent {
            label,
            event: tauri::WindowEvent::CloseRequested { api, .. },
            ..
        } if label == "main" => {
            // Prevent destruction so the webview handle stays alive for
            // the next palette toggle. Hiding mirrors what
            // `hide_main_palette` does, and clearing the captured
            // frontmost matches the `close_palette` / `hide_palette`
            // command paths so a later open re-snapshots cleanly.
            api.prevent_close();
            if let Some(window) = handle.get_webview_window("main") {
                let _ = window.hide();
            }
            if let Some(state) = handle.try_state::<AppState>() {
                state.clear_previous_frontmost();
            }
        }
        tauri::RunEvent::WindowEvent {
            label,
            event: tauri::WindowEvent::CloseRequested { api, .. },
            ..
        } if label == "settings" => {
            // Match the palette: intercept the OS close (red traffic-light
            // on macOS, Alt+F4 on Windows/Linux) and hide instead of
            // destroying, so re-opening Settings from the tray reuses the
            // already-loaded webview instead of paying the cold-load cost.
            api.prevent_close();
            if let Some(window) = handle.get_webview_window("settings") {
                let _ = window.hide();
            }
        }
        tauri::RunEvent::WindowEvent {
            label,
            event: tauri::WindowEvent::CloseRequested { .. },
            ..
        } if label == fallback::FALLBACK_WINDOW_LABEL => {
            // Fallback mode runs without `AppState` and without the
            // capture / maintenance loops, so there is nothing to
            // drain — but `perform_exit_cleanup` already guards on
            // `try_state` so calling `exit` here remains safe. The
            // user's only path out of the fallback is to close the
            // window, so closing it must terminate the process; the
            // hidden main window would otherwise keep the app alive
            // on macOS (and in the systray on Windows / Linux).
            handle.exit(0);
        }
        _ => {}
    }
}

/// Block the tauri runtime briefly so background workers and optional
/// soft-delete complete before it destroys the tokio runtime. The
/// background drain mirrors daemon shutdown; the clear-on-quit ceiling keeps
/// a wedged DB from freezing the quit path indefinitely.
fn perform_exit_cleanup(handle: &tauri::AppHandle) {
    use std::sync::atomic::Ordering;
    if EXIT_CLEANUP_FIRED.swap(true, Ordering::SeqCst) {
        return;
    }
    let Some(state) = handle.try_state::<AppState>() else {
        return;
    };
    let runtime = state.runtime.clone();
    tauri::async_runtime::block_on(async move {
        state
            .shutdown_background_tasks(BACKGROUND_TASK_SHUTDOWN_GRACE)
            .await;
        if runtime.current_settings().clear_on_quit {
            // Persist a purge-pending marker *before* the delete so a timeout
            // (below), crash, or kill mid-purge is completed on the next launch
            // (fail-closed) instead of silently leaving the history the user
            // asked to clear.
            match state.mark_clear_on_quit_pending() {
                Ok(()) => {
                    // The marker covers a timeout, so the 1 s ceiling can keep a
                    // wedged DB from freezing quit: if it elapses (or errors) the
                    // marker stays and `try_new_at` finishes the job at next
                    // launch.
                    let purged = tokio::time::timeout(
                        std::time::Duration::from_secs(1),
                        runtime.clear_non_pinned(),
                    )
                    .await;
                    if matches!(purged, Ok(Ok(_))) {
                        // Only a completed purge removes the marker. A removal
                        // failure would make the next launch re-purge, so
                        // surface it; that fail-closed re-purge is harmless
                        // (idempotent at the start of a session).
                        if let Err(err) = state.clear_clear_on_quit_pending() {
                            tracing::warn!(error = %err, "clear_on_quit_marker_remove_failed");
                        }
                    } else {
                        tracing::warn!("clear_on_quit_incomplete_purge_deferred_to_next_launch");
                    }
                }
                Err(err) => {
                    // No resume marker could be written, so the timeout path
                    // would be fail-open (a timed-out purge would leave data
                    // with nothing to finish it on next launch). Await the purge
                    // to completion instead — honouring clear-on-quit now is the
                    // only remaining guarantee. A DELETE on the local SQLite file
                    // does not hang indefinitely; a genuine failure here means a
                    // broken filesystem we cannot recover from at quit.
                    tracing::warn!(error = %err, "clear_on_quit_marker_write_failed");
                    if let Err(err) = runtime.clear_non_pinned().await {
                        tracing::error!(error = %err, "clear_on_quit_purge_failed_without_marker");
                    }
                }
            }
            // `clear_on_quit` promises a clean slate on exit; extend that to
            // the plaintext Quick Look cache so a previewed Public body is not
            // left behind in `/tmp` for the next user of the machine.
            commands::purge_preview_temp_dir();
        }
    });
}

/// Spawn background tasks that subscribe to settings changes:
///   * keep the global hotkey in sync with `AppSettings.global_hotkey`,
///   * keep launch-at-login in sync with `AppSettings.auto_launch`,
///   * keep secondary global shortcuts in sync with
///     `AppSettings.secondary_hotkeys`,
///   * keep the system tray icon visible/hidden per
///     `AppSettings.show_in_menu_bar` (the macOS menu bar / Windows
///     notification area / Linux `StatusNotifierItem` entry),
///   * notify the user once when capture is paused / resumed,
///   * notify the user when the AI provider transitions into `enabled` so
///     they realise remote calls may now happen.
#[allow(clippy::too_many_lines)]
fn spawn_settings_subscribers(handle: &tauri::AppHandle) {
    use std::collections::BTreeMap;
    use tauri::Emitter;
    use tauri_plugin_global_shortcut::GlobalShortcutExt;
    use tauri_plugin_notification::NotificationExt;

    let app = handle.clone();
    let runtime = app.state::<AppState>().runtime.clone();
    let mut settings_rx = runtime.settings_subscribe();
    let mut settings_gate = app.state::<AppState>().settings_load_gate();

    tauri::async_runtime::spawn(async move {
        // Fail closed: wait for the one-shot startup settings load and abort
        // if it failed (or if shutdown beats it). The coordinator already
        // recorded the startup health; here we just bail, because registering
        // hotkeys / auto-launch from the compiled-in `Default` would clobber
        // the user-customised hotkey, capture flag and auto-launch state. On
        // success the loaded snapshot is already published to the runtime's
        // watch channel.
        let mut shutdown = runtime.shutdown_handle();
        if !state::settings_loaded_or_shutdown(&mut settings_gate, &mut shutdown).await {
            return;
        }
        let initial = runtime.current_settings();

        let mut current_hotkey = initial.global_hotkey.clone();
        let mut current_capture = initial.capture_enabled;
        let mut current_ai_enabled = initial.ai.enabled;
        let mut current_auto_launch = initial.auto_launch;
        let mut current_show_in_menu_bar = initial.show_in_menu_bar;
        let mut current_secondary: BTreeMap<SecondaryHotkeyAction, String> =
            initial.secondary_hotkeys.clone();

        if let Err(err) = register_primary_hotkey(&app, current_hotkey.as_str()) {
            tracing::warn!(error = %err, "global_shortcut_register_failed");
            // Surface to the UI so the settings page can prompt the user to
            // pick a different hotkey rather than silently leaving the
            // feature disabled. Caching on `AppState` covers the startup
            // race where the desktop webview attaches its listener after
            // this emit fires; the frontend re-hydrates via
            // `last_hotkey_failure`.
            record_and_emit_hotkey_failure(
                &app,
                current_hotkey.as_str(),
                &err.to_string(),
                HotkeyFailureKind::Primary,
                None,
            );
        } else {
            // Clear any prior cached failure so a stale toast doesn't
            // outlive a successful binding (e.g. a transient compositor
            // hiccup at first attempt followed by a clean rebind). The
            // emit on the resolved event also nudges any live frontend
            // store to drop its banner without waiting for dismiss.
            clear_and_notify_hotkey_failure(&app, HotkeyFailureKind::Primary, None);
        }

        // Reconcile auto-launch on startup so the LaunchAgent matches the
        // persisted preference even if the user toggled it via another
        // install.
        if let Err(err) = sync_auto_launch(&app, current_auto_launch) {
            tracing::warn!(error = %err, "auto_launch_sync_failed");
        }

        // Initial reconciliation for tray + secondary shortcuts. The active
        // map returned by the registrar reflects what actually bound — a
        // failure leaves the prior accelerator out of `current_secondary`
        // so later reconciles won't try to unregister something we never
        // registered (which would tear down a sibling action sharing the
        // same accelerator). Tray reconciliation runs on every OS; the
        // underlying `set_visible` is a no-op when the tray failed to
        // install (e.g. an unsupported Linux session).
        tray::set_visible(&app, current_show_in_menu_bar);
        current_secondary = register_secondary_hotkeys(&app, &BTreeMap::new(), &current_secondary);

        // Startup updater probe. Honours `auto_update_check` — a user
        // who has opted out of background network calls never sees a
        // request. `release.yaml` ships signed bundles for every target
        // (macOS `.app`/`.dmg`, Windows NSIS, Linux deb + AppImage) and
        // `latest.json` lists them all, so the probe runs on every OS.
        // The probe surfaces an OS notification on availability;
        // whether the result can be applied in place is decided per
        // install medium in `commands::check_for_updates`
        // (`download_supported`). Failures are logged at warn so a
        // transient network blip doesn't surface a banner.
        if initial.auto_update_check {
            startup::spawn_startup_update_probe(&app);
        }

        while settings_rx.changed().await.is_ok() {
            let snapshot = settings_rx.borrow().clone();

            // Broadcast the full snapshot to any open SettingsView so it
            // can merge external mutations (tray toggle, IPC client) into
            // its in-memory copy instead of silently overwriting them on
            // the next autosave. Serialized via `Into<AppSettingsDto>` so
            // the wire shape matches `get_settings`. Failures here are
            // best-effort — the receiving window may have just closed.
            //
            // Stamp the live revision so the receiving window advances its
            // compare-and-swap baseline as it adopts the snapshot; otherwise a
            // tray toggle from the palette would leave the settings window's
            // baseline stale and its next save would needlessly conflict.
            //
            // Read the body and revision as one consistent pair rather than
            // pairing the watch snapshot with a separate revision read: a write
            // landing between the two could broadcast body N with revision N+1,
            // and the window would then adopt N's values under N+1's token —
            // letting its next save pass the compare-and-swap and revert the
            // concurrent change. Re-reading the current pair may surface a value
            // a hair newer than the snapshot that woke this loop, which is fine
            // (it is still internally consistent and is the latest state). Fall
            // back to the watch snapshot only if the read fails.
            let dto = match runtime.get_settings_with_revision().await {
                Ok((settings, revision)) => {
                    let mut dto = dto::AppSettingsDto::from(settings);
                    dto.revision = revision;
                    dto
                }
                Err(_) => dto::AppSettingsDto::from(snapshot.clone()),
            };
            let _ = app.emit(SETTINGS_CHANGED_EVENT, dto);

            if snapshot.global_hotkey != current_hotkey {
                let next = snapshot.global_hotkey.clone();
                let _ = app.global_shortcut().unregister(current_hotkey.as_str());
                if let Err(err) = register_primary_hotkey(&app, next.as_str()) {
                    tracing::warn!(
                        error = %err,
                        new = %next,
                        "global_shortcut_reregister_failed"
                    );
                    record_and_emit_hotkey_failure(
                        &app,
                        next.as_str(),
                        &err.to_string(),
                        HotkeyFailureKind::Primary,
                        None,
                    );
                    // Roll back to the accelerator that was bound before this
                    // tick. The user-facing failure above already points at
                    // `next`; if the rollback *also* fails the palette toggle
                    // is now bound to nothing, so log it rather than swallow
                    // the dead-toggle signal. The cached failure stays keyed to
                    // `next` (the desired accelerator) so the end-of-tick
                    // reconcile doesn't clear it.
                    if let Err(revert_err) = register_primary_hotkey(&app, current_hotkey.as_str())
                    {
                        tracing::warn!(
                            error = %revert_err,
                            previous = %current_hotkey,
                            "global_shortcut_revert_failed"
                        );
                    }
                } else {
                    current_hotkey = next;
                    // Successful rebind — drop any cached failure so the
                    // toast/banner does not linger past the resolved
                    // conflict. The accompanying resolved event clears
                    // a live frontend store too.
                    clear_and_notify_hotkey_failure(&app, HotkeyFailureKind::Primary, None);
                }
            }

            if snapshot.capture_enabled != current_capture {
                current_capture = snapshot.capture_enabled;
                let body = if current_capture {
                    "Clipboard capture resumed."
                } else {
                    "Clipboard capture paused."
                };
                let _ = app
                    .notification()
                    .builder()
                    .title("Nagori")
                    .body(body)
                    .show();
            }

            if snapshot.ai.enabled && !current_ai_enabled {
                current_ai_enabled = true;
                let _ = app
                    .notification()
                    .builder()
                    .title("Nagori AI")
                    .body("AI actions are now enabled.")
                    .show();
            } else if !snapshot.ai.enabled && current_ai_enabled {
                current_ai_enabled = false;
            }

            if snapshot.auto_launch != current_auto_launch {
                if let Err(err) = sync_auto_launch(&app, snapshot.auto_launch) {
                    tracing::warn!(error = %err, "auto_launch_sync_failed");
                } else {
                    current_auto_launch = snapshot.auto_launch;
                }
            }

            if snapshot.show_in_menu_bar != current_show_in_menu_bar {
                current_show_in_menu_bar = snapshot.show_in_menu_bar;
                tray::set_visible(&app, current_show_in_menu_bar);
            }

            if snapshot.secondary_hotkeys != current_secondary {
                current_secondary = register_secondary_hotkeys(
                    &app,
                    &current_secondary,
                    &snapshot.secondary_hotkeys,
                );
            }

            // Clear any cached failure whose accelerator is no longer
            // current after this tick — either because the user edited
            // the binding away (no register call would run to emit a
            // resolved event) or because the *same* accelerator just
            // bound successfully under a different action. Without
            // this, the toast/banner outlives both resolution paths.
            reconcile_cached_hotkey_failures(&app, &snapshot, &current_secondary);

            // Refresh the tray menu so the "Pause Capture" / "Resume
            // Capture" label tracks the current state. Runs on every OS so
            // Windows / Linux trays stay in sync with the persisted
            // capture flag.
            tray::refresh(&app, current_capture);
        }
    });
}

fn sync_auto_launch(
    app: &tauri::AppHandle,
    enabled: bool,
) -> std::result::Result<(), Box<dyn std::error::Error>> {
    use tauri_plugin_autostart::ManagerExt;
    let manager = app.autolaunch();
    if enabled && !manager.is_enabled()? {
        manager.enable()?;
    } else if !enabled && manager.is_enabled()? {
        manager.disable()?;
    }
    Ok(())
}

/// Inter-tick cadence for the tray tooltip refresher. Short enough that
/// a degraded capture loop is visible within a few seconds of crossing
/// the threshold (the loop's degraded threshold is 3 ticks at the
/// default 500 ms cadence — 1.5 s — so 5 s buys us at most one missed
/// refresh between cliff and reveal), but long enough that the per-poll
/// cost (two `Mutex` locks + a tray FFI write) stays below the noise
/// floor.
const TRAY_HEALTH_REFRESH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);

/// Spawn a background task that periodically refreshes the tray tooltip
/// from the live `CaptureHealth` / `MaintenanceHealth` snapshots so the
/// tray surfaces the same degraded state `nagori doctor` would. The
/// task exits when the runtime's shutdown signal fires.
fn spawn_tray_health_refresher(handle: &tauri::AppHandle) {
    let Some(state) = handle.try_state::<AppState>() else {
        return;
    };
    let mut shutdown = state.runtime.shutdown_handle();
    let app = handle.clone();
    tauri::async_runtime::spawn(async move {
        // Initial refresh so a fresh process whose capture loop already
        // tripped during init does not have to wait a full interval
        // before the tooltip catches up.
        tray::refresh_tooltip(&app);
        loop {
            tokio::select! {
                () = shutdown.cancelled() => return,
                () = tokio::time::sleep(TRAY_HEALTH_REFRESH_INTERVAL) => {
                    tray::refresh_tooltip(&app);
                }
            }
        }
    });
}

/// First launch: bring the Settings window forward on the Setup tab so the
/// user lands directly on the Accessibility grant flow instead of discovering
/// the `StatusBar` indicator on their own. Gates on the same markers
/// `SettingsView` uses for its default tab (`completed_at` +
/// `accessibility_first_granted_at` both unset) rather than `completed_at`
/// alone — `completed_at` is reserved for a future explicit dismissal, so
/// keying on it would re-pop the window on every launch until then.
///
/// Also gated on the host actually having a setup step: the Setup tab only
/// exists where auto-paste needs user action (macOS Accessibility, Linux
/// `wtype`), so a host where it just works (Windows) would otherwise pop an
/// empty Settings window on every fresh launch. This mirrors `SettingsView`'s
/// `setupNeeded` gate so the window-surface and the tab-visibility decisions
/// stay in sync. The daemon / hotkey registration is intentionally left
/// running (§3.1); this only surfaces the window.
///
/// The onboarding markers live in `AppSettings`, whose watch channel starts
/// at the compiled-in default until the coordinator's one-shot startup load
/// publishes the persisted snapshot. Reading them synchronously from
/// `setup()` would race that load and see "never set up" on every launch for
/// a configured user, so the decision runs in a spawned task that awaits the
/// settings-load gate — fail-closed (no window) on load failure or shutdown,
/// like every other gated subscriber.
fn surface_first_launch_setup(handle: &tauri::AppHandle) {
    let Some(state) = handle.try_state::<AppState>() else {
        // No state means setup() bailed before `manage(state)` ran.
        return;
    };
    // Nothing to set up means no Setup tab to land on — don't surface the
    // window. Matches `SettingsView`'s capability-driven Setup gate.
    let setup_needed = matches!(
        state.runtime.capabilities().auto_paste,
        nagori_platform::Capability::RequiresPermission { .. }
            | nagori_platform::Capability::RequiresExternalTool { .. }
    );
    if !setup_needed {
        return;
    }
    let runtime = state.runtime.clone();
    let mut settings_gate = state.settings_load_gate();
    let mut shutdown = runtime.shutdown_handle();
    let app = handle.clone();
    tauri::async_runtime::spawn(async move {
        if !state::settings_loaded_or_shutdown(&mut settings_gate, &mut shutdown).await {
            return;
        }
        let onboarding = runtime.current_settings().onboarding;
        if onboarding.completed_at.is_some() || onboarding.accessibility_first_granted_at.is_some()
        {
            return;
        }
        if let Err(err) = commands::show_settings_window(&app) {
            tracing::warn!(error = ?err, "first_launch_setup_window_failed");
        }
    });
}

pub(crate) fn toggle_main_palette(app: &tauri::AppHandle) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    if window.is_visible().unwrap_or(false) {
        if let Some(state) = app.try_state::<AppState>() {
            state.clear_previous_frontmost();
        }
        let _ = window.hide();
    } else {
        // Snapshot whichever app is frontmost *before* we steal focus —
        // the paste flow needs it to re-focus the user's source app. See
        // `AppState::remember_previous_frontmost`.
        if let Some(state) = app.try_state::<AppState>() {
            state.remember_previous_frontmost();
        }
        // Re-home the palette onto the monitor under the cursor before it
        // becomes visible — `tauri.conf.json`'s `center: true` only pins it to
        // the primary display once at creation. Done while hidden to avoid a
        // visible jump.
        commands::recenter_palette_on_cursor_monitor(&window);
        let _ = window.show();
        let _ = window.set_focus();
    }
}

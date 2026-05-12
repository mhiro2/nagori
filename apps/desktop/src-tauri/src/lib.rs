mod commands;
mod dto;
mod error;
mod state;
#[cfg(target_os = "macos")]
mod tray;

use nagori_core::{EntryId, is_text_safe_for_default_output};
use nagori_daemon::NagoriRuntime;
use state::AppState;
use tauri::Manager;

#[cfg(target_os = "macos")]
use nagori_core::SecondaryHotkeyAction;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_log::Builder::default().build())
        .plugin(tauri_plugin_notification::init())
        .register_asynchronous_uri_scheme_protocol("nagori-image", dispatch_image_request);

    #[cfg(target_os = "macos")]
    let builder = {
        // launch-at-login. The plugin no-ops on platforms it does not
        // support, so we only register on macOS for MVP.
        use tauri_plugin_autostart::MacosLauncher;
        builder
            .plugin(tauri_plugin_autostart::init(
                MacosLauncher::LaunchAgent,
                None,
            ))
            // Per-shortcut handlers attach in `spawn_settings_subscribers`
            // (primary palette toggle and `register_secondary_hotkeys`) so
            // each accelerator only fires its own callback. Registering a
            // global `with_handler` here would additionally run the palette
            // toggle for *every* shortcut, hijacking secondary hotkeys.
            .plugin(tauri_plugin_global_shortcut::Builder::new().build())
            // Updater plugin reads `plugins.updater.endpoints` and the
            // bundled signing pubkey from `tauri.conf.json`. The
            // `commands::check_for_updates` command exposes a manual
            // trigger; the startup probe in `spawn_settings_subscribers`
            // honours `auto_update_check` and emits
            // `nagori://update_available` for the frontend.
            .plugin(tauri_plugin_updater::Builder::new().build())
    };

    builder
        .setup(|app| {
            let state = match AppState::try_new() {
                Ok(state) => state,
                Err(err) => {
                    // setup() is called before any UI is mounted, so we
                    // can't render a recovery dialog. Log to tracing (which
                    // tauri-plugin-log fans out to the OS log) and to
                    // stderr so the user gets the actionable hint baked
                    // into the error — it includes the DB path and the
                    // exact `mv` to move the file aside.
                    tracing::error!(error = %err, "startup_failed");
                    eprintln!("nagori: failed to start: {err}");
                    return Err(Box::new(err));
                }
            };
            app.manage(state);
            #[cfg(target_os = "macos")]
            app.state::<AppState>().spawn_background_tasks();

            #[cfg(target_os = "macos")]
            {
                tray::install(app.handle())?;
            }

            #[cfg(target_os = "macos")]
            spawn_settings_subscribers(app.handle());

            // Surface a "ready" notification once everything is wired
            // up. The notification plugin no-ops if the user has not
            // granted permission yet, so this is best-effort.
            #[cfg(target_os = "macos")]
            {
                use tauri_plugin_notification::NotificationExt;
                let _ = app
                    .notification()
                    .builder()
                    .title("Nagori")
                    .body("Clipboard history is ready.")
                    .show();
            }
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
            commands::copy_entry_from_palette,
            commands::get_entry_preview,
            commands::add_entry,
            commands::delete_entry,
            commands::delete_entries,
            commands::copy_entries_combined,
            commands::clear_history,
            commands::repaste_last,
            commands::pin_entry,
            commands::run_ai_action,
            commands::save_ai_result,
            commands::get_settings,
            commands::update_settings,
            commands::set_capture_enabled,
            commands::get_permissions,
            commands::open_accessibility_settings,
            commands::toggle_palette,
            commands::hide_palette,
            commands::check_for_updates,
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
            #[cfg(target_os = "macos")]
            on_run_event(app_handle, &event);
            #[cfg(not(target_os = "macos"))]
            {
                let _ = app_handle;
                let _ = event;
            }
        });
}

/// One-shot guard so multiple `RunEvent::ExitRequested` deliveries cannot
/// run shutdown cleanup twice. A second pass would race the tokio runtime
/// teardown that the first pass started.
#[cfg(target_os = "macos")]
static EXIT_CLEANUP_FIRED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

#[cfg(target_os = "macos")]
const BACKGROUND_TASK_SHUTDOWN_GRACE: std::time::Duration = std::time::Duration::from_secs(5);

/// macOS-only run-event hook. `RunEvent::ExitRequested` fires for tray
/// "Quit", `Cmd+Q`, and dock-menu Quit, all of which actually tear the
/// process down — so it's the single right place to honour
/// `clear_on_quit`. We deliberately do **not** wire a parallel
/// `WindowEvent::CloseRequested` handler: on macOS, closing the main
/// window only hides it, and previously the close hook ran the soft-delete
/// via `block_on` on the UI thread, freezing the user's desktop for up to
/// the 1 s timeout while purging history they never asked to lose.
#[cfg(target_os = "macos")]
fn on_run_event(handle: &tauri::AppHandle, event: &tauri::RunEvent) {
    if !matches!(event, tauri::RunEvent::ExitRequested { .. }) {
        return;
    }
    perform_exit_cleanup(handle);
}

/// Block the tauri runtime briefly so background workers and optional
/// soft-delete complete before it destroys the tokio runtime. The
/// background drain mirrors daemon shutdown; the clear-on-quit ceiling keeps
/// a wedged DB from freezing the quit path indefinitely.
#[cfg(target_os = "macos")]
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
            let _ = tokio::time::timeout(
                std::time::Duration::from_secs(1),
                runtime.clear_non_pinned(),
            )
            .await;
        }
    });
}

/// Spawn background tasks that subscribe to settings changes:
///   * keep the global hotkey in sync with `AppSettings.global_hotkey`,
///   * keep launch-at-login in sync with `AppSettings.auto_launch`,
///   * keep secondary global shortcuts in sync with
///     `AppSettings.secondary_hotkeys`,
///   * keep the menu-bar tray icon visible/hidden per
///     `AppSettings.show_in_menu_bar`,
///   * notify the user once when capture is paused / resumed,
///   * notify the user when the AI provider transitions into `enabled` so
///     they realise remote calls may now happen.
#[cfg(target_os = "macos")]
#[allow(clippy::too_many_lines)]
fn spawn_settings_subscribers(handle: &tauri::AppHandle) {
    use nagori_core::SettingsRepository;
    use std::collections::BTreeMap;
    use tauri::Emitter;
    use tauri_plugin_global_shortcut::GlobalShortcutExt;
    use tauri_plugin_notification::NotificationExt;

    let app = handle.clone();
    let runtime = app.state::<AppState>().runtime.clone();
    let mut settings_rx = runtime.settings_subscribe();

    tauri::async_runtime::spawn(async move {
        // Fail closed: if the persisted settings can't be loaded we abort
        // the subscriber. Falling back to `Default` would clobber the
        // user-customised hotkey, capture flag and auto-launch state.
        let store = runtime.store().clone();
        let initial = match store.get_settings().await {
            Ok(s) => s,
            Err(err) => {
                tracing::error!(error = %err, "settings_load_failed_aborting_subscribers");
                return;
            }
        };

        let mut current_hotkey = initial.global_hotkey.clone();
        let mut current_capture = initial.capture_enabled;
        let mut current_ai_enabled = initial.ai_enabled;
        let mut current_auto_launch = initial.auto_launch;
        let mut current_show_in_menu_bar = initial.show_in_menu_bar;
        let mut current_secondary: BTreeMap<SecondaryHotkeyAction, String> =
            initial.secondary_hotkeys.clone();

        if let Err(err) = register_primary_hotkey(&app, current_hotkey.as_str()) {
            tracing::warn!(error = %err, "global_shortcut_register_failed");
            // Surface to the UI so the settings page can prompt the user to
            // pick a different hotkey rather than silently leaving the
            // feature disabled.
            let _ = app.emit(
                "nagori://hotkey_register_failed",
                serde_json::json!({
                    "hotkey": current_hotkey.clone(),
                    "error": err.to_string(),
                }),
            );
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
        // same accelerator).
        tray::set_visible(&app, current_show_in_menu_bar);
        current_secondary = register_secondary_hotkeys(&app, &BTreeMap::new(), &current_secondary);

        // Startup updater probe. Honours `auto_update_check` *and*
        // `local_only_mode` so a user who has opted out of background
        // network calls never sees a request. Emits
        // `nagori://update_available` with `{version, currentVersion,
        // releaseNotes}` when an update is found; failures are logged at
        // warn so a transient network blip doesn't surface a banner.
        if initial.auto_update_check && !initial.local_only_mode {
            spawn_startup_update_probe(&app);
        }

        while settings_rx.changed().await.is_ok() {
            let snapshot = settings_rx.borrow().clone();

            if snapshot.global_hotkey != current_hotkey {
                let next = snapshot.global_hotkey.clone();
                let _ = app.global_shortcut().unregister(current_hotkey.as_str());
                if let Err(err) = register_primary_hotkey(&app, next.as_str()) {
                    tracing::warn!(
                        error = %err,
                        new = %next,
                        "global_shortcut_reregister_failed"
                    );
                    let _ = app.emit(
                        "nagori://hotkey_register_failed",
                        serde_json::json!({
                            "hotkey": next.clone(),
                            "error": err.to_string(),
                        }),
                    );
                    let _ = register_primary_hotkey(&app, current_hotkey.as_str());
                } else {
                    current_hotkey = next;
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

            if snapshot.ai_enabled && !current_ai_enabled {
                current_ai_enabled = true;
                let _ = app
                    .notification()
                    .builder()
                    .title("Nagori AI")
                    .body("AI actions are now enabled.")
                    .show();
            } else if !snapshot.ai_enabled && current_ai_enabled {
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

            // Refresh the tray menu so the "Pause Capture" / "Resume
            // Capture" label tracks the current state.
            tray::refresh(&app, current_capture);
        }
    });
}

/// Register the primary palette-toggle hotkey with its own handler. We use
/// `on_shortcut` rather than the plugin-level `with_handler` so the toggle
/// only fires when the user presses *this* accelerator — secondary hotkeys
/// (registered with their own handlers) would otherwise also trigger the
/// palette toggle because `with_handler` runs for every shortcut.
#[cfg(target_os = "macos")]
fn register_primary_hotkey(
    app: &tauri::AppHandle,
    accelerator: &str,
) -> std::result::Result<(), tauri_plugin_global_shortcut::Error> {
    use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};
    app.global_shortcut()
        .on_shortcut(accelerator, |handle, _shortcut, event| {
            if matches!(event.state(), ShortcutState::Pressed) {
                toggle_main_palette(handle);
            }
        })
}

/// Reconcile the registered secondary global shortcuts. Each entry maps a
/// `SecondaryHotkeyAction` to an accelerator string; we unregister anything
/// that disappeared or whose binding changed, then register the new set with
/// per-action handlers. Returns the map of bindings that are *actually*
/// registered after this call so the caller can carry partial-failure state
/// into the next reconcile (otherwise a later reconcile would unregister an
/// accelerator we never managed to bind in the first place, taking down a
/// sibling action that happened to share it).
#[cfg(target_os = "macos")]
fn register_secondary_hotkeys(
    app: &tauri::AppHandle,
    previous: &std::collections::BTreeMap<SecondaryHotkeyAction, String>,
    next: &std::collections::BTreeMap<SecondaryHotkeyAction, String>,
) -> std::collections::BTreeMap<SecondaryHotkeyAction, String> {
    use tauri::Emitter;
    use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};

    let mut active = previous.clone();

    for (action, accel) in previous {
        // Only unregister if the next map either drops the binding or
        // changes the accelerator — leaving an unchanged binding alone
        // avoids a brief window where the shortcut is unregistered.
        if next.get(action) != Some(accel) {
            let _ = app.global_shortcut().unregister(accel.as_str());
            active.remove(action);
        }
    }

    for (action, accel) in next {
        if accel.trim().is_empty() {
            continue;
        }
        if previous.get(action) == Some(accel) {
            continue;
        }
        let captured = *action;
        let result =
            app.global_shortcut()
                .on_shortcut(accel.as_str(), move |handle, _shortcut, event| {
                    if matches!(event.state(), ShortcutState::Pressed) {
                        dispatch_secondary_hotkey(handle, captured);
                    }
                });
        if let Err(err) = result {
            tracing::warn!(
                error = %err,
                accel = %accel,
                action = ?action,
                "secondary_hotkey_register_failed",
            );
            let _ = app.emit(
                "nagori://hotkey_register_failed",
                serde_json::json!({
                    "hotkey": accel,
                    "error": err.to_string(),
                    "kind": "secondary",
                }),
            );
        } else {
            active.insert(*action, accel.clone());
        }
    }

    active
}

#[cfg(target_os = "macos")]
fn dispatch_secondary_hotkey(handle: &tauri::AppHandle, action: SecondaryHotkeyAction) {
    use tauri::Emitter;
    use tauri_plugin_notification::NotificationExt;

    let app = handle.clone();
    tauri::async_runtime::spawn(async move {
        let state = app.state::<AppState>();
        match action {
            SecondaryHotkeyAction::RepasteLast => {
                // Empty-history is silent; other failures surface via the
                // toast event so the user knows their hotkey did nothing.
                match state.repaste_last_or_recency().await {
                    Ok(()) | Err(nagori_core::AppError::NotFound) => {}
                    Err(err) => {
                        tracing::warn!(error = %err, "repaste_last_paste_failed");
                        let _ = app.emit(
                            "nagori://paste_failed",
                            serde_json::json!({ "error": err.to_string() }),
                        );
                    }
                }
            }
            SecondaryHotkeyAction::ClearHistory => match state.runtime.clear_non_pinned().await {
                Ok(purged) => {
                    state.clear_last_pasted();
                    let _ = app
                        .notification()
                        .builder()
                        .title("Nagori")
                        .body(format!("Cleared {purged} non-pinned entries."))
                        .show();
                }
                Err(err) => {
                    tracing::warn!(error = %err, "clear_history_failed");
                }
            },
        }
    });
}

#[cfg(target_os = "macos")]
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

/// Fire a one-shot background updater probe at launch and surface the
/// result via an OS notification (consistent with how capture / AI
/// transitions are signalled). The notification is best-effort —
/// permission may be denied, and a transient network failure should not
/// pop a scary banner. The download/install hand-off remains
/// user-confirmed via the manual `commands::check_for_updates` trigger.
#[cfg(target_os = "macos")]
fn spawn_startup_update_probe(app: &tauri::AppHandle) {
    use tauri_plugin_notification::NotificationExt;
    use tauri_plugin_updater::UpdaterExt;

    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let updater = match app.updater() {
            Ok(updater) => updater,
            Err(err) => {
                tracing::warn!(error = %err, "startup_update_probe_unavailable");
                return;
            }
        };
        match updater.check().await {
            Ok(Some(update)) => {
                let _ = app
                    .notification()
                    .builder()
                    .title("Nagori update available")
                    .body(format!(
                        "Version {} is ready. Open Settings → Advanced → Updates to learn more.",
                        update.version
                    ))
                    .show();
            }
            Ok(None) => {}
            Err(err) => {
                tracing::warn!(error = %err, "startup_update_probe_failed");
            }
        }
    });
}

/// Entry point for the `nagori-image://` async URI scheme.
///
/// Validates the request via [`check_image_request_preconditions`] and
/// then defers to [`build_image_response`] for the actual fetch on the
/// async runtime.
///
/// Tauri's protocol handler trait passes the context, request, and
/// responder by value; we only inspect the request, but matching that
/// signature is what lets us be registered as the handler. The lint
/// suppression scopes the by-value waiver to this glue function.
#[allow(clippy::needless_pass_by_value)]
fn dispatch_image_request(
    ctx: tauri::UriSchemeContext<'_, tauri::Wry>,
    request: tauri::http::Request<Vec<u8>>,
    responder: tauri::UriSchemeResponder,
) {
    if let Some(early) =
        check_image_request_preconditions(ctx.webview_label(), request.uri().host())
    {
        responder.respond(early);
        return;
    }
    let path = request.uri().path().to_owned();
    let app = ctx.app_handle().clone();
    tauri::async_runtime::spawn(async move {
        let response = match app.try_state::<AppState>() {
            Some(state) => build_image_response(&state.runtime, &path).await,
            None => plain_response(
                tauri::http::StatusCode::SERVICE_UNAVAILABLE,
                "app state unavailable",
            ),
        };
        responder.respond(response);
    });
}

/// Reject requests that didn't come from our bundled webview or from a
/// host we issue ourselves. Returning `Some(response)` short-circuits
/// dispatch with a 403 before any backend lookup runs.
///
/// Defence-in-depth: only the bundled "main" webview should ever resolve
/// this scheme. The OS already keys protocol handlers to this process,
/// but if a future release ships an extra webview (settings window, AI
/// panel, …) we want explicit allow-listing here, not implicit access
/// through a shared scheme.
///
/// Tauri produces `nagori-image://localhost/<id>` on macOS / Linux / iOS
/// and `http://nagori-image.localhost/<id>` on Windows / Android; anything
/// else (e.g. an arbitrary host slipped in via a crafted `<img src>` like
/// `nagori-image://evil/<id>`) gets 403 instead of resolving against our
/// backend.
fn check_image_request_preconditions(
    webview_label: &str,
    host: Option<&str>,
) -> Option<tauri::http::Response<Vec<u8>>> {
    if webview_label != "main" {
        return Some(plain_response(
            tauri::http::StatusCode::FORBIDDEN,
            "webview not allowed",
        ));
    }
    // Match `Some(...)` explicitly so a `None` host (request URL with no
    // authority component, e.g. `nagori-image:///id`) is rejected by the
    // catch-all arm rather than being coerced into the empty string and
    // matched against the allow-list. The previous `unwrap_or("")` form
    // kept the door open for a future allow-list entry to accidentally
    // include `""` and silently let host-less requests through.
    match host {
        Some("localhost" | "nagori-image.localhost") => None,
        _ => Some(plain_response(
            tauri::http::StatusCode::FORBIDDEN,
            "host not allowed",
        )),
    }
}

/// Stream raw image bytes for `nagori-image://<host>/<entry_id>` requests.
///
/// We deliberately bypass the IPC base64 path here: at 5 MB the encode +
/// JSON-serialise round trip can take >100ms and forces the entire payload
/// through the webview's data: URL parser, doubling memory residency. Going
/// through a custom scheme lets the OS hand the bytes to WebKit/WebView2 as
/// a normal HTTP-like response with a single allocation.
///
/// The same privacy guard from `get_entry` applies: Private/Secret/Blocked
/// entries return 403 instead of leaking bytes, and missing rows return 404.
async fn build_image_response(
    runtime: &NagoriRuntime,
    path: &str,
) -> tauri::http::Response<Vec<u8>> {
    let Ok(entry_id) = parse_image_entry_id(path) else {
        return plain_response(tauri::http::StatusCode::BAD_REQUEST, "invalid entry id");
    };
    let entry = match runtime.get_entry(entry_id).await {
        Ok(Some(entry)) => entry,
        Ok(None) => return plain_response(tauri::http::StatusCode::NOT_FOUND, "not found"),
        Err(err) => {
            tracing::warn!(error = %err, "image_scheme_get_entry_failed");
            return plain_response(
                tauri::http::StatusCode::INTERNAL_SERVER_ERROR,
                "lookup failed",
            );
        }
    };
    if !is_text_safe_for_default_output(entry.sensitivity) {
        return plain_response(tauri::http::StatusCode::FORBIDDEN, "sensitivity withheld");
    }
    let (bytes, mime) = match runtime.get_payload(entry_id).await {
        Ok(Some(payload)) => payload,
        Ok(None) => return plain_response(tauri::http::StatusCode::NOT_FOUND, "no payload"),
        Err(err) => {
            tracing::warn!(error = %err, "image_scheme_get_payload_failed");
            return plain_response(
                tauri::http::StatusCode::INTERNAL_SERVER_ERROR,
                "payload read failed",
            );
        }
    };
    let Some(safe_mime) = sanitise_image_mime(&mime) else {
        tracing::warn!(mime = %mime, "image_scheme_blocked_mime");
        return plain_response(
            tauri::http::StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "mime not allowed",
        );
    };
    tauri::http::Response::builder()
        .status(tauri::http::StatusCode::OK)
        .header(tauri::http::header::CONTENT_TYPE, safe_mime)
        // Force the browser to honour the Content-Type we set instead of
        // sniffing the bytes. Without this, a `payload_mime` of
        // `image/png` plus actual SVG/HTML bytes (corruption or future
        // ingestion bug) could still be rendered as a script-bearing
        // document by some engines.
        .header(tauri::http::header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        // Mark the response as inline-only so the byte stream can't be
        // hijacked into a save / open dialog with a misleading filename.
        .header(tauri::http::header::CONTENT_DISPOSITION, "inline")
        // Disallow embedding the bytes in any frame other than our own
        // webview, and prevent caching of clipboard imagery between
        // entries (the URL is keyed by entry id which we treat as the
        // cache key, but the content can be deleted at any time).
        .header(tauri::http::header::CACHE_CONTROL, "no-store")
        .body(bytes)
        .unwrap_or_else(|_| {
            tauri::http::Response::builder()
                .status(tauri::http::StatusCode::INTERNAL_SERVER_ERROR)
                .body(Vec::new())
                .expect("status-only response")
        })
}

/// Allow-list of MIME types we'll serve over `nagori-image://`.
///
/// Restricted to raster formats whose decoders are well-tested in
/// WebKit/WebView2 and which carry no scripting capability. SVG is
/// deliberately excluded — it can host `<script>` and event handlers
/// that would execute in the webview's privileged origin if served
/// inline. Anything not on this list is replaced with a 415 response
/// rather than silently downgraded to `application/octet-stream`,
/// because a misclassified payload almost always indicates either
/// corruption or an attempt to abuse the scheme as a generic file
/// transport.
const ALLOWED_IMAGE_MIME: &[&str] = &[
    "image/png",
    "image/jpeg",
    "image/gif",
    "image/webp",
    "image/bmp",
    "image/tiff",
];

fn sanitise_image_mime(raw: &str) -> Option<&'static str> {
    // Strip MIME parameters (`; charset=...`, `; profile=...`) and
    // normalise case before comparison — the IANA registry says the
    // type/subtype is case-insensitive, and downstream stores have
    // historically rendered both `image/PNG` and `image/png`.
    let bare = raw.split(';').next()?.trim().to_ascii_lowercase();
    ALLOWED_IMAGE_MIME
        .iter()
        .copied()
        .find(|allowed| *allowed == bare)
}

fn parse_image_entry_id(path: &str) -> std::result::Result<EntryId, ()> {
    // Paths come through as `/<uuid>` (mac/iOS/Linux origin) or via the
    // platform-specific http-based mapping; both encode the id as the first
    // path segment.
    path.trim_start_matches('/')
        .trim_end_matches('/')
        .parse::<EntryId>()
        .map_err(|_| ())
}

fn plain_response(
    status: tauri::http::StatusCode,
    body: &'static str,
) -> tauri::http::Response<Vec<u8>> {
    tauri::http::Response::builder()
        .status(status)
        .header(
            tauri::http::header::CONTENT_TYPE,
            "text/plain; charset=utf-8",
        )
        .body(body.as_bytes().to_vec())
        .expect("static plain response always builds")
}

#[cfg(target_os = "macos")]
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
        let _ = window.show();
        let _ = window.set_focus();
    }
}

#[cfg(test)]
mod image_scheme_tests {
    use super::*;
    use nagori_core::{
        ClipboardContent, ClipboardEntry, EntryFactory, EntryId, EntryRepository, ImageContent,
        PayloadRef, Sensitivity,
    };
    use nagori_daemon::NagoriRuntime;
    use nagori_storage::SqliteStore;
    use tauri::http::StatusCode;

    // Smallest valid PNG: a 1x1 transparent pixel, used so the response body
    // we assert on is byte-stable.
    const TINY_PNG: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F,
        0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0x00,
        0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x49,
        0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];

    fn build_runtime() -> NagoriRuntime {
        let store = SqliteStore::open_memory().expect("memory store");
        NagoriRuntime::builder(store).build()
    }

    fn make_image_entry(sensitivity: Sensitivity) -> ClipboardEntry {
        let content = ClipboardContent::Image(ImageContent {
            payload_ref: PayloadRef::DatabaseBlob(String::new()),
            width: Some(1),
            height: Some(1),
            byte_count: TINY_PNG.len(),
            mime_type: Some("image/png".to_owned()),
            pending_bytes: Some(TINY_PNG.to_vec()),
        });
        let mut entry = EntryFactory::from_content(content, None, None);
        entry.sensitivity = sensitivity;
        entry
    }

    async fn insert(runtime: &NagoriRuntime, entry: ClipboardEntry) -> EntryId {
        runtime.store().insert(entry).await.expect("insert image")
    }

    #[test]
    fn preconditions_allow_main_webview_with_localhost_host() {
        assert!(
            check_image_request_preconditions("main", Some("localhost")).is_none(),
            "macOS / Linux / iOS host"
        );
        assert!(
            check_image_request_preconditions("main", Some("nagori-image.localhost")).is_none(),
            "Windows / Android host"
        );
    }

    #[test]
    fn preconditions_reject_non_main_webview() {
        let resp = check_image_request_preconditions("settings", Some("localhost"))
            .expect("non-main webview blocked");
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
        assert_eq!(resp.body().as_slice(), b"webview not allowed");
    }

    #[test]
    fn preconditions_reject_unknown_host() {
        let resp =
            check_image_request_preconditions("main", Some("evil")).expect("foreign host blocked");
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
        assert_eq!(resp.body().as_slice(), b"host not allowed");
    }

    #[test]
    fn preconditions_reject_missing_host() {
        // A relative URI with no authority slips through `Uri::host()` as
        // `None`; treat it the same as an unrecognised host.
        let resp = check_image_request_preconditions("main", None).expect("missing host blocked");
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
        assert_eq!(resp.body().as_slice(), b"host not allowed");
    }

    #[test]
    fn preconditions_reject_empty_host_string() {
        // Regression: the previous form `host.unwrap_or("")` collapsed both
        // `None` and `Some("")` into the same empty-string sentinel, so a
        // future allow-list edit accidentally including `""` would have
        // silently let host-less requests through. The explicit `Some("…")`
        // match arm rejects empty strings without relying on allow-list
        // hygiene.
        let resp =
            check_image_request_preconditions("main", Some("")).expect("empty host string blocked");
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
        assert_eq!(resp.body().as_slice(), b"host not allowed");
    }

    #[tokio::test]
    async fn build_response_returns_bytes_for_public_image() {
        let runtime = build_runtime();
        let id = insert(&runtime, make_image_entry(Sensitivity::Public)).await;

        let resp = build_image_response(&runtime, &format!("/{id}")).await;

        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers()
                .get(tauri::http::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()),
            Some("image/png"),
        );
        assert_eq!(
            resp.headers()
                .get(tauri::http::header::CACHE_CONTROL)
                .and_then(|v| v.to_str().ok()),
            Some("no-store"),
        );
        assert_eq!(
            resp.headers()
                .get(tauri::http::header::X_CONTENT_TYPE_OPTIONS)
                .and_then(|v| v.to_str().ok()),
            Some("nosniff"),
        );
        assert_eq!(
            resp.headers()
                .get(tauri::http::header::CONTENT_DISPOSITION)
                .and_then(|v| v.to_str().ok()),
            Some("inline"),
        );
        assert_eq!(resp.body().as_slice(), TINY_PNG);
    }

    #[tokio::test]
    async fn build_response_rejects_disallowed_mime_even_when_payload_decodes() {
        // SVG, application/octet-stream, text/html etc. must be refused
        // even if the bytes parse and the entry is otherwise public.
        // Otherwise a misclassified entry could ship inline scriptable
        // content into our privileged origin.
        let runtime = build_runtime();
        let mut entry = make_image_entry(Sensitivity::Public);
        if let ClipboardContent::Image(img) = &mut entry.content {
            img.mime_type = Some("image/svg+xml".to_owned());
        }
        let id = insert(&runtime, entry).await;

        let resp = build_image_response(&runtime, &format!("/{id}")).await;
        assert_eq!(resp.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
        assert_eq!(resp.body().as_slice(), b"mime not allowed");
    }

    #[test]
    fn sanitise_image_mime_strips_parameters_and_lowercases() {
        assert_eq!(sanitise_image_mime("image/png"), Some("image/png"));
        assert_eq!(sanitise_image_mime("IMAGE/PNG"), Some("image/png"));
        assert_eq!(
            sanitise_image_mime("image/png; charset=utf-8"),
            Some("image/png"),
        );
        assert_eq!(sanitise_image_mime("  image/jpeg  "), Some("image/jpeg"));
    }

    #[test]
    fn sanitise_image_mime_rejects_disallowed_types() {
        assert_eq!(sanitise_image_mime("image/svg+xml"), None);
        assert_eq!(sanitise_image_mime("text/html"), None);
        assert_eq!(sanitise_image_mime("application/octet-stream"), None);
        assert_eq!(sanitise_image_mime(""), None);
    }

    #[tokio::test]
    async fn build_response_rejects_invalid_entry_id() {
        let runtime = build_runtime();
        let resp = build_image_response(&runtime, "/not-a-uuid").await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        assert_eq!(resp.body().as_slice(), b"invalid entry id");
    }

    #[tokio::test]
    async fn build_response_returns_404_for_unknown_entry() {
        let runtime = build_runtime();
        let missing = EntryId::new();
        let resp = build_image_response(&runtime, &format!("/{missing}")).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        assert_eq!(resp.body().as_slice(), b"not found");
    }

    #[tokio::test]
    async fn build_response_withholds_private_entry() {
        let runtime = build_runtime();
        let id = insert(&runtime, make_image_entry(Sensitivity::Private)).await;
        let resp = build_image_response(&runtime, &format!("/{id}")).await;
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
        assert_eq!(resp.body().as_slice(), b"sensitivity withheld");
    }

    #[tokio::test]
    async fn build_response_withholds_secret_entry() {
        let runtime = build_runtime();
        let id = insert(&runtime, make_image_entry(Sensitivity::Secret)).await;
        let resp = build_image_response(&runtime, &format!("/{id}")).await;
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn build_response_withholds_blocked_entry() {
        let runtime = build_runtime();
        let id = insert(&runtime, make_image_entry(Sensitivity::Blocked)).await;
        let resp = build_image_response(&runtime, &format!("/{id}")).await;
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }
}

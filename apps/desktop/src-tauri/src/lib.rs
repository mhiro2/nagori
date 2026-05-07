mod commands;
mod dto;
mod error;
mod state;
#[cfg(target_os = "macos")]
mod tray;

use nagori_core::{EntryId, Sensitivity};
use nagori_daemon::NagoriRuntime;
use state::AppState;
use tauri::Manager;

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
            .plugin(
                tauri_plugin_global_shortcut::Builder::new()
                    .with_handler(|app, _shortcut, event| {
                        use tauri_plugin_global_shortcut::ShortcutState;
                        if matches!(event.state(), ShortcutState::Pressed) {
                            toggle_main_palette(app);
                        }
                    })
                    .build(),
            )
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
            #[cfg(target_os = "macos")]
            state.spawn_background_tasks();
            app.manage(state);

            #[cfg(target_os = "macos")]
            tray::install(app.handle())?;

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
        ])
        .run(tauri::generate_context!())
        .unwrap_or_else(|err| {
            // Replacing the previous `expect` so the user sees the
            // underlying error (DB path, permission, etc.) instead of
            // only the generic panic banner. Exit non-zero so launchd /
            // login items can detect the failure.
            tracing::error!(error = %err, "tauri_run_failed");
            eprintln!("nagori: tauri runtime failed: {err}");
            std::process::exit(1);
        });
}

/// Spawn background tasks that subscribe to settings changes:
///   * keep the global hotkey in sync with `AppSettings.global_hotkey`,
///   * keep launch-at-login in sync with `AppSettings.auto_launch`,
///   * notify the user once when capture is paused / resumed,
///   * notify the user when the AI provider transitions into `enabled` so
///     they realise remote calls may now happen.
#[cfg(target_os = "macos")]
fn spawn_settings_subscribers(handle: &tauri::AppHandle) {
    use nagori_core::SettingsRepository;
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

        if let Err(err) = app.global_shortcut().register(current_hotkey.as_str()) {
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

        while settings_rx.changed().await.is_ok() {
            let snapshot = settings_rx.borrow().clone();

            if snapshot.global_hotkey != current_hotkey {
                let next = snapshot.global_hotkey.clone();
                let _ = app.global_shortcut().unregister(current_hotkey.as_str());
                if let Err(err) = app.global_shortcut().register(next.as_str()) {
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
                    let _ = app.global_shortcut().register(current_hotkey.as_str());
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

            // Refresh the tray menu so the "Pause Capture" / "Resume
            // Capture" label tracks the current state.
            tray::refresh(&app, current_capture);
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
    if !matches!(host.unwrap_or(""), "localhost" | "nagori-image.localhost") {
        return Some(plain_response(
            tauri::http::StatusCode::FORBIDDEN,
            "host not allowed",
        ));
    }
    None
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
    if matches!(
        entry.sensitivity,
        Sensitivity::Private | Sensitivity::Secret | Sensitivity::Blocked
    ) {
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
    tauri::http::Response::builder()
        .status(tauri::http::StatusCode::OK)
        .header(tauri::http::header::CONTENT_TYPE, mime)
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
        assert_eq!(resp.body().as_slice(), TINY_PNG);
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

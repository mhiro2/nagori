use crate::commands;
use crate::state::{self, AppState};
use crate::toggle_main_palette;
use nagori_core::SecondaryHotkeyAction;
use tauri::Manager;

/// Event name emitted when global-shortcut registration fails. The
/// Settings view subscribes via `TAURI_EVENTS.hotkeyRegisterFailed` and
/// surfaces the error inline. Keep the literal in lockstep with the
/// frontend `lib/tauri.ts` constant — drift is caught by
/// `SettingsView.test.ts` (mocks the same string).
pub(crate) const HOTKEY_REGISTER_FAILED_EVENT: &str = "nagori://hotkey_register_failed";

/// Event name emitted when a previously failed global-shortcut binds
/// successfully on a later reconcile, so the live frontend store can
/// drop the stale toast/banner instead of waiting for a manual dismiss.
/// Payload mirrors the failure event's `kind` field (`Some("secondary")`
/// for secondaries, omitted for the primary), plus an `action`
/// discriminator for secondaries so the frontend can route a resolve to
/// the exact failing action rather than wiping every cached secondary.
/// Keep in lockstep with `TAURI_EVENTS.hotkeyRegisterResolved` in
/// `lib/tauri.ts`.
pub(crate) const HOTKEY_REGISTER_RESOLVED_EVENT: &str = "nagori://hotkey_register_resolved";

/// Which side of the hotkey wiring produced a registration failure.
/// The emitted payload includes a `kind: "secondary"` tag for secondary
/// accelerators and omits the field for the primary palette shortcut,
/// so a future frontend handler can route the two without a new event
/// channel. The current `SettingsView.svelte` listener collapses both
/// into the same inline-error slot; the tag is preserved for that
/// future routing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HotkeyFailureKind {
    Primary,
    Secondary,
}

/// Build the JSON envelope emitted on `nagori://hotkey_register_failed`.
/// Extracted so the wire shape is locked down by unit tests without
/// needing a Tauri runtime — the desktop UI parses these fields on
/// every emit. Secondary payloads carry the action wire value so a
/// frontend store displaying a single secondary can match the eventual
/// resolved event by action identity rather than guessing.
fn build_hotkey_failure_payload(
    accelerator: &str,
    error: &str,
    kind: HotkeyFailureKind,
    action: Option<&str>,
) -> serde_json::Value {
    match (kind, action) {
        (HotkeyFailureKind::Primary, _) => serde_json::json!({
            "hotkey": accelerator,
            "error": error,
        }),
        (HotkeyFailureKind::Secondary, Some(action)) => serde_json::json!({
            "hotkey": accelerator,
            "error": error,
            "kind": "secondary",
            "action": action,
        }),
        (HotkeyFailureKind::Secondary, None) => serde_json::json!({
            "hotkey": accelerator,
            "error": error,
            "kind": "secondary",
        }),
    }
}

/// Mirror of `build_hotkey_failure_payload` that returns a typed
/// `HotkeyFailureRecord` for caching on `AppState`. Sharing the same
/// kind discriminator keeps the cached snapshot and the live emit
/// envelope structurally identical, so the frontend store can normalise
/// the two paths with one branch. Caller passes `None` for primary
/// registrations and the kebab-case wire value for secondaries — the
/// cache keys secondaries by action so two secondaries failing at once
/// don't clobber each other.
fn build_hotkey_failure_record(
    accelerator: &str,
    error: &str,
    kind: HotkeyFailureKind,
    action: Option<&str>,
) -> state::HotkeyFailureRecord {
    state::HotkeyFailureRecord {
        hotkey: accelerator.to_owned(),
        error: error.to_owned(),
        kind: match kind {
            HotkeyFailureKind::Primary => None,
            HotkeyFailureKind::Secondary => Some("secondary".to_owned()),
        },
        action: action.map(str::to_owned),
    }
}

/// Emit `nagori://hotkey_register_failed` and cache the failure on
/// `AppState` so a later-attached listener can re-hydrate via the
/// `last_hotkey_failure` command. Called from both initial registration
/// and reconciliation paths.
pub(crate) fn record_and_emit_hotkey_failure(
    app: &tauri::AppHandle,
    accelerator: &str,
    error: &str,
    kind: HotkeyFailureKind,
    action: Option<&str>,
) {
    use tauri::Emitter;
    let state = app.state::<AppState>();
    let record = build_hotkey_failure_record(accelerator, error, kind, action);
    state.record_hotkey_failure(record);
    let _ = app.emit(
        HOTKEY_REGISTER_FAILED_EVENT,
        build_hotkey_failure_payload(accelerator, error, kind, action),
    );
}

/// JSON envelope for the resolved event. The shape mirrors the failure
/// payload's `kind` discriminator so the frontend can route resolves
/// against the currently displayed failure (primary/secondary) and not
/// clear an unrelated banner. Secondary resolves additionally carry the
/// action wire value so a sibling secondary failure isn't dropped when
/// an unrelated secondary action resolves.
fn build_hotkey_resolved_payload(
    kind: HotkeyFailureKind,
    action: Option<&str>,
) -> serde_json::Value {
    match (kind, action) {
        (HotkeyFailureKind::Primary, _) => serde_json::json!({}),
        (HotkeyFailureKind::Secondary, Some(action)) => {
            serde_json::json!({ "kind": "secondary", "action": action })
        }
        (HotkeyFailureKind::Secondary, None) => serde_json::json!({ "kind": "secondary" }),
    }
}

/// Drop any cached failures that no longer reflect reality after a
/// settings tick. The user can resolve a failure three ways: rebind the
/// offending shortcut so the next register attempt succeeds, remove the
/// binding entirely, or — for a secondary — bind the same accelerator
/// to a *different* action whose register call succeeds. The plain
/// register path only emits a resolved event for the binding it just
/// touched, so without this reconciliation a stale cache (and the
/// banner riding on it) outlives every other resolution path.
///
/// Secondary clears require *both* "in desired snapshot" *and* "in the
/// active bound set" for the cached action: a cached failure is stale
/// only when *its own* action is desired with that accelerator and now
/// successfully bound. Walking the cache per-action is what stops a
/// sibling action's success from silently dropping a still-failing
/// secondary's banner.
pub(crate) fn reconcile_cached_hotkey_failures(
    app: &tauri::AppHandle,
    snapshot: &nagori_core::AppSettings,
    active_secondary: &std::collections::BTreeMap<SecondaryHotkeyAction, String>,
) {
    let state = app.state::<AppState>();
    let cache = state.hotkey_failure_cache_snapshot();
    if let Some(primary) = cache.primary
        && primary.hotkey != snapshot.global_hotkey
    {
        clear_and_notify_hotkey_failure(app, HotkeyFailureKind::Primary, None);
    }
    for (action_wire, record) in &cache.secondary {
        if should_clear_secondary_cache(
            &record.hotkey,
            action_wire,
            &snapshot.secondary_hotkeys,
            active_secondary,
        ) {
            clear_and_notify_hotkey_failure(
                app,
                HotkeyFailureKind::Secondary,
                Some(action_wire.as_str()),
            );
        }
    }
}

/// Decide whether a cached secondary failure is stale. Pure over the
/// desired and active accelerator sets so the predicate is directly
/// testable without spinning up a `tauri::AppHandle`. Stale means
/// "the binding no longer fails" for *this exact action*: the cached
/// action is no longer mapped to `accel` in the desired snapshot, or
/// it is mapped to `accel` and now sits in the active bound set. A
/// sibling action sharing the accelerator binding successfully does
/// not resolve the cached failure — only the failing action's own
/// register success does. An unrecognised cached action (e.g. a future
/// enum variant unknown to this binary) is treated conservatively as
/// "not stale" so its banner survives until the user resolves it
/// manually. Kept un-public — only the reconcile path should read this.
fn should_clear_secondary_cache(
    accel: &str,
    cached_action_wire: &str,
    desired: &std::collections::BTreeMap<SecondaryHotkeyAction, String>,
    active: &std::collections::BTreeMap<SecondaryHotkeyAction, String>,
) -> bool {
    let Some(action) = parse_secondary_action_wire(cached_action_wire) else {
        return false;
    };
    let still_desired = desired.get(&action).map(String::as_str) == Some(accel);
    let now_bound = active.get(&action).map(String::as_str) == Some(accel);
    !still_desired || now_bound
}

/// Kebab-case wire value for a `SecondaryHotkeyAction`. Matches the
/// `#[serde(rename_all = "kebab-case")]` representation on the enum,
/// without paying a `serde_json` round trip on every call.
const fn secondary_action_wire(action: SecondaryHotkeyAction) -> &'static str {
    match action {
        SecondaryHotkeyAction::RepasteLast => "repaste-last",
        SecondaryHotkeyAction::ClearHistory => "clear-history",
    }
}

/// Inverse of `secondary_action_wire`. Returns `None` for unknown
/// values so the caller can fall back to a looser match rather than
/// panicking on cache entries written by a future revision.
fn parse_secondary_action_wire(s: &str) -> Option<SecondaryHotkeyAction> {
    match s {
        "repaste-last" => Some(SecondaryHotkeyAction::RepasteLast),
        "clear-history" => Some(SecondaryHotkeyAction::ClearHistory),
        _ => None,
    }
}

/// Drop the cached failure for `(kind, action)` and emit
/// `nagori://hotkey_register_resolved` when a clear actually happened.
/// The clear is gated per-action so a primary success does not silently
/// drop any secondary failure, and a secondary success only drops its
/// own action's cached entry — sibling secondaries keep their banner.
/// The single-slot live store on the frontend uses the emitted kind +
/// action to scope its reset; passing `action` for secondaries lets it
/// keep an unrelated still-failing secondary on screen.
pub(crate) fn clear_and_notify_hotkey_failure(
    app: &tauri::AppHandle,
    kind: HotkeyFailureKind,
    action: Option<&str>,
) {
    use tauri::Emitter;
    let state = app.state::<AppState>();
    let kind_tag = match kind {
        HotkeyFailureKind::Primary => None,
        HotkeyFailureKind::Secondary => Some("secondary"),
    };
    if state.clear_hotkey_failure_for_kind_action(kind_tag, action) {
        let _ = app.emit(
            HOTKEY_REGISTER_RESOLVED_EVENT,
            build_hotkey_resolved_payload(kind, action),
        );
    }
}

/// Reconciliation plan for the secondary hotkey map. Pure data so
/// callers can apply registration results without re-deriving the diff.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct SecondaryHotkeyDiff {
    /// Accelerators that should be torn down before any new registration
    /// runs. Each entry corresponds to a `previous` binding whose
    /// accelerator was either removed in `next` or remapped to a
    /// different value. Trimmed-empty bindings count as removals.
    unregister: Vec<(SecondaryHotkeyAction, String)>,
    /// Bindings that should be registered. Excludes empty/whitespace
    /// accelerators and identical (action, accel) pairs already present
    /// in `previous`. Order is deterministic (`BTreeMap` iteration).
    register: Vec<(SecondaryHotkeyAction, String)>,
}

/// Compute which secondary hotkeys to (un)register when reconciling
/// from `previous` → `next`. Splitting this out keeps the diff logic —
/// the bit that's easy to get wrong on partial-failure reconciliation —
/// unit-testable without a `tauri::AppHandle`.
pub(crate) fn compute_secondary_hotkey_diff(
    previous: &std::collections::BTreeMap<SecondaryHotkeyAction, String>,
    next: &std::collections::BTreeMap<SecondaryHotkeyAction, String>,
) -> SecondaryHotkeyDiff {
    let mut diff = SecondaryHotkeyDiff::default();

    for (action, accel) in previous {
        // Only schedule an unregister if the binding actually changed.
        // Leaving an unchanged binding alone avoids a brief window
        // where the shortcut is unregistered between cycles.
        if next.get(action).map(String::as_str) != Some(accel.as_str()) {
            diff.unregister.push((*action, accel.clone()));
        }
    }

    for (action, accel) in next {
        if accel.trim().is_empty() {
            continue;
        }
        if previous.get(action) == Some(accel) {
            continue;
        }
        diff.register.push((*action, accel.clone()));
    }

    diff
}

/// Register the primary palette-toggle hotkey with its own handler. We use
/// `on_shortcut` rather than the plugin-level `with_handler` so the toggle
/// only fires when the user presses *this* accelerator — secondary hotkeys
/// (registered with their own handlers) would otherwise also trigger the
/// palette toggle because `with_handler` runs for every shortcut. On
/// Linux, the upstream `global-hotkey` backend is X11-only, so a Wayland
/// session without `XWayland` — or any compositor where `XGrabKey` is
/// rejected — fails this call; the caller surfaces that to the UI via
/// `nagori://hotkey_register_failed` so users can fall back to the
/// in-app open button.
pub(crate) fn register_primary_hotkey(
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
pub(crate) fn register_secondary_hotkeys(
    app: &tauri::AppHandle,
    previous: &std::collections::BTreeMap<SecondaryHotkeyAction, String>,
    next: &std::collections::BTreeMap<SecondaryHotkeyAction, String>,
) -> std::collections::BTreeMap<SecondaryHotkeyAction, String> {
    use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};

    let diff = compute_secondary_hotkey_diff(previous, next);
    let mut active = previous.clone();

    for (action, accel) in &diff.unregister {
        let _ = app.global_shortcut().unregister(accel.as_str());
        active.remove(action);
    }

    for (action, accel) in &diff.register {
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
            record_and_emit_hotkey_failure(
                app,
                accel.as_str(),
                &err.to_string(),
                HotkeyFailureKind::Secondary,
                Some(secondary_action_wire(*action)),
            );
        } else {
            active.insert(*action, accel.clone());
            // Per-action resolve: this exact action's binding just
            // succeeded, so drop only its cached failure. Sibling
            // actions' cached failures stay intact — they have their
            // own register attempts and their own resolve path.
            clear_and_notify_hotkey_failure(
                app,
                HotkeyFailureKind::Secondary,
                Some(secondary_action_wire(*action)),
            );
        }
    }

    // `reconcile_cached_hotkey_failures` runs after settings apply and
    // catches the remaining resolution paths (user clears a binding,
    // user remaps the failing action elsewhere) that don't produce a
    // successful register call here.
    active
}

pub(crate) fn dispatch_secondary_hotkey(handle: &tauri::AppHandle, action: SecondaryHotkeyAction) {
    use tauri_plugin_notification::NotificationExt;

    let app = handle.clone();
    tauri::async_runtime::spawn(async move {
        let state = app.state::<AppState>();
        match action {
            SecondaryHotkeyAction::RepasteLast => {
                // Empty-history is silent; other failures surface via the
                // toast event so the user knows their hotkey did nothing.
                // Route through `commands::emit_paste_failed` so the
                // palette-only scoping (always emit to "main") stays
                // identical across every paste-failure source — a
                // broadcast `emit` here would double-toast when both
                // windows are open.
                match state.repaste_last_or_recency().await {
                    Ok(()) | Err(nagori_core::AppError::NotFound) => {}
                    Err(err) => {
                        tracing::warn!(error = %err, "repaste_last_paste_failed");
                        // Classify before sanitising so the StatusBar chip gets
                        // the same per-reason hint as the palette paste path,
                        // and surface the curated `CommandError` message rather
                        // than the raw `AppError` Display.
                        let reason = commands::paste_failure_reason(&err);
                        let cmd_err: crate::error::CommandError = err.into();
                        commands::emit_paste_failed_with_reason(&app, &cmd_err.message, &reason);
                    }
                }
            }
            SecondaryHotkeyAction::ClearHistory => match state.runtime.clear_non_pinned().await {
                Ok(purged) => {
                    state.clear_last_pasted();
                    // Re-run an open palette's query so the cleared rows
                    // disappear live, matching the tray "Clear History" item.
                    {
                        use tauri::Emitter;
                        let _ = app.emit(crate::CLIPBOARD_CHANGED_EVENT, serde_json::json!({}));
                    }
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

#[cfg(test)]
mod hotkey_tests {
    use super::{
        HOTKEY_REGISTER_FAILED_EVENT, HOTKEY_REGISTER_RESOLVED_EVENT, HotkeyFailureKind,
        SecondaryHotkeyDiff, build_hotkey_failure_payload, build_hotkey_failure_record,
        build_hotkey_resolved_payload, compute_secondary_hotkey_diff, should_clear_secondary_cache,
    };
    use crate::{CLIPBOARD_CHANGED_EVENT, SETTINGS_CHANGED_EVENT};
    use nagori_core::SecondaryHotkeyAction;
    use std::collections::BTreeMap;

    fn map(entries: &[(SecondaryHotkeyAction, &str)]) -> BTreeMap<SecondaryHotkeyAction, String> {
        entries
            .iter()
            .map(|(action, accel)| (*action, (*accel).to_owned()))
            .collect()
    }

    #[test]
    fn event_name_matches_frontend_contract() {
        // The Settings view subscribes to this exact string via
        // `TAURI_EVENTS.hotkeyRegisterFailed` in `lib/tauri.ts`. Drift
        // would silently break the inline error surface, so lock the
        // wire constant here.
        assert_eq!(
            HOTKEY_REGISTER_FAILED_EVENT,
            "nagori://hotkey_register_failed"
        );
    }

    #[test]
    fn resolved_event_name_matches_frontend_contract() {
        // Paired with `hotkeyRegisterResolved` in `lib/tauri.ts`. Drift
        // here means the live frontend store never sees backend
        // success notifications and stale toasts outlive the resolved
        // conflict.
        assert_eq!(
            HOTKEY_REGISTER_RESOLVED_EVENT,
            "nagori://hotkey_register_resolved"
        );
    }

    #[test]
    fn resolved_payload_omits_kind_for_primary() {
        // Mirror the failure envelope: primary success carries no
        // `kind` discriminator so the frontend store only clears the
        // currently displayed primary failure. Including a "primary"
        // literal would change client routing.
        let payload = build_hotkey_resolved_payload(HotkeyFailureKind::Primary, None);
        assert!(
            payload.get("kind").is_none(),
            "primary resolved envelope must omit kind, got {payload}",
        );
        assert!(
            payload.get("action").is_none(),
            "primary resolved envelope must omit action, got {payload}",
        );
    }

    #[test]
    fn resolved_payload_tags_secondary_kind_and_action() {
        // The frontend needs to scope a resolve to the displayed
        // failure: a primary success should not silently wipe a
        // secondary banner. The action tag additionally lets the store
        // ignore a sibling secondary's resolve so two simultaneously
        // failing secondaries don't drop each other's banners.
        let payload =
            build_hotkey_resolved_payload(HotkeyFailureKind::Secondary, Some("repaste-last"));
        assert_eq!(payload["kind"], "secondary");
        assert_eq!(payload["action"], "repaste-last");
    }

    #[test]
    fn failure_record_kind_round_trips_via_build_helpers() {
        // Lock the relationship between the typed kind enum and the
        // wire-string discriminator the cache stores. The frontend
        // matches a resolve event against the cached `kind`, so a
        // drift between `build_hotkey_failure_record` and
        // `build_hotkey_resolved_payload` would silently break the
        // "clear my banner" path.
        let primary_record =
            build_hotkey_failure_record("Cmd+Shift+V", "boom", HotkeyFailureKind::Primary, None);
        assert_eq!(primary_record.kind, None);
        assert_eq!(primary_record.action, None);
        let secondary_record = build_hotkey_failure_record(
            "Cmd+Shift+R",
            "boom",
            HotkeyFailureKind::Secondary,
            Some("repaste-last"),
        );
        assert_eq!(secondary_record.kind.as_deref(), Some("secondary"));
        assert_eq!(secondary_record.action.as_deref(), Some("repaste-last"));
    }

    #[test]
    fn settings_changed_event_name_matches_frontend_contract() {
        // Same lockstep as above for the settings-broadcast channel —
        // `TAURI_EVENTS.settingsChanged` in `lib/tauri.ts` subscribes to
        // this literal. Drift would silently break the SettingsView
        // merge path that keeps a tray-toggled `captureEnabled` from
        // being clobbered by an open Settings window's next autosave.
        assert_eq!(SETTINGS_CHANGED_EVENT, "nagori://settings_changed");
    }

    #[test]
    fn clipboard_changed_event_name_matches_frontend_contract() {
        // `TAURI_EVENTS.clipboardChanged` in `lib/tauri.ts` subscribes to
        // this literal so the palette can refresh after background capture.
        assert_eq!(CLIPBOARD_CHANGED_EVENT, "nagori://clipboard_changed");
    }

    #[test]
    fn primary_failure_payload_omits_kind_field() {
        // Primary failures are surfaced as the global-hotkey error in
        // Settings → General. The frontend treats the absence of `kind`
        // as "primary", so emitting a `kind: "primary"` literal would
        // change its handling. Keep the envelope minimal.
        let payload = build_hotkey_failure_payload(
            "Cmd+Shift+V",
            "already in use",
            HotkeyFailureKind::Primary,
            None,
        );
        assert_eq!(payload["hotkey"], "Cmd+Shift+V");
        assert_eq!(payload["error"], "already in use");
        assert!(
            payload.get("kind").is_none(),
            "primary failure envelope must not include a kind tag, got {payload}",
        );
        assert!(
            payload.get("action").is_none(),
            "primary failure envelope must not include an action tag, got {payload}",
        );
    }

    #[test]
    fn secondary_failure_payload_tags_kind_and_action() {
        // Secondary failures land in the same channel but the frontend
        // routes them differently — the `kind: "secondary"` tag is how
        // it distinguishes "your repaste hotkey clashed" from "the main
        // palette shortcut is broken". The `action` tag additionally
        // pins which secondary action's binding broke, so the eventual
        // resolved event can scope its banner clear by action and not
        // wipe a sibling secondary still in failure.
        let payload = build_hotkey_failure_payload(
            "Cmd+Shift+R",
            "shortcut already registered",
            HotkeyFailureKind::Secondary,
            Some("repaste-last"),
        );
        assert_eq!(payload["hotkey"], "Cmd+Shift+R");
        assert_eq!(payload["error"], "shortcut already registered");
        assert_eq!(payload["kind"], "secondary");
        assert_eq!(payload["action"], "repaste-last");
    }

    #[test]
    fn diff_is_empty_when_previous_equals_next() {
        // No-op reconciliation: nothing has changed, so no register /
        // unregister calls should be queued. Without this guard a
        // settings-watch tick that re-publishes the same accelerator
        // would tear the binding down for a heartbeat before
        // re-establishing it.
        let m = map(&[
            (SecondaryHotkeyAction::RepasteLast, "Cmd+Shift+R"),
            (SecondaryHotkeyAction::ClearHistory, "Cmd+Shift+X"),
        ]);
        let diff = compute_secondary_hotkey_diff(&m, &m);
        assert_eq!(diff, SecondaryHotkeyDiff::default());
    }

    #[test]
    fn diff_skips_empty_and_whitespace_accelerators() {
        // An accelerator string that's empty (or all whitespace) means
        // "this action has no binding" — the user cleared the input.
        // Treat it as both a request to unregister whatever was there
        // before *and* a no-op on the register side.
        let previous = map(&[(SecondaryHotkeyAction::RepasteLast, "Cmd+Shift+R")]);
        let next = map(&[
            (SecondaryHotkeyAction::RepasteLast, ""),
            (SecondaryHotkeyAction::ClearHistory, "   "),
        ]);
        let diff = compute_secondary_hotkey_diff(&previous, &next);
        assert_eq!(
            diff.unregister,
            vec![(SecondaryHotkeyAction::RepasteLast, "Cmd+Shift+R".to_owned())],
        );
        assert!(diff.register.is_empty(), "empty bindings must not register");
    }

    #[test]
    fn diff_remaps_action_to_new_accelerator() {
        // Action stays bound but the user changed the accelerator. The
        // old binding has to be torn down before the new one can take
        // its place — otherwise the OS rejects the second register
        // with "already in use".
        let previous = map(&[(SecondaryHotkeyAction::RepasteLast, "Cmd+Shift+R")]);
        let next = map(&[(SecondaryHotkeyAction::RepasteLast, "Cmd+Alt+R")]);
        let diff = compute_secondary_hotkey_diff(&previous, &next);
        assert_eq!(
            diff.unregister,
            vec![(SecondaryHotkeyAction::RepasteLast, "Cmd+Shift+R".to_owned())],
        );
        assert_eq!(
            diff.register,
            vec![(SecondaryHotkeyAction::RepasteLast, "Cmd+Alt+R".to_owned())],
        );
    }

    #[test]
    fn diff_unregisters_dropped_action() {
        // The user removed an action from the map (action no longer
        // bound). Just unregister; nothing to register for it.
        let previous = map(&[
            (SecondaryHotkeyAction::RepasteLast, "Cmd+Shift+R"),
            (SecondaryHotkeyAction::ClearHistory, "Cmd+Shift+X"),
        ]);
        let next = map(&[(SecondaryHotkeyAction::RepasteLast, "Cmd+Shift+R")]);
        let diff = compute_secondary_hotkey_diff(&previous, &next);
        assert_eq!(
            diff.unregister,
            vec![(
                SecondaryHotkeyAction::ClearHistory,
                "Cmd+Shift+X".to_owned()
            )],
        );
        assert!(diff.register.is_empty());
    }

    #[test]
    fn diff_registers_brand_new_action() {
        // A previously empty action gains a binding. No unregister
        // needed because there was nothing to tear down.
        let previous = map(&[]);
        let next = map(&[(SecondaryHotkeyAction::RepasteLast, "Cmd+Shift+R")]);
        let diff = compute_secondary_hotkey_diff(&previous, &next);
        assert!(diff.unregister.is_empty());
        assert_eq!(
            diff.register,
            vec![(SecondaryHotkeyAction::RepasteLast, "Cmd+Shift+R".to_owned())],
        );
    }

    #[test]
    fn reconcile_keeps_cache_while_accel_still_desired_and_unbound() {
        // The failing action is still in the desired map and never made
        // it into the active (bound) set. The cached failure is the
        // current state of the world — reconcile must leave it alone or
        // the toast will silently disappear while the user is still
        // locked out of the binding.
        let desired = map(&[(SecondaryHotkeyAction::RepasteLast, "Cmd+Shift+R")]);
        let active = map(&[]);
        assert!(!should_clear_secondary_cache(
            "Cmd+Shift+R",
            "repaste-last",
            &desired,
            &active,
        ));
    }

    #[test]
    fn reconcile_clears_cache_when_user_removes_failing_binding() {
        // The user gave up on the conflicting accelerator and cleared
        // it. No register call ever ran for the removed action, so
        // without reconcile the cache (and the banner riding on it)
        // would persist forever.
        let desired = map(&[]);
        let active = map(&[]);
        assert!(should_clear_secondary_cache(
            "Cmd+Shift+R",
            "repaste-last",
            &desired,
            &active,
        ));
    }

    #[test]
    fn reconcile_clears_cache_when_same_action_now_binds_successfully() {
        // The failing action retried (e.g. the conflicting external
        // process released the accelerator) and the same binding now
        // sits in the active map. Cache is stale; clear it.
        let desired = map(&[(SecondaryHotkeyAction::RepasteLast, "Cmd+Shift+R")]);
        let active = map(&[(SecondaryHotkeyAction::RepasteLast, "Cmd+Shift+R")]);
        assert!(should_clear_secondary_cache(
            "Cmd+Shift+R",
            "repaste-last",
            &desired,
            &active,
        ));
    }

    #[test]
    fn reconcile_keeps_cache_when_sibling_shares_accel_but_failing_action_unbound() {
        // Regression guard for the codex-flagged duplicate-accel
        // scenario: two secondary actions are both assigned the same
        // accelerator. One register succeeds (`clear-history`), the
        // other fails (`repaste-last`). Without action-aware matching
        // `active.values().any()` would see the bound sibling and
        // clear the cache, silently hiding the real failure. With
        // action identity, the cache stays put.
        let desired = map(&[
            (SecondaryHotkeyAction::RepasteLast, "Cmd+Shift+R"),
            (SecondaryHotkeyAction::ClearHistory, "Cmd+Shift+R"),
        ]);
        let active = map(&[(SecondaryHotkeyAction::ClearHistory, "Cmd+Shift+R")]);
        assert!(!should_clear_secondary_cache(
            "Cmd+Shift+R",
            "repaste-last",
            &desired,
            &active,
        ));
    }

    #[test]
    fn reconcile_keeps_cache_when_unrelated_sibling_succeeds() {
        // Regression guard for the blanket-clear bug: the user adds a
        // brand new secondary binding that registers successfully, but
        // a *different* accelerator is still cached as a failure. The
        // success of the new binding must not wipe the still-failing
        // sibling's cache.
        let desired = map(&[
            (SecondaryHotkeyAction::RepasteLast, "Cmd+Shift+R"),
            (SecondaryHotkeyAction::ClearHistory, "Cmd+Shift+X"),
        ]);
        let active = map(&[(SecondaryHotkeyAction::ClearHistory, "Cmd+Shift+X")]);
        assert!(!should_clear_secondary_cache(
            "Cmd+Shift+R",
            "repaste-last",
            &desired,
            &active,
        ));
    }

    #[test]
    fn reconcile_clears_cache_when_failing_action_remapped_even_if_sibling_holds_old_accel() {
        // Regression for the desired-side accelerator-only check.
        // Cached failure: `repaste-last` / Cmd+Shift+R. User moves
        // `repaste-last` to Cmd+Shift+P (and that succeeds), while
        // `clear-history` independently keeps Cmd+Shift+R. The cached
        // record is now stale — `repaste-last` no longer wants that
        // accel — even though Cmd+Shift+R is still desired by some
        // other action. Action-aware matching on both halves catches
        // this; a values-only `still_desired` would miss it.
        let desired = map(&[
            (SecondaryHotkeyAction::RepasteLast, "Cmd+Shift+P"),
            (SecondaryHotkeyAction::ClearHistory, "Cmd+Shift+R"),
        ]);
        let active = map(&[(SecondaryHotkeyAction::RepasteLast, "Cmd+Shift+P")]);
        assert!(should_clear_secondary_cache(
            "Cmd+Shift+R",
            "repaste-last",
            &desired,
            &active,
        ));
    }

    #[test]
    fn reconcile_keeps_cache_when_cached_action_wire_value_is_unknown() {
        // Forward-compatibility: a future binary could write a cache
        // entry with an action wire value this build doesn't recognise.
        // We treat that as "not stale" so the banner survives on
        // upgrades — losing a still-current failure silently would be
        // worse than holding a stale one, which the user can dismiss.
        let desired = map(&[(SecondaryHotkeyAction::RepasteLast, "Cmd+Shift+R")]);
        let active = map(&[(SecondaryHotkeyAction::RepasteLast, "Cmd+Shift+R")]);
        assert!(!should_clear_secondary_cache(
            "Cmd+Shift+R",
            "future-action",
            &desired,
            &active,
        ));
    }

    #[test]
    fn diff_keeps_unchanged_bindings_untouched_when_sibling_changes() {
        // Regression guard for the partial-failure scenario described
        // in `register_secondary_hotkeys`'s doc comment: changing one
        // sibling must not produce a (un)register pair for the
        // unchanged action, otherwise a transient unregister window
        // would let the OS reassign the still-bound accelerator.
        let previous = map(&[
            (SecondaryHotkeyAction::RepasteLast, "Cmd+Shift+R"),
            (SecondaryHotkeyAction::ClearHistory, "Cmd+Shift+X"),
        ]);
        let next = map(&[
            (SecondaryHotkeyAction::RepasteLast, "Cmd+Shift+R"),
            (SecondaryHotkeyAction::ClearHistory, "Cmd+Alt+X"),
        ]);
        let diff = compute_secondary_hotkey_diff(&previous, &next);
        assert_eq!(
            diff.unregister,
            vec![(
                SecondaryHotkeyAction::ClearHistory,
                "Cmd+Shift+X".to_owned()
            )],
        );
        assert_eq!(
            diff.register,
            vec![(SecondaryHotkeyAction::ClearHistory, "Cmd+Alt+X".to_owned())],
        );
    }
}

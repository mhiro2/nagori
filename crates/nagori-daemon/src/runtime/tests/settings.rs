use std::sync::Arc;

use nagori_core::{OnboardingSettings, SettingsRepository};
use nagori_platform::PermissionState;

use super::super::*;
use super::{StubPermissionChecker, accessibility_row, runtime_with_memory_clipboard};

#[tokio::test]
async fn save_settings_preserves_persisted_onboarding_markers() {
    // The runtime owns the `onboarding` markers; an `update_settings`
    // IPC from the desktop shell (which round-trips a possibly-stale
    // snapshot of the markers) must never overwrite a marker that
    // the daemon stamped between the frontend's get_settings and
    // its follow-up update_settings. `save_settings` re-merges the
    // persisted `onboarding` block inside the write lock to enforce
    // that invariant.
    let store = SqliteStore::open_memory().expect("memory store should open");
    let stub = Arc::new(StubPermissionChecker::new(
        vec![accessibility_row(PermissionState::Granted)],
        accessibility_row(PermissionState::Granted),
    ));
    let runtime = NagoriRuntime::builder(store)
        .permissions(stub.clone())
        .build_for_test();
    // Stamp the marker via a permission_check observation.
    let _ = runtime.permission_check().await.expect("permission_check");
    let stamped = runtime
        .current_settings()
        .onboarding
        .accessibility_first_granted_at
        .expect("first_granted_at must be set after Granted observation");
    // Simulate a stale frontend snapshot: read settings, zero the
    // onboarding markers, then write back. The persisted markers
    // must survive.
    let mut stale = runtime.current_settings();
    stale.onboarding = OnboardingSettings::default();
    runtime
        .save_settings(stale)
        .await
        .expect("save_settings round-trip");
    let after = runtime.current_settings().onboarding;
    assert_eq!(
        after.accessibility_first_granted_at,
        Some(stamped),
        "save_settings must restore onboarding markers from the store",
    );
}

#[tokio::test]
async fn set_capture_enabled_does_not_roll_back_concurrent_field_edits() {
    // The tray's pause/resume toggles only `capture_enabled`. The old
    // implementation read a full settings snapshot *outside* the write
    // lock, then saved it — so a `save_settings` (e.g. an
    // `update_settings` IPC editing `global_hotkey`) landing in between
    // got silently rolled back by the stale blob. `mutate_settings`
    // reads-modifies-writes inside the lock, so whichever op commits
    // second still observes (and re-persists) the other's change.
    let (runtime, _) = runtime_with_memory_clipboard();
    assert!(runtime.current_settings().capture_enabled);

    let mut edited = runtime.current_settings();
    edited.global_hotkey = "CmdOrCtrl+Alt+V".to_owned();

    let (toggled, saved) = tokio::join!(
        runtime.set_capture_enabled(false),
        runtime.save_settings(edited)
    );
    let toggled = toggled.expect("capture toggle should succeed");
    saved.expect("concurrent settings save should succeed");

    // The toggle's own return value must reflect the persisted state,
    // not a pre-toggle snapshot.
    assert!(!toggled.capture_enabled);

    let persisted = runtime
        .store()
        .get_settings()
        .await
        .expect("settings should persist");
    assert_eq!(
        persisted.global_hotkey, "CmdOrCtrl+Alt+V",
        "capture toggle must not roll back a concurrent global_hotkey edit",
    );
}

use std::sync::Arc;

use nagori_platform::PermissionState;

use super::super::*;
use super::{StubPermissionChecker, accessibility_row};

#[tokio::test]
async fn request_accessibility_stamps_prompted_at_when_prompt_true() {
    // The "NotRequested vs PromptShownNotGranted" UI branch keys off
    // `onboarding.accessibility_prompted_at`. Verify the
    // runtime persists that timestamp the first time we ask the host
    // to surface the TCC dialog (`prompt = true`).
    let store = SqliteStore::open_memory().expect("memory store should open");
    let stub = Arc::new(StubPermissionChecker::new(
        vec![accessibility_row(PermissionState::NotDetermined)],
        accessibility_row(PermissionState::Denied),
    ));
    let runtime = NagoriRuntime::builder(store)
        .permissions(stub.clone())
        .build_for_test();
    // Pre-condition: never prompted, so the context the checker sees
    // should be empty.
    let _ = runtime.permission_check().await.expect("permission_check");
    let ctx = stub.observed_ctx().expect("check was invoked");
    assert!(ctx.accessibility_prompted_at.is_none());

    let _ = runtime
        .request_accessibility(true)
        .await
        .expect("request_accessibility");
    assert_eq!(stub.observed_prompt(), Some(true));

    // Post-condition: the runtime persisted the prompt timestamp, and a
    // follow-up check carries it through the context so the checker
    // can discriminate Denied from NotDetermined.
    let settings = runtime.current_settings();
    assert!(
        settings.onboarding.accessibility_prompted_at.is_some(),
        "prompt = true must stamp accessibility_prompted_at",
    );
    let _ = runtime.permission_check().await.expect("permission_check");
    let ctx_after = stub.observed_ctx().expect("check was invoked");
    assert!(ctx_after.accessibility_prompted_at.is_some());
}

#[tokio::test]
async fn request_accessibility_skips_prompted_at_when_prompt_false() {
    // `prompt = false` is the "just probe, don't surface UI" path
    // (`AXIsProcessTrustedWithOptions(prompt:NO)`); it must not move
    // the persisted prompt timestamp, otherwise a UI re-render that
    // calls the no-prompt probe would erroneously flip NotRequested.
    let store = SqliteStore::open_memory().expect("memory store should open");
    let stub = Arc::new(StubPermissionChecker::new(
        vec![accessibility_row(PermissionState::NotDetermined)],
        accessibility_row(PermissionState::Denied),
    ));
    let runtime = NagoriRuntime::builder(store)
        .permissions(stub.clone())
        .build_for_test();

    let _ = runtime
        .request_accessibility(false)
        .await
        .expect("request_accessibility");
    assert_eq!(stub.observed_prompt(), Some(false));

    let settings = runtime.current_settings();
    assert!(
        settings.onboarding.accessibility_prompted_at.is_none(),
        "prompt = false must leave accessibility_prompted_at untouched",
    );
}

#[tokio::test]
async fn permission_check_stamps_first_granted_once() {
    // `accessibility_first_granted_at` is a sticky onboarding marker:
    // once stamped, it must not be overwritten on subsequent grants
    // (the UI uses it for "you're set up" copy timing and onboarding
    // exit). Verify both the first-grant write and the no-op on a
    // second Granted observation.
    let store = SqliteStore::open_memory().expect("memory store should open");
    let stub = Arc::new(StubPermissionChecker::new(
        vec![accessibility_row(PermissionState::Granted)],
        accessibility_row(PermissionState::Granted),
    ));
    let runtime = NagoriRuntime::builder(store)
        .permissions(stub.clone())
        .build_for_test();

    let _ = runtime.permission_check().await.expect("first check");
    let stamped = runtime
        .current_settings()
        .onboarding
        .accessibility_first_granted_at
        .expect("first Granted observation must stamp the marker");

    // Tick the clock through a short sleep so any rewrite would
    // produce a strictly-later timestamp.
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    let _ = runtime.permission_check().await.expect("second check");
    let after = runtime
        .current_settings()
        .onboarding
        .accessibility_first_granted_at
        .expect("marker remains set on subsequent grants");
    assert_eq!(stamped, after, "first_granted_at must be sticky");
}

#[tokio::test]
async fn permission_check_does_not_stamp_when_not_granted() {
    // Symmetry with the sticky-marker test: a Denied / NotDetermined
    // observation must leave the marker absent, otherwise the Setup
    // card would skip its "Grant access" CTA and the doctor would
    // claim onboarding completed.
    let store = SqliteStore::open_memory().expect("memory store should open");
    let stub = Arc::new(StubPermissionChecker::new(
        vec![accessibility_row(PermissionState::Denied)],
        accessibility_row(PermissionState::Denied),
    ));
    let runtime = NagoriRuntime::builder(store)
        .permissions(stub.clone())
        .build_for_test();

    let _ = runtime.permission_check().await.expect("check");
    assert!(
        runtime
            .current_settings()
            .onboarding
            .accessibility_first_granted_at
            .is_none(),
    );

    // Flip to Granted and re-check; the marker should now appear.
    stub.set_check(vec![accessibility_row(PermissionState::Granted)]);
    let _ = runtime.permission_check().await.expect("check after grant");
    assert!(
        runtime
            .current_settings()
            .onboarding
            .accessibility_first_granted_at
            .is_some(),
    );
}

#[tokio::test]
async fn request_accessibility_stamps_first_granted_on_grant() {
    // The `request_accessibility` path also has to stamp the marker on
    // its own (rather than waiting for the next `permission_check`),
    // because the Setup card finishes its flow as soon as the trait
    // call resolves Granted — without this hook the marker would lag
    // by one full check cycle.
    let store = SqliteStore::open_memory().expect("memory store should open");
    let stub = Arc::new(StubPermissionChecker::new(
        vec![accessibility_row(PermissionState::NotDetermined)],
        accessibility_row(PermissionState::Granted),
    ));
    let runtime = NagoriRuntime::builder(store)
        .permissions(stub.clone())
        .build_for_test();
    let _ = runtime
        .request_accessibility(true)
        .await
        .expect("request_accessibility");
    let onboarding = runtime.current_settings().onboarding;
    assert!(
        onboarding.accessibility_first_granted_at.is_some(),
        "Granted result must stamp first_granted_at without an extra permission_check"
    );
    assert!(onboarding.accessibility_prompted_at.is_some());
    // Flip the response back to Denied and re-call: the sticky marker
    // must not regress, even though the new observation is not Granted.
    stub.set_request(accessibility_row(PermissionState::Denied));
    let before = runtime
        .current_settings()
        .onboarding
        .accessibility_first_granted_at;
    let _ = runtime
        .request_accessibility(true)
        .await
        .expect("request_accessibility");
    assert_eq!(
        runtime
            .current_settings()
            .onboarding
            .accessibility_first_granted_at,
        before,
        "first_granted_at must be sticky across later Denied results",
    );
}

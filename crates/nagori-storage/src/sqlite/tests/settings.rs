use nagori_core::{AppError, AppSettings, SettingsRepository};

use super::super::*;

#[tokio::test]
async fn settings_revision_bumps_and_cas_rejects_stale_writes() {
    let store = SqliteStore::open_memory().unwrap();
    // Fresh store: no settings row yet, so the revision baseline is 0 and
    // the body is the default — read as one consistent pair.
    let (settings, revision) = store.get_settings_with_revision().await.unwrap();
    assert_eq!(revision, 0);
    assert_eq!(settings, AppSettings::default());

    // A compare-and-swap save against the current (0) revision lands and
    // advances the token to 1.
    let rev = store
        .save_settings_checked(AppSettings::default(), 0)
        .await
        .unwrap();
    assert_eq!(rev, 1);
    assert_eq!(store.get_settings_with_revision().await.unwrap().1, 1);

    // A second save still using the stale base (0) is a conflict — this is
    // the lost-update a full-blob client would otherwise cause — and the
    // stored revision is left untouched.
    let err = store
        .save_settings_checked(AppSettings::default(), 0)
        .await
        .unwrap_err();
    assert!(matches!(err, AppError::Conflict(_)));
    assert_eq!(store.get_settings_with_revision().await.unwrap().1, 1);

    // Re-reading the current revision and retrying succeeds.
    let rev = store
        .save_settings_checked(AppSettings::default(), 1)
        .await
        .unwrap();
    assert_eq!(rev, 2);

    // A plain (force) save — the path the tray toggle / IPC client take —
    // also advances the revision, so a stale full-blob base is caught.
    store.save_settings(AppSettings::default()).await.unwrap();
    assert_eq!(store.get_settings_with_revision().await.unwrap().1, 3);
}

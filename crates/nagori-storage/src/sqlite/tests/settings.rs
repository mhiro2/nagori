use nagori_core::{AppError, AppSettings, SettingsRepository};
use rusqlite::params;

use super::super::*;

/// Overwrite the `app` settings row with a raw value, simulating a hand-edited
/// or downgraded database. Bumps the revision so both read paths see it.
fn write_raw_settings_row(store: &SqliteStore, value: &str) {
    let conn = store.conn().expect("lock conn");
    conn.execute(
        "INSERT INTO settings (key, value, updated_at, revision)
         VALUES ('app', ?1, '2026-01-01T00:00:00Z', 1)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, revision = settings.revision + 1",
        params![value],
    )
    .expect("write raw settings row");
}

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

#[tokio::test]
async fn get_settings_surfaces_unparseable_row() {
    // A row that is not valid JSON (truncated write, foreign tool) must fail
    // loudly rather than silently falling back to defaults — defaulting would
    // drop the user's privacy denylist (fail-open).
    let store = SqliteStore::open_memory().unwrap();
    write_raw_settings_row(&store, "{ this is not valid json");

    let err = store.get_settings().await.unwrap_err();
    assert!(
        matches!(err, AppError::Storage { .. }),
        "unparseable settings must surface as a storage error, got {err:?}"
    );
    // The revision-paired read mirrors the same behaviour.
    assert!(store.get_settings_with_revision().await.is_err());
}

#[tokio::test]
async fn get_settings_rejects_a_row_that_parses_but_fails_validation() {
    // A hand-edited or downgraded row can deserialize cleanly yet carry an
    // out-of-range value (`palette_row_count = 0`). `get_settings` runs the
    // same `validate()` gate as `save_settings`, so the corrupt row surfaces
    // as an error instead of wedging the palette with a zero row count.
    let store = SqliteStore::open_memory().unwrap();

    let bad = AppSettings {
        palette_row_count: 0,
        ..AppSettings::default()
    };
    assert!(bad.validate().is_err(), "fixture must be invalid");
    let blob = serde_json::to_string(&bad).unwrap();
    write_raw_settings_row(&store, &blob);

    let err = store.get_settings().await.unwrap_err();
    assert!(
        matches!(err, AppError::InvalidInput(_)),
        "out-of-range settings must surface as InvalidInput, got {err:?}"
    );
    assert!(store.get_settings_with_revision().await.is_err());
}

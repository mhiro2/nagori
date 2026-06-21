use std::sync::Arc;

use nagori_platform::MemoryClipboard;

use super::super::*;
use super::runtime_with_memory_clipboard;

#[tokio::test]
async fn doctor_report_reflects_startup_health_outcome() {
    // Lock the wiring from `StartupHealth` into the `Doctor` IPC
    // handler: `nagori doctor` is the operator-facing surface where
    // a silent capture-init abort has to be visible. Without this
    // test, dropping the `startup` field from `DoctorReport` (or
    // forgetting to record it) would compile cleanly and re-introduce
    // the original "looks ready, isn't" bug.
    let (runtime, _) = runtime_with_memory_clipboard();
    let pending = runtime
        .build_doctor_report()
        .await
        .expect("doctor report builds with default startup state");
    assert!(
        !pending.startup.ready,
        "default startup state must report not-ready"
    );
    assert!(pending.startup.last_error.is_none());

    runtime
        .startup_health()
        .record_capture_failed("could not load settings");
    let failed = runtime
        .build_doctor_report()
        .await
        .expect("doctor report builds after recording a failure");
    assert!(!failed.startup.ready);
    assert_eq!(
        failed.startup.last_error.as_deref(),
        Some("could not load settings"),
    );

    // Late `record_capture_ready` must not flip a recorded failure
    // back to ready — `StartupHealth` is first-outcome-wins.
    runtime.startup_health().record_capture_ready();
    let still_failed = runtime
        .build_doctor_report()
        .await
        .expect("doctor report builds after a no-op ready record");
    assert!(!still_failed.startup.ready);
    assert!(still_failed.startup.last_error.is_some());
}

#[tokio::test]
async fn doctor_report_marks_ready_once_capture_records_success() {
    // Positive case: once the host process records readiness, the
    // doctor surface reports it without needing any additional
    // wiring. Pair with the failure test above so a future refactor
    // that hard-codes `ready: false` or `ready: true` in the
    // builder is caught.
    let (runtime, _) = runtime_with_memory_clipboard();
    runtime.startup_health().record_capture_ready();
    let report = runtime
        .build_doctor_report()
        .await
        .expect("doctor report builds after recording readiness");
    assert!(report.startup.ready);
    assert!(report.startup.last_error.is_none());
}

#[tokio::test]
async fn doctor_report_echoes_the_configured_db_path() {
    // An operator probing over IPC has to be able to tell which store a daemon
    // launched with `--db` / `NAGORI_DB_PATH` is holding. Without the path
    // threaded into the runtime the IPC `Doctor` report's `db` row stayed
    // empty, so the local and IPC doctor arms disagreed about the same daemon.
    let store = nagori_storage::SqliteStore::open_memory().expect("memory store should open");
    let runtime = NagoriRuntime::builder(store)
        .clipboard(Arc::new(MemoryClipboard::new()))
        .db_path(std::path::PathBuf::from(
            "/tmp/nagori-doctor-test/history.db",
        ))
        .build_for_test();

    let report = runtime
        .build_doctor_report()
        .await
        .expect("doctor report builds with a configured db path");

    assert_eq!(report.db_path, "/tmp/nagori-doctor-test/history.db");
}

#[tokio::test]
async fn doctor_report_leaves_db_path_empty_when_unset() {
    // Hosts that don't thread a path (the desktop, tests, in-process callers)
    // must keep the `db` row omitted exactly as before — the text doctor gates
    // it on a non-empty string — and the cloud-sync warning is left to the
    // host that knows its own data dir.
    let (runtime, _) = runtime_with_memory_clipboard();

    let report = runtime
        .build_doctor_report()
        .await
        .expect("doctor report builds without a configured db path");

    assert!(report.db_path.is_empty());
    assert!(report.data_dir_sync_warning.is_none());
}

#[test]
fn builder_build_errors_when_clipboard_missing() {
    // `build()` is the production entry point: a missing clipboard
    // adapter means the runtime would silently fall back to an
    // in-memory stub and the app would come up with capture
    // forever-disabled. Pin the contract that this returns
    // `AppError::Configuration` instead, so wiring drift is caught
    // at startup rather than as "clipboard quietly stopped working".
    let store = SqliteStore::open_memory().expect("memory store");
    let result = NagoriRuntime::builder(store)
        .paste(Arc::new(nagori_platform::NoopPasteController))
        .build();
    match result {
        Err(AppError::Configuration(ref msg)) if msg.contains("clipboard") => {}
        Err(err) => panic!("expected Configuration(clipboard), got {err:?}"),
        Ok(_) => panic!("expected error, builder accepted missing clipboard"),
    }
}

#[test]
fn builder_build_errors_when_paste_missing() {
    // Symmetrically, a missing paste controller means
    // `paste_frontmost` would always be a no-op success on platforms
    // that forgot to wire their adapter. Surface this as
    // `AppError::Configuration` at build time.
    let store = SqliteStore::open_memory().expect("memory store");
    let result = NagoriRuntime::builder(store)
        .clipboard(Arc::new(MemoryClipboard::new()))
        .build();
    match result {
        Err(AppError::Configuration(ref msg)) if msg.contains("paste") => {}
        Err(err) => panic!("expected Configuration(paste), got {err:?}"),
        Ok(_) => panic!("expected error, builder accepted missing paste controller"),
    }
}

//! Snapshot-driven black-box tests for the `nagori` binary.
//!
//! Snapshots are checked in alongside the test file (`snapshots/`); update
//! them with `cargo insta review` after intentional CLI surface changes.

use std::path::PathBuf;
use std::process::Command;

use assert_cmd::prelude::*;
use predicates::prelude::*;
use tempfile::TempDir;

const UUID_REGEX: &str = r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}";
const RFC3339_REGEX: &str = r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:\d{2})";

fn temp_db() -> (TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("nagori.sqlite");
    (dir, path)
}

fn nagori(db: &PathBuf) -> Command {
    let mut cmd = Command::cargo_bin("nagori").expect("nagori binary");
    cmd.arg("--db").arg(db);
    cmd
}

fn stdout_string(output: &std::process::Output) -> String {
    String::from_utf8(output.stdout.clone()).expect("stdout utf-8")
}

fn redact(value: &str) -> String {
    let id = regex::Regex::new(UUID_REGEX).unwrap();
    let ts = regex::Regex::new(RFC3339_REGEX).unwrap();
    let intermediate = id.replace_all(value, "[id]");
    ts.replace_all(&intermediate, "[ts]").into_owned()
}

#[test]
fn list_empty_db_text_snapshot() {
    let (_dir, db) = temp_db();
    let output = nagori(&db).arg("list").output().expect("invoke nagori");
    assert!(output.status.success(), "exit: {:?}", output.status);
    insta::assert_snapshot!(&stdout_string(&output), @"");
}

#[test]
fn list_empty_db_json_snapshot() {
    let (_dir, db) = temp_db();
    let output = nagori(&db)
        .args(["--json", "list"])
        .output()
        .expect("invoke nagori");
    assert!(output.status.success(), "exit: {:?}", output.status);
    insta::assert_snapshot!(&stdout_string(&output), @"[]\n");
}

#[test]
fn list_empty_db_jsonl_snapshot() {
    let (_dir, db) = temp_db();
    let output = nagori(&db)
        .args(["--jsonl", "list"])
        .output()
        .expect("invoke nagori");
    assert!(output.status.success(), "exit: {:?}", output.status);
    insta::assert_snapshot!(&stdout_string(&output), @"");
}

#[test]
fn clear_without_scope_is_rejected() {
    // Regression: an earlier version of `nagori clear` defaulted to days=0
    // and silently wiped every unpinned entry. Require an explicit scope so
    // a stray `nagori clear` from the shell can't destroy history.
    let (_dir, db) = temp_db();
    let output = nagori(&db).arg("clear").output().expect("invoke nagori");
    assert!(
        !output.status.success(),
        "clear without scope must fail: {:?}",
        output.status,
    );
    let stderr = String::from_utf8(output.stderr).expect("stderr utf-8");
    assert!(
        stderr.contains("--all") || stderr.contains("--older-than-days"),
        "error must hint at the required flag, got: {stderr:?}",
    );
}

#[test]
fn clear_all_text_snapshot() {
    let (_dir, db) = temp_db();
    let output = nagori(&db)
        .args(["clear", "--all"])
        .output()
        .expect("invoke nagori");
    assert!(output.status.success(), "exit: {:?}", output.status);
    insta::assert_snapshot!(&stdout_string(&output), @"deleted 0\n");
}

#[test]
fn clear_all_json_snapshot() {
    let (_dir, db) = temp_db();
    let output = nagori(&db)
        .args(["--json", "clear", "--all"])
        .output()
        .expect("invoke nagori");
    assert!(output.status.success(), "exit: {:?}", output.status);
    insta::assert_snapshot!(&stdout_string(&output), @r#"{"deleted":0}
"#);
}

#[test]
fn get_invalid_id_exits_with_invalid_input_code() {
    let (_dir, db) = temp_db();
    let output = nagori(&db)
        .args(["get", "not-a-valid-uuid"])
        .output()
        .expect("invoke nagori");
    assert!(!output.status.success());
    assert_eq!(output.status.code(), Some(2), "invalid_input → exit 2");
    let stderr = String::from_utf8(output.stderr).expect("stderr utf-8");
    assert!(
        stderr.contains("invalid entry id"),
        "stderr was: {stderr:?}",
    );
}

#[test]
fn copy_invalid_id_does_not_require_clipboard_access() {
    let (_dir, db) = temp_db();
    let output = nagori(&db)
        .args(["copy", "not-a-valid-uuid"])
        .output()
        .expect("invoke nagori");
    assert!(!output.status.success());
    assert_eq!(output.status.code(), Some(2), "invalid_input → exit 2");
    let stderr = String::from_utf8(output.stderr).expect("stderr utf-8");
    assert!(
        stderr.contains("invalid entry id"),
        "stderr was: {stderr:?}",
    );
}

#[test]
fn get_unknown_id_exits_with_not_found_code() {
    let (_dir, db) = temp_db();
    // A syntactically valid UUID that has no row in the empty DB.
    let output = nagori(&db)
        .args(["get", "00000000-0000-0000-0000-000000000000"])
        .output()
        .expect("invoke nagori");
    assert!(!output.status.success());
    assert_eq!(output.status.code(), Some(4), "not_found → exit 4");
}

#[test]
#[cfg(unix)]
fn unwritable_db_path_exits_with_storage_code() {
    // A regression guard for `exit_code_for`'s default arm. Earlier versions
    // returned 1 for any error that wasn't an explicitly-listed AppError
    // variant, conflating "transient internal failure" with shells' generic
    // "command failed" code. Forcing a Storage error (parent dir cannot be
    // created because /dev/null is a file, not a directory) confirms the
    // AppError downcast still produces 8 and that no upstream layer rewraps
    // the cause back into anyhow.
    let mut cmd = Command::cargo_bin("nagori").expect("nagori binary");
    let output = cmd
        .arg("--db")
        .arg("/dev/null/cannot-create/nagori.sqlite")
        .arg("list")
        .output()
        .expect("invoke nagori");
    assert!(!output.status.success());
    assert_eq!(output.status.code(), Some(8), "storage → exit 8");
}

#[test]
fn clear_invalid_older_than_days_exits_non_one() {
    // The CLI must not return exit 1 for argument-parse failures — clap's
    // default for `unexpected argument` is 2 (InvalidInput), which matches
    // the AppError mapping. The test pins that contract so a future switch
    // away from clap's default doesn't silently re-introduce a generic
    // exit 1 for malformed flags.
    let (_dir, db) = temp_db();
    let output = nagori(&db)
        .args(["clear", "--older-than-days", "not-a-number"])
        .output()
        .expect("invoke nagori");
    assert!(!output.status.success());
    assert_eq!(output.status.code(), Some(2), "clap parse failure → exit 2");
}

#[test]
fn add_then_get_text_round_trip_snapshot() {
    // Exercises the full add → get pipeline, redacting the entry id and
    // timestamps before the snapshot so the assertion is deterministic.
    let (_dir, db) = temp_db();
    let add = nagori(&db)
        .args(["add", "--text", "snapshot value"])
        .output()
        .expect("invoke nagori add");
    assert!(add.status.success(), "exit: {:?}", add.status);

    let added_redacted = redact(&stdout_string(&add));
    insta::assert_snapshot!(&added_redacted, @"snapshot value\n");

    let list = nagori(&db).args(["list"]).output().expect("invoke list");
    assert!(list.status.success());
    let list_redacted = redact(&stdout_string(&list));
    insta::assert_snapshot!(&list_redacted, @"[id]\tText\tsnapshot value\n");
}

#[test]
fn add_then_list_jsonl_snapshot() {
    let (_dir, db) = temp_db();
    let _ = nagori(&db)
        .args(["add", "--text", "jsonl value"])
        .output()
        .expect("invoke add");

    let list = nagori(&db)
        .args(["--jsonl", "list"])
        .output()
        .expect("invoke list");
    assert!(list.status.success(), "exit: {:?}", list.status);

    let stdout = stdout_string(&list);
    // JSONL: exactly one record terminated by a newline.
    let lines: Vec<&str> = stdout.split_inclusive('\n').collect();
    assert_eq!(
        lines.len(),
        1,
        "expected single JSONL record, got {stdout:?}"
    );

    let parsed: serde_json::Value =
        serde_json::from_str(lines[0].trim_end()).expect("valid JSONL record");
    assert_eq!(parsed["text"], serde_json::json!("jsonl value"));
    assert_eq!(parsed["preview"], serde_json::json!("jsonl value"));
    assert_eq!(parsed["sensitivity"], serde_json::json!("Public"));
    assert_eq!(parsed["pinned"], serde_json::json!(false));
}

#[test]
fn daemon_stop_without_ipc_errors_with_invalid_usage() {
    let (_dir, db) = temp_db();
    let mut cmd = nagori(&db);
    cmd.args(["daemon", "stop"]);
    cmd.assert()
        .failure()
        .stderr(predicate::str::contains("daemon stop requires"));
}

#[test]
fn write_with_db_is_refused_while_an_instance_owns_the_store() {
    // Holding the single-instance lock stands in for a running desktop app
    // or daemon. A direct write underneath the owner would land in SQLite
    // without ever invalidating its in-memory caches, so the CLI must
    // refuse rather than desync it.
    let (dir, db) = temp_db();
    let _owner = nagori_storage::ProcessLock::try_acquire(dir.path())
        .expect("lock io")
        .expect("lock should be free");

    let output = nagori(&db)
        .args(["add", "--text", "should not land"])
        .output()
        .expect("invoke add");
    assert!(
        !output.status.success(),
        "a direct write must be refused while the lock is held",
    );
    let stderr = String::from_utf8(output.stderr).expect("stderr utf-8");
    assert!(
        stderr.contains("owns"),
        "the refusal should name the owning instance: {stderr}"
    );
}

#[test]
fn read_with_db_is_allowed_while_an_instance_owns_the_store() {
    // Reads tolerate a concurrent owner (SQLite WAL); only writes are
    // gated on the lock.
    let (dir, db) = temp_db();
    let _ = nagori(&db)
        .args(["add", "--text", "seeded before lock"])
        .output()
        .expect("invoke add");
    let _owner = nagori_storage::ProcessLock::try_acquire(dir.path())
        .expect("lock io")
        .expect("lock should be free");

    let output = nagori(&db).arg("list").output().expect("invoke list");
    assert!(
        output.status.success(),
        "reads must stay lock-free: {:?}",
        output.status,
    );
    assert!(stdout_string(&output).contains("seeded before lock"));
}

/// Write commands without `--db` must fall back to a direct write when no
/// instance is running (nothing to desync) instead of erroring. The fake
/// `HOME` / `XDG_DATA_HOME` isolate both the default DB and the default
/// IPC endpoint inside the tempdir, so the probe can never reach a real
/// nagori on the development machine.
#[cfg(unix)]
#[test]
fn write_without_db_falls_back_to_direct_write_when_nothing_runs() {
    let home = tempfile::tempdir().expect("tempdir");
    let isolated = |args: &[&str]| {
        let mut cmd = Command::cargo_bin("nagori").expect("nagori binary");
        cmd.env("HOME", home.path());
        cmd.env("XDG_DATA_HOME", home.path().join(".local/share"));
        cmd.env_remove("NAGORI_DB_PATH");
        cmd.args(args);
        cmd
    };

    let output = isolated(&["add", "--text", "fallback write"])
        .output()
        .expect("invoke add");
    assert!(
        output.status.success(),
        "with nothing running, a write should fall back to the local DB: {}",
        String::from_utf8_lossy(&output.stderr),
    );

    let list = isolated(&["list"]).output().expect("invoke list");
    assert!(list.status.success(), "exit: {:?}", list.status);
    assert!(
        stdout_string(&list).contains("fallback write"),
        "the fallback write must land in the default DB",
    );
}

/// A `--db` pointing at the real store through a file symlink must contend
/// for the real directory's lock, not the symlink's parent — otherwise an
/// alias path writes underneath a running instance.
#[cfg(unix)]
#[test]
fn write_through_symlinked_db_is_refused_while_the_real_store_is_owned() {
    let (real_dir, real_db) = temp_db();
    // The DB file must exist for the alias to resolve.
    let seeded = nagori(&real_db)
        .args(["add", "--text", "seed"])
        .output()
        .expect("invoke add");
    assert!(seeded.status.success(), "seeding the real DB should work");
    let _owner = nagori_storage::ProcessLock::try_acquire(real_dir.path())
        .expect("lock io")
        .expect("lock should be free");

    let alias_dir = tempfile::tempdir().expect("tempdir");
    let alias_db = alias_dir.path().join("alias.sqlite");
    std::os::unix::fs::symlink(&real_db, &alias_db).expect("create symlink");

    let output = nagori(&alias_db)
        .args(["add", "--text", "via alias"])
        .output()
        .expect("invoke add");
    assert!(
        !output.status.success(),
        "an aliased write must contend for the real directory's lock",
    );
    let stderr = String::from_utf8(output.stderr).expect("stderr utf-8");
    assert!(
        stderr.contains("owns"),
        "the refusal should name the owning instance: {stderr}"
    );
}

/// With an instance owning the default store but no reachable endpoint
/// (`cli_ipc_enabled` off), a write without `--db` must fail with the
/// Settings hint instead of silently writing underneath the owner.
#[cfg(unix)]
#[test]
fn write_without_db_is_refused_with_hint_when_owner_has_no_endpoint() {
    let home = tempfile::tempdir().expect("tempdir");
    #[cfg(target_os = "macos")]
    let data_dir = home.path().join("Library/Application Support/nagori");
    #[cfg(not(target_os = "macos"))]
    let data_dir = home.path().join(".local/share/nagori");
    std::fs::create_dir_all(&data_dir).expect("create data dir");
    let _owner = nagori_storage::ProcessLock::try_acquire(&data_dir)
        .expect("lock io")
        .expect("lock should be free");

    let mut cmd = Command::cargo_bin("nagori").expect("nagori binary");
    cmd.env("HOME", home.path());
    cmd.env("XDG_DATA_HOME", home.path().join(".local/share"));
    cmd.env_remove("NAGORI_DB_PATH");
    cmd.args(["add", "--text", "should not land"]);
    let output = cmd.output().expect("invoke add");
    assert!(
        !output.status.success(),
        "a write must not bypass an owner whose endpoint is unreachable",
    );
    let stderr = String::from_utf8(output.stderr).expect("stderr utf-8");
    assert!(
        stderr.contains("cli_ipc_enabled"),
        "the failure should hint at the Settings toggle: {stderr}"
    );
}

#[test]
fn add_then_get_json_shape_snapshot() {
    // Pins the `--json get` payload shape: a downstream script reading
    // `nagori get <id> --json` must keep seeing these fields. The id and
    // timestamps are redacted so the snapshot stays deterministic.
    let (_dir, db) = temp_db();
    let add = nagori(&db)
        .args(["--json", "add", "--text", "json get value"])
        .output()
        .expect("invoke add");
    assert!(add.status.success(), "exit: {:?}", add.status);
    let added: serde_json::Value =
        serde_json::from_str(&stdout_string(&add)).expect("add --json emits one JSON document");
    let id = added["id"].as_str().expect("add output carries the id");

    let get = nagori(&db)
        .args(["--json", "get", id])
        .output()
        .expect("invoke get");
    assert!(get.status.success(), "exit: {:?}", get.status);
    insta::assert_snapshot!(redact(&stdout_string(&get)), @r#"{
  "created_at": "[ts]",
  "id": "[id]",
  "kind": "Text",
  "last_used_at": null,
  "pinned": false,
  "preview": "json get value",
  "sensitivity": "Public",
  "text": "json get value",
  "updated_at": "[ts]",
  "use_count": 0
}
"#);
}

#[test]
fn search_jsonl_emits_one_parseable_record_per_line() {
    let (_dir, db) = temp_db();
    let _ = nagori(&db)
        .args(["add", "--text", "searchable jsonl payload"])
        .output()
        .expect("invoke add");

    let search = nagori(&db)
        .args(["--jsonl", "search", "searchable"])
        .output()
        .expect("invoke search");
    assert!(search.status.success(), "exit: {:?}", search.status);
    let stdout = stdout_string(&search);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 1, "one hit → one JSONL line, got {stdout:?}");
    let record: serde_json::Value = serde_json::from_str(lines[0]).expect("valid JSONL record");
    assert_eq!(
        record["preview"],
        serde_json::json!("searchable jsonl payload")
    );
    assert_eq!(record["kind"], serde_json::json!("Text"));
    assert_eq!(record["pinned"], serde_json::json!(false));
    assert!(record["score"].is_number(), "score must be numeric");
    assert!(
        record["rank_reasons"].is_array() || record["rank_reasons"].is_null(),
        "rank_reasons shape drifted: {record}"
    );
}

#[test]
fn quick_action_json_shape_snapshot() {
    // `quick` shares `print_ai_output` with `nagori ai --no-stream`; pinning
    // the deterministic quick-action output also pins the AI output DTO
    // shape (`text` / `created_entry` / `warnings`) without needing a model.
    let (_dir, db) = temp_db();
    let add = nagori(&db)
        .args(["--json", "add", "--text", "{\"b\":2,\"a\":1}"])
        .output()
        .expect("invoke add");
    assert!(add.status.success(), "exit: {:?}", add.status);
    let added: serde_json::Value =
        serde_json::from_str(&stdout_string(&add)).expect("add --json emits one JSON document");
    let id = added["id"].as_str().expect("add output carries the id");

    let quick = nagori(&db)
        .args(["--json", "quick", "format-json", id])
        .output()
        .expect("invoke quick");
    assert!(quick.status.success(), "exit: {:?}", quick.status);
    insta::assert_snapshot!(redact(&stdout_string(&quick)), @r#"{
  "text": "{\n  \"a\": 1,\n  \"b\": 2\n}",
  "created_entry": null,
  "warnings": []
}
"#);

    let quick_jsonl = nagori(&db)
        .args(["--jsonl", "quick", "format-json", id])
        .output()
        .expect("invoke quick");
    assert!(quick_jsonl.status.success());
    let stdout = stdout_string(&quick_jsonl);
    assert_eq!(
        stdout.lines().count(),
        1,
        "--jsonl must emit exactly one line, got {stdout:?}"
    );
}

#[test]
fn add_stdin_oversized_exits_with_invalid_input_code() {
    // The CLI-side stdin bound: one byte over `MAX_ENTRY_SIZE_BYTES` must be
    // rejected as invalid input (exit 2), not OOM the process or surface as
    // an internal error.
    let (_dir, db) = temp_db();
    let oversized = "a".repeat(nagori_core::MAX_ENTRY_SIZE_BYTES + 1);
    let mut cmd = assert_cmd::Command::cargo_bin("nagori").expect("nagori binary");
    cmd.arg("--db")
        .arg(&db)
        .args(["add", "--stdin"])
        .write_stdin(oversized);
    let assert = cmd.assert().failure().code(2);
    let output = assert.get_output();
    let stderr = String::from_utf8(output.stderr.clone()).expect("stderr utf-8");
    assert!(
        stderr.contains("maximum entry size"),
        "stderr should explain the size cap: {stderr:?}"
    );
}

#[test]
fn capabilities_jsonl_is_a_single_line_record() {
    // docs/cli.md: `--jsonl` is one record per line. The capability matrix
    // is a single record, so the output must be exactly one parseable line.
    let (_dir, db) = temp_db();
    let output = nagori(&db)
        .args(["--jsonl", "capabilities"])
        .output()
        .expect("invoke capabilities");
    assert!(output.status.success(), "exit: {:?}", output.status);
    let stdout = stdout_string(&output);
    assert_eq!(
        stdout.lines().count(),
        1,
        "--jsonl capabilities must be one line, got {stdout:?}"
    );
    let record: serde_json::Value =
        serde_json::from_str(stdout.trim_end()).expect("valid JSON record");
    assert!(record["platform"].is_string(), "shape drifted: {record}");
}

#[test]
fn doctor_jsonl_is_a_single_line_record() {
    // Regression: the doctor report used to be pretty-printed (multi-line)
    // under `--jsonl`, and the local arm ignored `--json` entirely.
    let (_dir, db) = temp_db();
    let output = nagori(&db)
        .args(["--jsonl", "doctor"])
        .output()
        .expect("invoke doctor");
    assert!(output.status.success(), "exit: {:?}", output.status);
    let stdout = stdout_string(&output);
    assert_eq!(
        stdout.lines().count(),
        1,
        "--jsonl doctor must be one line, got {stdout:?}"
    );
    let record: serde_json::Value =
        serde_json::from_str(stdout.trim_end()).expect("valid JSON record");
    assert_eq!(
        record["version"],
        serde_json::json!(env!("CARGO_PKG_VERSION"))
    );
    assert!(record["permissions"].is_array(), "shape drifted: {record}");
    // The local arm must emit the same schema as a daemon-served report: a
    // consumer deserializes either into `DoctorReport` without branching on
    // which transport answered.
    let _typed: nagori_ipc::DoctorReport =
        serde_json::from_str(stdout.trim_end()).expect("local doctor JSON matches DoctorReport");
}

#[test]
fn daemon_status_local_does_not_claim_ok() {
    // Without `--ipc` / `--auto-ipc` the status command reads the local DB
    // and never probes a daemon — so it must not print `ok`, which reads as
    // "the daemon is healthy" even when nothing is running.
    let (_dir, db) = temp_db();
    let text = nagori(&db)
        .args(["daemon", "status"])
        .output()
        .expect("invoke daemon status");
    assert!(text.status.success(), "exit: {:?}", text.status);
    let stdout = stdout_string(&text);
    assert!(
        stdout.starts_with("local (daemon not probed)\t"),
        "local status must name its source, got {stdout:?}"
    );

    let jsonl = nagori(&db)
        .args(["--jsonl", "daemon", "status"])
        .output()
        .expect("invoke daemon status");
    assert!(jsonl.status.success());
    let stdout = stdout_string(&jsonl);
    assert_eq!(
        stdout.lines().count(),
        1,
        "--jsonl daemon status must be one line, got {stdout:?}"
    );
    let record: serde_json::Value =
        serde_json::from_str(stdout.trim_end()).expect("valid JSON record");
    assert_eq!(record["source"], serde_json::json!("local"));
    assert_eq!(record["daemon_probed"], serde_json::json!(false));
    assert!(
        record.get("ok").is_none(),
        "the unprobed arm must not claim ok: {record}"
    );
}

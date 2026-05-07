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

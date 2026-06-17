use super::super::*;
use super::loop_for;

use nagori_core::settings::SecretHandling;
use nagori_platform::{ClipboardWriter, MemoryClipboard};
use nagori_storage::SqliteStore;

#[tokio::test]
async fn capture_once_skips_when_capture_disabled() {
    let clipboard = Arc::new(MemoryClipboard::new());
    let store = SqliteStore::open_memory().expect("memory store");
    let settings = AppSettings {
        capture_enabled: false,
        ..AppSettings::default()
    };
    let mut loop_ = loop_for(clipboard.clone(), store.clone(), settings);
    clipboard
        .write_text("ignored value")
        .await
        .expect("clipboard write");

    assert!(loop_.capture_once().await.unwrap().is_none());
    assert!(store.list_recent(10).await.unwrap().is_empty());
}

#[tokio::test]
async fn capture_once_skips_disabled_content_kind_before_classification() {
    let clipboard = Arc::new(MemoryClipboard::new());
    let store = SqliteStore::open_memory().expect("memory store");
    let settings = AppSettings {
        capture_kinds: std::iter::once(nagori_core::ContentKind::Image).collect(),
        ..AppSettings::default()
    };
    let health = CaptureHealth::new();
    let mut loop_ =
        loop_for(clipboard.clone(), store.clone(), settings).with_capture_health(health.clone());
    clipboard
        .write_text("plain text should be ignored")
        .await
        .expect("clipboard write");

    assert!(loop_.capture_once().await.unwrap().is_none());
    assert!(store.list_recent(10).await.unwrap().is_empty());
    // The kind filter is part of capture *policy*, not an error
    // condition. The drop must land as `Policy` so the doctor /
    // tray hint matches `entry_blocked` / `secret_blocked` rather
    // than silently disappearing.
    let report = health.report();
    assert_eq!(report.consecutive_failures, 0);
    assert_eq!(
        report.last_event_category,
        Some(nagori_ipc::CaptureEventCategory::Policy)
    );
}

#[tokio::test]
async fn capture_once_skips_oversized_blocked_text() {
    let clipboard = Arc::new(MemoryClipboard::new());
    let store = SqliteStore::open_memory().expect("memory store");
    // Drop max_entry_size_bytes so any short clip is classified as
    // oversized and the capture loop must skip insertion.
    let settings = AppSettings {
        max_entry_size_bytes: 4,
        ..AppSettings::default()
    };
    let mut loop_ = loop_for(clipboard.clone(), store.clone(), settings);
    clipboard
        .write_text("this is too long for the policy")
        .await
        .expect("clipboard write");

    assert!(loop_.capture_once().await.unwrap().is_none());
    assert!(store.list_recent(10).await.unwrap().is_empty());
}

#[tokio::test]
async fn capture_once_drops_user_regex_match() {
    // Regex_denylist UI promises "Captures matching any pattern are
    // dropped" — so a UserRegex-matched clip must never reach SQLite,
    // not even with a redacted body. Regression for the original
    // behaviour where UserRegex classified as Private and the raw
    // text was persisted as `entry.content`.
    let clipboard = Arc::new(MemoryClipboard::new());
    let store = SqliteStore::open_memory().expect("memory store");
    let settings = AppSettings {
        regex_denylist: vec![r"INTERNAL-\d+".to_owned()],
        ..AppSettings::default()
    };
    let mut loop_ = loop_for(clipboard.clone(), store.clone(), settings);
    clipboard
        .write_text("ticket INTERNAL-123 must stay local")
        .await
        .expect("clipboard write");

    assert!(loop_.capture_once().await.unwrap().is_none());
    assert!(store.list_recent(10).await.unwrap().is_empty());
}

#[tokio::test]
async fn capture_once_blocks_secret_when_handling_is_block() {
    // SecretHandling::Block must drop Secret-tagged content entirely
    // (not just redact it). Regression for the original behaviour where
    // the secret_handling setting was ignored and every Secret payload
    // was persisted verbatim.
    let clipboard = Arc::new(MemoryClipboard::new());
    let store = SqliteStore::open_memory().expect("memory store");
    let settings = AppSettings {
        secret_handling: SecretHandling::Block,
        ..AppSettings::default()
    };
    let mut loop_ = loop_for(clipboard.clone(), store.clone(), settings);
    clipboard
        .write_text("token = ghp_abcdefghijklmnopqrstuvwxyz123456")
        .await
        .expect("clipboard write");

    assert!(loop_.capture_once().await.unwrap().is_none());
    assert!(store.list_recent(10).await.unwrap().is_empty());
}

#[tokio::test]
async fn capture_once_blocks_secret_when_sensitive_capture_block_is_enabled() {
    let clipboard = Arc::new(MemoryClipboard::new());
    let store = SqliteStore::open_memory().expect("memory store");
    let settings = AppSettings {
        block_sensitive_captures: true,
        secret_handling: SecretHandling::StoreFull,
        ..AppSettings::default()
    };
    let mut loop_ = loop_for(clipboard.clone(), store.clone(), settings);
    clipboard
        .write_text("token = ghp_abcdefghijklmnopqrstuvwxyz123456")
        .await
        .expect("clipboard write");

    assert!(loop_.capture_once().await.unwrap().is_none());
    assert!(store.list_recent(10).await.unwrap().is_empty());
}

#[tokio::test]
async fn capture_once_redacts_secret_by_default() {
    // The default SecretHandling::StoreRedacted has to land a row whose
    // durable body is the redacted form. An exported DB must never
    // expose the raw token.
    let clipboard = Arc::new(MemoryClipboard::new());
    let store = SqliteStore::open_memory().expect("memory store");
    let mut loop_ = loop_for(clipboard.clone(), store.clone(), AppSettings::default());
    clipboard
        .write_text("token = ghp_abcdefghijklmnopqrstuvwxyz123456")
        .await
        .expect("clipboard write");

    let id = loop_
        .capture_once()
        .await
        .unwrap()
        .expect("redacted secret should be persisted");
    let stored = store.get(id).await.unwrap().expect("stored row");
    assert_eq!(stored.sensitivity, Sensitivity::Secret);
    let body = stored.plain_text().expect("body").to_owned();
    assert!(
        !body.contains("ghp_abcdefghijklmnopqrstuvwxyz123456"),
        "default secret_handling must not store the raw token: {body:?}",
    );
    assert!(body.contains("[REDACTED]"));
}

#[tokio::test]
async fn capture_once_keeps_secret_full_when_opted_in() {
    let clipboard = Arc::new(MemoryClipboard::new());
    let store = SqliteStore::open_memory().expect("memory store");
    let settings = AppSettings {
        secret_handling: SecretHandling::StoreFull,
        ..AppSettings::default()
    };
    let mut loop_ = loop_for(clipboard.clone(), store.clone(), settings);
    clipboard
        .write_text("token = ghp_abcdefghijklmnopqrstuvwxyz123456")
        .await
        .expect("clipboard write");

    let id = loop_.capture_once().await.unwrap().expect("entry id");
    let stored = store.get(id).await.unwrap().expect("stored row");
    assert_eq!(
        stored.plain_text(),
        Some("token = ghp_abcdefghijklmnopqrstuvwxyz123456"),
    );
    // Even with the raw body retained, the search preview must still be
    // the redacted form so UI surfaces never leak the secret.
    assert!(stored.search.preview.contains("[REDACTED]"));
}

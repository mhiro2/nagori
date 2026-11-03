use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use nagori_core::{
    AppError, Bytes, RecognizedLine, RecognizedText, SearchQuery, Sensitivity, TextBounds,
    TextRecognizer, normalize_text,
};
use nagori_storage::SqliteStore;

use super::*;

/// Recognizer that "reads" a fixed set of lines and counts its calls.
struct FixedRecognizer {
    lines: Vec<&'static str>,
    calls: AtomicUsize,
}

#[async_trait]
impl TextRecognizer for FixedRecognizer {
    async fn recognize(&self, _image: Bytes) -> Result<RecognizedText> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(RecognizedText {
            lines: self
                .lines
                .iter()
                .map(|text| RecognizedLine {
                    text: (*text).to_owned(),
                    bounds: TextBounds {
                        x: 0.0,
                        y: 0.0,
                        width: 1.0,
                        height: 0.1,
                    },
                })
                .collect(),
        })
    }
}

fn runtime_with_recognizer(lines: Vec<&'static str>) -> (NagoriRuntime, Arc<FixedRecognizer>) {
    let recognizer = Arc::new(FixedRecognizer {
        lines,
        calls: AtomicUsize::new(0),
    });
    let runtime = NagoriRuntime::builder(SqliteStore::open_memory().expect("memory store"))
        .text_recognizer(recognizer.clone())
        .build_for_test();
    (runtime, recognizer)
}

async fn enable_image_text_index(runtime: &NagoriRuntime, enabled: bool) {
    let mut settings = runtime.current_settings();
    settings.ai.image_text_index_enabled = enabled;
    runtime
        .save_settings(settings)
        .await
        .expect("save settings");
}

async fn search(runtime: &NagoriRuntime, query: &str) -> Vec<nagori_core::SearchResult> {
    runtime
        .search(SearchQuery::new(query, normalize_text(query), 10))
        .await
        .expect("search")
}

#[tokio::test]
async fn extract_text_reads_an_image_and_reports_the_capability() {
    let (runtime, recognizer) = runtime_with_recognizer(vec!["error: connection refused"]);
    assert!(matches!(
        runtime.capabilities().image_text_recognition,
        nagori_platform::Capability::Available
    ));
    let id = add_image_entry(&runtime).await;
    let extracted = runtime.recognize_entry_text(id).await.expect("recognize");
    assert_eq!(extracted.text.text(), "error: connection refused");
    assert!(!extracted.masked);
    // Image text search is off: the result is shown, not indexed.
    assert!(search(&runtime, "refused").await.is_empty());

    let text_entry = runtime.add_text("plain".to_owned()).await.expect("seed");
    assert!(matches!(
        runtime.recognize_entry_text(text_entry).await,
        Err(AppError::InvalidInput(_))
    ));
    assert_eq!(recognizer.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn extract_text_is_unsupported_without_a_recognizer() {
    let (runtime, _) = runtime_with_memory_clipboard();
    assert!(matches!(
        runtime.capabilities().image_text_recognition,
        nagori_platform::Capability::Unsupported { .. }
    ));
    let id = add_image_entry(&runtime).await;
    assert!(matches!(
        runtime.recognize_entry_text(id).await,
        Err(AppError::Unsupported(_))
    ));
}

#[tokio::test]
async fn secret_text_is_redacted_and_masks_the_image() {
    let (runtime, _) =
        runtime_with_recognizer(vec!["token ghp_aBcDeFgHiJkLmNoPqRsTuVwXyZ0123456789"]);
    enable_image_text_index(&runtime, true).await;
    let id = add_image_entry(&runtime).await;
    let extracted = runtime.recognize_entry_text(id).await.expect("recognize");
    let text = extracted.text.text();
    assert!(!text.contains("ghp_"), "{text}");
    assert!(extracted.masked);
    let entry = runtime.get_entry(id).await.expect("get").expect("entry");
    assert_eq!(entry.sensitivity, Sensitivity::Secret);
    // A masked image is not read again.
    assert!(matches!(
        runtime.recognize_entry_text(id).await,
        Err(AppError::InvalidInput(_))
    ));
}

#[tokio::test]
async fn background_indexer_reads_images_and_erases_the_text_when_turned_off() {
    let (runtime, recognizer) = runtime_with_recognizer(vec!["quarterly report draft"]);
    let id = add_image_entry(&runtime).await;
    enable_image_text_index(&runtime, true).await;

    let worker = tokio::spawn(
        runtime
            .clone()
            .run_text_recognition_indexer(runtime.shutdown_handle()),
    );
    let found = async {
        loop {
            if search(&runtime, "quarterly")
                .await
                .first()
                .map(|r| r.entry_id)
                == Some(id)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    };
    tokio::time::timeout(Duration::from_secs(10), found)
        .await
        .expect("the indexer reads the image");
    // An already-read image is answered from the store.
    runtime.recognize_entry_text(id).await.expect("recognize");
    assert_eq!(recognizer.calls.load(Ordering::SeqCst), 1);

    enable_image_text_index(&runtime, false).await;
    let cleared = async {
        while !search(&runtime, "quarterly").await.is_empty() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    };
    tokio::time::timeout(Duration::from_secs(10), cleared)
        .await
        .expect("turning the setting off erases the text");
    runtime.shutdown_handle().cancel();
    let _ = worker.await;
}

#[tokio::test]
async fn secret_text_masks_the_image_even_with_image_text_search_off() {
    let (runtime, _) =
        runtime_with_recognizer(vec!["token ghp_aBcDeFgHiJkLmNoPqRsTuVwXyZ0123456789"]);
    let id = add_image_entry(&runtime).await;
    let extracted = runtime.recognize_entry_text(id).await.expect("recognize");
    assert!(extracted.masked);
    let entry = runtime.get_entry(id).await.expect("get").expect("entry");
    assert_eq!(entry.sensitivity, Sensitivity::Secret);
    // The text itself was not stored while the setting is off.
    assert!(search(&runtime, "token").await.is_empty());
}

/// Recognizer that waits for a release before answering, to land a result
/// after the setting changed.
struct GatedRecognizer {
    release: tokio::sync::Notify,
}

#[async_trait]
impl TextRecognizer for GatedRecognizer {
    async fn recognize(&self, _image: Bytes) -> Result<RecognizedText> {
        self.release.notified().await;
        Ok(RecognizedText {
            lines: vec![RecognizedLine {
                text: "late result".to_owned(),
                bounds: TextBounds {
                    x: 0.0,
                    y: 0.0,
                    width: 1.0,
                    height: 0.1,
                },
            }],
        })
    }
}

#[tokio::test]
async fn a_read_finishing_after_the_setting_is_turned_off_is_not_stored() {
    let recognizer = Arc::new(GatedRecognizer {
        release: tokio::sync::Notify::new(),
    });
    let runtime = NagoriRuntime::builder(SqliteStore::open_memory().expect("memory store"))
        .text_recognizer(recognizer.clone())
        .build_for_test();
    enable_image_text_index(&runtime, true).await;
    let id = add_image_entry(&runtime).await;
    let extract = tokio::spawn({
        let runtime = runtime.clone();
        async move { runtime.recognize_entry_text(id).await }
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    enable_image_text_index(&runtime, false).await;
    recognizer.release.notify_one();
    let extracted = extract.await.expect("join").expect("recognize");
    assert_eq!(extracted.text.text(), "late result");
    assert!(search(&runtime, "late").await.is_empty());
    assert!(!runtime.store.has_text_recognition().await.expect("probe"));
}

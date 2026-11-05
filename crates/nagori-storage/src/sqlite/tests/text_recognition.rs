use nagori_core::{
    ClipboardContent, EntryFactory, EntryId, EntryRepository, ImageContent, RecognizedLine,
    RecognizedText, SearchQuery, Sensitivity, TextBounds,
};
use nagori_search::normalize_text;

use super::super::*;

const POLICY: &str = "policy-a";

async fn insert_image(store: &SqliteStore, seed: u8) -> EntryId {
    let content = ClipboardContent::Image(ImageContent {
        width: Some(800),
        height: Some(600),
        byte_count: 4,
        mime_type: Some("image/png".to_owned()),
        pending_bytes: Some(vec![seed, 1, 2, 3].into()),
    });
    store
        .insert(EntryFactory::from_content(content, None, None))
        .await
        .unwrap()
}

fn recognized(lines: &[&str]) -> RecognizedText {
    RecognizedText {
        lines: lines
            .iter()
            .enumerate()
            .map(|(i, text)| RecognizedLine {
                text: (*text).to_owned(),
                bounds: TextBounds {
                    x: 0.1,
                    #[allow(clippy::cast_precision_loss)]
                    y: 0.1 * i as f32,
                    width: 0.5,
                    height: 0.05,
                },
            })
            .collect(),
    }
}

async fn search(store: &SqliteStore, query: &str) -> Vec<nagori_core::SearchResult> {
    store
        .search(SearchQuery::new(query, normalize_text(query), 10))
        .await
        .unwrap()
}

#[tokio::test]
async fn recognized_text_makes_an_image_searchable_with_an_excerpt() {
    let store = SqliteStore::open_memory().unwrap();
    let id = insert_image(&store, 0).await;
    let pending = store
        .text_recognition_pending(10, POLICY.to_owned())
        .await
        .unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].entry_id, id);
    assert!(search(&store, "refused").await.is_empty());

    let text = recognized(&["Terminal", "error: connection refused (os error 61)"]);
    assert!(
        store
            .record_text_recognition(
                id,
                pending[0].content_hash.clone(),
                POLICY.to_owned(),
                TextRecognitionOutcome::Recognized {
                    text: text.clone(),
                    raise_to: None,
                },
            )
            .await
            .unwrap()
    );
    assert!(
        store
            .text_recognition_pending(10, POLICY.to_owned())
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        store
            .get_text_recognition(id, POLICY.to_owned())
            .await
            .unwrap(),
        Some(text)
    );

    let results = search(&store, "connection refused").await;
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].entry_id, id);
    assert!(
        results[0].preview.contains("connection refused"),
        "{}",
        results[0].preview
    );
    // CJK-free recall also works through the full-text branch for a word
    // that sits mid-line.
    assert_eq!(search(&store, "terminal").await[0].entry_id, id);
}

#[tokio::test]
async fn a_raised_sensitivity_sticks_and_hides_the_text_from_excerpts() {
    let store = SqliteStore::open_memory().unwrap();
    let id = insert_image(&store, 1).await;
    let hash = store
        .text_recognition_pending(10, POLICY.to_owned())
        .await
        .unwrap()[0]
        .content_hash
        .clone();
    store
        .record_text_recognition(
            id,
            hash,
            POLICY.to_owned(),
            TextRecognitionOutcome::Recognized {
                text: recognized(&["token [REDACTED]"]),
                raise_to: Some(Sensitivity::Secret),
            },
        )
        .await
        .unwrap();
    let entry = store.get(id).await.unwrap().unwrap();
    assert_eq!(entry.sensitivity, Sensitivity::Secret);
    let results = search(&store, "token").await;
    assert_eq!(results[0].entry_id, id);
    assert!(
        !results[0].preview.contains("token"),
        "masked rows keep their stored preview: {}",
        results[0].preview
    );
}

#[tokio::test]
async fn stale_or_masked_outcomes_are_dropped() {
    let store = SqliteStore::open_memory().unwrap();
    let id = insert_image(&store, 2).await;
    let outcome = || TextRecognitionOutcome::Recognized {
        text: recognized(&["hello"]),
        raise_to: None,
    };
    assert!(
        !store
            .record_text_recognition(id, "other-bytes".to_owned(), POLICY.to_owned(), outcome())
            .await
            .unwrap()
    );
    let hash = store
        .text_recognition_pending(10, POLICY.to_owned())
        .await
        .unwrap()[0]
        .content_hash
        .clone();
    store
        .conn()
        .unwrap()
        .execute(
            "UPDATE entries SET sensitivity = 'private' WHERE id = ?1",
            [id.to_string()],
        )
        .unwrap();
    assert!(
        store
            .text_recognition_pending(10, POLICY.to_owned())
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        !store
            .record_text_recognition(id, hash, POLICY.to_owned(), outcome())
            .await
            .unwrap()
    );
    assert!(search(&store, "hello").await.is_empty());
}

#[tokio::test]
async fn failures_are_not_retried_and_clearing_unindexes_the_text() {
    let store = SqliteStore::open_memory().unwrap();
    let read = insert_image(&store, 3).await;
    let unreadable = insert_image(&store, 4).await;
    for pending in store
        .text_recognition_pending(10, POLICY.to_owned())
        .await
        .unwrap()
    {
        let outcome = if pending.entry_id == read {
            TextRecognitionOutcome::Recognized {
                text: recognized(&["quarterly report"]),
                raise_to: None,
            }
        } else {
            TextRecognitionOutcome::Failed
        };
        store
            .record_text_recognition(
                pending.entry_id,
                pending.content_hash,
                POLICY.to_owned(),
                outcome,
            )
            .await
            .unwrap();
    }
    assert!(
        store
            .text_recognition_pending(10, POLICY.to_owned())
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        store
            .get_text_recognition(unreadable, POLICY.to_owned())
            .await
            .unwrap(),
        None
    );
    assert!(store.has_text_recognition().await.unwrap());
    assert_eq!(search(&store, "quarterly").await.len(), 1);

    // Both rows go, the failed one included.
    assert_eq!(store.clear_text_recognition(None).await.unwrap(), 2);
    assert!(!store.has_text_recognition().await.unwrap());
    assert!(search(&store, "quarterly").await.is_empty());
    assert_eq!(
        store
            .text_recognition_pending(10, POLICY.to_owned())
            .await
            .unwrap()
            .len(),
        2
    );
}

#[tokio::test]
async fn a_failed_duplicate_read_does_not_hide_indexed_text_and_policy_edits_reread() {
    let store = SqliteStore::open_memory().unwrap();
    let id = insert_image(&store, 5).await;
    let hash = store
        .text_recognition_pending(10, POLICY.to_owned())
        .await
        .unwrap()[0]
        .content_hash
        .clone();
    store
        .record_text_recognition(
            id,
            hash.clone(),
            POLICY.to_owned(),
            TextRecognitionOutcome::Recognized {
                text: recognized(&["invoice 2026"]),
                raise_to: None,
            },
        )
        .await
        .unwrap();
    // A slower read of the same image times out afterwards.
    assert!(
        !store
            .record_text_recognition(id, hash, POLICY.to_owned(), TextRecognitionOutcome::Failed)
            .await
            .unwrap()
    );
    assert!(
        store
            .get_text_recognition(id, POLICY.to_owned())
            .await
            .unwrap()
            .is_some()
    );

    // Under another policy the stored text is not served, the image is read
    // again, and clearing the stale rows un-indexes them.
    assert!(
        store
            .get_text_recognition(id, "policy-b".to_owned())
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        store
            .text_recognition_pending(10, "policy-b".to_owned())
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        store
            .clear_text_recognition(Some("policy-b".to_owned()))
            .await
            .unwrap(),
        1
    );
    assert!(search(&store, "invoice").await.is_empty());
}

#[tokio::test]
async fn a_masked_image_is_not_handed_out_for_reading_and_can_be_masked_alone() {
    let store = SqliteStore::open_memory().unwrap();
    let id = insert_image(&store, 6).await;
    let hash = store
        .text_recognition_pending(10, POLICY.to_owned())
        .await
        .unwrap()[0]
        .content_hash
        .clone();
    assert!(
        store
            .readable_image_payload(id, hash.clone())
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        store
            .readable_image_payload(id, "other".to_owned())
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .raise_image_sensitivity(id, hash.clone(), Sensitivity::Secret)
            .await
            .unwrap()
    );
    assert_eq!(
        store.get(id).await.unwrap().unwrap().sensitivity,
        Sensitivity::Secret
    );
    assert!(
        store
            .readable_image_payload(id, hash.clone())
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        !store
            .raise_image_sensitivity(id, hash, Sensitivity::Private)
            .await
            .unwrap()
    );
}

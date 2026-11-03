use nagori_core::{AppError, EntryLabel, SearchQuery, Sensitivity, normalize_text};

use super::*;

#[tokio::test]
async fn naming_an_entry_pins_it_and_makes_it_searchable() {
    let (runtime, _) = runtime_with_memory_clipboard();
    let id = runtime
        .add_text("123-4567 Tokyo, Chiyoda 1-1".to_owned())
        .await
        .expect("seed");
    let label = EntryLabel::new("配送先", &["shipping address".to_owned()]).expect("label");
    runtime
        .set_entry_label(id, Some(label.clone()))
        .await
        .expect("name");

    let entry = runtime.get_entry(id).await.expect("get").expect("entry");
    assert!(entry.lifecycle.pinned, "naming pins the entry");
    assert_eq!(
        runtime.get_entry_label(id).await.expect("label"),
        Some(label)
    );

    let results = runtime
        .search(SearchQuery::new("shipping", normalize_text("shipping"), 10))
        .await
        .expect("search");
    assert_eq!(results[0].entry_id, id);
    assert_eq!(results[0].label.as_deref(), Some("配送先"));

    // Clearing the name keeps the pin: unpinning stays a separate choice.
    runtime.set_entry_label(id, None).await.expect("clear");
    let entry = runtime.get_entry(id).await.expect("get").expect("entry");
    assert!(entry.lifecycle.pinned);
    assert_eq!(runtime.get_entry_label(id).await.expect("label"), None);
}

#[tokio::test]
async fn updating_entry_text_replaces_the_body_in_place() {
    let (runtime, _) = runtime_with_memory_clipboard();
    let id = runtime
        .add_text("SELECT * FROM orders WHERE id = 1".to_owned())
        .await
        .expect("seed");
    assert_eq!(
        runtime.editable_entry_text(id).await.expect("text"),
        "SELECT * FROM orders WHERE id = 1"
    );
    runtime
        .update_entry_text(id, "SELECT * FROM orders WHERE id = 42".to_owned())
        .await
        .expect("update");
    let entry = runtime.get_entry(id).await.expect("get").expect("entry");
    assert_eq!(
        entry.plain_text(),
        Some("SELECT * FROM orders WHERE id = 42")
    );

    // The new body runs through the same checks as a new entry.
    assert!(matches!(
        runtime.update_entry_text(id, String::new()).await,
        Err(AppError::InvalidInput(_))
    ));
}

#[tokio::test]
async fn updating_entry_text_classifies_the_new_body() {
    let (runtime, _) = runtime_with_memory_clipboard();
    let id = runtime
        .add_text("token goes here".to_owned())
        .await
        .expect("seed");
    runtime
        .update_entry_text(
            id,
            "token ghp_aBcDeFgHiJkLmNoPqRsTuVwXyZ0123456789".to_owned(),
        )
        .await
        .expect("update");
    let entry = runtime.get_entry(id).await.expect("get").expect("entry");
    assert_eq!(entry.sensitivity, Sensitivity::Secret);
    assert!(
        !entry.search.preview.contains("ghp_"),
        "{}",
        entry.search.preview
    );
}

#[tokio::test]
async fn masked_or_non_text_entries_cannot_be_edited() {
    let (runtime, _) = runtime_with_memory_clipboard();
    let secret = runtime
        .add_text("token ghp_aBcDeFgHiJkLmNoPqRsTuVwXyZ0123456789".to_owned())
        .await
        .expect("seed");
    assert!(matches!(
        runtime.update_entry_text(secret, "plain".to_owned()).await,
        Err(AppError::InvalidInput(_))
    ));
    assert!(matches!(
        runtime.editable_entry_text(secret).await,
        Err(AppError::InvalidInput(_))
    ));
    let image = add_image_entry(&runtime).await;
    assert!(matches!(
        runtime.update_entry_text(image, "plain".to_owned()).await,
        Err(AppError::InvalidInput(_))
    ));
    assert!(matches!(
        runtime
            .update_entry_text(EntryId::new(), "plain".to_owned())
            .await,
        Err(AppError::NotFound)
    ));
}

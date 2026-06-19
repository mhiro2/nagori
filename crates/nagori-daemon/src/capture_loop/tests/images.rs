use super::super::*;

use nagori_storage::SqliteStore;

#[tokio::test]
async fn capture_once_persists_image_clipboard_entries() {
    // The capture loop must keep image snapshots flowing through to the
    // store even though they have no plain text — otherwise the
    // README's "Captures text/URL/image" promise quietly turns into
    // text-only and image rows never reach search/preview.
    use std::sync::Mutex;

    use async_trait::async_trait;
    use nagori_core::{
        ClipboardData, ClipboardRepresentation, ClipboardSequence, ClipboardSnapshot, ContentHash,
    };
    use nagori_platform::ClipboardReader;
    use time::OffsetDateTime;

    struct ImageReader {
        bytes: Vec<u8>,
        mime: &'static str,
        // Pretend the user only just copied — read once then "stable" so
        // capture_once's sequence-dedup short-circuit does not fire on a
        // second tick within the same test.
        seq_called: Mutex<bool>,
    }

    #[async_trait]
    impl ClipboardReader for ImageReader {
        async fn current_snapshot(&self) -> Result<ClipboardSnapshot> {
            Ok(ClipboardSnapshot {
                sequence: ClipboardSequence::content_hash(ContentHash::sha256(&self.bytes).value),
                captured_at: OffsetDateTime::now_utc(),
                source: None,
                representations: vec![ClipboardRepresentation {
                    mime_type: self.mime.to_owned(),
                    data: ClipboardData::Bytes(self.bytes.clone()),
                }],
            })
        }

        async fn current_sequence(&self) -> Result<ClipboardSequence> {
            let mut guard = self.seq_called.lock().unwrap();
            let _ = &*guard;
            *guard = true;
            Ok(ClipboardSequence::content_hash(
                ContentHash::sha256(&self.bytes).value,
            ))
        }
    }

    let bytes = vec![137u8, 80, 78, 71, 13, 10, 26, 10, 1, 2, 3, 4];
    let reader = ImageReader {
        bytes: bytes.clone(),
        mime: "image/png",
        seq_called: Mutex::new(false),
    };
    let store = SqliteStore::open_memory().expect("memory store");
    let mut loop_ = CaptureLoop::new(reader, store.clone(), store.clone(), AppSettings::default());

    let id = loop_
        .capture_once()
        .await
        .unwrap()
        .expect("image entry should be inserted");
    let stored = store.get(id).await.unwrap().expect("row");
    match &stored.content {
        ClipboardContent::Image(img) => {
            assert_eq!(img.byte_count, bytes.len());
            assert_eq!(img.mime_type.as_deref(), Some("image/png"));
        }
        other => panic!("expected Image content, got {other:?}"),
    }
    let payload = store.get_payload(id).await.unwrap();
    assert_eq!(payload, Some((bytes, "image/png".to_owned())));
}

#[test]
fn probe_image_dimensions_reads_png_header_only() {
    use image::ImageEncoder as _;
    // A real, fully-encoded PNG so the header probe can read its IHDR.
    let img = image::RgbImage::new(7, 5);
    let mut bytes = Vec::new();
    image::codecs::png::PngEncoder::new(&mut bytes)
        .write_image(img.as_raw(), 7, 5, image::ExtendedColorType::Rgb8)
        .unwrap();
    assert_eq!(probe_image_dimensions(&bytes), Some((7, 5)));
}

#[test]
fn probe_image_dimensions_returns_none_for_unparseable_bytes() {
    // The truncated PNG magic used elsewhere in these tests has no
    // decodable header, so the probe declines and capture proceeds with
    // `None` dimensions rather than erroring.
    assert_eq!(
        probe_image_dimensions(&[137u8, 80, 78, 71, 13, 10, 26, 10, 1, 2, 3, 4]),
        None
    );
    assert_eq!(probe_image_dimensions(b"not an image"), None);
}

#[tokio::test]
async fn capture_once_records_image_dimensions_from_header() {
    // A valid PNG must land in the store with its pixel dimensions filled
    // in by the capture-time header probe (the factory leaves them `None`).
    use std::sync::Mutex;

    use async_trait::async_trait;
    use nagori_core::{
        ClipboardData, ClipboardRepresentation, ClipboardSequence, ClipboardSnapshot, ContentHash,
    };
    use nagori_platform::ClipboardReader;
    use time::OffsetDateTime;

    struct PngReader {
        bytes: Vec<u8>,
        seq_called: Mutex<bool>,
    }

    #[async_trait]
    impl ClipboardReader for PngReader {
        async fn current_snapshot(&self) -> Result<ClipboardSnapshot> {
            Ok(ClipboardSnapshot {
                sequence: ClipboardSequence::content_hash(ContentHash::sha256(&self.bytes).value),
                captured_at: OffsetDateTime::now_utc(),
                source: None,
                representations: vec![ClipboardRepresentation {
                    mime_type: "image/png".to_owned(),
                    data: ClipboardData::Bytes(self.bytes.clone()),
                }],
            })
        }

        async fn current_sequence(&self) -> Result<ClipboardSequence> {
            let mut guard = self.seq_called.lock().unwrap();
            let _ = &*guard;
            *guard = true;
            Ok(ClipboardSequence::content_hash(
                ContentHash::sha256(&self.bytes).value,
            ))
        }
    }

    use image::ImageEncoder as _;
    let img = image::RgbImage::new(24, 16);
    let mut bytes = Vec::new();
    image::codecs::png::PngEncoder::new(&mut bytes)
        .write_image(img.as_raw(), 24, 16, image::ExtendedColorType::Rgb8)
        .unwrap();

    let reader = PngReader {
        bytes,
        seq_called: Mutex::new(false),
    };
    let store = SqliteStore::open_memory().expect("memory store");
    let mut loop_ = CaptureLoop::new(reader, store.clone(), store.clone(), AppSettings::default());

    let id = loop_
        .capture_once()
        .await
        .unwrap()
        .expect("image entry should be inserted");
    let stored = store.get(id).await.unwrap().expect("row");
    match &stored.content {
        ClipboardContent::Image(img) => {
            assert_eq!(img.width, Some(24));
            assert_eq!(img.height, Some(16));
        }
        other => panic!("expected Image content, got {other:?}"),
    }
}

#[tokio::test]
async fn capture_once_skips_oversized_image_payloads() {
    // The size guard must be denominated in image byte_count for image
    // snapshots, and measured against the *image* budget
    // (`max_image_entry_size_bytes`) rather than the text budget — an image
    // over its own budget is still dropped.
    use std::sync::Mutex;

    use async_trait::async_trait;
    use nagori_core::{
        ClipboardData, ClipboardRepresentation, ClipboardSequence, ClipboardSnapshot, ContentHash,
    };
    use nagori_platform::ClipboardReader;
    use time::OffsetDateTime;

    struct ImageReader {
        bytes: Vec<u8>,
        seq_called: Mutex<bool>,
    }

    #[async_trait]
    impl ClipboardReader for ImageReader {
        async fn current_snapshot(&self) -> Result<ClipboardSnapshot> {
            Ok(ClipboardSnapshot {
                sequence: ClipboardSequence::content_hash(ContentHash::sha256(&self.bytes).value),
                captured_at: OffsetDateTime::now_utc(),
                source: None,
                representations: vec![ClipboardRepresentation {
                    mime_type: "image/png".to_owned(),
                    data: ClipboardData::Bytes(self.bytes.clone()),
                }],
            })
        }

        async fn current_sequence(&self) -> Result<ClipboardSequence> {
            let mut guard = self.seq_called.lock().unwrap();
            let _ = &*guard;
            *guard = true;
            Ok(ClipboardSequence::content_hash(
                ContentHash::sha256(&self.bytes).value,
            ))
        }
    }

    // Valid PNG magic so the snapshot survives magic-number validation and
    // the oversize verdict is the *size* guard, not a bad-signature drop.
    let mut bytes = vec![137u8, 80, 78, 71, 13, 10, 26, 10];
    bytes.resize(256, 0);
    let reader = ImageReader {
        bytes: bytes.clone(),
        seq_called: Mutex::new(false),
    };
    let store = SqliteStore::open_memory().expect("memory store");
    let settings = AppSettings {
        max_image_entry_size_bytes: 64,
        ..AppSettings::default()
    };
    let mut loop_ = CaptureLoop::new(reader, store.clone(), store.clone(), settings);

    assert!(loop_.capture_once().await.unwrap().is_none());
    assert!(store.list_recent(10).await.unwrap().is_empty());
}

#[tokio::test]
async fn capture_once_admits_image_over_text_budget_but_within_image_budget() {
    // The motivating fix: a screenshot that exceeds the (small, IPC-tied)
    // text budget must still be captured as long as it fits the separate,
    // larger image budget. Pre-fix the single `max_entry_size_bytes` budget
    // dropped every Retina screenshot as `oversized`.
    use std::sync::Mutex;

    use async_trait::async_trait;
    use nagori_core::{
        ClipboardData, ClipboardRepresentation, ClipboardSequence, ClipboardSnapshot, ContentHash,
    };
    use nagori_platform::ClipboardReader;
    use time::OffsetDateTime;

    struct ImageReader {
        bytes: Vec<u8>,
        seq_called: Mutex<bool>,
    }

    #[async_trait]
    impl ClipboardReader for ImageReader {
        async fn current_snapshot(&self) -> Result<ClipboardSnapshot> {
            Ok(ClipboardSnapshot {
                sequence: ClipboardSequence::content_hash(ContentHash::sha256(&self.bytes).value),
                captured_at: OffsetDateTime::now_utc(),
                source: None,
                representations: vec![ClipboardRepresentation {
                    mime_type: "image/png".to_owned(),
                    data: ClipboardData::Bytes(self.bytes.clone()),
                }],
            })
        }

        async fn current_sequence(&self) -> Result<ClipboardSequence> {
            *self.seq_called.lock().unwrap() = true;
            Ok(ClipboardSequence::content_hash(
                ContentHash::sha256(&self.bytes).value,
            ))
        }
    }

    // 4 KiB image (valid PNG magic + padding), over a 1 KiB text budget but
    // under a 1 MiB image budget.
    let mut bytes = vec![137u8, 80, 78, 71, 13, 10, 26, 10];
    bytes.resize(4096, 0);
    let reader = ImageReader {
        bytes: bytes.clone(),
        seq_called: Mutex::new(false),
    };
    let store = SqliteStore::open_memory().expect("memory store");
    let settings = AppSettings {
        max_entry_size_bytes: 1024,
        max_image_entry_size_bytes: 1024 * 1024,
        ..AppSettings::default()
    };
    let mut loop_ = CaptureLoop::new(reader, store.clone(), store.clone(), settings);

    let id = loop_
        .capture_once()
        .await
        .unwrap()
        .expect("image over the text budget must still be captured under the image budget");
    let stored = store.get(id).await.unwrap().expect("row");
    match &stored.content {
        ClipboardContent::Image(img) => assert_eq!(img.byte_count, bytes.len()),
        other => panic!("expected Image content, got {other:?}"),
    }
}

#[tokio::test]
async fn capture_once_skips_oversized_rich_text_primary() {
    // RichText's primary stores HTML/RTF markup, so a short plain-text
    // sibling with a large markup body has to be rejected by the size
    // guard. Pre-fix, the guard inspected `plain_text().len()` which
    // sees the trimmed plain-text projection, so a multi-KB markup body
    // slipped past a small `max_entry_size_bytes` budget.
    use std::sync::Mutex;

    use async_trait::async_trait;
    use nagori_core::{
        ClipboardData, ClipboardRepresentation, ClipboardSequence, ClipboardSnapshot, ContentHash,
    };
    use nagori_platform::ClipboardReader;
    use time::OffsetDateTime;

    struct RichTextReader {
        html: String,
        plain: String,
        seq_called: Mutex<bool>,
    }

    #[async_trait]
    impl ClipboardReader for RichTextReader {
        async fn current_snapshot(&self) -> Result<ClipboardSnapshot> {
            Ok(ClipboardSnapshot {
                sequence: ClipboardSequence::content_hash(
                    ContentHash::sha256(self.html.as_bytes()).value,
                ),
                captured_at: OffsetDateTime::now_utc(),
                source: None,
                representations: vec![
                    ClipboardRepresentation {
                        mime_type: "text/html".to_owned(),
                        data: ClipboardData::Text(self.html.clone()),
                    },
                    ClipboardRepresentation {
                        mime_type: "text/plain".to_owned(),
                        data: ClipboardData::Text(self.plain.clone()),
                    },
                ],
            })
        }

        async fn current_sequence(&self) -> Result<ClipboardSequence> {
            let mut guard = self.seq_called.lock().unwrap();
            *guard = true;
            Ok(ClipboardSequence::content_hash(
                ContentHash::sha256(self.html.as_bytes()).value,
            ))
        }
    }

    let html = format!("<p>{}</p>", "x".repeat(2048));
    let reader = RichTextReader {
        html,
        plain: "ok".to_owned(),
        seq_called: Mutex::new(false),
    };
    let store = SqliteStore::open_memory().expect("memory store");
    let settings = AppSettings {
        max_entry_size_bytes: 64,
        ..AppSettings::default()
    };
    let mut loop_ = CaptureLoop::new(reader, store.clone(), store.clone(), settings);

    assert!(loop_.capture_once().await.unwrap().is_none());
    assert!(store.list_recent(10).await.unwrap().is_empty());
}

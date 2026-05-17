use std::fmt::Write as _;

use time::OffsetDateTime;

use crate::{
    ClipboardContent, ClipboardData, ClipboardEntry, ClipboardRepresentation, ClipboardSnapshot,
    ContentHash, EntryId, EntryLifecycle, EntryMetadata, FileListContent, ImageContent,
    RepresentationDataRef, RepresentationRole, RichTextContent, RichTextMarkup, SearchDocument,
    StoredClipboardRepresentation, normalize_text,
};

#[derive(Debug, Clone)]
pub struct EntryFactory;

impl EntryFactory {
    pub fn from_text(text: impl Into<String>) -> ClipboardEntry {
        Self::from_content(ClipboardContent::from_plain_text(text.into()), None, None)
    }

    pub fn from_snapshot(snapshot: ClipboardSnapshot) -> Option<ClipboardEntry> {
        // Run every snapshot rep through the allowlist + magic-number gate,
        // pick the richest survivor as primary, and persist the remainder as
        // plain_fallback / alternatives so copy-back can re-publish each
        // flavour the source advertised.
        let normalized = normalize_representations(&snapshot.representations);
        let (content, primary_idx, has_plain_fallback) = pick_primary(&normalized)?;
        let mut entry = Self::from_content(content, snapshot.source, Some(snapshot.captured_at));
        let stored = build_stored_set(&normalized, primary_idx, has_plain_fallback);
        let set_hash = compute_representation_set_hash(&stored);
        entry.metadata.representation_set_hash = Some(set_hash);
        entry.pending_representations = stored;
        Some(entry)
    }

    pub fn from_content(
        content: ClipboardContent,
        source: Option<crate::SourceApp>,
        captured_at: Option<OffsetDateTime>,
    ) -> ClipboardEntry {
        let id = EntryId::new();
        let plain = content.plain_text().unwrap_or_default();
        // Image entries hash their raw bytes, not the placeholder `plain`
        // string — otherwise every captured image would collide on the
        // empty-string SHA and be deduped to the first one.
        let hash = match &content {
            ClipboardContent::Image(img) => match img.pending_bytes.as_deref() {
                Some(bytes) => ContentHash::sha256(bytes),
                None => ContentHash::sha256(plain.as_bytes()),
            },
            _ => ContentHash::sha256(plain.as_bytes()),
        };
        let mut metadata = EntryMetadata::new(hash, source);
        // Prefer the snapshot's `captured_at` so the row reflects when the
        // user copied the content rather than when the daemon woke up to
        // process it. Falls back to `now_utc()` for callers that synthesise
        // entries without a snapshot (CLI `add`, tests, etc.).
        if let Some(when) = captured_at {
            metadata.created_at = when;
            metadata.updated_at = when;
        }
        let search = SearchDocument::new(id, &content, normalize_text(plain));
        ClipboardEntry {
            id,
            content,
            metadata,
            search,
            sensitivity: crate::Sensitivity::Unknown,
            lifecycle: EntryLifecycle::default(),
            // `from_text` and the snapshot-less synthesis paths build a
            // single primary representation, so the persisted-rep list
            // starts empty and the storage layer falls back to deriving
            // the primary row from `content`. The snapshot path overrides
            // this in `from_snapshot` below.
            pending_representations: Vec::new(),
        }
    }
}

/// A snapshot representation reduced to its canonical, persist-ready form.
///
/// `normalize_representations` walks the snapshot in order and only filters,
/// so the index into the returned `Vec<NormalizedRep>` already preserves
/// occurrence order — there is no need to carry the original snapshot index
/// alongside.
#[derive(Debug, Clone)]
struct NormalizedRep {
    payload: NormalizedPayload,
}

#[derive(Debug, Clone)]
enum NormalizedPayload {
    /// `text/plain`, `text/html`, `text/rtf`, `application/rtf` — already
    /// trimmed-non-empty and canonicalised to the IANA form even when the
    /// source rep used an Apple UTI alias.
    Text {
        canonical_mime: &'static str,
        text: String,
    },
    /// Magic-number-verified image bytes. `mime` is the canonical lowercase
    /// IANA form so dedupe and copy-back never disagree on case.
    Image { mime: String, bytes: Vec<u8> },
    /// Non-empty file-URL list. The stored row keeps the original mime so a
    /// future copy-back path can re-publish it under the same UTI / IANA
    /// flavour the source advertised.
    FilePaths { mime: String, paths: Vec<String> },
}

/// Walk the snapshot's representations once and drop everything that fails
/// the allowlist + magic-number + non-empty checks. Logs every drop at
/// `debug!` so packet-level diagnosis is possible without surfacing raw
/// payload bytes.
fn normalize_representations(reps: &[ClipboardRepresentation]) -> Vec<NormalizedRep> {
    let mut out = Vec::with_capacity(reps.len());
    for rep in reps {
        let bare = bare_mime(&rep.mime_type);
        if let Some(canonical) = canonical_text_mime(bare) {
            let text = match &rep.data {
                ClipboardData::Text(t) => t.clone(),
                ClipboardData::Bytes(b) => {
                    if let Ok(s) = std::str::from_utf8(b) {
                        s.to_owned()
                    } else {
                        tracing::debug!(
                            mime_type = %rep.mime_type,
                            byte_count = b.len(),
                            "representation_dropped reason=non_utf8_text"
                        );
                        continue;
                    }
                }
                ClipboardData::FilePaths(_) => {
                    tracing::debug!(
                        mime_type = %rep.mime_type,
                        "representation_dropped reason=text_mime_carried_file_paths"
                    );
                    continue;
                }
            };
            if text.trim().is_empty() {
                continue;
            }
            out.push(NormalizedRep {
                payload: NormalizedPayload::Text {
                    canonical_mime: canonical,
                    text,
                },
            });
            continue;
        }

        if starts_with_image_prefix(&rep.mime_type) {
            let ClipboardData::Bytes(bytes) = &rep.data else {
                continue;
            };
            if bytes.is_empty() {
                continue;
            }
            if !is_allowlisted_image_mime(bare) {
                tracing::debug!(
                    mime_type = %rep.mime_type,
                    byte_count = bytes.len(),
                    "representation_dropped reason=image_mime_not_allowlisted"
                );
                continue;
            }
            if !crate::image_signature::matches_declared_mime(&rep.mime_type, bytes) {
                let detected = crate::image_signature::detect(bytes)
                    .map(crate::image_signature::ImageFormat::mime_type);
                tracing::warn!(
                    declared_mime = %rep.mime_type,
                    detected_mime = ?detected,
                    byte_count = bytes.len(),
                    "image_signature_mismatch_dropped"
                );
                continue;
            }
            out.push(NormalizedRep {
                payload: NormalizedPayload::Image {
                    mime: bare.to_ascii_lowercase(),
                    bytes: bytes.clone(),
                },
            });
            continue;
        }

        if let ClipboardData::FilePaths(paths) = &rep.data {
            if paths.is_empty() {
                continue;
            }
            out.push(NormalizedRep {
                payload: NormalizedPayload::FilePaths {
                    mime: rep.mime_type.clone(),
                    paths: paths.clone(),
                },
            });
            continue;
        }

        tracing::debug!(
            mime_type = %rep.mime_type,
            "representation_dropped reason=mime_not_allowlisted"
        );
    }
    out
}

/// Choose the richest representation as primary, matching the legacy
/// priority used by `pick_content`: file URLs → magic-matched image bytes →
/// rich text paired with plain text → plain text → markup-only rich text.
///
/// Returns the constructed [`ClipboardContent`], the index of the primary
/// inside `normalized`, and whether a sibling `text/plain` rep should be
/// labelled `plain_fallback` (only when the primary is a paired `RichText`).
fn pick_primary(normalized: &[NormalizedRep]) -> Option<(ClipboardContent, usize, bool)> {
    if let Some(picked) = pick_file_list_primary(normalized) {
        return Some(picked);
    }
    if let Some(picked) = pick_image_primary(normalized) {
        return Some(picked);
    }

    let plain_idx = find_text_idx(normalized, "text/plain");
    let html_idx = find_text_idx(normalized, "text/html");
    let rtf_idx = find_rtf_idx(normalized);

    if let Some(pi) = plain_idx {
        let plain_text = text_at(normalized, pi);
        if let Some(hi) = html_idx {
            return Some((
                build_rich_text(plain_text, text_at(normalized, hi), RichTextMarkup::Html),
                hi,
                true,
            ));
        }
        if let Some(ri) = rtf_idx {
            return Some((
                build_rich_text(plain_text, text_at(normalized, ri), RichTextMarkup::Rtf),
                ri,
                true,
            ));
        }
        return Some((ClipboardContent::from_plain_text(plain_text), pi, false));
    }

    if let Some(hi) = html_idx {
        let markup = text_at(normalized, hi);
        let stripped = strip_html(&markup);
        return Some((
            build_rich_text(stripped, markup, RichTextMarkup::Html),
            hi,
            false,
        ));
    }
    if let Some(ri) = rtf_idx {
        let markup = text_at(normalized, ri);
        return Some((
            build_rich_text(markup.clone(), markup, RichTextMarkup::Rtf),
            ri,
            false,
        ));
    }

    None
}

fn pick_file_list_primary(normalized: &[NormalizedRep]) -> Option<(ClipboardContent, usize, bool)> {
    let (idx, paths) = normalized
        .iter()
        .enumerate()
        .find_map(|(i, n)| match &n.payload {
            NormalizedPayload::FilePaths { paths, .. } => Some((i, paths.clone())),
            _ => None,
        })?;
    let display_text = paths.join("\n");
    Some((
        ClipboardContent::FileList(FileListContent {
            paths,
            display_text,
        }),
        idx,
        false,
    ))
}

fn pick_image_primary(normalized: &[NormalizedRep]) -> Option<(ClipboardContent, usize, bool)> {
    let (idx, mime, bytes) = normalized
        .iter()
        .enumerate()
        .find_map(|(i, n)| match &n.payload {
            NormalizedPayload::Image { mime, bytes } => Some((i, mime.clone(), bytes.clone())),
            _ => None,
        })?;
    let byte_count = bytes.len();
    Some((
        ClipboardContent::Image(ImageContent {
            width: None,
            height: None,
            byte_count,
            mime_type: Some(mime),
            pending_bytes: Some(bytes),
        }),
        idx,
        false,
    ))
}

const fn build_rich_text(
    plain_text: String,
    markup: String,
    kind: RichTextMarkup,
) -> ClipboardContent {
    ClipboardContent::RichText(RichTextContent {
        plain_text,
        markup: Some(markup),
        markup_kind: Some(kind),
    })
}

fn find_text_idx(normalized: &[NormalizedRep], mime: &str) -> Option<usize> {
    normalized.iter().position(|n| {
        matches!(&n.payload, NormalizedPayload::Text { canonical_mime, .. } if *canonical_mime == mime)
    })
}

fn find_rtf_idx(normalized: &[NormalizedRep]) -> Option<usize> {
    normalized.iter().position(|n| {
        matches!(
            &n.payload,
            NormalizedPayload::Text { canonical_mime, .. }
                if *canonical_mime == "text/rtf" || *canonical_mime == "application/rtf"
        )
    })
}

fn text_at(normalized: &[NormalizedRep], idx: usize) -> String {
    match &normalized[idx].payload {
        NormalizedPayload::Text { text, .. } => text.clone(),
        _ => panic!("expected text normalized payload at index {idx}"),
    }
}

/// Assemble the persisted representation set in role-major order
/// (`primary` → `plain_fallback` → `alternative`). Within each role bucket
/// the snapshot's original index is preserved so a multi-alternative entry
/// keeps the same ranking copy-back would otherwise reconstruct.
fn build_stored_set(
    normalized: &[NormalizedRep],
    primary_idx: usize,
    has_plain_fallback: bool,
) -> Vec<StoredClipboardRepresentation> {
    let mut out = Vec::with_capacity(normalized.len());
    let mut consumed = vec![false; normalized.len()];
    out.push(stored_from(
        &normalized[primary_idx],
        RepresentationRole::Primary,
        0,
    ));
    consumed[primary_idx] = true;

    let mut ordinal: u32 = 1;
    if has_plain_fallback
        && let Some(pi) = find_text_idx(normalized, "text/plain")
        && !consumed[pi]
    {
        out.push(stored_from(
            &normalized[pi],
            RepresentationRole::PlainFallback,
            ordinal,
        ));
        consumed[pi] = true;
        ordinal = ordinal.saturating_add(1);
    }

    for (idx, rep) in normalized.iter().enumerate() {
        if consumed[idx] {
            continue;
        }
        out.push(stored_from(rep, RepresentationRole::Alternative, ordinal));
        ordinal = ordinal.saturating_add(1);
    }
    out
}

fn stored_from(
    n: &NormalizedRep,
    role: RepresentationRole,
    ordinal: u32,
) -> StoredClipboardRepresentation {
    match &n.payload {
        NormalizedPayload::Text {
            canonical_mime,
            text,
        } => StoredClipboardRepresentation {
            role,
            mime_type: (*canonical_mime).to_owned(),
            ordinal,
            data: RepresentationDataRef::InlineText(text.clone()),
        },
        NormalizedPayload::Image { mime, bytes } => StoredClipboardRepresentation {
            role,
            mime_type: mime.clone(),
            ordinal,
            data: RepresentationDataRef::DatabaseBlob(bytes.clone()),
        },
        NormalizedPayload::FilePaths { mime, paths } => StoredClipboardRepresentation {
            role,
            mime_type: mime.clone(),
            ordinal,
            data: RepresentationDataRef::FilePaths(paths.clone()),
        },
    }
}

/// SHA-256 over the canonical encoding of every persisted representation.
///
/// Encodes each rep as `role|mime|ordinal|sha256(payload_bytes)` and joins
/// with newlines after sorting by (role, ordinal, mime). The hash diverges
/// from `content_hash` once an entry carries alternatives or a plain
/// fallback, giving dedupe a way to recognise "same representation set" vs
/// "same primary body". `representation_set_hash` is recomputed by the
/// budget-trim path if any rep is dropped so the hash stays in sync with
/// what storage actually wrote.
#[must_use]
pub fn compute_representation_set_hash(reps: &[StoredClipboardRepresentation]) -> ContentHash {
    let mut sorted: Vec<&StoredClipboardRepresentation> = reps.iter().collect();
    sorted.sort_by(|a, b| {
        (a.role as u8, a.ordinal, a.mime_type.as_str()).cmp(&(
            b.role as u8,
            b.ordinal,
            b.mime_type.as_str(),
        ))
    });
    let mut buf = String::new();
    for (i, r) in sorted.iter().enumerate() {
        if i > 0 {
            buf.push('\n');
        }
        let payload_hash = match &r.data {
            RepresentationDataRef::InlineText(text) => ContentHash::sha256(text.as_bytes()),
            RepresentationDataRef::DatabaseBlob(bytes) => ContentHash::sha256(bytes),
            RepresentationDataRef::FilePaths(paths) => {
                ContentHash::sha256(paths.join("\n").as_bytes())
            }
        };
        // `write!` to a String is infallible — drop the Result on purpose.
        let _ = write!(
            &mut buf,
            "{}|{}|{}|{}",
            r.role.as_str(),
            r.mime_type,
            r.ordinal,
            payload_hash.value
        );
    }
    ContentHash::sha256(buf.as_bytes())
}

/// Bare mime type ("text/plain") stripped of any parameter list. Used as
/// the matching key against the allowlist so `text/plain;charset=utf-8`
/// doesn't get classified differently from the parameter-less form.
fn bare_mime(mime: &str) -> &str {
    mime.split(';').next().unwrap_or(mime).trim()
}

/// Map a bare mime / Apple UTI to its canonical IANA form when allowlisted.
/// Returning `Some` is sufficient for the rep to enter the text family.
const fn canonical_text_mime(bare: &str) -> Option<&'static str> {
    if bare.eq_ignore_ascii_case("text/plain")
        || bare.eq_ignore_ascii_case("public.utf8-plain-text")
    {
        Some("text/plain")
    } else if bare.eq_ignore_ascii_case("text/html") || bare.eq_ignore_ascii_case("public.html") {
        Some("text/html")
    } else if bare.eq_ignore_ascii_case("application/rtf")
        || bare.eq_ignore_ascii_case("public.rtf")
    {
        Some("application/rtf")
    } else if bare.eq_ignore_ascii_case("text/rtf") {
        Some("text/rtf")
    } else {
        None
    }
}

fn is_allowlisted_image_mime(bare: &str) -> bool {
    matches!(
        bare.to_ascii_lowercase().as_str(),
        "image/png" | "image/jpeg" | "image/gif" | "image/webp" | "image/tiff"
    )
}

/// Case-insensitive `image/...` prefix check.
///
/// IANA says the type/subtype is case-insensitive, and producers in
/// the wild (some browsers, old screenshot tools) do publish
/// `IMAGE/PNG`. The capture branch uses a plain `starts_with` for
/// speed but routes the comparison through this helper so it matches
/// the case-insensitive semantics of `image_signature::matches_declared_mime`.
fn starts_with_image_prefix(mime: &str) -> bool {
    mime.get(..6)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("image/"))
}

/// Tiny tag-stripper used as a last-resort fallback when the pasteboard
/// only exposes HTML and no `text/plain`. It is intentionally lossy — full
/// HTML rendering belongs in the `WebView`, not the capture path.
///
/// Tracks attribute quoting so a `>` inside `href="x>y"` does not prematurely
/// close the tag.
fn strip_html(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    let mut quote: Option<char> = None;
    for ch in html.chars() {
        if in_tag {
            if let Some(q) = quote {
                if ch == q {
                    quote = None;
                }
            } else {
                match ch {
                    '"' | '\'' => quote = Some(ch),
                    '>' => in_tag = false,
                    _ => {}
                }
            }
        } else if ch == '<' {
            in_tag = true;
            quote = None;
        } else {
            out.push(ch);
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use time::OffsetDateTime;

    use super::*;
    use crate::SourceApp;

    #[test]
    fn snapshot_text_representation_preserves_source() {
        let source = SourceApp {
            bundle_id: Some("com.example.editor".to_owned()),
            name: Some("Example Editor".to_owned()),
            executable_path: None,
        };
        let snapshot = ClipboardSnapshot {
            sequence: crate::ClipboardSequence::content_hash("1"),
            captured_at: OffsetDateTime::now_utc(),
            source: Some(source.clone()),
            representations: vec![ClipboardRepresentation {
                mime_type: "text/plain".to_owned(),
                data: ClipboardData::Text("Clipboard text".to_owned()),
            }],
        };

        let entry =
            EntryFactory::from_snapshot(snapshot).expect("text snapshot should build entry");

        assert_eq!(entry.plain_text(), Some("Clipboard text"));
        assert_eq!(entry.metadata.source, Some(source));
        assert_eq!(entry.search.normalized_text, "clipboard text");
    }

    #[test]
    fn snapshot_uses_file_paths_when_text_is_absent() {
        let snapshot = ClipboardSnapshot {
            sequence: crate::ClipboardSequence::content_hash("2"),
            captured_at: OffsetDateTime::now_utc(),
            source: None,
            representations: vec![
                ClipboardRepresentation {
                    mime_type: "application/octet-stream".to_owned(),
                    data: ClipboardData::Bytes(vec![1, 2, 3]),
                },
                ClipboardRepresentation {
                    mime_type: "text/uri-list".to_owned(),
                    data: ClipboardData::FilePaths(vec![
                        "/tmp/one.txt".to_owned(),
                        "/tmp/two.txt".to_owned(),
                    ]),
                },
            ],
        };

        let entry = EntryFactory::from_snapshot(snapshot).expect("file paths should build entry");

        assert_eq!(entry.plain_text(), Some("/tmp/one.txt\n/tmp/two.txt"));
    }

    #[test]
    fn snapshot_without_recognised_data_is_ignored() {
        let snapshot = ClipboardSnapshot {
            sequence: crate::ClipboardSequence::content_hash("3"),
            captured_at: OffsetDateTime::now_utc(),
            source: None,
            representations: vec![ClipboardRepresentation {
                mime_type: "application/x-unknown".to_owned(),
                data: ClipboardData::Bytes(vec![0xDE, 0xAD, 0xBE, 0xEF]),
            }],
        };

        assert!(EntryFactory::from_snapshot(snapshot).is_none());
    }

    #[test]
    fn snapshot_image_with_mismatched_signature_falls_through_to_text() {
        // Producer labelled HTML bytes as `image/png`. The factory must
        // reject the image representation but still build an entry from
        // the sibling text/plain rep so a single misclassified payload
        // doesn't shadow legitimate clipboard content.
        let html_bytes = b"<!doctype html><html><body>oops</body></html>".to_vec();
        let snapshot = ClipboardSnapshot {
            sequence: crate::ClipboardSequence::content_hash("img-mismatch-text"),
            captured_at: OffsetDateTime::now_utc(),
            source: None,
            representations: vec![
                ClipboardRepresentation {
                    mime_type: "image/png".to_owned(),
                    data: ClipboardData::Bytes(html_bytes),
                },
                ClipboardRepresentation {
                    mime_type: "text/plain".to_owned(),
                    data: ClipboardData::Text("fallback".to_owned()),
                },
            ],
        };

        let entry = EntryFactory::from_snapshot(snapshot)
            .expect("text fallback should still build an entry");
        match entry.content {
            crate::ClipboardContent::Text(text) => assert_eq!(text.text, "fallback"),
            other => panic!("expected Text fallback, got {other:?}"),
        }
    }

    #[test]
    fn snapshot_image_only_with_invalid_signature_is_dropped() {
        // No sibling representation, so an image rep that fails the
        // magic-number check leaves the snapshot with nothing to
        // persist. The whole snapshot must be discarded rather than
        // saved as an empty / unsafe entry.
        let html_bytes = b"<!doctype html>nope".to_vec();
        let snapshot = ClipboardSnapshot {
            sequence: crate::ClipboardSequence::content_hash("img-mismatch-only"),
            captured_at: OffsetDateTime::now_utc(),
            source: None,
            representations: vec![ClipboardRepresentation {
                mime_type: "image/png".to_owned(),
                data: ClipboardData::Bytes(html_bytes),
            }],
        };

        assert!(EntryFactory::from_snapshot(snapshot).is_none());
    }

    #[test]
    fn snapshot_image_mime_prefix_matches_case_insensitively() {
        // IANA says the type/subtype is case-insensitive; some
        // screenshot producers publish `IMAGE/PNG`. The factory must
        // route those reps through the image branch (and therefore the
        // signature gate) instead of letting them fall through to text.
        let png_bytes = vec![137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13];
        let snapshot = ClipboardSnapshot {
            sequence: crate::ClipboardSequence::content_hash("img-case"),
            captured_at: OffsetDateTime::now_utc(),
            source: None,
            representations: vec![ClipboardRepresentation {
                mime_type: "IMAGE/PNG".to_owned(),
                data: ClipboardData::Bytes(png_bytes),
            }],
        };

        let entry = EntryFactory::from_snapshot(snapshot)
            .expect("upper-case mime should still build entry");
        assert!(matches!(entry.content, crate::ClipboardContent::Image(_)));
    }

    #[test]
    fn snapshot_jpeg_signature_is_accepted() {
        // The factory's existing PNG test covers RFC 2083's magic; this
        // one locks down that JPEG (FF D8 FF…) also flows through the
        // signature gate so we don't regress one allow-listed format
        // while polishing another.
        let jpeg_bytes = vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, b'J', b'F', b'I', b'F'];
        let snapshot = ClipboardSnapshot {
            sequence: crate::ClipboardSequence::content_hash("jpeg-ok"),
            captured_at: OffsetDateTime::now_utc(),
            source: None,
            representations: vec![ClipboardRepresentation {
                mime_type: "image/jpeg".to_owned(),
                data: ClipboardData::Bytes(jpeg_bytes.clone()),
            }],
        };

        let entry = EntryFactory::from_snapshot(snapshot).expect("jpeg should build entry");
        match entry.content {
            crate::ClipboardContent::Image(img) => {
                assert_eq!(img.mime_type.as_deref(), Some("image/jpeg"));
                assert_eq!(img.byte_count, jpeg_bytes.len());
            }
            other => panic!("expected Image, got {other:?}"),
        }
    }

    #[test]
    fn snapshot_tiff_signature_is_accepted() {
        // The macOS reader emits `image/tiff` for screenshots and rich-text
        // copies that carry an embedded TIFF preview. The allowlist guards
        // against bytes-vs-mime spoofing but must still accept TIFF, both
        // little-endian (II) and big-endian (MM) magic.
        let tiff_le_header = vec![0x49, 0x49, 0x2A, 0x00, 0x08, 0x00, 0x00, 0x00];
        let snapshot = ClipboardSnapshot {
            sequence: crate::ClipboardSequence::content_hash("tiff-ok"),
            captured_at: OffsetDateTime::now_utc(),
            source: None,
            representations: vec![ClipboardRepresentation {
                mime_type: "image/tiff".to_owned(),
                data: ClipboardData::Bytes(tiff_le_header.clone()),
            }],
        };

        let entry = EntryFactory::from_snapshot(snapshot).expect("tiff should build entry");
        match entry.content {
            crate::ClipboardContent::Image(img) => {
                assert_eq!(img.mime_type.as_deref(), Some("image/tiff"));
                assert_eq!(img.byte_count, tiff_le_header.len());
            }
            other => panic!("expected Image, got {other:?}"),
        }
    }

    #[test]
    fn snapshot_image_bytes_yields_image_content() {
        let png_bytes = vec![137, 80, 78, 71, 13, 10, 26, 10];
        let snapshot = ClipboardSnapshot {
            sequence: crate::ClipboardSequence::content_hash("3"),
            captured_at: OffsetDateTime::now_utc(),
            source: None,
            representations: vec![ClipboardRepresentation {
                mime_type: "image/png".to_owned(),
                data: ClipboardData::Bytes(png_bytes.clone()),
            }],
        };

        let entry = EntryFactory::from_snapshot(snapshot).expect("png should build entry");
        match &entry.content {
            crate::ClipboardContent::Image(img) => {
                assert_eq!(img.byte_count, png_bytes.len());
                assert_eq!(img.mime_type.as_deref(), Some("image/png"));
                assert_eq!(img.pending_bytes.as_deref(), Some(png_bytes.as_slice()));
            }
            other => panic!("expected Image, got {other:?}"),
        }
        // Hash must reflect the bytes, not the (empty) plain text — otherwise
        // every captured image would dedupe against the empty-string SHA.
        let expected_hash = ContentHash::sha256(&png_bytes).value;
        assert_eq!(entry.metadata.content_hash.value, expected_hash);
    }

    #[test]
    fn snapshot_uses_captured_at_for_metadata_timestamps() {
        // The capture loop runs on a 500ms tick, so the snapshot is the
        // closest signal we have to "when did the user actually copy this".
        // `EntryMetadata` must inherit it instead of stamping `now_utc()`.
        let captured_at = OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap();
        let snapshot = ClipboardSnapshot {
            sequence: crate::ClipboardSequence::content_hash("4"),
            captured_at,
            source: None,
            representations: vec![ClipboardRepresentation {
                mime_type: "text/plain".to_owned(),
                data: ClipboardData::Text("hello".to_owned()),
            }],
        };

        let entry =
            EntryFactory::from_snapshot(snapshot).expect("text snapshot should build entry");

        assert_eq!(entry.metadata.created_at, captured_at);
        assert_eq!(entry.metadata.updated_at, captured_at);
    }

    #[test]
    fn snapshot_with_html_paired_with_text_yields_richtext_with_markup() {
        let snapshot = ClipboardSnapshot {
            sequence: crate::ClipboardSequence::content_hash("rt-1"),
            captured_at: OffsetDateTime::now_utc(),
            source: None,
            representations: vec![
                ClipboardRepresentation {
                    mime_type: "text/html".to_owned(),
                    data: ClipboardData::Text("<p>hello</p>".to_owned()),
                },
                ClipboardRepresentation {
                    mime_type: "text/plain".to_owned(),
                    data: ClipboardData::Text("hello".to_owned()),
                },
            ],
        };

        let entry = EntryFactory::from_snapshot(snapshot).expect("rich text should build entry");
        match entry.content {
            crate::ClipboardContent::RichText(rt) => {
                assert_eq!(rt.plain_text, "hello");
                assert_eq!(rt.markup.as_deref(), Some("<p>hello</p>"));
                assert_eq!(rt.markup_kind, Some(crate::RichTextMarkup::Html));
            }
            other => panic!("expected RichText, got {other:?}"),
        }
    }

    #[test]
    fn snapshot_html_only_strips_tags_for_plain_text() {
        let snapshot = ClipboardSnapshot {
            sequence: crate::ClipboardSequence::content_hash("rt-2"),
            captured_at: OffsetDateTime::now_utc(),
            source: None,
            representations: vec![ClipboardRepresentation {
                mime_type: "text/html".to_owned(),
                data: ClipboardData::Text("<p>hello <b>world</b></p>".to_owned()),
            }],
        };

        let entry = EntryFactory::from_snapshot(snapshot).expect("html-only should still build");
        match entry.content {
            crate::ClipboardContent::RichText(rt) => {
                assert_eq!(rt.plain_text, "hello world");
                assert_eq!(rt.markup_kind, Some(crate::RichTextMarkup::Html));
            }
            other => panic!("expected RichText, got {other:?}"),
        }
    }

    #[test]
    fn strip_html_handles_angle_brackets_inside_attribute_quotes() {
        // Regression: a naive in_tag toggle would close on the `>` in href="x>y",
        // leaking attribute fragments into the plain text output.
        let stripped = super::strip_html(r#"<a href="x>y">link</a> tail"#);
        assert_eq!(stripped, "link tail");

        let stripped = super::strip_html(r#"<img alt='a > b' src="t"/>caption"#);
        assert_eq!(stripped, "caption");
    }

    #[test]
    fn snapshot_file_paths_yields_file_list_content() {
        let snapshot = ClipboardSnapshot {
            sequence: crate::ClipboardSequence::content_hash("fl-1"),
            captured_at: OffsetDateTime::now_utc(),
            source: None,
            representations: vec![ClipboardRepresentation {
                mime_type: "text/uri-list".to_owned(),
                data: ClipboardData::FilePaths(vec![
                    "/tmp/one.txt".to_owned(),
                    "/tmp/two.txt".to_owned(),
                ]),
            }],
        };

        let entry = EntryFactory::from_snapshot(snapshot).expect("file list should build entry");
        match entry.content {
            crate::ClipboardContent::FileList(value) => {
                assert_eq!(value.paths.len(), 2);
                assert_eq!(value.display_text, "/tmp/one.txt\n/tmp/two.txt");
            }
            other => panic!("expected FileList, got {other:?}"),
        }
    }

    #[test]
    fn snapshot_empty_plain_text_falls_through_to_html() {
        // Some apps (Notes, Mail) publish both a `text/plain` and a
        // `text/html` rep but leave plain empty when the source is
        // markup-only. The factory must not let the empty plain shadow the
        // non-empty html, otherwise the persisted entry has nothing to
        // search or preview.
        let snapshot = ClipboardSnapshot {
            sequence: crate::ClipboardSequence::content_hash("rt-empty"),
            captured_at: OffsetDateTime::now_utc(),
            source: None,
            representations: vec![
                ClipboardRepresentation {
                    mime_type: "text/plain".to_owned(),
                    data: ClipboardData::Text(String::new()),
                },
                ClipboardRepresentation {
                    mime_type: "text/html".to_owned(),
                    data: ClipboardData::Text("<p>hi</p>".to_owned()),
                },
            ],
        };

        let entry = EntryFactory::from_snapshot(snapshot).expect("html should build entry");
        match entry.content {
            crate::ClipboardContent::RichText(rt) => {
                assert_eq!(rt.plain_text, "hi");
                assert_eq!(rt.markup.as_deref(), Some("<p>hi</p>"));
            }
            other => panic!("expected RichText, got {other:?}"),
        }
    }

    #[test]
    fn snapshot_text_plain_with_charset_param_is_recognised() {
        let snapshot = ClipboardSnapshot {
            sequence: crate::ClipboardSequence::content_hash("charset"),
            captured_at: OffsetDateTime::now_utc(),
            source: None,
            representations: vec![ClipboardRepresentation {
                mime_type: "text/plain;charset=utf-8".to_owned(),
                data: ClipboardData::Text("hello".to_owned()),
            }],
        };

        let entry = EntryFactory::from_snapshot(snapshot).expect("text should build entry");
        assert_eq!(entry.plain_text(), Some("hello"));
    }

    #[test]
    fn from_snapshot_normalizes_with_nfkc_and_lowercase() {
        // EntryFactory must use the workspace-canonical normalizer (full-width
        // → ASCII via NFKC, then lowercase) so the search backend doesn't
        // diverge from the document it indexes.
        let captured_at = OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap();
        let snapshot = ClipboardSnapshot {
            sequence: crate::ClipboardSequence::content_hash("5"),
            captured_at,
            source: None,
            representations: vec![ClipboardRepresentation {
                mime_type: "text/plain".to_owned(),
                data: ClipboardData::Text("ＡＢＣ  １２３".to_owned()),
            }],
        };

        let entry =
            EntryFactory::from_snapshot(snapshot).expect("text snapshot should build entry");

        assert_eq!(entry.search.normalized_text, "abc 123");
    }

    #[test]
    fn snapshot_with_html_plain_rtf_assigns_roles_in_priority_order() {
        // HTML+plain pair wins primary (RichText), the sibling plain rep
        // becomes plain_fallback (so a paste-as-plain path has it ready),
        // and the leftover RTF rep is an alternative — exactly the layering
        // the multi-rep store needs to round-trip back to the OS clipboard.
        let snapshot = ClipboardSnapshot {
            sequence: crate::ClipboardSequence::content_hash("multi-rt"),
            captured_at: OffsetDateTime::now_utc(),
            source: None,
            representations: vec![
                ClipboardRepresentation {
                    mime_type: "text/html".to_owned(),
                    data: ClipboardData::Text("<p>hello</p>".to_owned()),
                },
                ClipboardRepresentation {
                    mime_type: "text/plain".to_owned(),
                    data: ClipboardData::Text("hello".to_owned()),
                },
                ClipboardRepresentation {
                    mime_type: "application/rtf".to_owned(),
                    data: ClipboardData::Text("{\\rtf1 hello}".to_owned()),
                },
            ],
        };

        let entry = EntryFactory::from_snapshot(snapshot).expect("entry should build");
        let reps = &entry.pending_representations;
        assert_eq!(reps.len(), 3);

        assert_eq!(reps[0].role, RepresentationRole::Primary);
        assert_eq!(reps[0].mime_type, "text/html");
        assert_eq!(reps[0].ordinal, 0);

        assert_eq!(reps[1].role, RepresentationRole::PlainFallback);
        assert_eq!(reps[1].mime_type, "text/plain");
        assert_eq!(reps[1].ordinal, 1);

        assert_eq!(reps[2].role, RepresentationRole::Alternative);
        assert_eq!(reps[2].mime_type, "application/rtf");
        assert_eq!(reps[2].ordinal, 2);
    }

    #[test]
    fn snapshot_with_image_plus_html_keeps_html_as_alternative() {
        // Image wins primary because it's the richest format the user
        // copied. The accompanying HTML rep (often a screenshot's alt-text
        // or surrounding markup) should survive as an alternative so the
        // dataset still carries the textual context — without it, full-text
        // search would lose the only searchable companion.
        let png_bytes = vec![137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13];
        let snapshot = ClipboardSnapshot {
            sequence: crate::ClipboardSequence::content_hash("img-html"),
            captured_at: OffsetDateTime::now_utc(),
            source: None,
            representations: vec![
                ClipboardRepresentation {
                    mime_type: "image/png".to_owned(),
                    data: ClipboardData::Bytes(png_bytes.clone()),
                },
                ClipboardRepresentation {
                    mime_type: "text/html".to_owned(),
                    data: ClipboardData::Text("<p>caption</p>".to_owned()),
                },
            ],
        };

        let entry = EntryFactory::from_snapshot(snapshot).expect("entry should build");
        assert!(matches!(entry.content, crate::ClipboardContent::Image(_)));

        let reps = &entry.pending_representations;
        assert_eq!(reps.len(), 2);

        assert_eq!(reps[0].role, RepresentationRole::Primary);
        assert_eq!(reps[0].mime_type, "image/png");
        match &reps[0].data {
            RepresentationDataRef::DatabaseBlob(bytes) => assert_eq!(bytes, &png_bytes),
            other => panic!("primary should carry image bytes, got {other:?}"),
        }

        assert_eq!(reps[1].role, RepresentationRole::Alternative);
        assert_eq!(reps[1].mime_type, "text/html");
    }

    #[test]
    fn snapshot_image_mismatch_drops_image_rep_while_text_persists() {
        // The signature-gate test above checks that the primary content
        // falls through to text; this one locks in that the bad image rep
        // doesn't leak into `pending_representations` either. A failed
        // magic-number check has to remove the rep from every downstream
        // path or storage will write bytes the daemon already rejected.
        let html_disguised_as_png = b"<!doctype html>oops".to_vec();
        let snapshot = ClipboardSnapshot {
            sequence: crate::ClipboardSequence::content_hash("img-drop-rep"),
            captured_at: OffsetDateTime::now_utc(),
            source: None,
            representations: vec![
                ClipboardRepresentation {
                    mime_type: "image/png".to_owned(),
                    data: ClipboardData::Bytes(html_disguised_as_png),
                },
                ClipboardRepresentation {
                    mime_type: "text/plain".to_owned(),
                    data: ClipboardData::Text("alt".to_owned()),
                },
            ],
        };

        let entry = EntryFactory::from_snapshot(snapshot).expect("entry should build");
        let reps = &entry.pending_representations;
        assert_eq!(reps.len(), 1);
        assert_eq!(reps[0].role, RepresentationRole::Primary);
        assert_eq!(reps[0].mime_type, "text/plain");
        assert!(
            !reps.iter().any(|r| r.mime_type == "image/png"),
            "image rep with bad magic must not be persisted"
        );
    }

    #[test]
    fn representation_set_hash_is_stable_across_equivalent_snapshots() {
        // Two snapshots whose reps differ only by `captured_at` must produce
        // the same `representation_set_hash` so dedupe can recognise "user
        // copied the same thing twice" rather than treating every paste as
        // a brand-new entry.
        let build = |captured_at: OffsetDateTime| ClipboardSnapshot {
            sequence: crate::ClipboardSequence::content_hash("hash-stable"),
            captured_at,
            source: None,
            representations: vec![
                ClipboardRepresentation {
                    mime_type: "text/html".to_owned(),
                    data: ClipboardData::Text("<p>x</p>".to_owned()),
                },
                ClipboardRepresentation {
                    mime_type: "text/plain".to_owned(),
                    data: ClipboardData::Text("x".to_owned()),
                },
            ],
        };

        let a = EntryFactory::from_snapshot(build(
            OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap(),
        ))
        .unwrap();
        let b = EntryFactory::from_snapshot(build(
            OffsetDateTime::from_unix_timestamp(1_700_000_999).unwrap(),
        ))
        .unwrap();

        let ha = a.metadata.representation_set_hash.expect("hash present");
        let hb = b.metadata.representation_set_hash.expect("hash present");
        assert_eq!(ha.value, hb.value);
    }

    #[test]
    fn representation_set_hash_diverges_from_content_hash_when_alternatives_present() {
        // Snapshot-derived entries always emit a canonical set hash; the
        // role/mime/ordinal/sha256(payload) encoding means a multi-rep set
        // fingerprints a wider surface than the primary body alone, and the
        // two columns are expected to diverge.
        let snapshot = ClipboardSnapshot {
            sequence: crate::ClipboardSequence::content_hash("hash-diverge"),
            captured_at: OffsetDateTime::now_utc(),
            source: None,
            representations: vec![
                ClipboardRepresentation {
                    mime_type: "text/html".to_owned(),
                    data: ClipboardData::Text("<p>hi</p>".to_owned()),
                },
                ClipboardRepresentation {
                    mime_type: "text/plain".to_owned(),
                    data: ClipboardData::Text("hi".to_owned()),
                },
            ],
        };

        let entry = EntryFactory::from_snapshot(snapshot).unwrap();
        let set_hash = entry
            .metadata
            .representation_set_hash
            .expect("multi-rep entry must carry a representation_set_hash");
        assert_ne!(set_hash.value, entry.metadata.content_hash.value);
    }
}

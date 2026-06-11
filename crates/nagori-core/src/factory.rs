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
    // Stays in lockstep with `image_signature::SUPPORTED_IMAGE_MIMES` and the
    // desktop `ALLOWED_IMAGE_MIME`. BMP is intentionally absent on every
    // side: none of the platform crates can publish it back to the system
    // clipboard, so accepting BMP at capture would silently strand entries
    // that look pasteable in the palette but fail on Cmd+Enter.
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
mod tests;

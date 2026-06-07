//! Selecting a single stored representation to re-publish to the OS
//! clipboard ("paste as PNG / plain text / files").
//!
//! A captured entry can hold several representations (a copied file can
//! carry the file URLs, a rendered image, and a plain-text label all at
//! once). The default copy-back re-offers every publishable representation
//! so the receiving app picks the richest one it understands. This module
//! is the other direction: enumerate the distinct representations the user
//! can deliberately force, and resolve a requested MIME back to the one
//! canonical representation that should be written.
//!
//! The set of MIMEs treated as publishable here is the intersection every
//! platform adapter (`nagori-platform-{macos,windows,linux}`) maps onto a
//! clipboard format, so an option offered from this module is one the
//! strict single-representation writer can actually publish.

use serde::{Deserialize, Serialize};

use super::{RepresentationDataRef, StoredClipboardRepresentation};

/// User-facing grouping for a pasteable representation.
///
/// Surfaced as a stable token the desktop maps to a localized label. Coarser
/// than the MIME so the label stays meaningful (`Image` rather than
/// `image/png`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PasteCategory {
    Files,
    Image,
    PlainText,
    Html,
    RichText,
}

/// One representation the user can paste on its own, identified by its
/// canonical MIME plus the category that drives its label.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PasteOption {
    pub mime: String,
    pub category: PasteCategory,
}

/// Normalize a MIME for comparison.
///
/// Lower-cased, parameters (`; charset=…`) dropped, surrounding whitespace
/// trimmed. Callers compare a requested MIME against [`canonical_pasteable_mime`]
/// through this so a client that sends `text/plain; charset=utf-8` or
/// `TEXT/PLAIN` still resolves.
#[must_use]
pub fn normalize_mime(mime: &str) -> String {
    let base = mime.split(';').next().unwrap_or(mime);
    base.trim().to_ascii_lowercase()
}

/// The canonical MIME a representation can be published under on its own, or
/// `None` when no platform adapter maps it to a clipboard format.
///
/// The raw stored MIME is matched exactly, the same way each platform adapter's
/// `has_publishable_representation` pre-scan does, so every MIME offered here is
/// one the strict writer will actually publish — no normalisation gap that
/// would offer a format the adapter then rejects. (Stored MIMEs are already the
/// bare, lower-cased forms the capture pipeline normalises on insert.) Payloads
/// that would publish an empty offer — an empty file list or zero-byte image
/// bytes from a corrupt row — return `None` so the picker never blanks the
/// clipboard.
#[must_use]
pub fn canonical_pasteable_mime(rep: &StoredClipboardRepresentation) -> Option<&'static str> {
    match (rep.mime_type.as_str(), &rep.data) {
        ("text/plain", RepresentationDataRef::InlineText(_)) => Some("text/plain"),
        ("text/html", RepresentationDataRef::InlineText(_)) => Some("text/html"),
        ("application/rtf", RepresentationDataRef::InlineText(_)) => Some("application/rtf"),
        ("image/png", RepresentationDataRef::DatabaseBlob(b)) if !b.is_empty() => Some("image/png"),
        ("image/tiff", RepresentationDataRef::DatabaseBlob(b)) if !b.is_empty() => {
            Some("image/tiff")
        }
        ("image/jpeg", RepresentationDataRef::DatabaseBlob(b)) if !b.is_empty() => {
            Some("image/jpeg")
        }
        ("image/gif", RepresentationDataRef::DatabaseBlob(b)) if !b.is_empty() => Some("image/gif"),
        ("image/webp", RepresentationDataRef::DatabaseBlob(b)) if !b.is_empty() => {
            Some("image/webp")
        }
        ("text/uri-list", RepresentationDataRef::FilePaths(paths)) if !paths.is_empty() => {
            Some("text/uri-list")
        }
        _ => None,
    }
}

/// The category for a canonical pasteable MIME (as returned by
/// [`canonical_pasteable_mime`]).
#[must_use]
fn category_for(mime: &str) -> PasteCategory {
    match mime {
        "text/uri-list" => PasteCategory::Files,
        "text/plain" => PasteCategory::PlainText,
        "text/html" => PasteCategory::Html,
        "application/rtf" => PasteCategory::RichText,
        // Every remaining canonical MIME is an image variant.
        _ => PasteCategory::Image,
    }
}

/// Enumerate the distinct representations the user can paste individually,
/// keeping the stored order (role precedence then ordinal) and the first MIME
/// of each kind.
///
/// Two representations that resolve to the same canonical MIME collapse to one
/// option: a clipboard format holds a single value, so re-publishing the first
/// (canonical-order) one is the only meaningful choice.
#[must_use]
pub fn build_paste_options(reps: &[StoredClipboardRepresentation]) -> Vec<PasteOption> {
    let mut out: Vec<PasteOption> = Vec::new();
    for rep in reps {
        if let Some(mime) = canonical_pasteable_mime(rep)
            && !out.iter().any(|opt| opt.mime == mime)
        {
            out.push(PasteOption {
                mime: mime.to_owned(),
                category: category_for(mime),
            });
        }
    }
    out
}

/// Pick the single canonical representation to publish for a requested MIME.
///
/// `reps` must be in canonical order (role precedence then ordinal, as
/// `EntryRepository::list_representations` returns). The first representation
/// whose canonical MIME matches the normalized request wins, so a duplicate
/// MIME always resolves to its primary/lowest-ordinal copy. Returns `None`
/// when the entry holds no pasteable representation for that MIME.
#[must_use]
pub fn select_representation<'a>(
    reps: &'a [StoredClipboardRepresentation],
    requested_mime: &str,
) -> Option<&'a StoredClipboardRepresentation> {
    let requested = normalize_mime(requested_mime);
    reps.iter()
        .find(|rep| canonical_pasteable_mime(rep) == Some(requested.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RepresentationRole;

    fn text_rep(
        role: RepresentationRole,
        mime: &str,
        ordinal: u32,
        body: &str,
    ) -> StoredClipboardRepresentation {
        StoredClipboardRepresentation {
            role,
            mime_type: mime.to_owned(),
            ordinal,
            data: RepresentationDataRef::InlineText(body.to_owned()),
        }
    }

    fn image_rep(mime: &str, ordinal: u32) -> StoredClipboardRepresentation {
        StoredClipboardRepresentation {
            role: RepresentationRole::Alternative,
            mime_type: mime.to_owned(),
            ordinal,
            data: RepresentationDataRef::DatabaseBlob(vec![1, 2, 3]),
        }
    }

    fn files_rep(paths: &[&str]) -> StoredClipboardRepresentation {
        StoredClipboardRepresentation {
            role: RepresentationRole::Primary,
            mime_type: "text/uri-list".to_owned(),
            ordinal: 0,
            data: RepresentationDataRef::FilePaths(paths.iter().map(|p| (*p).to_owned()).collect()),
        }
    }

    #[test]
    fn normalize_strips_params_and_case() {
        assert_eq!(normalize_mime("text/plain; charset=utf-8"), "text/plain");
        assert_eq!(normalize_mime("  TEXT/HTML  "), "text/html");
        assert_eq!(normalize_mime("image/PNG"), "image/png");
    }

    #[test]
    fn canonical_matches_publishable_pairs_only() {
        assert_eq!(
            canonical_pasteable_mime(&text_rep(
                RepresentationRole::Primary,
                "text/plain",
                0,
                "hi"
            )),
            Some("text/plain")
        );
        assert_eq!(
            canonical_pasteable_mime(&image_rep("image/png", 0)),
            Some("image/png")
        );
        assert_eq!(
            canonical_pasteable_mime(&files_rep(&["/a"])),
            Some("text/uri-list")
        );
        // An image MIME carrying inline text (corrupt row) is not pasteable.
        assert_eq!(
            canonical_pasteable_mime(&text_rep(RepresentationRole::Primary, "image/png", 0, "x")),
            None
        );
        // A MIME outside the table is not pasteable.
        assert_eq!(
            canonical_pasteable_mime(&text_rep(
                RepresentationRole::Alternative,
                "application/json",
                0,
                "{}"
            )),
            None
        );
        // The raw MIME is matched exactly (mirroring the adapters), so an
        // uppercased or parameterised form is not offered — the adapters would
        // reject it, and offering it would surface a paste error.
        assert_eq!(
            canonical_pasteable_mime(&text_rep(
                RepresentationRole::Primary,
                "TEXT/PLAIN",
                0,
                "hi"
            )),
            None
        );
        assert_eq!(
            canonical_pasteable_mime(&text_rep(
                RepresentationRole::Primary,
                "text/plain; charset=utf-8",
                0,
                "hi"
            )),
            None
        );
    }

    #[test]
    fn empty_payloads_are_not_pasteable() {
        // An empty file list or zero-byte image would blank the clipboard.
        assert_eq!(canonical_pasteable_mime(&files_rep(&[])), None);
        let empty_image = StoredClipboardRepresentation {
            role: RepresentationRole::Alternative,
            mime_type: "image/png".to_owned(),
            ordinal: 0,
            data: RepresentationDataRef::DatabaseBlob(Vec::new()),
        };
        assert_eq!(canonical_pasteable_mime(&empty_image), None);
    }

    #[test]
    fn build_options_dedupes_by_mime_keeping_order() {
        let reps = vec![
            files_rep(&["/a", "/b"]),
            image_rep("image/png", 0),
            text_rep(RepresentationRole::Alternative, "text/plain", 1, "label"),
            // A second plain-text rep collapses into the first option.
            text_rep(RepresentationRole::Alternative, "text/plain", 2, "dup"),
            // An unsupported rep is skipped entirely.
            text_rep(RepresentationRole::Alternative, "application/json", 3, "{}"),
        ];
        let options = build_paste_options(&reps);
        assert_eq!(
            options,
            vec![
                PasteOption {
                    mime: "text/uri-list".to_owned(),
                    category: PasteCategory::Files
                },
                PasteOption {
                    mime: "image/png".to_owned(),
                    category: PasteCategory::Image
                },
                PasteOption {
                    mime: "text/plain".to_owned(),
                    category: PasteCategory::PlainText
                },
            ]
        );
    }

    #[test]
    fn select_picks_first_canonical_match() {
        let first = text_rep(RepresentationRole::PlainFallback, "text/plain", 0, "first");
        let second = text_rep(RepresentationRole::Alternative, "text/plain", 1, "second");
        let reps = vec![first.clone(), second];
        let picked = select_representation(&reps, "text/plain; charset=utf-8").unwrap();
        assert_eq!(picked, &first);
        assert!(select_representation(&reps, "image/png").is_none());
    }

    #[test]
    fn category_serializes_to_stable_tokens() {
        let json = serde_json::to_string(&PasteCategory::PlainText).unwrap();
        assert_eq!(json, "\"plainText\"");
        let json = serde_json::to_string(&PasteCategory::RichText).unwrap();
        assert_eq!(json, "\"richText\"");
        let json = serde_json::to_string(&PasteCategory::Files).unwrap();
        assert_eq!(json, "\"files\"");
    }
}

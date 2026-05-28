use serde::{Deserialize, Serialize};
use url::Url;

use super::search::keyword_followed_by_whitespace;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClipboardContent {
    Text(TextContent),
    Url(UrlContent),
    Code(CodeContent),
    Image(ImageContent),
    FileList(FileListContent),
    RichText(RichTextContent),
    Unknown(UnknownContent),
}

impl ClipboardContent {
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text(TextContent::new(text.into()))
    }

    pub fn from_plain_text(text: impl Into<String>) -> Self {
        let text = text.into();
        if let Some(url) = UrlContent::parse(&text) {
            Self::Url(url)
        } else if CodeContent::looks_like_code(&text) {
            Self::Code(CodeContent::new(text, None))
        } else {
            Self::Text(TextContent::new(text))
        }
    }

    pub const fn kind(&self) -> ContentKind {
        match self {
            Self::Text(_) => ContentKind::Text,
            Self::Url(_) => ContentKind::Url,
            Self::Code(_) => ContentKind::Code,
            Self::Image(_) => ContentKind::Image,
            Self::FileList(_) => ContentKind::FileList,
            Self::RichText(_) => ContentKind::RichText,
            Self::Unknown(_) => ContentKind::Unknown,
        }
    }

    pub fn plain_text(&self) -> Option<&str> {
        match self {
            Self::Text(value) => Some(&value.text),
            Self::Url(value) => Some(&value.raw),
            Self::Code(value) => Some(&value.text),
            Self::FileList(value) => Some(&value.display_text),
            Self::RichText(value) => Some(&value.plain_text),
            Self::Unknown(value) => value.plain_text.as_deref(),
            Self::Image(_) => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextContent {
    pub text: String,
    pub char_count: usize,
    pub byte_count: usize,
    pub line_count: usize,
}

impl TextContent {
    pub fn new(text: String) -> Self {
        Self {
            char_count: text.chars().count(),
            byte_count: text.len(),
            line_count: text.lines().count().max(1),
            text,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UrlContent {
    pub raw: String,
    pub normalized: String,
    pub domain: Option<String>,
}

impl UrlContent {
    pub fn parse(raw: &str) -> Option<Self> {
        let trimmed = raw.trim();
        if !(trimmed.starts_with("http://") || trimmed.starts_with("https://")) {
            return None;
        }
        let parsed = Url::parse(trimmed).ok()?;
        // `Url::parse` already lower-cases the scheme and host per WHATWG, so
        // we only need to strip a trailing slash for dedupe parity. Lower-
        // casing the whole string here used to break case-sensitive paths
        // and query parameters (e.g. signed S3 URLs whose signature is
        // mixed-case), so the canonical form is taken verbatim from the
        // parser.
        Some(Self {
            raw: raw.to_owned(),
            normalized: parsed.as_str().trim_end_matches('/').to_owned(),
            domain: parsed.domain().map(ToOwned::to_owned),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeContent {
    pub text: String,
    pub language_hint: Option<String>,
}

impl CodeContent {
    pub const fn new(text: String, language_hint: Option<String>) -> Self {
        Self {
            text,
            language_hint,
        }
    }

    pub fn looks_like_code(text: &str) -> bool {
        // Each keyword must be a whole token *followed by ASCII whitespace*
        // so a URL path segment like `/function/docs` does not trip the
        // heuristic — in real code these keywords are always followed by an
        // identifier (`fn foo`, `class Foo`), never by `/` or `?`.
        const KEYWORDS: &[&str] = &["fn", "function", "class", "package"];
        let trimmed = text.trim();
        if !trimmed.contains('\n') {
            return false;
        }
        if KEYWORDS
            .iter()
            .any(|kw| keyword_followed_by_whitespace(trimmed, kw))
        {
            return true;
        }
        trimmed.contains("=>")
            || trimmed.contains("#include")
            || (trimmed.contains('{') && trimmed.contains('}'))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageContent {
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub byte_count: usize,
    /// Mime type of the stored payload (e.g. `image/png`, `image/tiff`).
    /// Optional so older rows that never carried a mime still deserialise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    /// In-memory bytes carried from capture → factory → storage. Always
    /// `None` after deserialisation; the storage layer reads the same data
    /// out of the dependent `entry_representations` row instead.
    #[serde(skip)]
    pub pending_bytes: Option<Vec<u8>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileListContent {
    pub paths: Vec<String>,
    pub display_text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RichTextContent {
    pub plain_text: String,
    /// Inline rich-text source (HTML or RTF) when the source pasteboard
    /// exposed it. Optional because the capture pipeline still falls back
    /// to plain text when `markup_kind` is unsupported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub markup: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub markup_kind: Option<RichTextMarkup>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RichTextMarkup {
    Html,
    Rtf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnknownContent {
    pub mime_type: Option<String>,
    pub plain_text: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum ContentKind {
    Text,
    Url,
    Code,
    Image,
    FileList,
    RichText,
    Unknown,
}

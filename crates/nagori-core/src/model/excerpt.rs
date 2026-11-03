use super::EntryId;
use super::content::ContentKind;
use super::search::PREVIEW_MAX_CHARS;

/// Raw characters a provider fetches ahead of a match.
///
/// Wider than the context the excerpt finally keeps so whitespace runs
/// (indentation, blank lines) collapsing away still leave enough text in
/// front of the match.
pub const MATCH_WINDOW_BEFORE_CHARS: usize = 64;

/// Raw characters a provider fetches after the end of a match, with the same
/// whitespace headroom as [`MATCH_WINDOW_BEFORE_CHARS`].
pub const MATCH_WINDOW_AFTER_CHARS: usize = PREVIEW_MAX_CHARS * 2;

/// A match starting within this many characters of the body's start is
/// already visible in the head preview, so no excerpt is built for it.
pub const MATCH_EXCERPT_HEAD_CHARS: usize = 24;

/// Characters of context an excerpt keeps in front of the match.
const EXCERPT_CONTEXT_CHARS: usize = 16;

/// Most query terms tried per entry when looking for a displayable match.
pub const MAX_EXCERPT_TERMS: usize = 4;

const ELLIPSIS: char = '…';

/// A bounded slice of an entry's displayable plain text around one literal
/// query match.
///
/// Providers fetch these for the final, already-truncated result page only,
/// so a search never pulls a whole body into memory just to explain a match.
/// Offsets are in Unicode scalar values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchWindow {
    pub entry_id: EntryId,
    /// Raw (uncompacted) body text around the match.
    pub text: String,
    /// Offset of the match inside `text`.
    pub match_start: usize,
    /// Length of the match.
    pub match_chars: usize,
    /// `text` begins at the body's first character.
    pub starts_body: bool,
    /// `text` ends at the body's last character.
    pub ends_body: bool,
}

impl MatchWindow {
    /// Render the window as a one-line excerpt that leads with a little
    /// context before the match, capped at [`PREVIEW_MAX_CHARS`].
    ///
    /// Code rows narrow the window to the line holding the match first, so a
    /// row reads like the line an editor would jump to rather than a run of
    /// tokens joined across lines. Returns `None` when the window does not
    /// actually contain the match offsets it claims.
    #[must_use]
    pub fn excerpt(&self, kind: ContentKind) -> Option<String> {
        let chars: Vec<char> = self.text.chars().collect();
        let match_end = self.match_start.checked_add(self.match_chars)?;
        if self.match_chars == 0 || match_end > chars.len() {
            return None;
        }
        let (mut start, mut end) = (0, chars.len());
        if kind == ContentKind::Code {
            if let Some(newline) = chars[..self.match_start].iter().rposition(|&c| c == '\n') {
                start = newline + 1;
            }
            if let Some(newline) = chars[match_end..].iter().position(|&c| c == '\n') {
                end = match_end + newline;
            }
        }
        let cut_before = start > 0 || !self.starts_body;
        let cut_after = end < chars.len() || !self.ends_body;

        let before = compact(&chars[start..self.match_start]);
        let rest = compact(&chars[self.match_start..end]);
        // `compact` trims both ends, so restore the separator between the
        // context and the match when the raw text had one.
        let gap = self.match_start > start && chars[self.match_start - 1].is_whitespace();

        let mut lead = before.as_slice();
        let mut lead_cut = cut_before;
        if lead.len() > EXCERPT_CONTEXT_CHARS {
            lead = &lead[lead.len() - EXCERPT_CONTEXT_CHARS..];
            lead_cut = true;
            // Prefer starting on a word boundary when one sits near the cut so
            // the excerpt does not open on half a word.
            if let Some(space) = lead[..EXCERPT_CONTEXT_CHARS / 2]
                .iter()
                .position(|c| c.is_whitespace())
            {
                lead = &lead[space + 1..];
            }
        }

        let mut out: Vec<char> = Vec::with_capacity(PREVIEW_MAX_CHARS);
        if lead_cut {
            out.push(ELLIPSIS);
        }
        out.extend_from_slice(lead);
        if gap && !lead.is_empty() {
            out.push(' ');
        }
        let room = PREVIEW_MAX_CHARS - out.len();
        if rest.len() > room || cut_after {
            let keep = rest.len().min(room - 1);
            out.extend_from_slice(&rest[..keep]);
            out.push(ELLIPSIS);
        } else {
            out.extend_from_slice(&rest);
        }
        Some(out.into_iter().collect())
    }
}

/// Collapse whitespace runs to a single space and trim both ends, matching
/// the shape of [`super::search::make_preview`].
fn compact(chars: &[char]) -> Vec<char> {
    let mut out = Vec::with_capacity(chars.len());
    let mut pending_space = false;
    for &c in chars {
        if c.is_whitespace() {
            pending_space = !out.is_empty();
        } else {
            if pending_space {
                out.push(' ');
                pending_space = false;
            }
            out.push(c);
        }
    }
    out
}

/// Literal terms to look for when building an excerpt for `normalized`.
///
/// The whole query comes first, then its whitespace-separated terms longest
/// first, so a multi-word query still explains a row that matched only some
/// words.
#[must_use]
pub fn excerpt_terms(normalized: &str) -> Vec<String> {
    let whole = normalized.trim();
    if whole.is_empty() {
        return Vec::new();
    }
    let mut terms = vec![whole.to_owned()];
    let mut words: Vec<&str> = whole.split_whitespace().collect();
    if words.len() > 1 {
        words.sort_by_key(|word| std::cmp::Reverse(word.chars().count()));
        for word in words {
            if terms.len() == MAX_EXCERPT_TERMS {
                break;
            }
            if !terms.iter().any(|term| term == word) {
                terms.push(word.to_owned());
            }
        }
    }
    terms
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(text: &str, needle: &str, starts_body: bool, ends_body: bool) -> MatchWindow {
        let byte = text.find(needle).expect("needle present");
        MatchWindow {
            entry_id: EntryId::new(),
            text: text.to_owned(),
            match_start: text[..byte].chars().count(),
            match_chars: needle.chars().count(),
            starts_body,
            ends_body,
        }
    }

    #[test]
    fn excerpt_leads_with_context_before_the_match() {
        let w = window(
            "議事録 2026年11月 定例 参加者 全員 本日の議題として接続プールの上限を20に変更する件",
            "接続プール",
            true,
            true,
        );
        let excerpt = w.excerpt(ContentKind::Text).unwrap();
        assert!(excerpt.starts_with('…'), "{excerpt}");
        assert!(
            excerpt.contains("接続プールの上限を20に変更する件"),
            "{excerpt}"
        );
        assert!(!excerpt.ends_with('…'), "{excerpt}");
        assert!(!excerpt.contains("議事録"), "{excerpt}");
    }

    #[test]
    fn excerpt_snaps_to_a_word_boundary_and_compacts_whitespace() {
        let w = window(
            "lorem ipsum dolor sit amet,\n\n   the   connection pool limit was raised",
            "connection",
            true,
            true,
        );
        assert_eq!(
            w.excerpt(ContentKind::Text).unwrap(),
            "…sit amet, the connection pool limit was raised",
        );
    }

    #[test]
    fn excerpt_marks_a_window_cut_from_a_longer_body() {
        let w = window(
            "prefix text with the needle and more",
            "needle",
            false,
            false,
        );
        let excerpt = w.excerpt(ContentKind::Text).unwrap();
        assert!(
            excerpt.starts_with('…') && excerpt.ends_with('…'),
            "{excerpt}"
        );
    }

    #[test]
    fn excerpt_is_capped_at_the_preview_length() {
        let text = format!("{} needle {}", "a".repeat(40), "b ".repeat(400));
        let w = window(&text, "needle", true, true);
        let excerpt = w.excerpt(ContentKind::Text).unwrap();
        assert_eq!(excerpt.chars().count(), PREVIEW_MAX_CHARS);
        assert!(excerpt.ends_with('…'));
        assert!(excerpt.contains("needle"));
    }

    #[test]
    fn code_excerpt_keeps_only_the_matching_line() {
        let w = window(
            "fn main() {\n    let pool = Pool::new();\n    pool.set_max_connections(20);\n}\n",
            "set_max_connections",
            true,
            true,
        );
        assert_eq!(
            w.excerpt(ContentKind::Code).unwrap(),
            "…pool.set_max_connections(20);…",
        );
    }

    #[test]
    fn excerpt_rejects_offsets_outside_the_window() {
        let mut w = window("short", "short", true, true);
        w.match_start = 3;
        assert_eq!(w.excerpt(ContentKind::Text), None);
        w.match_start = 0;
        w.match_chars = 0;
        assert_eq!(w.excerpt(ContentKind::Text), None);
    }

    #[test]
    fn excerpt_terms_try_the_whole_query_then_longest_words() {
        assert_eq!(excerpt_terms("  "), Vec::<String>::new());
        assert_eq!(excerpt_terms("pool"), vec!["pool"]);
        assert_eq!(
            excerpt_terms("db connection pool db"),
            vec!["db connection pool db", "connection", "pool", "db"],
        );
    }
}

use serde::{Deserialize, Serialize};

use crate::{AppError, Result, text::normalize_text};

/// Longest name or alias, in Unicode scalar values.
pub const LABEL_MAX_CHARS: usize = 80;

/// Most aliases one entry can carry.
pub const LABEL_MAX_ALIASES: usize = 8;

/// A user-given name for an entry, plus extra words that should find it.
///
/// Names turn a pinned entry into something recalled by the user's own words
/// ("PR review request", "shipping address") rather than by its body. They
/// live beside the entry, not in its search document, so renaming never
/// rewrites the body index and a body edit never drops the name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntryLabel {
    pub name: String,
    pub aliases: Vec<String>,
}

impl EntryLabel {
    /// Validate and tidy a name and its aliases.
    ///
    /// Both are trimmed with inner whitespace runs collapsed; empty aliases and
    /// aliases that repeat the name or each other (after normalization) are
    /// dropped.
    ///
    /// # Errors
    ///
    /// [`AppError::InvalidInput`] when the name is empty, or when the name, an
    /// alias, or the alias count exceeds its limit.
    pub fn new(name: &str, aliases: &[String]) -> Result<Self> {
        let name = tidy(name);
        if name.is_empty() {
            return Err(AppError::InvalidInput("name must not be empty".to_owned()));
        }
        check_length(&name, "name")?;
        let mut seen = vec![normalize_text(&name)];
        let mut kept = Vec::new();
        for alias in aliases {
            let alias = tidy(alias);
            if alias.is_empty() {
                continue;
            }
            check_length(&alias, "alias")?;
            let normalized = normalize_text(&alias);
            if seen.contains(&normalized) {
                continue;
            }
            seen.push(normalized);
            kept.push(alias);
        }
        if kept.len() > LABEL_MAX_ALIASES {
            return Err(AppError::InvalidInput(format!(
                "at most {LABEL_MAX_ALIASES} aliases are allowed"
            )));
        }
        Ok(Self {
            name,
            aliases: kept,
        })
    }

    /// Newline-delimited normalized name and aliases, with a leading and
    /// trailing newline, so a store can test a whole-word hit with
    /// `"\n" || query || "\n"`, a prefix with `"\n" || query`, and any other
    /// hit with a plain substring test. Normalized text never contains a
    /// newline, so the delimiter cannot occur inside a term.
    #[must_use]
    pub fn search_text(&self) -> String {
        let mut out = String::from("\n");
        for term in std::iter::once(&self.name).chain(&self.aliases) {
            out.push_str(&normalize_text(term));
            out.push('\n');
        }
        out
    }
}

fn tidy(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn check_length(value: &str, what: &str) -> Result<()> {
    if value.chars().count() > LABEL_MAX_CHARS {
        return Err(AppError::InvalidInput(format!(
            "{what} must be at most {LABEL_MAX_CHARS} characters"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn aliases(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn tidies_and_dedups_name_and_aliases() {
        let label = EntryLabel::new(
            "  PR  review\trequest ",
            &aliases(&[
                "",
                "pr review request",
                "レビュー依頼",
                " レビュー依頼 ",
                "review",
            ]),
        )
        .unwrap();
        assert_eq!(label.name, "PR review request");
        assert_eq!(label.aliases, vec!["レビュー依頼", "review"]);
        assert_eq!(
            label.search_text(),
            "\npr review request\nレビュー依頼\nreview\n"
        );
    }

    #[test]
    fn rejects_empty_or_oversized_labels() {
        assert!(EntryLabel::new("   ", &[]).is_err());
        assert!(EntryLabel::new(&"a".repeat(LABEL_MAX_CHARS + 1), &[]).is_err());
        assert!(EntryLabel::new("ok", &aliases(&[&"b".repeat(LABEL_MAX_CHARS + 1)])).is_err());
        let many: Vec<String> = (0..=LABEL_MAX_ALIASES)
            .map(|i| format!("alias {i}"))
            .collect();
        assert!(EntryLabel::new("ok", &many).is_err());
        assert!(EntryLabel::new("ok", &many[..LABEL_MAX_ALIASES]).is_ok());
    }
}

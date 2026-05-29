//! Longest-common-prefix delta-isation of partial snapshots.
//!
//! `FoundationModels`' `streamResponse` yields growing *partial snapshots*,
//! not deltas. The usual case is a strict append, but the
//! model can re-write earlier text (guided output, punctuation fix-ups, CJK
//! re-composition), so we compare each snapshot against the previous one and
//! emit either the appended tail or a full replacement. All splits land on
//! `char` boundaries, so emitted strings are always valid UTF-8.

/// The result of comparing a new snapshot against the previous one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SnapshotDelta {
    /// Snapshot is byte-identical to the previous one; nothing to emit.
    Unchanged,
    /// Snapshot strictly extends the previous one; carries the appended tail.
    Append(String),
    /// Snapshot diverged from the previous prefix; carries the full snapshot.
    Replace(String),
}

/// Length, in bytes, of the longest common prefix of `a` and `b` measured on
/// `char` boundaries (so the returned index is always a valid split point for
/// both strings).
fn common_prefix_len(a: &str, b: &str) -> usize {
    let mut matched = 0;
    for ((idx, ca), cb) in a.char_indices().zip(b.chars()) {
        if ca != cb {
            return idx;
        }
        matched = idx + ca.len_utf8();
    }
    matched
}

/// Diff `next` against `prev`, returning the [`SnapshotDelta`] to emit.
///
/// ```
/// use nagori_ai_apple::{diff_snapshot, SnapshotDelta};
///
/// assert_eq!(diff_snapshot("ab", "ab"), SnapshotDelta::Unchanged);
/// assert_eq!(diff_snapshot("ab", "abc"), SnapshotDelta::Append("c".to_owned()));
/// assert_eq!(diff_snapshot("abc", "abx"), SnapshotDelta::Replace("abx".to_owned()));
/// ```
#[must_use]
pub fn diff_snapshot(prev: &str, next: &str) -> SnapshotDelta {
    if prev == next {
        return SnapshotDelta::Unchanged;
    }
    let lcp = common_prefix_len(prev, next);
    if lcp == prev.len() {
        // `next` extends `prev` (the common prefix covers all of `prev`).
        SnapshotDelta::Append(next[lcp..].to_owned())
    } else {
        // The shared prefix is shorter than `prev`: `next` rewrote earlier
        // text (or shrank), so the consumer must replace its buffer.
        SnapshotDelta::Replace(next.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::{SnapshotDelta, common_prefix_len, diff_snapshot};

    #[test]
    fn identical_is_unchanged() {
        assert_eq!(diff_snapshot("hello", "hello"), SnapshotDelta::Unchanged);
        assert_eq!(diff_snapshot("", ""), SnapshotDelta::Unchanged);
    }

    #[test]
    fn first_snapshot_appends_everything() {
        assert_eq!(
            diff_snapshot("", "hello"),
            SnapshotDelta::Append("hello".to_owned())
        );
    }

    #[test]
    fn strict_extension_is_append() {
        assert_eq!(
            diff_snapshot("foo", "foobar"),
            SnapshotDelta::Append("bar".to_owned())
        );
    }

    #[test]
    fn divergence_is_replace() {
        assert_eq!(
            diff_snapshot("foobar", "foobaz"),
            SnapshotDelta::Replace("foobaz".to_owned())
        );
    }

    #[test]
    fn shrinking_snapshot_is_replace() {
        assert_eq!(
            diff_snapshot("foobar", "foo"),
            SnapshotDelta::Replace("foo".to_owned())
        );
    }

    #[test]
    fn multibyte_append_splits_on_char_boundary() {
        // "あい" + "う": the appended tail must be the full 3-byte char.
        assert_eq!(
            diff_snapshot("あい", "あいう"),
            SnapshotDelta::Append("う".to_owned())
        );
    }

    #[test]
    fn multibyte_divergence_is_replace() {
        assert_eq!(
            diff_snapshot("あいう", "あいえ"),
            SnapshotDelta::Replace("あいえ".to_owned())
        );
    }

    #[test]
    fn emoji_append_splits_on_char_boundary() {
        assert_eq!(
            diff_snapshot("hi ", "hi 🦀"),
            SnapshotDelta::Append("🦀".to_owned())
        );
    }

    #[test]
    fn common_prefix_len_lands_on_char_boundary() {
        // Shared "あい" is 6 bytes; divergence starts at byte 6.
        assert_eq!(common_prefix_len("あいう", "あいえ"), 6);
        // Shared prefix shorter than both, multibyte first char differs.
        assert_eq!(common_prefix_len("🦀x", "🦞x"), 0);
        // Full prefix of the shorter string.
        assert_eq!(common_prefix_len("ab", "abc"), 2);
        assert_eq!(common_prefix_len("abc", "ab"), 2);
    }
}

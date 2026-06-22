use nagori_core::{
    ContentKind, EntryId, RankReason, Ranker, RecentOrder, SearchCandidate, SearchResult,
};
use time::OffsetDateTime;
use tracing::debug;

/// Concrete [`Ranker`] used by `nagori-storage` to score candidate entries.
///
/// Constructed as a unit struct so callers can pass `&DefaultRanker` to
/// [`SearchService::new`](nagori_core::SearchService::new) without any
/// allocations.
#[derive(Debug, Default, Clone, Copy)]
pub struct DefaultRanker;

impl Ranker for DefaultRanker {
    // Score arithmetic intentionally uses f32 with usize/u32 inputs;
    // precision loss is irrelevant for ranking.
    #[allow(clippy::cast_precision_loss)]
    fn rank(
        &self,
        query: &str,
        candidate: SearchCandidate,
        fts_score: f32,
        ngram_overlap: f32,
        now: OffsetDateTime,
        recent_order: RecentOrder,
    ) -> Option<SearchResult> {
        let text = &candidate.normalized_text;
        let query = query.trim();
        if query.is_empty() {
            return Some(rank_recent(candidate, recent_order));
        }

        // Defend against NaN/Inf coming from the storage layer (e.g. an
        // unexpectedly malformed bm25 row) so the final `score > 0.0` cull
        // below does not silently drop otherwise valid candidates.
        let fts_score = sanitize_signal(fts_score, "fts_score", candidate.entry_id);
        let ngram_overlap = sanitize_signal(ngram_overlap, "ngram_overlap", candidate.entry_id);

        let mut score = 0.0;
        let mut reasons = Vec::new();
        if text == query {
            score += 100.0;
            reasons.push(RankReason::ExactMatch);
        }
        if text.starts_with(query) {
            score += 60.0;
            reasons.push(RankReason::PrefixMatch);
        }
        if text.contains(query) {
            score += 35.0;
            reasons.push(RankReason::SubstringMatch);
        }
        // FTS5 bm25 is non-positive (0 == no match, negative == better
        // match). Raw values are typically small (e.g. -0.5 .. -5.0); scale
        // so a clear FTS hit reliably contributes meaningful score before
        // length penalties.
        if fts_score < 0.0 {
            let magnitude = (-fts_score).clamp(0.0, 10.0);
            let contribution = (magnitude * 10.0).min(50.0);
            score += contribution;
            reasons.push(RankReason::FullTextMatch);
        }
        if ngram_overlap > 0.0 {
            score += (ngram_overlap * 40.0).min(40.0);
            reasons.push(RankReason::NgramMatch);
        }

        let age_hours = (now - candidate.created_at).whole_hours();
        if age_hours < 24 {
            score += 20.0;
            reasons.push(RankReason::Recent);
        } else if age_hours < 24 * 7 {
            score += 10.0;
            reasons.push(RankReason::Recent);
        }

        if candidate.use_count > 0 {
            score += (candidate.use_count as f32).ln_1p().min(15.0);
            reasons.push(RankReason::FrequentlyUsed);
        }
        if candidate.pinned {
            score += 25.0;
            reasons.push(RankReason::Pinned);
        }
        if matches!(candidate.content_kind, ContentKind::Code) && query.chars().count() > 1 {
            score += 3.0;
            // Nagori leans developer-oriented: a code snippet copied from an
            // editor / terminal / IDE is a stronger recall target than the
            // same text pasted from a chat window. Keep the bump small and
            // strictly code-scoped so it nudges ties without letting ordinary
            // prose copied in a dev app outrank a real text match.
            if is_developer_source_app(&candidate) {
                score += 2.0;
            }
        }

        // Length penalty: very long entries receive a small score reduction
        // so tightly matching short snippets outrank incidental substring
        // hits in multi-megabyte blobs. Capped at half of the current score
        // so a real match never gets pushed to zero (and dropped) by length
        // alone.
        let text_len = text.chars().count();
        if text_len > 200 && score > 0.0 {
            let extra = (text_len - 200) as f32;
            let penalty = ((extra / 2_000.0).min(1.0) * 15.0).min(score / 2.0);
            score -= penalty;
        }

        (score > 0.0).then(|| result(candidate, score, reasons))
    }
}

/// Replace NaN/Inf in upstream ranking signals with `0.0`.
///
/// `score > 0.0` returns `false` for NaN, so an unsanitized NaN propagating
/// from a corrupt FTS row would silently drop a candidate that might still
/// match on substring or pin signals. Logging at `debug!` keeps the cost
/// negligible while leaving a trail for later diagnosis.
fn sanitize_signal(value: f32, signal: &'static str, entry_id: EntryId) -> f32 {
    if value.is_finite() {
        value
    } else {
        debug!(
            target: "nagori::search::ranker",
            entry_id = %entry_id,
            signal,
            value = ?value,
            "non-finite ranking signal coerced to 0.0"
        );
        0.0
    }
}

#[allow(clippy::cast_precision_loss)]
fn rank_recent(candidate: SearchCandidate, recent_order: RecentOrder) -> SearchResult {
    let mut score = 1.0;
    let mut reasons = vec![RankReason::Recent];
    match recent_order {
        RecentOrder::ByRecency => {}
        RecentOrder::ByUseCount => {
            if candidate.use_count > 0 {
                score += (candidate.use_count as f32).ln_1p().min(15.0);
                reasons.push(RankReason::FrequentlyUsed);
            }
        }
        RecentOrder::PinnedFirstThenRecency => {
            if candidate.pinned {
                score += 25.0;
                reasons.push(RankReason::Pinned);
            }
        }
    }
    result(candidate, score, reasons)
}

fn result(candidate: SearchCandidate, score: f32, rank_reason: Vec<RankReason>) -> SearchResult {
    // The candidate projection already carries the canonical code language
    // and image dimensions, so the result row shows the same metadata the
    // preview pane does without a second round-trip.
    SearchResult {
        entry_id: candidate.entry_id,
        score,
        rank_reason,
        preview: candidate.preview,
        content_kind: candidate.content_kind,
        created_at: candidate.created_at,
        pinned: candidate.pinned,
        sensitivity: candidate.sensitivity,
        source_app_name: candidate.source_app_name,
        language: candidate.language,
        image_width: candidate.image_width,
        image_height: candidate.image_height,
    }
}

/// Whether the clip's source app is a developer tool (editor, terminal, IDE).
///
/// Matched case-insensitively against a small substring set covering the
/// common macOS / Windows / Linux apps. Substring (not exact) so variants
/// like "Visual Studio Code - Insiders" or "iTerm2" still match; the set is
/// deliberately narrow to avoid sweeping in general-purpose apps.
fn is_developer_source_app(candidate: &SearchCandidate) -> bool {
    const DEV_APP_MARKERS: &[&str] = &[
        "code",     // VS Code / VSCodium / "Visual Studio Code"
        "terminal", // macOS Terminal, GNOME Terminal
        "iterm",    // iTerm2
        "alacritty",
        "kitty",
        "wezterm",
        "warp",
        "ghostty",
        "jetbrains",
        "intellij",
        "pycharm",
        "goland",
        "rustrover",
        "webstorm",
        "android studio",
        "xcode",
        "sublime text",
        "zed",
        "neovim",
        "nvim",
        "vim",
        "emacs",
        "windows terminal",
        "powershell",
        "konsole",
    ];
    let Some(name) = candidate.source_app_name.as_deref() else {
        return false;
    };
    let lower = name.to_ascii_lowercase();
    DEV_APP_MARKERS.iter().any(|marker| lower.contains(marker))
}

#[cfg(test)]
mod tests {
    use nagori_core::EntryFactory;
    use time::Duration;

    use super::*;
    use crate::normalize_text;

    fn entry(text: &str) -> SearchCandidate {
        let mut entry = EntryFactory::from_text(text);
        entry.search.normalized_text = normalize_text(text);
        SearchCandidate::from_entry(&entry)
    }

    fn rank(
        query: &str,
        candidate: SearchCandidate,
        fts_score: f32,
        ngram_overlap: f32,
        now: OffsetDateTime,
        recent_order: RecentOrder,
    ) -> Option<SearchResult> {
        DefaultRanker.rank(
            query,
            candidate,
            fts_score,
            ngram_overlap,
            now,
            recent_order,
        )
    }

    fn now() -> OffsetDateTime {
        OffsetDateTime::now_utc()
    }

    #[test]
    fn empty_query_returns_recent_result() {
        let result = rank(
            "   ",
            entry("anything"),
            0.0,
            0.0,
            now(),
            RecentOrder::ByRecency,
        )
        .expect("recent result");

        assert!((result.score - 1.0).abs() < f32::EPSILON);
        assert_eq!(result.rank_reason, vec![RankReason::Recent]);
    }

    #[test]
    fn exact_prefix_and_substring_reasons_stack() {
        let result = rank(
            "clipboard",
            entry("clipboard"),
            0.0,
            0.0,
            now(),
            RecentOrder::ByRecency,
        )
        .expect("exact match");

        assert!(result.rank_reason.contains(&RankReason::ExactMatch));
        assert!(result.rank_reason.contains(&RankReason::PrefixMatch));
        assert!(result.rank_reason.contains(&RankReason::SubstringMatch));
        assert!(result.score > 190.0);
    }

    #[test]
    fn pinned_and_frequent_entries_gain_reasons() {
        let mut entry = entry("clipboard manager");
        entry.pinned = true;
        entry.use_count = 9;

        let result = rank("manager", entry, 0.0, 0.0, now(), RecentOrder::ByRecency)
            .expect("substring match");

        assert!(result.pinned);
        assert!(result.rank_reason.contains(&RankReason::Pinned));
        assert!(result.rank_reason.contains(&RankReason::FrequentlyUsed));
    }

    #[test]
    fn fts_and_ngram_signals_are_reported() {
        let result = rank(
            "gamma",
            entry("alpha beta"),
            -2.0,
            0.5,
            now(),
            RecentOrder::ByRecency,
        )
        .expect("fts/ngram match");

        assert!(result.rank_reason.contains(&RankReason::FullTextMatch));
        assert!(result.rank_reason.contains(&RankReason::NgramMatch));
        assert!(result.score >= 40.0);
    }

    #[test]
    fn old_non_matching_candidate_is_dropped() {
        let now = OffsetDateTime::now_utc();
        let mut entry = entry("alpha beta");
        entry.created_at = now - Duration::days(8);

        assert!(rank("missing", entry, 0.0, 0.0, now, RecentOrder::ByRecency).is_none());
    }

    #[test]
    fn long_entry_penalty_does_not_remove_real_match() {
        let text = format!("needle {}", "x".repeat(4_000));
        let result = rank(
            "needle",
            entry(&text),
            0.0,
            0.0,
            now(),
            RecentOrder::ByRecency,
        )
        .expect("long substring match");

        assert!(result.score > 0.0);
        assert!(result.rank_reason.contains(&RankReason::PrefixMatch));
    }

    #[test]
    fn code_from_developer_source_app_outranks_same_code_from_chat() {
        // A code snippet copied from an editor is a stronger recall target
        // than the identical snippet pasted from a chat app, but the bump is
        // small and strictly code-scoped so it only nudges ties.
        let body = "fn main() {\n    handler();\n}";
        let mut from_editor = entry(body);
        from_editor.source_app_name = Some("Visual Studio Code".to_owned());
        let mut from_chat = entry(body);
        from_chat.source_app_name = Some("Slack".to_owned());
        assert_eq!(from_editor.content_kind, ContentKind::Code);

        let editor = rank(
            "handler",
            from_editor,
            0.0,
            0.0,
            now(),
            RecentOrder::ByRecency,
        )
        .expect("code match");
        let chat = rank(
            "handler",
            from_chat,
            0.0,
            0.0,
            now(),
            RecentOrder::ByRecency,
        )
        .expect("code match");
        assert!(
            editor.score > chat.score,
            "developer-source code should score higher: editor={} chat={}",
            editor.score,
            chat.score
        );
    }

    #[test]
    fn developer_source_boost_does_not_apply_to_plain_text() {
        // The dev-source bump is gated on `ContentKind::Code`; a plain text
        // clip from a terminal must not get it, so ordinary prose copied in a
        // dev app never outranks a real text match elsewhere.
        let body = "just a plain sentence about handler stuff";
        let mut from_term = entry(body);
        from_term.source_app_name = Some("iTerm2".to_owned());
        let plain = entry(body);
        assert_eq!(from_term.content_kind, ContentKind::Text);

        let term = rank(
            "handler",
            from_term,
            0.0,
            0.0,
            now(),
            RecentOrder::ByRecency,
        )
        .expect("text match");
        let none =
            rank("handler", plain, 0.0, 0.0, now(), RecentOrder::ByRecency).expect("text match");
        assert!(
            (term.score - none.score).abs() < f32::EPSILON,
            "source app must not affect non-code ranking",
        );
    }

    #[test]
    fn single_char_cjk_query_does_not_trigger_code_boost() {
        // The code boost gates on the query *character* count, not its byte
        // length. `検` is a single character but three UTF-8 bytes, so a
        // byte-length check would wrongly fire the boost on a single-character
        // CJK query — exactly the kind of query the gate is meant to exclude.
        let body = "検索エンジン";
        let mut code = entry(body);
        code.content_kind = ContentKind::Code;
        let mut text = entry(body);
        text.content_kind = ContentKind::Text;

        let code_score = rank("検", code, 0.0, 0.0, now(), RecentOrder::ByRecency)
            .expect("substring match")
            .score;
        let text_score = rank("検", text, 0.0, 0.0, now(), RecentOrder::ByRecency)
            .expect("substring match")
            .score;
        assert!(
            (code_score - text_score).abs() < f32::EPSILON,
            "single-char CJK query must not boost Code: code={code_score} text={text_score}",
        );
    }

    #[test]
    fn multi_char_cjk_query_still_boosts_code() {
        // A genuine multi-character CJK query keeps the code bump so the gate
        // only suppresses single-character noise, not real CJK matches.
        let body = "検索エンジン";
        let mut code = entry(body);
        code.content_kind = ContentKind::Code;
        let mut text = entry(body);
        text.content_kind = ContentKind::Text;

        let code_score = rank("検索", code, 0.0, 0.0, now(), RecentOrder::ByRecency)
            .expect("substring match")
            .score;
        let text_score = rank("検索", text, 0.0, 0.0, now(), RecentOrder::ByRecency)
            .expect("substring match")
            .score;
        assert!(
            code_score > text_score,
            "multi-char CJK query should still boost Code: code={code_score} text={text_score}",
        );
    }

    #[test]
    fn non_finite_signals_do_not_drop_real_match() {
        // NaN fts_score and ngram_overlap must be coerced to 0 so the
        // substring match still survives the `score > 0.0` cull.
        let result = rank(
            "needle",
            entry("needle text"),
            f32::NAN,
            f32::INFINITY,
            now(),
            RecentOrder::ByRecency,
        )
        .expect("substring match should survive non-finite ranking signals");
        assert!(result.score.is_finite());
        assert!(result.score > 0.0);
        assert!(!result.rank_reason.contains(&RankReason::FullTextMatch));
        assert!(!result.rank_reason.contains(&RankReason::NgramMatch));
    }
}

use nagori_core::{
    ClipboardEntry, ContentKind, EntryId, RankReason, Ranker, RecentOrder, SearchResult,
};
use time::OffsetDateTime;
use tracing::debug;

#[derive(Debug, Clone)]
pub struct RankedCandidate {
    pub entry: ClipboardEntry,
    /// `bm25` score from FTS5 (lower is better in `SQLite`). Pass the raw value
    /// from `bm25(search_fts)`; the ranker inverts it. Use `0.0` when the
    /// candidate did not come through FTS at all.
    pub fts_score: f32,
    /// Ratio in `[0.0, 1.0]` of query ngrams matched in the document. The
    /// caller is responsible for computing this; the ranker performs no I/O
    /// or fallback ngram generation.
    pub ngram_overlap: f32,
}

/// Adapter that lets the canonical [`rank_candidate`] function be plugged into
/// `nagori-core`'s [`Ranker`] trait.
///
/// Constructed as a unit struct so callers can pass `&DefaultRanker` to
/// [`SearchService::new`](nagori_core::SearchService::new) without any
/// allocations.
#[derive(Debug, Default, Clone, Copy)]
pub struct DefaultRanker;

impl Ranker for DefaultRanker {
    fn rank(
        &self,
        query: &str,
        entry: ClipboardEntry,
        fts_score: f32,
        ngram_overlap: f32,
        now: OffsetDateTime,
        recent_order: RecentOrder,
    ) -> Option<SearchResult> {
        rank_candidate(
            query,
            RankedCandidate {
                entry,
                fts_score,
                ngram_overlap,
            },
            now,
            recent_order,
        )
    }
}

// Score arithmetic intentionally uses f32 with usize/u32 inputs; precision loss is irrelevant for ranking.
#[allow(clippy::cast_precision_loss)]
pub fn rank_candidate(
    query: &str,
    candidate: RankedCandidate,
    now: OffsetDateTime,
    recent_order: RecentOrder,
) -> Option<SearchResult> {
    let text = &candidate.entry.search.normalized_text;
    let query = query.trim();
    if query.is_empty() {
        return Some(rank_recent(candidate.entry, recent_order));
    }

    // Defend against NaN/Inf coming from the storage layer (e.g. an
    // unexpectedly malformed bm25 row) so the final `score > 0.0` cull below
    // does not silently drop otherwise valid candidates.
    let fts_score = sanitize_signal(candidate.fts_score, "fts_score", candidate.entry.id);
    let ngram_overlap =
        sanitize_signal(candidate.ngram_overlap, "ngram_overlap", candidate.entry.id);

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
    // FTS5 bm25 is non-positive (0 == no match, negative == better match).
    // Raw values are typically small (e.g. -0.5 .. -5.0); scale so a clear
    // FTS hit reliably contributes meaningful score before length penalties.
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

    let age_hours = (now - candidate.entry.metadata.created_at).whole_hours();
    if age_hours < 24 {
        score += 20.0;
        reasons.push(RankReason::Recent);
    } else if age_hours < 24 * 7 {
        score += 10.0;
        reasons.push(RankReason::Recent);
    }

    if candidate.entry.metadata.use_count > 0 {
        score += (candidate.entry.metadata.use_count as f32)
            .ln_1p()
            .min(15.0);
        reasons.push(RankReason::FrequentlyUsed);
    }
    if candidate.entry.lifecycle.pinned {
        score += 25.0;
        reasons.push(RankReason::Pinned);
    }
    if matches!(candidate.entry.content_kind(), ContentKind::Code) && query.len() > 1 {
        score += 3.0;
    }

    // Length penalty: very long entries receive a small score reduction so
    // tightly matching short snippets outrank incidental substring hits in
    // multi-megabyte blobs. Capped at half of the current score so a real
    // match never gets pushed to zero (and dropped) by length alone.
    let text_len = text.chars().count();
    if text_len > 200 && score > 0.0 {
        let extra = (text_len - 200) as f32;
        let penalty = ((extra / 2_000.0).min(1.0) * 15.0).min(score / 2.0);
        score -= penalty;
    }

    (score > 0.0).then(|| result(candidate.entry, score, reasons))
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
fn rank_recent(entry: ClipboardEntry, recent_order: RecentOrder) -> SearchResult {
    let mut score = 1.0;
    let mut reasons = vec![RankReason::Recent];
    match recent_order {
        RecentOrder::ByRecency => {}
        RecentOrder::ByUseCount => {
            if entry.metadata.use_count > 0 {
                score += (entry.metadata.use_count as f32).ln_1p().min(15.0);
                reasons.push(RankReason::FrequentlyUsed);
            }
        }
        RecentOrder::PinnedFirstThenRecency => {
            if entry.lifecycle.pinned {
                score += 25.0;
                reasons.push(RankReason::Pinned);
            }
        }
    }
    result(entry, score, reasons)
}

fn result(entry: ClipboardEntry, score: f32, rank_reason: Vec<RankReason>) -> SearchResult {
    let content_kind = entry.content_kind();
    let source_app_name = entry
        .metadata
        .source
        .as_ref()
        .and_then(|source| source.name.clone());
    SearchResult {
        entry_id: entry.id,
        score,
        rank_reason,
        preview: entry.search.preview,
        content_kind,
        created_at: entry.metadata.created_at,
        pinned: entry.lifecycle.pinned,
        sensitivity: entry.sensitivity,
        source_app_name,
    }
}

#[cfg(test)]
mod tests {
    use nagori_core::{EntryFactory, EntryLifecycle};
    use time::Duration;

    use super::*;
    use crate::normalize_text;

    fn candidate(text: &str) -> RankedCandidate {
        let mut entry = EntryFactory::from_text(text);
        entry.search.normalized_text = normalize_text(text);
        RankedCandidate {
            entry,
            fts_score: 0.0,
            ngram_overlap: 0.0,
        }
    }

    fn now() -> OffsetDateTime {
        OffsetDateTime::now_utc()
    }

    #[test]
    fn empty_query_returns_recent_result() {
        let result = rank_candidate("   ", candidate("anything"), now(), RecentOrder::ByRecency)
            .expect("recent result");

        assert!((result.score - 1.0).abs() < f32::EPSILON);
        assert_eq!(result.rank_reason, vec![RankReason::Recent]);
    }

    #[test]
    fn exact_prefix_and_substring_reasons_stack() {
        let result = rank_candidate(
            "clipboard",
            candidate("clipboard"),
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
        let mut candidate = candidate("clipboard manager");
        candidate.entry.lifecycle = EntryLifecycle {
            pinned: true,
            ..Default::default()
        };
        candidate.entry.metadata.use_count = 9;

        let result = rank_candidate("manager", candidate, now(), RecentOrder::ByRecency)
            .expect("substring match");

        assert!(result.pinned);
        assert!(result.rank_reason.contains(&RankReason::Pinned));
        assert!(result.rank_reason.contains(&RankReason::FrequentlyUsed));
    }

    #[test]
    fn fts_and_ngram_signals_are_reported() {
        let mut candidate = candidate("alpha beta");
        candidate.fts_score = -2.0;
        candidate.ngram_overlap = 0.5;

        let result = rank_candidate("gamma", candidate, now(), RecentOrder::ByRecency)
            .expect("fts/ngram match");

        assert!(result.rank_reason.contains(&RankReason::FullTextMatch));
        assert!(result.rank_reason.contains(&RankReason::NgramMatch));
        assert!(result.score >= 40.0);
    }

    #[test]
    fn old_non_matching_candidate_is_dropped() {
        let now = OffsetDateTime::now_utc();
        let mut candidate = candidate("alpha beta");
        candidate.entry.metadata.created_at = now - Duration::days(8);

        assert!(rank_candidate("missing", candidate, now, RecentOrder::ByRecency).is_none());
    }

    #[test]
    fn long_entry_penalty_does_not_remove_real_match() {
        let text = format!("needle {}", "x".repeat(4_000));
        let result = rank_candidate("needle", candidate(&text), now(), RecentOrder::ByRecency)
            .expect("long substring match");

        assert!(result.score > 0.0);
        assert!(result.rank_reason.contains(&RankReason::PrefixMatch));
    }

    #[test]
    fn non_finite_signals_do_not_drop_real_match() {
        // NaN fts_score and ngram_overlap must be coerced to 0 so the
        // substring match still survives the `score > 0.0` cull.
        let mut cand = candidate("needle text");
        cand.fts_score = f32::NAN;
        cand.ngram_overlap = f32::INFINITY;

        let result = rank_candidate("needle", cand, now(), RecentOrder::ByRecency)
            .expect("substring match should survive non-finite ranking signals");
        assert!(result.score.is_finite());
        assert!(result.score > 0.0);
        assert!(!result.rank_reason.contains(&RankReason::FullTextMatch));
        assert!(!result.rank_reason.contains(&RankReason::NgramMatch));
    }
}

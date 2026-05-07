use std::collections::HashMap;

use async_trait::async_trait;
use time::OffsetDateTime;

use crate::{
    AppError, ClipboardEntry, EntryId, RecentOrder, Result, SearchFilters, SearchMode, SearchQuery,
    SearchResult, text::normalize_text,
};

/// Maximum number of `SearchResult`s returned regardless of caller-requested
/// `limit`. Mirrors the previous `SqliteStore::search` clamp.
const MAX_RESULT_LIMIT: usize = 200;

/// Multiplier applied to the requested `limit` when fetching candidates from
/// the provider so the ranker has enough headroom to pick winners after
/// dedup + score-sort.
const CANDIDATE_OVERSAMPLE: usize = 8;

/// FTS hit returned by a [`SearchCandidateProvider`].
///
/// `fts_score` carries the raw `bm25` value (lower is better in `SQLite`).
/// The [`Ranker`] inverts it; the provider must not.
#[derive(Debug, Clone)]
pub struct FtsCandidate {
    pub entry: ClipboardEntry,
    pub fts_score: f32,
}

/// Ngram hit returned by a [`SearchCandidateProvider`].
///
/// `ngram_overlap` is the ratio in `[0.0, 1.0]` of query ngrams matched in the
/// document. The provider is responsible for the heuristic of when ngram
/// matching is worthwhile (e.g. CJK or short queries) and may return an empty
/// vector when it isn't.
#[derive(Debug, Clone)]
pub struct NgramCandidate {
    pub entry: ClipboardEntry,
    pub ngram_overlap: f32,
}

/// Storage-facing seam for search.
///
/// Each method returns raw candidates with whatever per-method signal the
/// ranker needs; the [`SearchService`] is responsible for plan dispatch,
/// dedup, ranking, sorting, and truncation.
#[async_trait]
pub trait SearchCandidateProvider: Send + Sync {
    /// Most recent active entries, optionally with pinned rows hoisted to the
    /// top. Used for the `Recent` plan and as the empty-query fallback.
    async fn recent_entries(
        &self,
        filters: &SearchFilters,
        order: RecentOrder,
        limit: usize,
    ) -> Result<Vec<ClipboardEntry>>;

    /// Substring (LIKE) matches against `normalized_text`.
    ///
    /// `bounded` lets the orchestrator opt into a "recent window" scan when
    /// substring is one branch of a hybrid plan — FTS / ngram backstop the
    /// older history in that case, so it's safe to bound the LIKE walk for
    /// predictable per-keystroke latency. For an explicit `Exact` query the
    /// orchestrator passes `false` so the implementation walks the full
    /// (non-blocked, non-deleted) corpus and never silently drops a real
    /// match outside the window.
    async fn substring_candidates(
        &self,
        normalized: &str,
        filters: &SearchFilters,
        limit: usize,
        bounded: bool,
    ) -> Result<Vec<ClipboardEntry>>;

    /// Full-text matches with raw `bm25` scores attached.
    async fn fulltext_candidates(
        &self,
        normalized: &str,
        filters: &SearchFilters,
        limit: usize,
    ) -> Result<Vec<FtsCandidate>>;

    /// Ngram-overlap matches. May return empty when the implementation deems
    /// ngram fan-out unprofitable (long ASCII queries are the canonical case).
    async fn ngram_candidates(
        &self,
        normalized: &str,
        filters: &SearchFilters,
        limit: usize,
    ) -> Result<Vec<NgramCandidate>>;
}

/// Final scoring step. Implementations may drop a candidate (`None`) when the
/// signals don't justify surfacing it.
pub trait Ranker: Send + Sync {
    fn rank(
        &self,
        query: &str,
        entry: ClipboardEntry,
        fts_score: f32,
        ngram_overlap: f32,
        now: OffsetDateTime,
        recent_order: RecentOrder,
    ) -> Option<SearchResult>;
}

/// Internal dispatch for [`SearchService`]. Public so callers (and tests) can
/// observe how a [`SearchMode`] resolves for a given normalized query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchPlan {
    Recent,
    Exact,
    FullText,
    Fuzzy,
    Hybrid,
}

impl SearchPlan {
    /// Resolve a [`SearchMode`] into a concrete plan, or surface an explicit
    /// `Unsupported` error for modes the build can't honour.
    ///
    /// `Semantic` previously aliased to `Hybrid`, which silently masked the
    /// fact that no semantic indexer is wired into the live search path —
    /// users requesting it would get text results indistinguishable from
    /// `Auto`. Returning `Unsupported` lets the UI hide / disable the mode
    /// instead of pretending it works.
    pub fn try_resolve(mode: SearchMode, normalized: &str) -> Result<Self> {
        if normalized.is_empty() {
            return Ok(Self::Recent);
        }
        Ok(match mode {
            SearchMode::Recent => Self::Recent,
            SearchMode::Exact => Self::Exact,
            SearchMode::FullText => Self::FullText,
            SearchMode::Fuzzy => Self::Fuzzy,
            SearchMode::Auto => Self::Hybrid,
            SearchMode::Semantic => {
                return Err(AppError::Unsupported(
                    "semantic search is not enabled in this build; choose Auto, FullText, \
                     Fuzzy, Exact, or Recent"
                        .to_owned(),
                ));
            }
        })
    }
}

/// Stateless orchestrator that turns a [`SearchQuery`] into ranked results.
///
/// Decoupled from any particular storage backend so the ranking + plan logic
/// can be exercised in tests with in-memory providers.
pub struct SearchService<'a, P: SearchCandidateProvider + ?Sized, R: Ranker + ?Sized> {
    provider: &'a P,
    ranker: &'a R,
}

impl<'a, P, R> SearchService<'a, P, R>
where
    P: SearchCandidateProvider + ?Sized,
    R: Ranker + ?Sized,
{
    pub const fn new(provider: &'a P, ranker: &'a R) -> Self {
        Self { provider, ranker }
    }

    pub async fn search(&self, query: SearchQuery) -> Result<Vec<SearchResult>> {
        let normalized = if query.normalized.is_empty() {
            normalize_text(&query.raw)
        } else {
            query.normalized.clone()
        };
        let limit = query.limit.clamp(1, MAX_RESULT_LIMIT);
        let plan = SearchPlan::try_resolve(query.mode, &normalized)?;
        let candidate_limit = limit.saturating_mul(CANDIDATE_OVERSAMPLE).max(limit);
        let filters = &query.filters;

        let mut entries: Vec<ClipboardEntry> = Vec::new();
        let mut fts_scores: HashMap<EntryId, f32> = HashMap::new();
        let mut ngram_overlap: HashMap<EntryId, f32> = HashMap::new();

        if matches!(plan, SearchPlan::Recent) {
            for entry in self
                .provider
                .recent_entries(filters, query.recent_order, candidate_limit)
                .await?
            {
                push_unique(&mut entries, entry);
            }
        }

        // Fan the substring/FTS/ngram fetches out concurrently for the
        // hybrid plan. Previously each ran sequentially through a shared
        // SQLite mutex, so a single slow branch (typically the LIKE scan
        // on long histories) gated the rest. With the storage-side
        // connection pool, joining lets reads overlap on physically
        // separate connections — capture writes no longer queue behind
        // a slow keystroke and per-keystroke search latency tracks the
        // slowest single branch instead of the sum.
        let want_substring = matches!(
            plan,
            SearchPlan::Exact | SearchPlan::Fuzzy | SearchPlan::Hybrid
        );
        let want_fts = matches!(plan, SearchPlan::FullText | SearchPlan::Hybrid);
        let want_ngram = matches!(plan, SearchPlan::Fuzzy | SearchPlan::Hybrid);

        // Only the implicit `Hybrid` plan opts into the recent-window
        // bound. There FTS gives word-level recall and ngram gives CJK
        // recall over the full corpus, so trading substring coverage for
        // predictable per-keystroke latency on large histories is a fair
        // deal. Explicit modes (`Exact`, `Fuzzy`) preserve full coverage:
        //
        // * `Exact` — substring is the only branch, so bounding it would
        //   silently hide older matches.
        // * `Fuzzy` — ngram returns empty for long ASCII queries (CJK or
        //   ≤ 8 chars only), so substring is the de-facto matcher there
        //   and bounding it would regress non-CJK fuzzy searches.
        let bounded_substring = matches!(plan, SearchPlan::Hybrid);
        let substring_fut = async {
            if want_substring {
                self.provider
                    .substring_candidates(&normalized, filters, candidate_limit, bounded_substring)
                    .await
            } else {
                Ok(Vec::new())
            }
        };
        let fts_fut = async {
            if want_fts {
                self.provider
                    .fulltext_candidates(&normalized, filters, candidate_limit)
                    .await
            } else {
                Ok(Vec::new())
            }
        };
        let ngram_fut = async {
            if want_ngram {
                self.provider
                    .ngram_candidates(&normalized, filters, candidate_limit)
                    .await
            } else {
                Ok(Vec::new())
            }
        };

        let (substring_hits, fts_hits, ngram_hits) =
            tokio::try_join!(substring_fut, fts_fut, ngram_fut)?;

        for entry in substring_hits {
            push_unique(&mut entries, entry);
        }
        for hit in fts_hits {
            fts_scores.insert(hit.entry.id, hit.fts_score);
            push_unique(&mut entries, hit.entry);
        }
        for hit in ngram_hits {
            ngram_overlap.insert(hit.entry.id, hit.ngram_overlap);
            push_unique(&mut entries, hit.entry);
        }

        let mut results = self.rank_all(
            &normalized,
            entries,
            &fts_scores,
            &ngram_overlap,
            query.recent_order,
        );
        // `slice::sort_by` is stable, so ties preserve provider ordering
        // (pinned-first chronological for Recent).
        results.sort_by(|left, right| {
            right
                .score
                .partial_cmp(&left.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        results.truncate(limit);
        Ok(results)
    }

    fn rank_all(
        &self,
        normalized: &str,
        entries: Vec<ClipboardEntry>,
        fts_scores: &HashMap<EntryId, f32>,
        ngram_overlap: &HashMap<EntryId, f32>,
        recent_order: RecentOrder,
    ) -> Vec<SearchResult> {
        let now = OffsetDateTime::now_utc();
        let mut results = Vec::with_capacity(entries.len());
        for entry in entries {
            let id = entry.id;
            if let Some(result) = self.ranker.rank(
                normalized,
                entry,
                fts_scores.get(&id).copied().unwrap_or(0.0),
                ngram_overlap.get(&id).copied().unwrap_or(0.0),
                now,
                recent_order,
            ) {
                results.push(result);
            }
        }
        results
    }
}

fn push_unique(entries: &mut Vec<ClipboardEntry>, entry: ClipboardEntry) {
    if !entries.iter().any(|existing| existing.id == entry.id) {
        entries.push(entry);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use crate::{EntryFactory, RankReason, model::SearchResult};

    use super::*;

    #[derive(Default)]
    struct StubProvider {
        recent: Vec<ClipboardEntry>,
        substring: Vec<ClipboardEntry>,
        fts: Vec<FtsCandidate>,
        ngram: Vec<NgramCandidate>,
        seen: Mutex<Vec<&'static str>>,
    }

    #[async_trait]
    impl SearchCandidateProvider for StubProvider {
        async fn recent_entries(
            &self,
            _filters: &SearchFilters,
            _order: RecentOrder,
            _limit: usize,
        ) -> Result<Vec<ClipboardEntry>> {
            self.seen.lock().unwrap().push("recent");
            Ok(self.recent.clone())
        }

        async fn substring_candidates(
            &self,
            _normalized: &str,
            _filters: &SearchFilters,
            _limit: usize,
            _bounded: bool,
        ) -> Result<Vec<ClipboardEntry>> {
            self.seen.lock().unwrap().push("substring");
            Ok(self.substring.clone())
        }

        async fn fulltext_candidates(
            &self,
            _normalized: &str,
            _filters: &SearchFilters,
            _limit: usize,
        ) -> Result<Vec<FtsCandidate>> {
            self.seen.lock().unwrap().push("fts");
            Ok(self.fts.clone())
        }

        async fn ngram_candidates(
            &self,
            _normalized: &str,
            _filters: &SearchFilters,
            _limit: usize,
        ) -> Result<Vec<NgramCandidate>> {
            self.seen.lock().unwrap().push("ngram");
            Ok(self.ngram.clone())
        }
    }

    /// Identity ranker: surfaces every candidate with score = sum of raw
    /// signals so tests can inspect the orchestration behavior independently
    /// from `nagori-search`'s real ranker.
    struct SumRanker;

    impl Ranker for SumRanker {
        fn rank(
            &self,
            _query: &str,
            entry: ClipboardEntry,
            fts_score: f32,
            ngram_overlap: f32,
            _now: OffsetDateTime,
            _recent_order: RecentOrder,
        ) -> Option<SearchResult> {
            let score = fts_score.abs() + ngram_overlap + 1.0;
            let content_kind = entry.content_kind();
            let source_app_name = entry
                .metadata
                .source
                .as_ref()
                .and_then(|source| source.name.clone());
            Some(SearchResult {
                entry_id: entry.id,
                score,
                rank_reason: vec![RankReason::Recent],
                content_kind,
                created_at: entry.metadata.created_at,
                pinned: entry.lifecycle.pinned,
                sensitivity: entry.sensitivity,
                preview: entry.search.preview,
                source_app_name,
            })
        }
    }

    fn entry(text: &str) -> ClipboardEntry {
        EntryFactory::from_text(text)
    }

    #[tokio::test]
    async fn recent_plan_only_calls_recent_primitive() {
        let provider = StubProvider {
            recent: vec![entry("alpha"), entry("beta")],
            ..Default::default()
        };
        let svc = SearchService::new(&provider, &SumRanker);

        let mut q = SearchQuery::new("", String::new(), 10);
        q.mode = SearchMode::Recent;
        let results = svc.search(q).await.unwrap();

        assert_eq!(results.len(), 2);
        assert_eq!(*provider.seen.lock().unwrap(), vec!["recent"]);
    }

    #[tokio::test]
    async fn hybrid_plan_fans_out_substring_fts_and_ngram() {
        let a = entry("alpha");
        let b = entry("beta");
        let c = entry("gamma");
        let provider = StubProvider {
            substring: vec![a.clone()],
            fts: vec![FtsCandidate {
                entry: b.clone(),
                fts_score: -1.5,
            }],
            ngram: vec![NgramCandidate {
                entry: c.clone(),
                ngram_overlap: 0.75,
            }],
            ..Default::default()
        };
        let svc = SearchService::new(&provider, &SumRanker);

        let q = SearchQuery::new("xyz", "xyz".to_owned(), 10);
        let results = svc.search(q).await.unwrap();
        let calls = provider.seen.lock().unwrap().clone();

        assert_eq!(calls, vec!["substring", "fts", "ngram"]);
        assert_eq!(results.len(), 3);
    }

    #[tokio::test]
    async fn duplicate_candidates_collapse_signals_per_entry() {
        let shared = entry("dup");
        let provider = StubProvider {
            substring: vec![shared.clone()],
            fts: vec![FtsCandidate {
                entry: shared.clone(),
                fts_score: -2.0,
            }],
            ngram: vec![NgramCandidate {
                entry: shared.clone(),
                ngram_overlap: 0.5,
            }],
            ..Default::default()
        };
        let svc = SearchService::new(&provider, &SumRanker);

        let q = SearchQuery::new("xyz", "xyz".to_owned(), 10);
        let results = svc.search(q).await.unwrap();

        assert_eq!(results.len(), 1, "deduped to a single entry");
        let result = &results[0];
        // SumRanker score = |fts_score| + ngram_overlap + 1.0 = 2.0 + 0.5 + 1.0
        assert!((result.score - 3.5).abs() < f32::EPSILON);
    }

    #[tokio::test]
    async fn results_sorted_descending_and_truncated_to_limit() {
        let mut hits = Vec::new();
        for fts in [-5.0_f32, -1.0, -3.0, -2.0] {
            hits.push(FtsCandidate {
                entry: entry(&format!("fts{fts}")),
                fts_score: fts,
            });
        }
        let provider = StubProvider {
            fts: hits,
            ..Default::default()
        };
        let svc = SearchService::new(&provider, &SumRanker);

        let mut q = SearchQuery::new("xyz", "xyz".to_owned(), 2);
        q.mode = SearchMode::FullText;
        let results = svc.search(q).await.unwrap();

        let scores: Vec<f32> = results.iter().map(|r| r.score).collect();
        assert_eq!(scores.len(), 2);
        assert!(scores[0] >= scores[1]);
        // Top score must come from the strongest |fts| signal (5.0 → 6.0).
        assert!((scores[0] - 6.0).abs() < f32::EPSILON);
    }

    #[test]
    fn search_plan_resolves_modes() {
        assert_eq!(
            SearchPlan::try_resolve(SearchMode::Auto, "").unwrap(),
            SearchPlan::Recent
        );
        assert_eq!(
            SearchPlan::try_resolve(SearchMode::Auto, "needle").unwrap(),
            SearchPlan::Hybrid,
        );
        assert_eq!(
            SearchPlan::try_resolve(SearchMode::Recent, "needle").unwrap(),
            SearchPlan::Recent,
        );
        assert_eq!(
            SearchPlan::try_resolve(SearchMode::Exact, "needle").unwrap(),
            SearchPlan::Exact,
        );
        assert_eq!(
            SearchPlan::try_resolve(SearchMode::FullText, "needle").unwrap(),
            SearchPlan::FullText,
        );
        assert_eq!(
            SearchPlan::try_resolve(SearchMode::Fuzzy, "needle").unwrap(),
            SearchPlan::Fuzzy,
        );
        // Semantic must surface as Unsupported now that it no longer
        // silently aliases to Hybrid.
        assert!(matches!(
            SearchPlan::try_resolve(SearchMode::Semantic, "needle"),
            Err(AppError::Unsupported(_)),
        ));
    }
}

use std::collections::{HashMap, HashSet};

use async_trait::async_trait;
use time::OffsetDateTime;

use crate::{
    ClipboardEntry, EntryId, RecentOrder, Result, SearchFilters, SearchMode, SearchQuery,
    SearchResult,
    text::{has_cjk, normalize_text},
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
/// document. The orchestrator passes a [`NgramQueryMode`] to encode the
/// plan-level policy (which grams to use); the provider applies it plus its own
/// "is this worth running at all" net and may return an empty vector.
#[derive(Debug, Clone)]
pub struct NgramCandidate {
    pub entry: ClipboardEntry,
    pub ngram_overlap: f32,
}

/// How the ngram candidate fetch should treat the query's grams.
///
/// The orchestrator owns this policy because it depends on the resolved
/// [`SearchPlan`], which the provider never sees:
///
/// * `Full` — use every query gram. The explicit `Fuzzy` plan needs this so
///   short ASCII typos (`needel` → `needle`) still match via gram overlap.
/// * `CjkOnly` — keep only grams that contain a CJK character. The implicit
///   `Hybrid` (Auto) plan uses this: ASCII word recall is already covered by
///   FTS + the bounded substring scan, and common ASCII bigrams own huge
///   posting lists that make the `gram IN (...)` union explode on large
///   histories. Filtering to CJK grams preserves CJK and mixed-script recall
///   while shedding that cost. A pure-ASCII query yields no CJK grams, so the
///   fetch short-circuits to empty.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NgramQueryMode {
    Full,
    CjkOnly,
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

    /// Ngram-overlap matches. `mode` carries the plan-level gram policy (see
    /// [`NgramQueryMode`]). May return empty when no usable grams survive the
    /// mode filter or when the implementation deems the fan-out unprofitable
    /// (long ASCII queries under [`NgramQueryMode::Full`] are the canonical
    /// case).
    async fn ngram_candidates(
        &self,
        normalized: &str,
        filters: &SearchFilters,
        limit: usize,
        mode: NgramQueryMode,
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
    /// Vector-similarity ranking. Resolved by [`SearchService`] to *no*
    /// text-candidate fan-out: semantic search needs a query embedding, which
    /// only the daemon (where the `Embedder` backend lives) can produce. The
    /// daemon routes `Semantic` queries to its own embed-then-rank path before
    /// this service is reached, so a `Semantic` plan arriving here (a direct
    /// store/test caller with no embedder) yields an empty result set rather
    /// than an error.
    Semantic,
}

impl SearchPlan {
    /// Resolve a [`SearchMode`] into a concrete plan.
    ///
    /// `Semantic` resolves to [`SearchPlan::Semantic`]; the daemon's search
    /// facade computes a query embedding and ranks against the on-device index
    /// for that mode. Direct store/test callers that lack an embedder get an
    /// empty result set (the service performs no text fan-out for the plan)
    /// rather than the old hard `Unsupported` error.
    pub const fn try_resolve(mode: SearchMode, normalized: &str) -> Result<Self> {
        if normalized.is_empty() {
            return Ok(Self::Recent);
        }
        Ok(match mode {
            SearchMode::Recent => Self::Recent,
            SearchMode::Exact => Self::Exact,
            SearchMode::FullText => Self::FullText,
            SearchMode::Fuzzy => Self::Fuzzy,
            SearchMode::Auto => Self::Hybrid,
            SearchMode::Semantic => Self::Semantic,
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
        let mut seen: HashSet<EntryId> = HashSet::new();
        let mut fts_scores: HashMap<EntryId, f32> = HashMap::new();
        let mut ngram_overlap: HashMap<EntryId, f32> = HashMap::new();

        if matches!(plan, SearchPlan::Recent) {
            for entry in self
                .provider
                .recent_entries(filters, query.recent_order, candidate_limit)
                .await?
            {
                push_unique(&mut entries, &mut seen, entry);
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
        // Ngram fan-out runs for `Fuzzy` (full grams — its typo tolerance comes
        // from gram overlap) and for `Hybrid` *only when the query carries CJK*.
        // A pure-ASCII `Hybrid` query is fully served by FTS + bounded
        // substring, so we skip even dispatching the blocking ngram fetch and
        // dodge the common-bigram posting-list explosion on large histories.
        // Mixed CJK+ASCII queries still reach the provider, where
        // `NgramQueryMode::CjkOnly` strips the costly ASCII grams.
        let want_ngram = match plan {
            SearchPlan::Fuzzy => true,
            SearchPlan::Hybrid => has_cjk(&normalized),
            _ => false,
        };
        let ngram_mode = if matches!(plan, SearchPlan::Hybrid) {
            NgramQueryMode::CjkOnly
        } else {
            NgramQueryMode::Full
        };

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
                    .ngram_candidates(&normalized, filters, candidate_limit, ngram_mode)
                    .await
            } else {
                Ok(Vec::new())
            }
        };

        let (substring_hits, fts_hits, ngram_hits) =
            tokio::try_join!(substring_fut, fts_fut, ngram_fut)?;

        for entry in substring_hits {
            push_unique(&mut entries, &mut seen, entry);
        }
        for hit in fts_hits {
            fts_scores.insert(hit.entry.id, hit.fts_score);
            push_unique(&mut entries, &mut seen, hit.entry);
        }
        for hit in ngram_hits {
            ngram_overlap.insert(hit.entry.id, hit.ngram_overlap);
            push_unique(&mut entries, &mut seen, hit.entry);
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

fn push_unique(
    entries: &mut Vec<ClipboardEntry>,
    seen: &mut HashSet<EntryId>,
    entry: ClipboardEntry,
) {
    if seen.insert(entry.id) {
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
        ngram_mode: Mutex<Option<NgramQueryMode>>,
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
            mode: NgramQueryMode,
        ) -> Result<Vec<NgramCandidate>> {
            self.seen.lock().unwrap().push("ngram");
            *self.ngram_mode.lock().unwrap() = Some(mode);
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
    async fn hybrid_cjk_query_fans_out_substring_fts_and_ngram() {
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

        // A CJK query keeps ngram in the Auto/Hybrid fan-out, and the
        // orchestrator must ask for CJK-only grams there.
        let q = SearchQuery::new("検索", "検索".to_owned(), 10);
        let results = svc.search(q).await.unwrap();
        let calls = provider.seen.lock().unwrap().clone();

        assert_eq!(calls, vec!["substring", "fts", "ngram"]);
        assert_eq!(
            *provider.ngram_mode.lock().unwrap(),
            Some(NgramQueryMode::CjkOnly)
        );
        assert_eq!(results.len(), 3);
    }

    #[tokio::test]
    async fn hybrid_ascii_query_skips_ngram() {
        // Pure-ASCII Auto queries are served by FTS + bounded substring; ngram
        // is not dispatched at all, so the common-bigram posting-list scan
        // never runs. This is the P0 fix for the 100k fan-out blowup.
        let provider = StubProvider {
            substring: vec![entry("alpha")],
            fts: vec![FtsCandidate {
                entry: entry("beta"),
                fts_score: -1.5,
            }],
            ngram: vec![NgramCandidate {
                entry: entry("gamma"),
                ngram_overlap: 0.75,
            }],
            ..Default::default()
        };
        let svc = SearchService::new(&provider, &SumRanker);

        let q = SearchQuery::new("needle", "needle".to_owned(), 10);
        let results = svc.search(q).await.unwrap();
        let calls = provider.seen.lock().unwrap().clone();

        assert_eq!(calls, vec!["substring", "fts"], "ngram must not run");
        assert!(provider.ngram_mode.lock().unwrap().is_none());
        assert_eq!(results.len(), 2);
    }

    #[tokio::test]
    async fn fuzzy_ascii_query_uses_full_ngram_mode() {
        // Explicit Fuzzy keeps ngram for ASCII (its typo tolerance lives there)
        // and must request the full gram set, not the CJK-only subset.
        let provider = StubProvider {
            substring: vec![entry("alpha")],
            ngram: vec![NgramCandidate {
                entry: entry("gamma"),
                ngram_overlap: 0.75,
            }],
            ..Default::default()
        };
        let svc = SearchService::new(&provider, &SumRanker);

        let mut q = SearchQuery::new("needel", "needel".to_owned(), 10);
        q.mode = SearchMode::Fuzzy;
        let results = svc.search(q).await.unwrap();
        let calls = provider.seen.lock().unwrap().clone();

        // Fuzzy fans out substring + ngram (no FTS branch).
        assert_eq!(calls, vec!["substring", "ngram"]);
        assert_eq!(
            *provider.ngram_mode.lock().unwrap(),
            Some(NgramQueryMode::Full)
        );
        assert_eq!(results.len(), 2);
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

        // A CJK query exercises all three branches (substring + FTS + ngram)
        // in the Auto/Hybrid plan, so the dedup collapses every signal onto the
        // one shared entry.
        let q = SearchQuery::new("検索", "検索".to_owned(), 10);
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
        // Semantic resolves to its own plan; the daemon routes it to the
        // embed-then-rank path, and direct callers get an empty result set.
        assert_eq!(
            SearchPlan::try_resolve(SearchMode::Semantic, "needle").unwrap(),
            SearchPlan::Semantic,
        );
    }

    #[tokio::test]
    async fn semantic_plan_yields_no_text_candidates() {
        let provider = StubProvider {
            recent: vec![entry("alpha")],
            substring: vec![entry("beta")],
            fts: vec![FtsCandidate {
                entry: entry("gamma"),
                fts_score: -1.0,
            }],
            ngram: vec![NgramCandidate {
                entry: entry("delta"),
                ngram_overlap: 0.5,
            }],
            ..Default::default()
        };
        let svc = SearchService::new(&provider, &SumRanker);

        let mut q = SearchQuery::new("xyz", "xyz".to_owned(), 10);
        q.mode = SearchMode::Semantic;
        let results = svc.search(q).await.unwrap();

        // No text fan-out runs for the semantic plan, so a caller without an
        // embedder simply gets nothing rather than text results.
        assert!(results.is_empty());
        assert!(provider.seen.lock().unwrap().is_empty());
    }
}

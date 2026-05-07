use std::sync::{Arc, Mutex};

use nagori_core::{RecentOrder, SearchFilters, SearchMode, SearchQuery, SearchResult};

/// Maximum normalized-query length that participates in the cache.
///
/// Empty queries (the `Recent` plan) plus the first few keystrokes are the
/// hottest path — beyond that, results turn over too quickly for caching to
/// help.
pub const CACHEABLE_QUERY_LEN: usize = 8;

/// Default capacity for [`RecentSearchCache`]. Small on purpose: the cache
/// stores cloned `SearchResult` payloads and the working set for "empty +
/// short prefix" tends to be tiny.
pub const DEFAULT_CACHE_CAPACITY: usize = 32;

/// Composite key identifying a cacheable [`SearchQuery`].
///
/// `SearchFilters` carries `Vec<ContentKind>` and a couple of timestamps that
/// don't implement `Hash`, so the cache stays equality-based rather than
/// hashing — fine for a sub-32-entry working set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheKey {
    pub normalized: String,
    pub mode: SearchMode,
    pub recent_order: RecentOrder,
    pub limit: usize,
    pub filters: SearchFilters,
}

impl CacheKey {
    pub fn from_query(query: &SearchQuery) -> Self {
        // The storage layer renders `filters.kinds` as `WHERE kind IN (…)`,
        // so two queries that differ only in the ordering or duplicates of
        // the kinds list return the same rows. Normalising here ensures
        // they share a single cache slot — without this, cycling palette
        // filters in a different order would defeat the cache and double
        // the LRU's working set.
        let mut filters = query.filters.clone();
        filters.kinds.sort_unstable();
        filters.kinds.dedup();
        Self {
            normalized: query.normalized.clone(),
            mode: query.mode,
            recent_order: query.recent_order,
            limit: query.limit,
            filters,
        }
    }

    pub const fn is_eligible(&self) -> bool {
        self.normalized.len() <= CACHEABLE_QUERY_LEN
    }
}

/// Outcome of a single cache lookup performed under the cache lock.
///
/// On `Miss` the caller receives the cache's current `epoch`; threading it
/// back to [`RecentSearchCache::put_if_epoch`] lets the cache reject stale
/// inserts when a concurrent mutator invalidated between the `SQLite` read and
/// the put. Without that check, a slow query could overwrite a fresh empty
/// state with results that pre-date the mutation (TOCTOU).
#[derive(Debug)]
pub enum CacheLookup {
    Hit(Vec<SearchResult>),
    Miss { epoch: u64 },
}

/// Bounded LRU of recent search results, keyed by [`CacheKey`].
///
/// The cache lives in front of `SearchService` so the empty-query case
/// (`Recent` plan) and short prefix queries don't have to round-trip through
/// `SQLite` on every keystroke. Any mutation of the entry corpus
/// (insert / delete / pin / use-count bump / retention sweep) must call
/// [`RecentSearchCache::invalidate`] — stale hits would otherwise paper over
/// freshly captured rows or recently pinned entries.
///
/// `invalidate` bumps the epoch counter so the [`Self::lookup`] →
/// `store.search` → [`Self::put_if_epoch`] dance can detect concurrent
/// mutations and refuse to publish stale results.
#[derive(Debug)]
pub struct RecentSearchCache {
    capacity: usize,
    entries: Vec<(CacheKey, Vec<SearchResult>)>,
    epoch: u64,
}

impl RecentSearchCache {
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        Self {
            capacity,
            entries: Vec::with_capacity(capacity),
            epoch: 0,
        }
    }

    pub fn get(&mut self, key: &CacheKey) -> Option<Vec<SearchResult>> {
        let pos = self.entries.iter().position(|(k, _)| k == key)?;
        let entry = self.entries.remove(pos);
        let value = entry.1.clone();
        self.entries.push(entry);
        Some(value)
    }

    /// Atomic "get-or-snapshot-epoch": returns either a hit, or the current
    /// epoch the caller should hand back to [`Self::put_if_epoch`] after it
    /// finishes its underlying query. Both branches happen under the same
    /// lock acquisition, so the epoch the caller sees is the same epoch the
    /// cache is in at the moment we observed a miss.
    pub fn lookup(&mut self, key: &CacheKey) -> CacheLookup {
        if let Some(hit) = self.get(key) {
            CacheLookup::Hit(hit)
        } else {
            CacheLookup::Miss { epoch: self.epoch }
        }
    }

    pub fn put(&mut self, key: CacheKey, value: Vec<SearchResult>) {
        if let Some(pos) = self.entries.iter().position(|(k, _)| k == &key) {
            self.entries.remove(pos);
        } else if self.entries.len() == self.capacity {
            self.entries.remove(0);
        }
        self.entries.push((key, value));
    }

    /// Insert only if the cache has not been invalidated since the lookup
    /// that observed `epoch`. Returns `true` when the put was accepted.
    ///
    /// Without this guard, a query running concurrently with a mutation
    /// (capture insert, retention sweep, …) could land its pre-mutation
    /// results into the cache *after* the mutation called
    /// [`Self::invalidate`], so future hits would lie about the corpus until
    /// the next invalidation.
    pub fn put_if_epoch(&mut self, key: CacheKey, value: Vec<SearchResult>, epoch: u64) -> bool {
        if self.epoch != epoch {
            return false;
        }
        self.put(key, value);
        true
    }

    pub fn invalidate(&mut self) {
        self.entries.clear();
        // Wrapping is fine: even at one invalidation per nanosecond we'd need
        // ~584 years to wrap, and a wrap merely makes a stale `put_if_epoch`
        // accept a value the lookup pinned to the old epoch — i.e. equivalent
        // to it never having raced.
        self.epoch = self.epoch.wrapping_add(1);
    }

    pub const fn epoch(&self) -> u64 {
        self.epoch
    }

    pub const fn len(&self) -> usize {
        self.entries.len()
    }

    pub const fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl Default for RecentSearchCache {
    fn default() -> Self {
        Self::new(DEFAULT_CACHE_CAPACITY)
    }
}

/// Shared handle to a [`RecentSearchCache`].
///
/// Cloneable so the runtime, capture loop, and any future mutators all hold
/// the same logical cache. The std `Mutex` is fine here because the critical
/// section is a tiny `Vec` walk with no `.await` inside.
pub type SharedSearchCache = Arc<Mutex<RecentSearchCache>>;

pub fn new_shared_cache() -> SharedSearchCache {
    Arc::new(Mutex::new(RecentSearchCache::default()))
}

#[cfg(test)]
mod tests {
    use nagori_core::{ContentKind, EntryId, RankReason, Sensitivity};
    use time::OffsetDateTime;

    use super::*;

    fn sample_result(score: f32) -> SearchResult {
        SearchResult {
            entry_id: EntryId::new(),
            score,
            rank_reason: vec![RankReason::Recent],
            preview: String::new(),
            content_kind: ContentKind::Text,
            created_at: OffsetDateTime::now_utc(),
            pinned: false,
            sensitivity: Sensitivity::Public,
            source_app_name: None,
        }
    }

    fn key(normalized: &str) -> CacheKey {
        CacheKey {
            normalized: normalized.to_owned(),
            mode: SearchMode::Auto,
            recent_order: RecentOrder::ByRecency,
            limit: 50,
            filters: SearchFilters::default(),
        }
    }

    #[test]
    fn eligibility_covers_empty_and_short_queries() {
        assert!(key("").is_eligible());
        assert!(key("ab").is_eligible());
        assert!(key("abcdefgh").is_eligible());
        assert!(!key("abcdefghi").is_eligible());
    }

    #[test]
    fn put_then_get_roundtrips_results() {
        let mut cache = RecentSearchCache::new(4);
        let value = vec![sample_result(1.0)];
        cache.put(key("abc"), value.clone());
        let hit = cache.get(&key("abc")).expect("cache hit");
        assert_eq!(hit.len(), value.len());
        assert!((hit[0].score - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn lru_evicts_least_recently_used_when_full() {
        let mut cache = RecentSearchCache::new(2);
        cache.put(key("a"), vec![sample_result(1.0)]);
        cache.put(key("b"), vec![sample_result(2.0)]);
        // touch "a" so "b" becomes the LRU candidate
        let _ = cache.get(&key("a"));
        cache.put(key("c"), vec![sample_result(3.0)]);
        assert!(cache.get(&key("b")).is_none());
        assert!(cache.get(&key("a")).is_some());
        assert!(cache.get(&key("c")).is_some());
    }

    #[test]
    fn invalidate_clears_all_entries() {
        let mut cache = RecentSearchCache::new(4);
        cache.put(key("a"), vec![sample_result(1.0)]);
        cache.put(key("b"), vec![sample_result(2.0)]);
        cache.invalidate();
        assert!(cache.is_empty());
    }

    #[test]
    fn put_replaces_existing_value_for_same_key() {
        let mut cache = RecentSearchCache::new(2);
        cache.put(key("a"), vec![sample_result(1.0)]);
        cache.put(key("a"), vec![sample_result(2.0), sample_result(2.5)]);
        let hit = cache.get(&key("a")).expect("cache hit");
        assert_eq!(hit.len(), 2);
    }

    #[test]
    fn invalidate_advances_epoch() {
        let mut cache = RecentSearchCache::new(2);
        let before = cache.epoch();
        cache.invalidate();
        assert_eq!(cache.epoch(), before + 1);
        cache.invalidate();
        assert_eq!(cache.epoch(), before + 2);
    }

    #[test]
    fn lookup_returns_miss_with_current_epoch() {
        let mut cache = RecentSearchCache::new(2);
        cache.invalidate(); // epoch = 1
        match cache.lookup(&key("missing")) {
            CacheLookup::Miss { epoch } => assert_eq!(epoch, 1),
            CacheLookup::Hit(_) => panic!("empty cache must miss"),
        }
    }

    #[test]
    fn put_if_epoch_rejects_stale_inserts() {
        // Simulates the runtime race: `lookup` snapshots epoch = N, then a
        // concurrent mutator invalidates (epoch -> N+1). The lagging put
        // must be refused so the cache doesn't resurrect pre-mutation
        // results.
        let mut cache = RecentSearchCache::new(2);
        let lookup_epoch = match cache.lookup(&key("a")) {
            CacheLookup::Miss { epoch } => epoch,
            CacheLookup::Hit(_) => panic!("empty cache must miss"),
        };
        cache.invalidate();

        let accepted = cache.put_if_epoch(key("a"), vec![sample_result(1.0)], lookup_epoch);
        assert!(!accepted, "put after invalidation must be rejected");
        assert!(cache.is_empty(), "cache must remain empty");
    }

    #[test]
    fn from_query_normalises_filter_kinds_order_and_dupes() {
        // SQL `IN (Text, Url)` and `IN (Url, Text, Text)` return the same
        // rows, so the cache key must collapse them to one slot — otherwise
        // the LRU thrashes on what the storage layer treats as identical
        // queries.
        let query_a = SearchQuery {
            filters: SearchFilters {
                kinds: vec![ContentKind::Url, ContentKind::Text],
                ..SearchFilters::default()
            },
            ..SearchQuery::new("", String::new(), 10)
        };
        let query_b = SearchQuery {
            filters: SearchFilters {
                kinds: vec![ContentKind::Text, ContentKind::Url, ContentKind::Text],
                ..SearchFilters::default()
            },
            ..SearchQuery::new("", String::new(), 10)
        };

        let key_a = CacheKey::from_query(&query_a);
        let key_b = CacheKey::from_query(&query_b);

        assert_eq!(key_a, key_b);
        assert_eq!(
            key_a.filters.kinds,
            vec![ContentKind::Text, ContentKind::Url]
        );
    }

    #[test]
    fn put_if_epoch_accepts_fresh_inserts() {
        let mut cache = RecentSearchCache::new(2);
        let lookup_epoch = match cache.lookup(&key("a")) {
            CacheLookup::Miss { epoch } => epoch,
            CacheLookup::Hit(_) => panic!("empty cache must miss"),
        };

        let accepted = cache.put_if_epoch(key("a"), vec![sample_result(1.0)], lookup_epoch);
        assert!(
            accepted,
            "put without intervening invalidation must succeed"
        );
        assert!(cache.get(&key("a")).is_some());
    }
}

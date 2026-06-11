use nagori_core::{
    AppError, ContentKind, EntryFactory, EntryId, EntryRepository, RankReason, RecentOrder,
    SearchFilters, SearchMode, SearchQuery,
};
use nagori_search::normalize_text;
use time::OffsetDateTime;

use super::super::*;

use super::super::convert::fts_query;
use super::{backdate_entry, insert_text};

#[test]
fn fts_query_wraps_alnum_tokens_in_quotes() {
    assert_eq!(fts_query("hello world"), r#""hello" "world""#);
}

#[test]
fn fts_query_strips_fts5_metacharacters() {
    // `(`, `)`, `:`, `*`, `"` are all FTS5-meaningful outside a
    // phrase string. They must not survive into the rendered MATCH
    // expression — even quoted, an unmatched `"` would corrupt the
    // expression, and `:` could be parsed as a column filter when
    // we later switch to column-scoped queries.
    assert_eq!(fts_query("foo:bar"), r#""foo" "bar""#);
    assert_eq!(fts_query("foo*"), r#""foo""#);
    assert_eq!(fts_query("(foo)"), r#""foo""#);
    assert_eq!(fts_query(r#"say "hi""#), r#""say" "hi""#);
}

#[test]
fn fts_query_returns_empty_for_pure_punctuation() {
    // A query that collapses to zero tokens must produce the empty
    // string so the caller can short-circuit before issuing an
    // invalid FTS5 MATCH (the tokenizer would otherwise reject a
    // phrase that yields no terms).
    assert!(fts_query("(").is_empty());
    assert!(fts_query(":*").is_empty());
    assert!(fts_query("\"\"").is_empty());
    assert!(fts_query("   ").is_empty());
}

#[tokio::test]
async fn stores_and_searches_japanese_text() {
    let store = SqliteStore::open_memory().unwrap();
    let mut entry = EntryFactory::from_text("クリップボード履歴");
    entry.search.normalized_text = normalize_text(entry.plain_text().unwrap());
    let id = store.insert(entry).await.unwrap();

    let query = SearchQuery::new("クリップ", normalize_text("クリップ"), 10);
    let results = store.search(query).await.unwrap();
    assert_eq!(results[0].entry_id, id);
}

#[tokio::test]
async fn katakana_entry_is_found_by_hiragana_query() {
    // Kana folding lives in the ngram generator: a Katakana clip and a
    // Hiragana query share folded grams even though `normalize_text` (NFKC)
    // leaves the two scripts distinct.
    let store = SqliteStore::open_memory().unwrap();
    let id = insert_text(&store, "クリップボード履歴").await;

    let query = SearchQuery::new("くりっぷ", normalize_text("くりっぷ"), 10);
    let results = store.search(query).await.unwrap();
    assert!(
        results.iter().any(|r| r.entry_id == id),
        "hiragana query should recall the katakana entry via folded ngrams",
    );
}

#[tokio::test]
async fn single_kanji_query_recalls_entry() {
    // A lone ideograph matches the document-side Han 1-gram, so it recalls
    // even though `unicode61` FTS collapses the run to one token and the
    // 2/3-gram path needs ≥ 2 chars.
    let store = SqliteStore::open_memory().unwrap();
    let id = insert_text(&store, "設計資料のメモ").await;

    let query = SearchQuery::new("設", normalize_text("設"), 10);
    let results = store.search(query).await.unwrap();
    assert!(
        results.iter().any(|r| r.entry_id == id),
        "single-kanji query should recall the entry via the Han 1-gram",
    );
}

#[tokio::test]
async fn rebuild_stale_ngrams_drains_and_restamps() {
    // Simulate a pre-upgrade document: grams produced by an older generator
    // and a stale per-row version marker. The background rebuild must
    // regenerate the grams from the stored normalized_text and restamp the
    // row, without touching normalized_text / preview.
    let store = SqliteStore::open_memory().unwrap();
    let id = insert_text(&store, "設計資料のメモ").await;
    let (preview_before, normalized_before) = {
        let conn = store.conn().unwrap();
        conn.execute("DELETE FROM ngrams", []).unwrap();
        conn.execute("UPDATE search_documents SET ngram_index_version = 0", [])
            .unwrap();
        conn.query_row(
            "SELECT preview, normalized_text FROM search_documents WHERE entry_id = ?1",
            rusqlite::params![id.to_string()],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .unwrap()
    };

    assert_eq!(store.pending_ngram_rebuild().await.unwrap(), 1);

    let mut drained = 0;
    loop {
        let n = store.rebuild_stale_ngrams().await.unwrap();
        if n == 0 {
            break;
        }
        drained += n;
    }
    assert_eq!(drained, 1);
    assert_eq!(store.pending_ngram_rebuild().await.unwrap(), 0);

    // Grams regenerated, and preview/normalized_text untouched.
    let conn = store.conn().unwrap();
    let gram_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM ngrams", [], |row| row.get(0))
        .unwrap();
    assert!(gram_count > 0, "rebuild should regenerate grams");
    let (preview_after, normalized_after): (String, String) = conn
        .query_row(
            "SELECT preview, normalized_text FROM search_documents WHERE entry_id = ?1",
            rusqlite::params![id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(preview_before, preview_after);
    assert_eq!(normalized_before, normalized_after);
}

#[tokio::test]
async fn pinned_only_filter_excludes_others() {
    let store = SqliteStore::open_memory().unwrap();
    let pinned_id = insert_text(&store, "pinned snippet").await;
    store.set_pinned(pinned_id, true).await.unwrap();
    let _other = insert_text(&store, "regular snippet").await;

    let mut query = SearchQuery::new("snippet", normalize_text("snippet"), 10);
    query.filters = SearchFilters {
        pinned_only: true,
        ..Default::default()
    };
    let results = store.search(query).await.unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].entry_id, pinned_id);
}

#[tokio::test]
async fn exact_mode_skips_fts_only_matches() {
    let store = SqliteStore::open_memory().unwrap();
    let _ = insert_text(&store, "the quick brown fox").await;

    let mut query = SearchQuery::new("qui ck", normalize_text("qui ck"), 10);
    query.mode = SearchMode::Exact;
    let exact = store.search(query.clone()).await.unwrap();
    assert!(exact.is_empty());

    // Fuzzy keeps ASCII ngram recall, so the whitespace-insensitive match
    // on `quick` still surfaces there. Auto deliberately does not — see
    // `auto_skips_ascii_ngram_only_match`.
    query.mode = SearchMode::Fuzzy;
    let fuzzy = store.search(query).await.unwrap();
    assert!(!fuzzy.is_empty());
}

#[tokio::test]
async fn auto_skips_ascii_ngram_only_match() {
    // Regression for the ngram fan-out fix: the Auto/Hybrid plan must
    // not run ASCII ngram. `qui ck` reaches `the quick brown fox` only via
    // whitespace-stripped ngram overlap (`quick`) — FTS sees the tokens
    // `qui`/`ck` with no whole-token match, and the substring scan looks
    // for the literal `qui ck`. So Auto now returns nothing; ASCII
    // partial/typo recall lives in explicit Fuzzy.
    let store = SqliteStore::open_memory().unwrap();
    let _ = insert_text(&store, "the quick brown fox").await;

    let mut query = SearchQuery::new("qui ck", normalize_text("qui ck"), 10);
    query.mode = SearchMode::Auto;
    let auto = store.search(query).await.unwrap();
    assert!(
        auto.is_empty(),
        "Auto no longer chases ASCII ngram-only matches",
    );
}

#[tokio::test]
async fn exact_substring_walks_full_corpus_unbounded() {
    // Regression: an earlier iteration capped the substring CTE to the
    // most recent SUBSTRING_SCAN_WINDOW rows for *all* plans, which
    // silently dropped exact matches outside the window. The Exact
    // plan must always see the full live corpus because nothing else
    // (FTS / ngram) backstops it.
    use nagori_core::SearchCandidateProvider;
    use tokio_util::sync::CancellationToken;
    let store = SqliteStore::open_memory().unwrap();
    let _old = insert_text(&store, "needle in a haystack").await;
    for idx in 0..20 {
        let _ = insert_text(&store, &format!("filler {idx}")).await;
    }
    let cancel = CancellationToken::new();
    let bounded = store
        .substring_candidates("needle", &SearchFilters::default(), 10, true, &cancel)
        .await
        .unwrap();
    let unbounded = store
        .substring_candidates("needle", &SearchFilters::default(), 10, false, &cancel)
        .await
        .unwrap();
    // Both still find it on a 21-row DB (window is 5000), but the
    // unbounded path is what's used for explicit `Exact` searches —
    // confirming both shapes return the row guards against future
    // regressions where the bounded path swallows older matches.
    assert_eq!(bounded.len(), 1);
    assert_eq!(unbounded.len(), 1);
}

#[tokio::test]
async fn ngram_cjk_only_mode_drops_ascii_grams() {
    // Directly pins the gram filter. `CjkOnly` (the Auto/Hybrid policy)
    // keeps only CJK-bearing grams, so a pure-ASCII query yields nothing
    // while a mixed query still matches on its CJK / boundary grams. The
    // `Full` mode (explicit Fuzzy) keeps ASCII grams so the same ASCII
    // query matches there.
    use nagori_core::{NgramQueryMode, SearchCandidateProvider};
    use tokio_util::sync::CancellationToken;
    let store = SqliteStore::open_memory().unwrap();
    let cancel = CancellationToken::new();
    let mixed = {
        let mut entry = EntryFactory::from_text("メモ alpha 設計");
        entry.search.normalized_text = normalize_text(entry.plain_text().unwrap());
        store.insert(entry).await.unwrap()
    };
    let ascii = insert_text(&store, "needle in a haystack").await;

    // Pure-ASCII query under CjkOnly: every gram is filtered out → empty.
    let ascii_cjk_only = store
        .ngram_candidates(
            "needle",
            &SearchFilters::default(),
            10,
            NgramQueryMode::CjkOnly,
            &cancel,
        )
        .await
        .unwrap();
    assert!(
        ascii_cjk_only.is_empty(),
        "CjkOnly must drop every ASCII gram",
    );

    // Same ASCII query under Full still matches via the full gram set.
    let ascii_full = store
        .ngram_candidates(
            "needle",
            &SearchFilters::default(),
            10,
            NgramQueryMode::Full,
            &cancel,
        )
        .await
        .unwrap();
    assert!(
        ascii_full.iter().any(|c| c.candidate.entry_id == ascii),
        "Full keeps ASCII grams so the ASCII entry still matches",
    );

    // Mixed query under CjkOnly keeps the `設計` / boundary grams, so the
    // mixed entry is still reachable through ngram alone.
    let mixed_cjk_only = store
        .ngram_candidates(
            &normalize_text("alpha 設計"),
            &SearchFilters::default(),
            10,
            NgramQueryMode::CjkOnly,
            &cancel,
        )
        .await
        .unwrap();
    assert!(
        mixed_cjk_only.iter().any(|c| c.candidate.entry_id == mixed),
        "CjkOnly must keep CJK-bearing grams for mixed queries",
    );
}

#[tokio::test]
async fn run_search_blocking_interrupts_a_cancelled_query() {
    use std::time::Duration;

    use tokio_util::sync::CancellationToken;

    let store = SqliteStore::open_memory().unwrap();
    let cancel = CancellationToken::new();
    let canceller = cancel.clone();
    // Cancel shortly after the heavy query starts so the abort lands
    // mid-flight rather than before the connection is even acquired.
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        canceller.cancel();
    });

    // A recursive CTE that would run effectively forever; the cancellation
    // progress-handler installed by `run_search_blocking` must abort it so
    // the call returns promptly with an error instead of running to
    // completion and pinning the pooled connection.
    let result = tokio::time::timeout(
        Duration::from_secs(10),
        store.run_search_blocking(&cancel, |conn| {
            let count: i64 = conn
                .query_row(
                    "WITH RECURSIVE c(x) AS (
                         SELECT 1 UNION ALL SELECT x + 1 FROM c WHERE x < 100000000000
                     )
                     SELECT count(*) FROM c",
                    [],
                    |row| row.get(0),
                )
                .map_err(|err| AppError::storage(err.to_string()))?;
            Ok(count)
        }),
    )
    .await
    .expect("a cancelled query must be interrupted rather than run to completion");
    assert!(
        result.is_err(),
        "interrupting the query must surface as an error, got {result:?}",
    );

    // The connection is back in the pool, so a fresh query still works.
    let ok = store
        .run_search_blocking(&CancellationToken::new(), |conn| {
            conn.query_row("SELECT 1", [], |row| row.get::<_, i64>(0))
                .map_err(|err| AppError::storage(err.to_string()))
        })
        .await
        .expect("the connection should be reusable after an interrupt");
    assert_eq!(ok, 1);
}

#[tokio::test]
async fn kind_filter_limits_to_url_entries() {
    let store = SqliteStore::open_memory().unwrap();
    let _ = insert_text(&store, "https://example.com/foo").await;
    let _ = insert_text(&store, "plain text foo").await;

    let mut query = SearchQuery::new("foo", normalize_text("foo"), 10);
    query.filters = SearchFilters {
        kinds: vec![ContentKind::Url],
        ..Default::default()
    };
    let results = store.search(query).await.unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].content_kind, ContentKind::Url);
}

async fn insert_with_source(store: &SqliteStore, text: &str, bundle: &str) -> EntryId {
    let mut entry = EntryFactory::from_text(text);
    entry.search.normalized_text = normalize_text(entry.plain_text().unwrap());
    entry.metadata.source = Some(nagori_core::SourceApp {
        bundle_id: Some(bundle.to_owned()),
        name: None,
        executable_path: None,
    });
    store.insert(entry).await.unwrap()
}

#[tokio::test]
async fn recent_mode_returns_pinned_first_then_chronological() {
    let store = SqliteStore::open_memory().unwrap();
    let now = OffsetDateTime::now_utc();
    let oldest = insert_text(&store, "alpha row").await;
    let middle = insert_text(&store, "bravo row").await;
    let newest = insert_text(&store, "charlie row").await;
    backdate_entry(&store, oldest, now - time::Duration::hours(3));
    backdate_entry(&store, middle, now - time::Duration::hours(2));
    backdate_entry(&store, newest, now - time::Duration::hours(1));
    store.set_pinned(oldest, true).await.unwrap();

    let mut query = SearchQuery::new("", String::new(), 10);
    query.mode = SearchMode::Recent;
    query.recent_order = RecentOrder::PinnedFirstThenRecency;
    let results = store.search(query).await.unwrap();
    let ids = results.iter().map(|r| r.entry_id).collect::<Vec<_>>();
    assert_eq!(ids[0], oldest, "pinned row should rank first");
    assert!(ids.contains(&middle));
    assert!(ids.contains(&newest));
}

#[tokio::test]
async fn recent_mode_can_order_by_use_count() {
    let store = SqliteStore::open_memory().unwrap();
    let low = insert_text(&store, "low use").await;
    let high = insert_text(&store, "high use").await;
    store.increment_use_count(high).await.unwrap();
    store.increment_use_count(high).await.unwrap();
    store.increment_use_count(low).await.unwrap();

    let mut query = SearchQuery::new("", String::new(), 10);
    query.mode = SearchMode::Recent;
    query.recent_order = RecentOrder::ByUseCount;
    let results = store.search(query).await.unwrap();

    assert_eq!(results.first().map(|r| r.entry_id), Some(high));
    assert!(
        results
            .first()
            .is_some_and(|r| r.rank_reason.contains(&RankReason::FrequentlyUsed)),
    );
}

#[tokio::test]
async fn full_text_mode_matches_separated_tokens_in_any_order() {
    let store = SqliteStore::open_memory().unwrap();
    let target = insert_text(&store, "search relevance ranking notes").await;
    let _ = insert_text(&store, "completely unrelated note about lunch").await;

    let mut query = SearchQuery::new("ranking search", normalize_text("ranking search"), 10);
    query.mode = SearchMode::FullText;
    let results = store.search(query).await.unwrap();
    let hits = results.iter().map(|r| r.entry_id).collect::<Vec<_>>();
    assert!(
        hits.contains(&target),
        "FTS should find both terms regardless of order"
    );
    assert_eq!(hits.len(), 1);
}

#[tokio::test]
async fn fuzzy_mode_finds_partial_cjk_substring() {
    let store = SqliteStore::open_memory().unwrap();
    let target = {
        let mut entry = EntryFactory::from_text("クリップボード履歴の保存先");
        entry.search.normalized_text = normalize_text(entry.plain_text().unwrap());
        store.insert(entry).await.unwrap()
    };
    let _ = insert_text(&store, "完全に別の日本語の文章").await;

    let mut query = SearchQuery::new("ボード", normalize_text("ボード"), 10);
    query.mode = SearchMode::Fuzzy;
    let results = store.search(query).await.unwrap();
    assert!(results.iter().map(|r| r.entry_id).any(|x| x == target));
}

#[tokio::test]
async fn mixed_cjk_ascii_query_finds_entries_in_auto_mode() {
    let store = SqliteStore::open_memory().unwrap();
    let target = {
        let mut entry = EntryFactory::from_text("メモ alpha 設計");
        entry.search.normalized_text = normalize_text(entry.plain_text().unwrap());
        store.insert(entry).await.unwrap()
    };
    let _ = insert_text(&store, "純粋な日本語のメモ").await;
    let _ = insert_text(&store, "english only note").await;

    let query = SearchQuery::new("alpha 設計", normalize_text("alpha 設計"), 10);
    // Auto plan exercises LIKE + FTS + fuzzy together.
    let results = store.search(query).await.unwrap();
    assert!(results.iter().map(|r| r.entry_id).any(|x| x == target));
}

#[tokio::test]
async fn source_app_filter_isolates_by_bundle_id() {
    let store = SqliteStore::open_memory().unwrap();
    let editor =
        insert_with_source(&store, "shared keyword editor side", "com.example.editor").await;
    let _terminal = insert_with_source(
        &store,
        "shared keyword terminal side",
        "com.example.terminal",
    )
    .await;

    let mut query = SearchQuery::new("shared", normalize_text("shared"), 10);
    query.filters = SearchFilters {
        source_app: Some("com.example.editor".to_owned()),
        ..Default::default()
    };
    let results = store.search(query).await.unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].entry_id, editor);
}

#[tokio::test]
async fn created_after_and_before_filters_clip_window() {
    let store = SqliteStore::open_memory().unwrap();
    let now = OffsetDateTime::now_utc();
    let ancient = insert_text(&store, "window keyword ancient").await;
    let middle = insert_text(&store, "window keyword middle").await;
    let recent = insert_text(&store, "window keyword recent").await;
    backdate_entry(&store, ancient, now - time::Duration::days(10));
    backdate_entry(&store, middle, now - time::Duration::days(5));
    backdate_entry(&store, recent, now - time::Duration::days(1));

    let mut after_query = SearchQuery::new("window", normalize_text("window"), 10);
    after_query.filters = SearchFilters {
        created_after: Some(now - time::Duration::days(7)),
        ..Default::default()
    };
    let after_hits = store
        .search(after_query)
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.entry_id)
        .collect::<Vec<_>>();
    assert!(after_hits.contains(&middle));
    assert!(after_hits.contains(&recent));
    assert!(!after_hits.contains(&ancient));

    let mut before_query = SearchQuery::new("window", normalize_text("window"), 10);
    before_query.filters = SearchFilters {
        created_before: Some(now - time::Duration::days(3)),
        ..Default::default()
    };
    let before_hits = store
        .search(before_query)
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.entry_id)
        .collect::<Vec<_>>();
    assert!(before_hits.contains(&ancient));
    assert!(before_hits.contains(&middle));
    assert!(!before_hits.contains(&recent));
}

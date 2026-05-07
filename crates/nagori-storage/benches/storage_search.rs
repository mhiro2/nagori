//! Criterion benchmarks for `SqliteStore` at three corpus sizes (1k / 10k /
//! 100k entries). Each iteration runs a single search; criterion samples
//! enough iterations to derive median and tail latency.
//!
//! Run with `cargo bench -p nagori-storage -- search`.

use std::time::Duration;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use nagori_core::{EntryFactory, EntryRepository, SearchMode, SearchQuery};
use nagori_search::normalize_text;
use nagori_storage::SqliteStore;
use tokio::runtime::Builder;

const SIZES: [usize; 3] = [1_000, 10_000, 100_000];
const QUERY_RECENT: &str = "alpha";
const QUERY_FULLTEXT: &str = "needle";

fn make_text(idx: usize) -> String {
    // Inject a sparse `needle` token (≈0.1% of corpus) so FullText returns
    // a meaningful result set without flooding the candidate pool.
    let salt = match idx % 1000 {
        0 => "needle",
        1 => "alpha",
        _ => "filler",
    };
    format!("entry-{idx:08} {salt} the quick brown fox jumps over lazy dog {idx:04x}")
}

fn populate(store: &SqliteStore, n: usize, runtime: &tokio::runtime::Runtime) {
    runtime.block_on(async {
        for idx in 0..n {
            let text = make_text(idx);
            let mut entry = EntryFactory::from_text(&text);
            entry.search.normalized_text = normalize_text(&text);
            store.insert(entry).await.expect("insert");
        }
    });
}

fn run_search(
    store: &SqliteStore,
    mode: SearchMode,
    query: &str,
    runtime: &tokio::runtime::Runtime,
) {
    runtime.block_on(async {
        let mut q = SearchQuery::new(query, normalize_text(query), 50);
        q.mode = mode;
        let _ = store.search(q).await.expect("search");
    });
}

fn bench_search(c: &mut Criterion) {
    let runtime = Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");

    let mut group = c.benchmark_group("storage_search");
    group.sample_size(20);
    group.measurement_time(Duration::from_secs(8));

    for &size in &SIZES {
        let store = SqliteStore::open_memory().expect("memory store");
        populate(&store, size, &runtime);

        group.bench_with_input(BenchmarkId::new("recent", size), &size, |b, _| {
            b.iter(|| run_search(&store, SearchMode::Recent, QUERY_RECENT, &runtime));
        });
        group.bench_with_input(BenchmarkId::new("fulltext", size), &size, |b, _| {
            b.iter(|| run_search(&store, SearchMode::FullText, QUERY_FULLTEXT, &runtime));
        });
        group.bench_with_input(BenchmarkId::new("auto_hybrid", size), &size, |b, _| {
            b.iter(|| run_search(&store, SearchMode::Auto, QUERY_FULLTEXT, &runtime));
        });
    }

    group.finish();
}

criterion_group!(benches, bench_search);
criterion_main!(benches);

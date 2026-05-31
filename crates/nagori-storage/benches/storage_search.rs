//! Search latency harness for `SqliteStore` reporting p50 / p95 / max per
//! (dataset, corpus size, query) — the "hotkey → query → results" tail the
//! palette actually feels.
//!
//! Why a custom harness instead of criterion: criterion is built for robust
//! mean / median regression detection and does not surface per-operation p95 /
//! max, which is exactly the metric search latency is judged on. This binary
//! populates each dataset once against a **file-backed** store (so the real
//! pool-of-4 + WAL hybrid fan-out is exercised, not the capacity-1 in-memory
//! store), then times each query pattern over a warmup + sampled run and prints
//! percentiles.
//!
//! Run with `cargo bench -p nagori-storage`.
//!
//! Configuration (all optional, via env):
//! * `NAGORI_BENCH_FULL=1`  — every dataset at every size (heavy: 100k populate
//!   per dataset). Default is a representative subset.
//! * `NAGORI_BENCH_ITERS=N` — timed samples per query (default 200).
//! * `NAGORI_BENCH_SIZES=10000,100000` — override corpus sizes.
//! * `NAGORI_BENCH_DATASETS=text,url` — restrict to named datasets.

use std::time::{Duration, Instant};

use nagori_core::{EntryFactory, EntryRepository, SearchMode, SearchQuery};
use nagori_search::normalize_text;
use nagori_storage::SqliteStore;
use tokio::runtime::{Builder, Runtime};

/// How the runner classifies a query so it can attach the acceptance target
/// from the plan (empty/recent vs. a normal typed query) and tag OK / OVER.
#[derive(Clone, Copy, PartialEq, Eq)]
enum QueryKind {
    /// Empty query → `Recent` plan. Target: p95 < 50ms @ 10k.
    Empty,
    /// A "normal" typed query that returns a real result set. Target:
    /// p95 < 100ms @ 10k, p95 ~200ms @ 100k.
    Normal,
    /// Short / single-char / typo / domain probes — no hard acceptance target,
    /// measured for tuning (debounce, oversample, ngram fan-out).
    Probe,
}

struct QueryCase {
    name: &'static str,
    raw: &'static str,
    mode: SearchMode,
    kind: QueryKind,
}

struct Dataset {
    name: &'static str,
    /// Generates entry `idx`'s body. Content kind is inferred by
    /// `EntryFactory::from_text` (URL / code detection), so the generators
    /// shape the corpus mix.
    make: fn(usize) -> String,
    queries: &'static [QueryCase],
}

// ---- dataset generators -------------------------------------------------

fn gen_text(idx: usize) -> String {
    // Sparse `needle` token (~0.1%) so a normal query returns a small, realistic
    // result set instead of flooding the candidate pool.
    let salt = match idx % 1000 {
        0 => "needle",
        1 => "alpha",
        _ => "filler",
    };
    format!("entry-{idx:08} {salt} the quick brown fox jumps over the lazy dog {idx:04x}")
}

fn gen_url(idx: usize) -> String {
    let host = match idx % 7 {
        0 => "github.com",
        1 => "docs.rs",
        2 => "stackoverflow.com",
        3 => "developer.mozilla.org",
        4 => "news.ycombinator.com",
        5 => "crates.io",
        _ => "example.com",
    };
    format!("https://{host}/path/{idx}/page?ref={idx:x}")
}

fn gen_cjk(idx: usize) -> String {
    let marker = match idx % 1000 {
        0 => "検索エンジン",
        1 => "正規表現",
        _ => "サンプル",
    };
    format!("クリップボード履歴の項目{idx} {marker} 日本語テキストのテスト{idx:04x}")
}

fn gen_code(idx: usize) -> String {
    // `fn ` + newline + braces trips `CodeContent::looks_like_code`, so these
    // land as `ContentKind::Code`.
    let token = match idx % 1000 {
        0 => "kubectl",
        1 => "SELECT",
        _ => "println",
    };
    format!(
        "fn handler_{idx}() {{\n    let cmd = \"{token} get pods --ns {idx}\";\n    println!(\"{{cmd}}\");\n}}\n"
    )
}

fn gen_long(idx: usize) -> String {
    // ~1KB body, still under MAX_NGRAM_INPUT_CHARS (4096).
    let salt = if idx.is_multiple_of(1000) {
        "needle"
    } else {
        "filler"
    };
    let body = "lorem ipsum dolor sit amet consectetur ".repeat(28);
    format!("doc-{idx:08} {salt} {body}")
}

// ---- query patterns -----------------------------------------------------

const Q_TEXT: &[QueryCase] = &[
    QueryCase {
        name: "empty/recent",
        raw: "",
        mode: SearchMode::Recent,
        kind: QueryKind::Empty,
    },
    QueryCase {
        name: "one-char",
        raw: "n",
        mode: SearchMode::Auto,
        kind: QueryKind::Probe,
    },
    QueryCase {
        name: "short",
        raw: "ne",
        mode: SearchMode::Auto,
        kind: QueryKind::Probe,
    },
    QueryCase {
        name: "normal(auto)",
        raw: "needle",
        mode: SearchMode::Auto,
        kind: QueryKind::Normal,
    },
    QueryCase {
        name: "fulltext",
        raw: "needle",
        mode: SearchMode::FullText,
        kind: QueryKind::Probe,
    },
    QueryCase {
        name: "typo(fuzzy)",
        raw: "needel",
        mode: SearchMode::Fuzzy,
        kind: QueryKind::Probe,
    },
];

const Q_URL: &[QueryCase] = &[
    QueryCase {
        name: "empty/recent",
        raw: "",
        mode: SearchMode::Recent,
        kind: QueryKind::Empty,
    },
    QueryCase {
        name: "domain",
        raw: "github",
        mode: SearchMode::Auto,
        kind: QueryKind::Normal,
    },
    QueryCase {
        name: "short",
        raw: "do",
        mode: SearchMode::Auto,
        kind: QueryKind::Probe,
    },
    QueryCase {
        name: "full-url",
        raw: "github.com/path",
        mode: SearchMode::Auto,
        kind: QueryKind::Probe,
    },
];

const Q_CJK: &[QueryCase] = &[
    QueryCase {
        name: "empty/recent",
        raw: "",
        mode: SearchMode::Recent,
        kind: QueryKind::Empty,
    },
    QueryCase {
        name: "jp-one-char",
        raw: "検",
        mode: SearchMode::Auto,
        kind: QueryKind::Probe,
    },
    QueryCase {
        name: "jp-partial",
        raw: "検索",
        mode: SearchMode::Auto,
        kind: QueryKind::Normal,
    },
    QueryCase {
        name: "jp-phrase",
        raw: "検索エンジン",
        mode: SearchMode::Auto,
        kind: QueryKind::Probe,
    },
];

const Q_CODE: &[QueryCase] = &[
    QueryCase {
        name: "empty/recent",
        raw: "",
        mode: SearchMode::Recent,
        kind: QueryKind::Empty,
    },
    QueryCase {
        name: "code-token",
        raw: "kubectl",
        mode: SearchMode::Auto,
        kind: QueryKind::Normal,
    },
    QueryCase {
        name: "short",
        raw: "ku",
        mode: SearchMode::Auto,
        kind: QueryKind::Probe,
    },
];

const Q_LONG: &[QueryCase] = &[
    QueryCase {
        name: "empty/recent",
        raw: "",
        mode: SearchMode::Recent,
        kind: QueryKind::Empty,
    },
    QueryCase {
        name: "normal(auto)",
        raw: "needle",
        mode: SearchMode::Auto,
        kind: QueryKind::Normal,
    },
    QueryCase {
        name: "short",
        raw: "lo",
        mode: SearchMode::Auto,
        kind: QueryKind::Probe,
    },
];

const DATASETS: &[Dataset] = &[
    Dataset {
        name: "text",
        make: gen_text,
        queries: Q_TEXT,
    },
    Dataset {
        name: "url",
        make: gen_url,
        queries: Q_URL,
    },
    Dataset {
        name: "cjk",
        make: gen_cjk,
        queries: Q_CJK,
    },
    Dataset {
        name: "code",
        make: gen_code,
        queries: Q_CODE,
    },
    Dataset {
        name: "long",
        make: gen_long,
        queries: Q_LONG,
    },
];

// ---- runner -------------------------------------------------------------

fn populate(store: &SqliteStore, dataset: &Dataset, n: usize, rt: &Runtime) {
    rt.block_on(async {
        for idx in 0..n {
            let text = (dataset.make)(idx);
            let entry = EntryFactory::from_text(&text);
            store.insert(entry).await.expect("insert");
            if (idx + 1).is_multiple_of(20_000) {
                eprintln!("    populated {}/{n}", idx + 1);
            }
        }
    });
}

fn run_once(store: &SqliteStore, case: &QueryCase, rt: &Runtime) {
    rt.block_on(async {
        let mut q = SearchQuery::new(case.raw, normalize_text(case.raw), 50);
        q.mode = case.mode;
        let _ = store.search(q).await.expect("search");
    });
}

fn percentile(sorted: &[Duration], p: f64) -> Duration {
    if sorted.is_empty() {
        return Duration::ZERO;
    }
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]
    let rank = (p / 100.0 * (sorted.len() - 1) as f64).round() as usize;
    sorted[rank.min(sorted.len() - 1)]
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

/// Acceptance target for `(kind, size)`, or `None` when there is no hard goal.
const fn target_p95_ms(kind: QueryKind, size: usize) -> Option<f64> {
    match (kind, size) {
        (QueryKind::Empty, s) if s <= 10_000 => Some(50.0),
        (QueryKind::Normal, s) if s <= 10_000 => Some(100.0),
        (QueryKind::Normal, _) => Some(200.0),
        _ => None,
    }
}

fn measure(store: &SqliteStore, case: &QueryCase, size: usize, iters: usize, rt: &Runtime) {
    // Warm caches / prepared statements before timing.
    for _ in 0..10 {
        run_once(store, case, rt);
    }
    let mut samples = Vec::with_capacity(iters);
    for _ in 0..iters {
        let start = Instant::now();
        run_once(store, case, rt);
        samples.push(start.elapsed());
    }
    samples.sort_unstable();

    let p50 = percentile(&samples, 50.0);
    let p95 = percentile(&samples, 95.0);
    let max = *samples.last().unwrap();
    let marker = match target_p95_ms(case.kind, size) {
        Some(target) if ms(p95) <= target => format!("[OK <{target:.0}ms]"),
        Some(target) => format!("[OVER >{target:.0}ms]"),
        None => String::new(),
    };
    println!(
        "  {:<14} {:>4}  p50={:>8.3}ms  p95={:>8.3}ms  max={:>8.3}ms  {}",
        case.name,
        iters,
        ms(p50),
        ms(p95),
        ms(max),
        marker,
    );
}

/// Optional `NAGORI_BENCH_DATASETS=text,url` allowlist restricting which
/// datasets run. `None` means every dataset in the plan.
fn dataset_filter() -> Option<Vec<String>> {
    let raw = std::env::var("NAGORI_BENCH_DATASETS").ok()?;
    let names: Vec<String> = raw
        .split(',')
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .collect();
    (!names.is_empty()).then_some(names)
}

fn parse_sizes() -> Vec<usize> {
    if let Ok(raw) = std::env::var("NAGORI_BENCH_SIZES") {
        let sizes: Vec<usize> = raw
            .split(',')
            .filter_map(|s| s.trim().parse().ok())
            .collect();
        if !sizes.is_empty() {
            return sizes;
        }
    }
    vec![10_000]
}

/// `(dataset, size)` combinations to measure. The representative default
/// validates every acceptance target with the smallest populate cost; the full
/// matrix runs every dataset at every requested size.
fn plan(sizes: &[usize], full: bool) -> Vec<(&'static Dataset, usize)> {
    let mut combos = Vec::new();
    if full {
        for ds in DATASETS {
            for &size in sizes {
                combos.push((ds, size));
            }
        }
        return combos;
    }
    // Representative subset: every dataset at the smallest requested size for
    // breadth, plus `text` at the largest size for the 100k acceptance target.
    let small = *sizes.iter().min().unwrap();
    let large = *sizes.iter().max().unwrap();
    for ds in DATASETS {
        combos.push((ds, small));
    }
    if large != small {
        let text = DATASETS.iter().find(|d| d.name == "text").unwrap();
        combos.push((text, large));
    }
    combos
}

fn main() {
    let rt = Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");

    let full = std::env::var("NAGORI_BENCH_FULL").is_ok_and(|v| v == "1" || v == "true");
    let iters: usize = std::env::var("NAGORI_BENCH_ITERS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(200);
    let sizes = parse_sizes();
    let only = dataset_filter();
    let combos: Vec<_> = plan(&sizes, full)
        .into_iter()
        .filter(|(ds, _)| {
            only.as_ref()
                .is_none_or(|names| names.iter().any(|n| n == ds.name))
        })
        .collect();

    println!(
        "search latency harness — iters={iters} full={full} sizes={sizes:?} datasets={} (set NAGORI_BENCH_FULL=1 for the full matrix)",
        only.as_ref()
            .map_or_else(|| "all".to_owned(), |n| n.join(",")),
    );

    for (dataset, size) in combos {
        eprintln!("populating dataset={} size={size} ...", dataset.name);
        let dir = tempfile::tempdir().expect("tempdir");
        let store = SqliteStore::open(dir.path().join("bench.db")).expect("open store");
        populate(&store, dataset, size, &rt);

        println!("\ndataset={} size={size}", dataset.name);
        for case in dataset.queries {
            measure(&store, case, size, iters, &rt);
        }
    }
}

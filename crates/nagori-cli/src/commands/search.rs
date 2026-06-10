use anyhow::Result;
use nagori_core::SearchQuery;
use nagori_ipc::{IpcRequest, SearchRequest};
use nagori_search::normalize_text;

use super::{Executor, expect_search};
use crate::output::{print_dto_search, print_search_results};
use crate::{OutputFormat, SearchArgs};

pub async fn run(executor: &Executor, args: SearchArgs, format: OutputFormat) -> Result<()> {
    match executor {
        Executor::Local(ctx) => {
            let store = ctx.open_store()?;
            // The direct/local search path must not serve stale CJK grams: drain
            // any ngram rebuild left pending by a generator upgrade (kana folding
            // / Han 1-grams) before querying. When a daemon has already rebuilt —
            // the common case — this is a single zero-row check; only an offline
            // `--db` DB that no daemon has touched pays the one-time rebuild here.
            // Other direct commands (list/get) don't need current grams.
            while store.rebuild_stale_ngrams().await? > 0 {}
            let query = SearchQuery::new(&args.query, normalize_text(&args.query), args.limit);
            let results = store.search(query).await?;
            print_search_results(results, format)
        }
        Executor::Ipc(ctx) => {
            let resp = ctx
                .client
                .send(IpcRequest::Search(SearchRequest {
                    query: args.query,
                    limit: args.limit,
                }))
                .await?;
            print_dto_search(expect_search(resp)?, format)
        }
    }
}

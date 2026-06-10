use anyhow::Result;
use nagori_core::EntryRepository;
use nagori_ipc::{IpcRequest, ListPinnedRequest, ListRecentRequest};

use super::{Executor, expect_entries};
use crate::output::{print_dto_entries, print_entries};
use crate::{ListArgs, OutputFormat};

pub async fn run(executor: &Executor, args: &ListArgs, format: OutputFormat) -> Result<()> {
    match executor {
        Executor::Local(ctx) => {
            let store = ctx.open_store()?;
            let entries = if args.pinned {
                store.list_pinned().await?
            } else {
                store.list_recent(args.limit).await?
            };
            print_entries(entries, format, args.include_sensitive)
        }
        Executor::Ipc(ctx) => {
            let request = if args.pinned {
                IpcRequest::ListPinned(ListPinnedRequest {
                    include_sensitive: args.include_sensitive,
                })
            } else {
                IpcRequest::ListRecent(ListRecentRequest {
                    limit: args.limit,
                    include_sensitive: args.include_sensitive,
                })
            };
            let resp = ctx.client.send(request).await?;
            print_dto_entries(expect_entries(resp)?, format)
        }
    }
}

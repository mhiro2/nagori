use anyhow::Result;
use nagori_core::{AppError, EntryRepository, is_text_safe_for_default_output};
use nagori_ipc::{GetEntryRequest, IpcRequest};

use super::{Executor, expect_entry, parse_id};
use crate::output::{print_dto_entry, print_entry};
use crate::{GetArgs, OutputFormat};

pub async fn run(executor: &Executor, args: &GetArgs, format: OutputFormat) -> Result<()> {
    match executor {
        // The local arm opens the store *before* parsing the id — the order
        // the pre-split dispatcher used — so a broken DB keeps surfacing as
        // a storage error rather than being shadowed by input validation.
        Executor::Local(ctx) => {
            let store = ctx.open_store()?;
            let id = parse_id(&args.id)?;
            let entry = store
                .get(id)
                .await?
                .ok_or_else(|| anyhow::Error::new(AppError::NotFound))?;
            let include_text =
                args.include_sensitive || is_text_safe_for_default_output(entry.sensitivity);
            print_entry(&entry, format, include_text)
        }
        Executor::Ipc(ctx) => {
            let resp = ctx
                .client
                .send(IpcRequest::GetEntry(GetEntryRequest {
                    id: parse_id(&args.id)?,
                    include_sensitive: args.include_sensitive,
                }))
                .await?;
            print_dto_entry(&expect_entry(resp)?, format)
        }
    }
}

use anyhow::Result;
use nagori_core::EntryRepository;
use nagori_ipc::{DeleteEntryRequest, IpcRequest};

use super::{Executor, expect_ack, parse_id};
use crate::output::print_ack;
use crate::{IdArgs, OutputFormat};

pub async fn run(executor: &Executor, args: &IdArgs, format: OutputFormat) -> Result<()> {
    match executor {
        // Store first, id second — the pre-split dispatcher's precedence, so
        // a broken DB surfaces as a storage error rather than being shadowed
        // by input validation.
        Executor::Local(ctx) => {
            let store = ctx.open_store()?;
            store.mark_deleted(parse_id(&args.id)?).await?;
        }
        Executor::Ipc(ctx) => {
            expect_ack(
                ctx.client
                    .send(IpcRequest::DeleteEntry(DeleteEntryRequest {
                        id: parse_id(&args.id)?,
                    }))
                    .await?,
            )?;
        }
    }
    print_ack(format);
    Ok(())
}

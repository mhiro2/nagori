use anyhow::Result;
use nagori_core::EntryRepository;
use nagori_ipc::{DeleteEntryRequest, IpcRequest};

use super::{Executor, expect_ack, parse_id};
use crate::output::print_ack;
use crate::{IdArgs, OutputFormat};

pub async fn run(executor: &Executor, args: &IdArgs, format: OutputFormat) -> Result<()> {
    let id = parse_id(&args.id)?;
    match executor {
        Executor::Local(ctx) => {
            ctx.open_store()?.mark_deleted(id).await?;
        }
        Executor::Ipc(ctx) => {
            expect_ack(
                ctx.client
                    .send(IpcRequest::DeleteEntry(DeleteEntryRequest { id }))
                    .await?,
            )?;
        }
    }
    print_ack(format);
    Ok(())
}

use anyhow::Result;
use nagori_ipc::{CopyEntryRequest, IpcRequest};

use super::{Executor, build_runtime, expect_ack, parse_id};
use crate::output::print_ack;
use crate::{IdArgs, OutputFormat};

pub async fn run(executor: &Executor, args: &IdArgs, format: OutputFormat) -> Result<()> {
    let id = parse_id(&args.id)?;
    match executor {
        Executor::Local(ctx) => {
            let runtime = build_runtime(ctx.open_store()?)?;
            runtime.copy_entry(id).await?;
        }
        Executor::Ipc(ctx) => {
            expect_ack(
                ctx.client
                    .send(IpcRequest::CopyEntry(CopyEntryRequest { id }))
                    .await?,
            )?;
        }
    }
    print_ack(format);
    Ok(())
}

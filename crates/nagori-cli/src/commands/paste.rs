use anyhow::Result;
use nagori_ipc::{IpcRequest, PasteEntryRequest};

use super::{Executor, build_runtime, expect_ack, parse_id};
use crate::output::print_ack;
use crate::{IdArgs, OutputFormat};

pub async fn run(executor: &Executor, args: &IdArgs, format: OutputFormat) -> Result<()> {
    let id = parse_id(&args.id)?;
    match executor {
        Executor::Local(ctx) => {
            let runtime = build_runtime(ctx.open_store()?)?;
            runtime.paste_entry(id, None).await?;
        }
        Executor::Ipc(ctx) => {
            expect_ack(
                ctx.client
                    .send(IpcRequest::PasteEntry(PasteEntryRequest {
                        id,
                        format: None,
                    }))
                    .await?,
            )?;
        }
    }
    print_ack(format);
    Ok(())
}

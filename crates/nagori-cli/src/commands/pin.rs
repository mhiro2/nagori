use anyhow::Result;
use nagori_ipc::{IpcRequest, PinEntryRequest};

use super::{Executor, expect_ack, parse_id};
use crate::output::print_ack;
use crate::{IdArgs, OutputFormat};

/// Shared by `pin` (pinned = true) and `unpin` (pinned = false).
pub async fn run(
    executor: &Executor,
    args: &IdArgs,
    pinned: bool,
    format: OutputFormat,
) -> Result<()> {
    let id = parse_id(&args.id)?;
    match executor {
        Executor::Local(ctx) => {
            ctx.open_store()?.set_pinned(id, pinned).await?;
        }
        Executor::Ipc(ctx) => {
            expect_ack(
                ctx.client
                    .send(IpcRequest::PinEntry(PinEntryRequest { id, pinned }))
                    .await?,
            )?;
        }
    }
    print_ack(format);
    Ok(())
}

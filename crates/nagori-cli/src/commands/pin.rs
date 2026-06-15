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
    match executor {
        // Store first, id second — the pre-split dispatcher's precedence.
        Executor::Local(ctx) => {
            let store = ctx.open_store()?;
            store.set_pinned(parse_id(&args.id)?, pinned).await?;
        }
        Executor::Ipc(ctx) => {
            expect_ack(
                ctx.client
                    .send(IpcRequest::PinEntry(PinEntryRequest {
                        id: parse_id(&args.id)?,
                        pinned,
                    }))
                    .await?,
            )?;
        }
    }
    print_ack(format)
}

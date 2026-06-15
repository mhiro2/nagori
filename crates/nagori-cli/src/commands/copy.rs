use anyhow::Result;
use nagori_ipc::{CopyEntryRequest, IpcRequest};

use super::{Executor, build_runtime, expect_ack, parse_id};
use crate::output::print_ack;
use crate::{IdArgs, OutputFormat};

pub async fn run(executor: &Executor, args: &IdArgs, format: OutputFormat) -> Result<()> {
    match executor {
        // Store first, then id, then the clipboard-backed runtime — the
        // pre-split dispatcher's precedence: an invalid id must fail before
        // the runtime build so the command never needs clipboard access.
        Executor::Local(ctx) => {
            let store = ctx.open_store()?;
            let id = parse_id(&args.id)?;
            let runtime = build_runtime(store)?;
            runtime.copy_entry(id).await?;
        }
        Executor::Ipc(ctx) => {
            expect_ack(
                ctx.client
                    .send(IpcRequest::CopyEntry(CopyEntryRequest {
                        id: parse_id(&args.id)?,
                    }))
                    .await?,
            )?;
        }
    }
    print_ack(format)
}

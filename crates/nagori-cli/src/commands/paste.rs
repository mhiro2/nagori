use anyhow::Result;
use nagori_ipc::{IpcRequest, PasteEntryRequest};

use super::{Executor, build_runtime, expect_ack, parse_id};
use crate::output::print_ack;
use crate::{IdArgs, OutputFormat};

pub async fn run(executor: &Executor, args: &IdArgs, format: OutputFormat) -> Result<()> {
    match executor {
        // Store first, then id, then the clipboard-backed runtime — the
        // pre-split dispatcher's precedence (see `copy`).
        Executor::Local(ctx) => {
            let store = ctx.open_store()?;
            let id = parse_id(&args.id)?;
            let runtime = build_runtime(store)?;
            runtime.paste_entry(id, None).await?;
        }
        Executor::Ipc(ctx) => {
            expect_ack(
                ctx.client
                    .send(IpcRequest::PasteEntry(PasteEntryRequest {
                        id: parse_id(&args.id)?,
                        format: None,
                    }))
                    .await?,
            )?;
        }
    }
    print_ack(format)
}

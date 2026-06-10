use anyhow::Result;
use nagori_ipc::{IpcRequest, RunQuickActionRequest};

use super::{Executor, build_headless_runtime, expect_ai_output, parse_id};
use crate::output::print_ai_output;
use crate::{OutputFormat, QuickArgs};

pub async fn run(executor: &Executor, args: &QuickArgs, format: OutputFormat) -> Result<()> {
    match executor {
        // Store, then runtime, then id — the pre-split dispatcher's
        // precedence for this command.
        Executor::Local(ctx) => {
            let runtime = build_headless_runtime(ctx.open_store()?)?;
            let output = runtime
                .run_quick_action(parse_id(&args.id)?, args.action)
                .await?;
            print_ai_output(&output.into(), format)
        }
        Executor::Ipc(ctx) => {
            let resp = ctx
                .client
                .send(IpcRequest::RunQuickAction(RunQuickActionRequest {
                    id: parse_id(&args.id)?,
                    action: args.action,
                }))
                .await?;
            print_ai_output(&expect_ai_output(resp)?, format)
        }
    }
}

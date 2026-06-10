use anyhow::Result;
use nagori_core::AppError;
use nagori_ipc::{ClearRequest, IpcRequest};
use time::OffsetDateTime;

use super::{Executor, expect_cleared};
use crate::output::print_clear_result;
use crate::{ClearArgs, OutputFormat};

pub async fn run(executor: &Executor, args: &ClearArgs, format: OutputFormat) -> Result<()> {
    match executor {
        // Store first, args second — the pre-split dispatcher's precedence.
        Executor::Local(ctx) => {
            let store = ctx.open_store()?;
            let cutoff = match clear_request_from_args(args)? {
                ClearRequest::All => OffsetDateTime::now_utc(),
                ClearRequest::OlderThanDays { days } => {
                    OffsetDateTime::now_utc() - time::Duration::days(i64::from(days))
                }
            };
            let deleted = store.clear_older_than(cutoff).await?;
            print_clear_result(deleted, format);
        }
        Executor::Ipc(ctx) => {
            let request = clear_request_from_args(args)?;
            let resp = ctx.client.send(IpcRequest::Clear(request)).await?;
            print_clear_result(expect_cleared(resp)?.deleted, format);
        }
    }
    Ok(())
}

fn clear_request_from_args(args: &ClearArgs) -> Result<ClearRequest> {
    // The clap arg group enforces "exactly one of --all / --older-than-days",
    // so reaching this point with neither set means a clap bug or a manual
    // struct construction. Defend in depth.
    match (args.older_than_days, args.all) {
        (Some(days), false) => {
            let days = u32::try_from(days)
                .map_err(|_| AppError::InvalidInput("--older-than-days must be >= 0".into()))?;
            Ok(ClearRequest::OlderThanDays { days })
        }
        (None, true) => Ok(ClearRequest::All),
        _ => Err(AppError::InvalidInput("specify --all or --older-than-days".into()).into()),
    }
}

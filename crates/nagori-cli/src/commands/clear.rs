use anyhow::Result;
use nagori_core::{AppError, RetentionDays};
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
                ClearRequest::OlderThanDays { days } => days.cutoff(OffsetDateTime::now_utc()),
            };
            let deleted = store.clear_older_than(cutoff).await?;
            print_clear_result(deleted, format)?;
        }
        Executor::Ipc(ctx) => {
            let request = clear_request_from_args(args)?;
            let resp = ctx.client.send(IpcRequest::Clear(request)).await?;
            print_clear_result(expect_cleared(resp)?.deleted, format)?;
        }
    }
    Ok(())
}

fn clear_request_from_args(args: &ClearArgs) -> Result<ClearRequest> {
    // The clap arg group enforces "exactly one of --all / --older-than-days",
    // so reaching this point with neither set means a clap bug or a manual
    // struct construction. Defend in depth.
    match (args.older_than_days, args.all) {
        // `clap`'s `value_parser` range already rejects `0` and anything past
        // `MAX_RETENTION_DAYS`, so this only fires for a hand-built
        // `ClearArgs`. Validating here as well keeps the window bounded for
        // every caller rather than only the parsed one.
        (Some(days), false) => Ok(ClearRequest::OlderThanDays {
            days: RetentionDays::new(days)?,
        }),
        (None, true) => Ok(ClearRequest::All),
        _ => Err(AppError::InvalidInput("specify --all or --older-than-days".into()).into()),
    }
}

#[cfg(test)]
mod tests {
    use nagori_core::MAX_RETENTION_DAYS;

    use super::*;

    fn args(older_than_days: Option<u32>, all: bool) -> ClearArgs {
        ClearArgs {
            older_than_days,
            all,
        }
    }

    #[test]
    fn a_hand_built_window_is_still_bounded() {
        // `clap` guards the parsed path; this is the other one. Constructing
        // `ClearArgs` directly used to hand `u32::MAX` days straight to the
        // cutoff subtraction, which panicked out of `OffsetDateTime`'s range.
        for rejected in [0, MAX_RETENTION_DAYS + 1, u32::MAX] {
            let err = clear_request_from_args(&args(Some(rejected), false))
                .expect_err("an out-of-range window must be refused");
            assert!(
                err.downcast_ref::<AppError>()
                    .is_some_and(|err| matches!(err, AppError::InvalidInput(_))),
                "{rejected} days must map to InvalidInput, got {err:?}"
            );
        }
    }

    #[test]
    fn a_legal_window_and_all_map_to_their_requests() {
        assert!(matches!(
            clear_request_from_args(&args(Some(MAX_RETENTION_DAYS), false))
                .expect("the ceiling is a legal window"),
            ClearRequest::OlderThanDays { days } if days.get() == MAX_RETENTION_DAYS
        ));
        assert!(matches!(
            clear_request_from_args(&args(None, true)).expect("--all"),
            ClearRequest::All
        ));
        // Neither scope is a clap-group violation, so reaching the mapper
        // that way is a bug rather than a wipe-everything instruction.
        assert!(clear_request_from_args(&args(None, false)).is_err());
    }
}

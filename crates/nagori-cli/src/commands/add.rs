use anyhow::{Context as _, Result};
use nagori_core::{
    AppError, EntryRepository, MAX_ENTRY_SIZE_BYTES, is_text_safe_for_default_output,
};
use nagori_ipc::{AddEntryRequest, IpcRequest};

use super::{Executor, build_headless_runtime, expect_entry};
use crate::output::{print_dto_entry, print_entry};
use crate::{AddArgs, OutputFormat};

pub async fn run(executor: &Executor, args: AddArgs, format: OutputFormat) -> Result<()> {
    match executor {
        // The local arm opens the store *before* reading the input — the
        // order the pre-split dispatcher used — so a broken DB fails fast
        // instead of consuming stdin first.
        Executor::Local(ctx) => {
            let store = ctx.open_store()?;
            let text = read_text(args)?;
            let runtime = build_headless_runtime(store.clone())?;
            let id = runtime.add_text(text).await?;
            let entry = store
                .get(id)
                .await?
                .context("entry not found after insert")?;
            print_entry(
                &entry,
                format,
                is_text_safe_for_default_output(entry.sensitivity),
            )
        }
        Executor::Ipc(ctx) => {
            let text = read_text(args)?;
            let resp = ctx
                .client
                .send(IpcRequest::AddEntry(AddEntryRequest { text }))
                .await?;
            print_dto_entry(&expect_entry(resp)?, format)
        }
    }
}

fn read_text(args: AddArgs) -> Result<String> {
    if args.stdin {
        use std::io::Read;
        // Bound the read so an unbounded or hostile stdin (e.g. `cat /dev/zero |
        // nagori add --stdin`) cannot OOM the CLI process. The daemon's bounded
        // reader only protects the server side; this guards the client itself.
        // Read one byte past the ceiling so a payload sitting exactly at the cap
        // is still accepted while anything larger is rejected.
        //
        // Read raw bytes rather than straight into a `String`: `read_to_string`
        // validates UTF-8 while it fills the buffer, so an oversized input whose
        // `cap + 1` boundary splits a multi-byte char would surface as a UTF-8
        // error (exit 8) before the size check runs, escaping the "oversize =>
        // exit 2" contract. Check the length first, then validate UTF-8.
        let mut buffer = Vec::new();
        std::io::stdin()
            .take(MAX_ENTRY_SIZE_BYTES as u64 + 1)
            .read_to_end(&mut buffer)?;
        if buffer.len() > MAX_ENTRY_SIZE_BYTES {
            return Err(AppError::InvalidInput(format!(
                "stdin input exceeds the maximum entry size of {MAX_ENTRY_SIZE_BYTES} bytes"
            ))
            .into());
        }
        // A bad-input failure, not an internal one: classify it as
        // `InvalidInput` (exit 2) so it lines up with the oversize check above
        // instead of falling through to the internal-error bucket (exit 8).
        String::from_utf8(buffer).map_err(|err| {
            AppError::InvalidInput(format!("stdin input is not valid UTF-8: {err}")).into()
        })
    } else {
        // Same classification as the stdin errors: a missing input selector is
        // a usage error (exit 2), not an internal failure.
        args.text.ok_or_else(|| {
            AppError::InvalidInput("either --text or --stdin must be provided".to_owned()).into()
        })
    }
}

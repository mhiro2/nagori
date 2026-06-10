use anyhow::{Result, anyhow};
use futures::StreamExt;
use nagori_core::{AiActionId, AiEvent, AiRequestOptions, EntryId};
use nagori_daemon::NagoriRuntime;
use nagori_ipc::{AiOutputDto, IpcRequest, RunAiActionRequest};

use super::{Executor, build_headless_runtime, expect_ai_output, parse_id};
use crate::output::print_ai_output;
use crate::{AiArgs, OutputFormat};

pub async fn run(executor: &Executor, args: &AiArgs, format: OutputFormat) -> Result<()> {
    match executor {
        // Store first, then `--to` validation, then the id — the pre-split
        // dispatcher's precedence: a missing translate target must surface
        // before id validation on both paths.
        Executor::Local(ctx) => {
            let store = ctx.open_store()?;
            let options = ai_options_from_args(args)?;
            let runtime = build_headless_runtime(store)?;
            run_ai_streaming(
                &runtime,
                parse_id(&args.id)?,
                args.action,
                options,
                !args.no_stream,
                format,
            )
            .await
        }
        Executor::Ipc(ctx) => {
            let options = ai_options_from_args(args)?;
            // The daemon drives AI actions to completion over a one-shot
            // envelope — streaming runs in-process via the local path, so an
            // explicit `--ipc` connection always returns the final result.
            // Carry the per-request options over the wire so `--from`/`--to`
            // survive: without them the daemon would translate with default
            // options (no target language) and fail.
            let resp = ctx
                .client
                .send(IpcRequest::RunAiAction(RunAiActionRequest {
                    id: parse_id(&args.id)?,
                    action: args.action,
                    options,
                }))
                .await?;
            print_ai_output(&expect_ai_output(resp)?, format)
        }
    }
}

/// Builds the per-request [`AiRequestOptions`] from the `ai` subcommand args,
/// rejecting a `translate` with no `--to`. Shared by the local in-process path
/// and the `--ipc` path so both validate identically and the daemon receives
/// the same options the local driver would use.
fn ai_options_from_args(args: &AiArgs) -> Result<AiRequestOptions> {
    if matches!(args.action, AiActionId::Translate) && args.to.is_none() {
        anyhow::bail!("`nagori ai translate` requires --to <language> (e.g. --to ja)");
    }
    Ok(AiRequestOptions {
        source_language: args.from.clone(),
        target_language: args.to.clone(),
        ..AiRequestOptions::default()
    })
}

/// Drives a model-backed AI action in-process and renders its event stream.
///
/// Cancellation is wired two ways: `Ctrl-C` cancels the in-flight request (and
/// the process exits 130 once the stream drains), and dropping the stream — for
/// example on a broken stdout pipe — cancels it through the runtime's request
/// registry guard.
async fn run_ai_streaming(
    runtime: &NagoriRuntime,
    id: EntryId,
    action: AiActionId,
    options: AiRequestOptions,
    stream: bool,
    format: OutputFormat,
) -> Result<()> {
    use std::io::Write;

    let run = runtime.start_ai_action(id, action, options).await?;
    let request_id = run.request_id;
    let mut events = run.events;

    let mut interrupted = false;
    let mut warnings: Vec<String> = Vec::new();
    let mut final_text = String::new();
    let mut buffer = String::new();
    let stdout = std::io::stdout();

    loop {
        let item = tokio::select! {
            biased;
            // Ctrl-C cancels the request through the registry; keep draining so
            // the stream reaches its terminal `Cancelled`.
            res = tokio::signal::ctrl_c(), if !interrupted => {
                res.ok();
                interrupted = true;
                let _ = runtime.cancel_ai_action(request_id);
                continue;
            }
            item = events.next() => item,
        };
        let Some(item) = item else { break };
        let event = item.map_err(|err| anyhow!("{:?}: {}", err.code, err.message))?;

        if stream && format.is_json() {
            let mut handle = stdout.lock();
            writeln!(handle, "{}", serde_json::to_string(&event)?)?;
        }
        match event {
            AiEvent::Delta { text, .. } => {
                buffer.push_str(&text);
                if stream && !format.is_json() {
                    let mut handle = stdout.lock();
                    write!(handle, "{text}")?;
                    handle.flush()?;
                }
            }
            AiEvent::Replace { text, .. } => {
                if stream && !format.is_json() {
                    let mut handle = stdout.lock();
                    writeln!(handle)?;
                    write!(handle, "{text}")?;
                    handle.flush()?;
                }
                buffer = text;
            }
            AiEvent::Done {
                final_text: text,
                warnings: done_warnings,
                ..
            } => {
                final_text = text;
                warnings = done_warnings;
                break;
            }
            AiEvent::Cancelled => break,
        }
    }

    if !stream {
        // Non-streaming: the loop wrote nothing to stdout, so emit the
        // authoritative result once — text *and* JSON/JSONL alike. (A prior
        // version gated this on `!format.is_json()`, which left
        // `--no-stream --json/--jsonl` printing nothing and exiting 0.)
        let output = AiOutputDto {
            text: if final_text.is_empty() {
                buffer.clone()
            } else {
                final_text.clone()
            },
            created_entry: None,
            warnings: warnings.clone(),
        };
        print_ai_output(&output, format)?;
    } else if !format.is_json() && (!final_text.is_empty() || !buffer.is_empty()) {
        // Streaming text: terminate the streamed line. Streaming JSON Lines
        // needs nothing more here — each event was already emitted as it
        // arrived in the loop above.
        let mut handle = stdout.lock();
        writeln!(handle)?;
    }

    for warning in &warnings {
        eprintln!("warning: {warning}");
    }

    if interrupted {
        // SIGINT: mirror shells' 128 + signal-number convention.
        std::process::exit(130);
    }
    Ok(())
}

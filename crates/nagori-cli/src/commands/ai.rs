use anyhow::Result;
use futures::StreamExt;
use nagori_core::{AiActionId, AiEvent, AiRequestOptions, AppError, EntryId};
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
            let store = ctx.open_store().await?;
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
    let mut streamed = StreamedText::default();
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
        // Map the structured stream error through `AppError` (the same
        // conversion the daemon's IPC path uses) so the CLI derives a
        // classified exit code instead of stringifying the error and falling
        // through to the internal-error bucket.
        let event = item.map_err(|err| anyhow::Error::from(AppError::from(&err)))?;

        if stream && format.is_json() {
            let mut handle = stdout.lock();
            writeln!(handle, "{}", serde_json::to_string(&event)?)?;
        }
        if stream && !format.is_json() {
            let mut handle = stdout.lock();
            streamed.write_event(&mut handle, &event)?;
            handle.flush()?;
        }
        match event {
            AiEvent::Delta { text, .. } => buffer.push_str(&text),
            AiEvent::Replace { text, .. } => buffer = text,
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
    } else if !format.is_json() && !streamed.is_empty() {
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

/// The text a streaming run has written to stdout so far.
///
/// A backend does not always stream: translation and any action run with
/// `ai.allow_streaming = false` deliver their whole result in the terminal
/// `Done`, with no `Delta` / `Replace` before it. `Done.final_text` is also the
/// authoritative result even when deltas were streamed, so on `Done` the
/// output is reconciled with it — the unseen tail is appended, or the result is
/// rewritten on a fresh line when it diverges — instead of printing only what
/// the deltas carried (an empty line for a done-only run).
#[derive(Debug, Default)]
struct StreamedText {
    shown: String,
}

impl StreamedText {
    fn write_event(
        &mut self,
        out: &mut impl std::io::Write,
        event: &AiEvent,
    ) -> std::io::Result<()> {
        match event {
            AiEvent::Delta { text, .. } => {
                write!(out, "{text}")?;
                self.shown.push_str(text);
            }
            AiEvent::Replace { text, .. } => self.replace(out, text)?,
            // An empty final text carries nothing beyond what was streamed
            // (the non-streaming path falls back to the deltas the same way).
            AiEvent::Done { final_text, .. } if final_text.is_empty() => {}
            AiEvent::Done { final_text, .. } => {
                if let Some(tail) = final_text.strip_prefix(self.shown.as_str()) {
                    write!(out, "{tail}")?;
                    self.shown.push_str(tail);
                } else {
                    self.replace(out, final_text)?;
                }
            }
            AiEvent::Cancelled => {}
        }
        Ok(())
    }

    /// Start the full `text` on a fresh line: the streamed prefix no longer
    /// matches it and cannot be taken back.
    fn replace(&mut self, out: &mut impl std::io::Write, text: &str) -> std::io::Result<()> {
        if !self.shown.is_empty() {
            writeln!(out)?;
        }
        write!(out, "{text}")?;
        text.clone_into(&mut self.shown);
        Ok(())
    }

    /// Whether anything has been written, so the caller knows to end the line.
    const fn is_empty(&self) -> bool {
        self.shown.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(events: &[AiEvent]) -> String {
        let mut streamed = StreamedText::default();
        let mut out = Vec::new();
        for event in events {
            streamed.write_event(&mut out, event).expect("write");
        }
        String::from_utf8(out).expect("utf-8")
    }

    fn delta(text: &str) -> AiEvent {
        AiEvent::Delta {
            seq: 0,
            text: text.to_owned(),
        }
    }

    fn done(text: &str) -> AiEvent {
        AiEvent::Done {
            final_text: text.to_owned(),
            created_entry: None,
            warnings: Vec::new(),
        }
    }

    #[test]
    fn a_done_only_result_is_printed() {
        // Translation (and any action with streaming disabled) delivers its
        // whole result in `Done`; it must reach stdout, not just a newline.
        assert_eq!(render(&[done("[ja] hello")]), "[ja] hello");
    }

    #[test]
    fn a_fully_streamed_result_is_not_printed_twice() {
        assert_eq!(
            render(&[delta("Hello "), delta("world"), done("Hello world")]),
            "Hello world"
        );
    }

    #[test]
    fn the_unstreamed_tail_of_the_final_text_is_appended() {
        assert_eq!(render(&[delta("Hel"), done("Hello")]), "Hello");
    }

    #[test]
    fn a_diverging_final_text_is_rewritten_on_a_new_line() {
        assert_eq!(render(&[delta("draft"), done("final")]), "draft\nfinal");
    }

    #[test]
    fn an_empty_final_text_keeps_the_streamed_output() {
        assert_eq!(render(&[delta("streamed"), done("")]), "streamed");
    }

    #[test]
    fn a_replace_before_any_output_starts_without_a_blank_line() {
        let replace = AiEvent::Replace {
            seq: 0,
            text: "whole".to_owned(),
        };
        assert_eq!(render(&[replace, done("whole")]), "whole");
    }

    #[test]
    fn an_empty_result_prints_nothing() {
        let mut streamed = StreamedText::default();
        let mut out = Vec::new();
        streamed.write_event(&mut out, &done("")).expect("write");
        assert!(out.is_empty());
        assert!(streamed.is_empty());
    }
}

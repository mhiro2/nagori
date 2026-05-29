//! A deterministic [`TextGenerator`] fixture for tests and CI.
//!
//! `MockBackend` streams a canned response one character at a time, honouring
//! cancellation between pulls, and can be constructed in any availability
//! state. It lets the daemon, CLI, and desktop exercise the full engine path —
//! resolver, streaming, cancellation, and every availability branch — on hosts
//! with no Apple Intelligence environment.

use async_trait::async_trait;
use futures::StreamExt;
use nagori_core::AiEvent;
use tokio_util::sync::CancellationToken;

use crate::AiEventStream;
use crate::backend::{
    BackendAvailability, BackendUnavailableReason, TextGenerationCapabilities,
    TextGenerationRequest, TextGenerator,
};

/// A canned text-generation backend.
#[derive(Debug, Clone)]
pub struct MockBackend {
    availability: BackendAvailability,
    /// Fixed output, or `None` to derive a recognisable string from the input.
    output: Option<String>,
}

impl Default for MockBackend {
    fn default() -> Self {
        Self {
            availability: BackendAvailability::Available,
            output: None,
        }
    }
}

impl MockBackend {
    /// An available backend that derives its output from the request input.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// An available backend that always streams `output`.
    #[must_use]
    pub fn with_output(output: impl Into<String>) -> Self {
        Self {
            availability: BackendAvailability::Available,
            output: Some(output.into()),
        }
    }

    /// A backend that reports the given unavailable `reason`.
    #[must_use]
    pub const fn unavailable(reason: BackendUnavailableReason) -> Self {
        Self {
            availability: BackendAvailability::Unavailable(reason),
            output: None,
        }
    }

    fn render(&self, input: &str) -> String {
        self.output.clone().unwrap_or_else(|| {
            let first = input.trim().lines().next().unwrap_or_default().trim();
            format!("Summary: {first}")
        })
    }
}

#[async_trait]
impl TextGenerator for MockBackend {
    fn capabilities(&self) -> TextGenerationCapabilities {
        TextGenerationCapabilities {
            streaming: true,
            guided_generation: false,
            on_device: true,
        }
    }

    async fn availability(&self) -> BackendAvailability {
        self.availability
    }

    async fn stream_text(
        &self,
        req: TextGenerationRequest,
        cancel: CancellationToken,
    ) -> Result<AiEventStream, nagori_core::AiError> {
        if let BackendAvailability::Unavailable(reason) = self.availability {
            return Err(reason.into_error());
        }
        Ok(stream_chars(&self.render(&req.input), cancel))
    }
}

/// Internal unfold state for the per-character mock stream.
struct MockState {
    chars: Vec<char>,
    idx: usize,
    seq: u64,
    buf: String,
    cancel: CancellationToken,
    finished: bool,
}

/// Streams `output` one character at a time as `Delta` events, then a terminal
/// `Done` — or `Cancelled` if the token is tripped between pulls.
fn stream_chars(output: &str, cancel: CancellationToken) -> AiEventStream {
    let state = MockState {
        chars: output.chars().collect(),
        idx: 0,
        seq: 0,
        buf: String::new(),
        cancel,
        finished: false,
    };
    futures::stream::unfold(state, |mut st| async move {
        if st.finished {
            return None;
        }
        if st.cancel.is_cancelled() {
            st.finished = true;
            return Some((Ok(AiEvent::Cancelled), st));
        }
        if st.idx >= st.chars.len() {
            st.finished = true;
            let done = AiEvent::Done {
                final_text: std::mem::take(&mut st.buf),
                created_entry: None,
                warnings: Vec::new(),
            };
            return Some((Ok(done), st));
        }
        let ch = st.chars[st.idx];
        st.idx += 1;
        let seq = st.seq;
        st.seq += 1;
        st.buf.push(ch);
        Some((
            Ok(AiEvent::Delta {
                seq,
                text: ch.to_string(),
            }),
            st,
        ))
    })
    .boxed()
}

#[cfg(test)]
mod tests {
    use super::*;
    use nagori_core::{AiActionId, AiRequestOptions, RequestId};

    fn request(input: &str) -> TextGenerationRequest {
        TextGenerationRequest {
            request_id: RequestId::new(),
            action: AiActionId::Summarize,
            input: input.to_owned(),
            options: AiRequestOptions::default(),
            guided_schema: None,
        }
    }

    #[tokio::test]
    async fn streams_deltas_then_done() {
        let backend = MockBackend::with_output("ab世");
        let cancel = CancellationToken::new();
        let mut stream = backend
            .stream_text(request("ignored"), cancel)
            .await
            .unwrap();

        let mut buf = String::new();
        let mut terminal = None;
        while let Some(item) = stream.next().await {
            match item.unwrap() {
                AiEvent::Delta { text, .. } => buf.push_str(&text),
                AiEvent::Replace { text, .. } => buf = text,
                done @ AiEvent::Done { .. } => {
                    terminal = Some(done);
                    break;
                }
                AiEvent::Cancelled => panic!("unexpected cancel"),
            }
        }
        assert_eq!(buf, "ab世");
        assert_eq!(
            terminal,
            Some(AiEvent::Done {
                final_text: "ab世".to_owned(),
                created_entry: None,
                warnings: Vec::new(),
            })
        );
    }

    #[tokio::test]
    async fn cancellation_before_consume_yields_cancelled() {
        let backend = MockBackend::with_output("x".repeat(100));
        let cancel = CancellationToken::new();
        cancel.cancel();
        let mut stream = backend
            .stream_text(request("ignored"), cancel)
            .await
            .unwrap();
        let first = stream.next().await.unwrap().unwrap();
        assert_eq!(first, AiEvent::Cancelled);
    }

    #[tokio::test]
    async fn unavailable_backend_errors_synchronously() {
        let backend = MockBackend::unavailable(BackendUnavailableReason::NotEnabled);
        let cancel = CancellationToken::new();
        // `AiEventStream` is not `Debug`, so use let-else rather than `expect_err`.
        let Err(err) = backend.stream_text(request("ignored"), cancel).await else {
            panic!("unavailable backend must error");
        };
        assert_eq!(err.code, nagori_core::AiErrorCode::Unavailable);
    }
}

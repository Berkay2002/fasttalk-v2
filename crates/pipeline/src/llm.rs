use crate::{PipelineError, validate_loopback_endpoint};
use fasttalk_conversation::ClauseChunker;
use futures_util::StreamExt;
use serde::Serialize;
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug, Serialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum LlmEvent {
    Delta(String),
    Clause(String),
    Completed(String),
}

pub struct LlmClient {
    client: reqwest::Client,
    endpoint: String,
}

impl LlmClient {
    pub fn new(base_url: &str) -> Result<Self, PipelineError> {
        validate_loopback_endpoint(base_url, "http")?;
        Ok(Self {
            client: reqwest::Client::builder()
                .no_proxy()
                .build()
                .map_err(PipelineError::Http)?,
            endpoint: format!("{}/v1/chat/completions", base_url.trim_end_matches('/')),
        })
    }

    pub async fn stream_reply(
        &self,
        messages: Vec<ChatMessage>,
        cancellation: CancellationToken,
        events: mpsc::Sender<LlmEvent>,
    ) -> Result<String, PipelineError> {
        let response = self
            .client
            .post(&self.endpoint)
            .json(&serde_json::json!({
                "model": "fasttalk-local",
                "messages": messages,
                "max_tokens": 512,
                "temperature": 0.6,
                "stream": true
            }))
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            return Err(PipelineError::Worker {
                status: status.as_u16(),
                message: response.text().await.unwrap_or_default(),
            });
        }

        let mut stream = response.bytes_stream();
        let mut decoder = SseDecoder::default();
        let mut answer = String::new();
        let mut chunker = ClauseChunker::new(24, 30);
        loop {
            let chunk = tokio::select! {
                _ = cancellation.cancelled() => return Err(PipelineError::Cancelled),
                chunk = stream.next() => chunk,
            };
            let Some(chunk) = chunk else { break };
            let chunk = chunk?;
            for data in decoder.push(&chunk) {
                if data == "[DONE]" {
                    if let Some(clause) = chunker.finish() {
                        send_event(&events, LlmEvent::Clause(clause)).await?;
                    }
                    send_event(&events, LlmEvent::Completed(answer.clone())).await?;
                    return Ok(answer);
                }
                let event: Value = serde_json::from_str(&data)?;
                let delta = event
                    .pointer("/choices/0/delta/content")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if delta.is_empty() {
                    continue;
                }
                answer.push_str(delta);
                send_event(&events, LlmEvent::Delta(delta.to_owned())).await?;
                for clause in chunker.push(delta) {
                    send_event(&events, LlmEvent::Clause(clause)).await?;
                }
            }
        }
        Err(PipelineError::Protocol(
            "LLM stream ended before the [DONE] event".to_owned(),
        ))
    }
}

async fn send_event(events: &mpsc::Sender<LlmEvent>, event: LlmEvent) -> Result<(), PipelineError> {
    events
        .send(event)
        .await
        .map_err(|_| PipelineError::Cancelled)
}

#[derive(Default)]
struct SseDecoder {
    buffer: Vec<u8>,
}

impl SseDecoder {
    fn push(&mut self, bytes: &[u8]) -> Vec<String> {
        self.buffer.extend_from_slice(bytes);
        let mut events = Vec::new();
        while let Some((boundary, separator_len)) = find_event_boundary(&self.buffer) {
            let event = self.buffer.drain(..boundary).collect::<Vec<_>>();
            self.buffer.drain(..separator_len);
            let text = String::from_utf8_lossy(&event);
            let data = text
                .lines()
                .filter_map(|line| line.strip_prefix("data:"))
                .map(str::trim_start)
                .collect::<Vec<_>>()
                .join("\n");
            if !data.is_empty() {
                events.push(data);
            }
        }
        events
    }
}

fn find_event_boundary(buffer: &[u8]) -> Option<(usize, usize)> {
    let crlf = buffer
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|position| (position, 4));
    let lf = buffer
        .windows(2)
        .position(|window| window == b"\n\n")
        .map(|position| (position, 2));
    match (crlf, lf) {
        (Some(left), Some(right)) => Some(if left.0 <= right.0 { left } else { right }),
        (Some(boundary), None) | (None, Some(boundary)) => Some(boundary),
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decoder_handles_split_sse_events_and_crlf() {
        let mut decoder = SseDecoder::default();
        assert!(decoder.push(b"data: {\"a\":1}\r\n").is_empty());
        assert_eq!(
            decoder.push(b"\r\ndata: [DONE]\n\n"),
            ["{\"a\":1}", "[DONE]"]
        );
    }

    #[test]
    fn decoder_uses_the_earliest_mixed_line_ending() {
        let mut decoder = SseDecoder::default();
        assert_eq!(
            decoder.push(b"data: first\n\ndata: second\r\n\r\n"),
            ["first", "second"]
        );
    }
}

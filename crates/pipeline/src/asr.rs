use crate::{PipelineError, validate_loopback_endpoint};
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use serde::Serialize;
use serde_json::Value;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::{Bytes, Message};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

#[derive(Clone, Debug, PartialEq)]
pub enum AsrEvent {
    SessionReady,
    Partial(String),
    Final(String),
    Committed,
    Cleared,
}

pub struct RealtimeAsrClient {
    endpoint: String,
}

impl RealtimeAsrClient {
    pub fn new(endpoint: impl Into<String>) -> Result<Self, PipelineError> {
        let endpoint = endpoint.into();
        validate_loopback_endpoint(&endpoint, "ws")?;
        Ok(Self { endpoint })
    }

    pub async fn connect(&self) -> Result<(AsrSender, AsrReceiver), PipelineError> {
        let (mut socket, _) = connect_async(&self.endpoint).await?;
        socket
            .send(Message::Text(
                serde_json::to_string(&SessionUpdate::default())?.into(),
            ))
            .await?;
        let (sink, stream) = socket.split();
        Ok((AsrSender { sink }, AsrReceiver { stream }))
    }
}

#[derive(Serialize)]
struct SessionUpdate {
    r#type: &'static str,
    session: SessionConfig,
}

impl Default for SessionUpdate {
    fn default() -> Self {
        Self {
            r#type: "session.update",
            session: SessionConfig {
                sample_rate: 16_000,
                language: "en-US",
                automatic_punctuation: true,
                word_timestamps: false,
                speaker_diarization: false,
            },
        }
    }
}

#[derive(Serialize)]
struct SessionConfig {
    sample_rate: u32,
    language: &'static str,
    automatic_punctuation: bool,
    word_timestamps: bool,
    speaker_diarization: bool,
}

pub struct AsrSender {
    sink: SplitSink<Socket, Message>,
}

impl AsrSender {
    pub async fn send_f32(&mut self, samples: &[f32]) -> Result<(), PipelineError> {
        let mut pcm = Vec::with_capacity(samples.len() * 2);
        for sample in samples {
            let sample = (sample.clamp(-1.0, 1.0) * i16::MAX as f32).round() as i16;
            pcm.extend_from_slice(&sample.to_le_bytes());
        }
        self.sink
            .send(Message::Binary(Bytes::from(pcm)))
            .await
            .map_err(Into::into)
    }

    pub async fn commit(&mut self) -> Result<(), PipelineError> {
        self.send_control("input_audio_buffer.commit").await
    }

    pub async fn clear(&mut self) -> Result<(), PipelineError> {
        self.send_control("input_audio_buffer.clear").await
    }

    pub async fn close(mut self) -> Result<(), PipelineError> {
        self.sink.close().await.map_err(Into::into)
    }

    async fn send_control(&mut self, event_type: &str) -> Result<(), PipelineError> {
        let message = serde_json::json!({ "type": event_type }).to_string();
        self.sink
            .send(Message::Text(message.into()))
            .await
            .map_err(Into::into)
    }
}

pub struct AsrReceiver {
    stream: SplitStream<Socket>,
}

impl AsrReceiver {
    pub async fn next_event(&mut self) -> Option<Result<AsrEvent, PipelineError>> {
        loop {
            let message = self.stream.next().await?;
            match message {
                Ok(Message::Text(text)) => match parse_server_event(&text) {
                    Ok(Some(event)) => return Some(Ok(event)),
                    Ok(None) => continue,
                    Err(error) => return Some(Err(error)),
                },
                Ok(Message::Close(_)) => return None,
                Ok(_) => continue,
                Err(error) => return Some(Err(error.into())),
            }
        }
    }
}

fn parse_server_event(text: &str) -> Result<Option<AsrEvent>, PipelineError> {
    let event: Value = serde_json::from_str(text)?;
    let event_type = event
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| PipelineError::Protocol("ASR event is missing type".to_owned()))?;
    let parsed = match event_type {
        "session.created" | "session.updated" => Some(AsrEvent::SessionReady),
        "conversation.item.input_audio_transcription.delta" => Some(AsrEvent::Partial(
            event
                .get("delta")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        )),
        "conversation.item.input_audio_transcription.completed" => Some(AsrEvent::Final(
            event
                .get("transcript")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        )),
        "input_audio_buffer.committed" => Some(AsrEvent::Committed),
        "input_audio_buffer.cleared" => Some(AsrEvent::Cleared),
        "error" => {
            return Err(PipelineError::Protocol(
                event
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .unwrap_or("ASR worker returned an unknown error")
                    .to_owned(),
            ));
        }
        _ => None,
    };
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_delta_and_final_events() {
        assert_eq!(
            parse_server_event(
                r#"{"type":"conversation.item.input_audio_transcription.delta","delta":"hello"}"#
            )
            .unwrap(),
            Some(AsrEvent::Partial("hello".to_owned()))
        );
        assert_eq!(
            parse_server_event(r#"{"type":"conversation.item.input_audio_transcription.completed","transcript":"hello world"}"#).unwrap(),
            Some(AsrEvent::Final("hello world".to_owned()))
        );
    }

    #[test]
    fn surfaces_worker_errors_without_event_payloads() {
        let error =
            parse_server_event(r#"{"type":"error","error":{"message":"bad audio"}}"#).unwrap_err();
        assert_eq!(error.to_string(), "bad audio");
    }
}

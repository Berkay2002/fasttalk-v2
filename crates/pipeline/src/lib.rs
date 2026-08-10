mod asr;
mod llm;
mod tts;

pub use asr::{AsrEvent, AsrReceiver, AsrSender, RealtimeAsrClient};
pub use llm::{ChatMessage, LlmClient, LlmEvent};
pub use tokio_util::sync::CancellationToken;
pub use tts::{KokoroClient, MagpieClient, TtsEvent};

use std::net::SocketAddr;

fn validate_loopback_endpoint(endpoint: &str, expected_scheme: &str) -> Result<(), PipelineError> {
    let url = reqwest::Url::parse(endpoint)
        .map_err(|error| PipelineError::Protocol(format!("invalid endpoint: {error}")))?;
    if url.scheme() != expected_scheme {
        return Err(PipelineError::Protocol(format!(
            "endpoint must use {expected_scheme}"
        )));
    }
    if !url.username().is_empty() || url.password().is_some() || url.query().is_some() {
        return Err(PipelineError::Protocol(
            "endpoint must not contain credentials or a query string".to_owned(),
        ));
    }
    let host = url
        .host_str()
        .ok_or_else(|| PipelineError::Protocol("endpoint is missing a host".to_owned()))?;
    let port = url
        .port_or_known_default()
        .ok_or_else(|| PipelineError::Protocol("endpoint is missing a port".to_owned()))?;
    let address: SocketAddr = format!("{host}:{port}").parse().map_err(|_| {
        PipelineError::Protocol("endpoint must use a numeric IP address".to_owned())
    })?;
    if !address.ip().is_loopback() {
        return Err(PipelineError::Protocol(
            "native inference endpoint must remain on loopback".to_owned(),
        ));
    }
    Ok(())
}

#[derive(Debug)]
pub enum PipelineError {
    Http(reqwest::Error),
    WebSocket(tokio_tungstenite::tungstenite::Error),
    Json(serde_json::Error),
    Protocol(String),
    Worker { status: u16, message: String },
    Cancelled,
}

impl std::fmt::Display for PipelineError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Http(error) => error.fmt(formatter),
            Self::WebSocket(error) => error.fmt(formatter),
            Self::Json(error) => error.fmt(formatter),
            Self::Protocol(message) => formatter.write_str(message),
            Self::Worker { status, message } => {
                write!(formatter, "native worker returned HTTP {status}: {message}")
            }
            Self::Cancelled => formatter.write_str("pipeline operation cancelled"),
        }
    }
}

impl std::error::Error for PipelineError {}

impl From<reqwest::Error> for PipelineError {
    fn from(error: reqwest::Error) -> Self {
        Self::Http(error)
    }
}

impl From<tokio_tungstenite::tungstenite::Error> for PipelineError {
    fn from(error: tokio_tungstenite::tungstenite::Error) -> Self {
        Self::WebSocket(error)
    }
}

impl From<serde_json::Error> for PipelineError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoints_must_be_numeric_loopback_without_credentials() {
        assert!(validate_loopback_endpoint("http://127.0.0.1:18080", "http").is_ok());
        assert!(validate_loopback_endpoint("http://0.0.0.0:18080", "http").is_err());
        assert!(validate_loopback_endpoint("http://localhost:18080", "http").is_err());
        assert!(validate_loopback_endpoint("http://user@127.0.0.1:18080", "http").is_err());
    }
}

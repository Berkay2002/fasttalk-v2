use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::fmt;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ConversationState {
    #[default]
    Idle,
    Listening,
    Thinking,
    Speaking,
    Interrupted,
    Faulted,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", content = "payload", rename_all = "camelCase")]
pub enum ConversationEvent {
    StartListening,
    PartialTranscript { text: String },
    EndOfSpeech { transcript: String },
    FirstToken,
    ClauseReady { text: String },
    PlaybackDrained,
    Interrupt,
    Fail { message: String },
    Reset,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineSnapshot {
    pub state: ConversationState,
    pub turn_id: u64,
    pub revision: u64,
    pub cancellation_epoch: u64,
    pub partial_transcript: String,
    pub committed_transcript: String,
    pub pending_clause: Option<String>,
    pub last_error: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InvalidTransition {
    pub state: ConversationState,
    pub event: ConversationEvent,
}

impl fmt::Display for InvalidTransition {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "event {:?} is invalid while the engine is {:?}",
            self.event, self.state
        )
    }
}

impl std::error::Error for InvalidTransition {}

#[derive(Clone, Debug, Default)]
pub struct ConversationEngine {
    snapshot: EngineSnapshot,
}

impl Default for EngineSnapshot {
    fn default() -> Self {
        Self {
            state: ConversationState::Idle,
            turn_id: 0,
            revision: 0,
            cancellation_epoch: 0,
            partial_transcript: String::new(),
            committed_transcript: String::new(),
            pending_clause: None,
            last_error: None,
        }
    }
}

impl ConversationEngine {
    #[must_use]
    pub fn snapshot(&self) -> EngineSnapshot {
        self.snapshot.clone()
    }

    #[must_use]
    pub fn cancellation_token(&self) -> CancellationToken {
        CancellationToken {
            epoch: self.snapshot.cancellation_epoch,
        }
    }

    #[must_use]
    pub fn accepts(&self, token: CancellationToken) -> bool {
        token.epoch == self.snapshot.cancellation_epoch
    }

    pub fn apply(&mut self, event: ConversationEvent) -> Result<EngineSnapshot, InvalidTransition> {
        use ConversationEvent as Event;
        use ConversationState as State;

        let state = self.snapshot.state;
        match (&state, &event) {
            (State::Idle | State::Interrupted, Event::StartListening) => {
                self.snapshot.turn_id += 1;
                self.snapshot.state = State::Listening;
                self.snapshot.partial_transcript.clear();
                self.snapshot.committed_transcript.clear();
                self.snapshot.pending_clause = None;
                self.snapshot.last_error = None;
            }
            (State::Listening, Event::PartialTranscript { text }) => {
                self.snapshot.partial_transcript.clone_from(text);
            }
            (State::Listening, Event::EndOfSpeech { transcript }) => {
                self.snapshot.state = State::Thinking;
                self.snapshot.partial_transcript.clear();
                self.snapshot.committed_transcript.clone_from(transcript);
            }
            (State::Thinking, Event::FirstToken) => {}
            (State::Thinking | State::Speaking, Event::ClauseReady { text }) => {
                self.snapshot.state = State::Speaking;
                self.snapshot.pending_clause = Some(text.clone());
            }
            (State::Speaking, Event::PlaybackDrained) => {
                self.snapshot.turn_id += 1;
                self.snapshot.state = State::Listening;
                self.snapshot.pending_clause = None;
                self.snapshot.committed_transcript.clear();
            }
            (State::Listening | State::Thinking | State::Speaking, Event::Interrupt) => {
                self.snapshot.state = State::Interrupted;
                self.snapshot.cancellation_epoch += 1;
                self.snapshot.pending_clause = None;
            }
            (_, Event::Fail { message }) => {
                self.snapshot.state = State::Faulted;
                self.snapshot.cancellation_epoch += 1;
                self.snapshot.last_error = Some(message.clone());
                self.snapshot.pending_clause = None;
            }
            (_, Event::Reset) => {
                let revision = self.snapshot.revision;
                let cancellation_epoch = self.snapshot.cancellation_epoch + 1;
                self.snapshot = EngineSnapshot {
                    revision,
                    cancellation_epoch,
                    ..EngineSnapshot::default()
                };
            }
            _ => {
                return Err(InvalidTransition { state, event });
            }
        }

        self.snapshot.revision += 1;
        Ok(self.snapshot())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CancellationToken {
    epoch: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EndpointPolicy {
    pub llm: SocketAddr,
    pub speech: SocketAddr,
}

impl Default for EndpointPolicy {
    fn default() -> Self {
        let loopback = IpAddr::V4(Ipv4Addr::LOCALHOST);
        Self {
            llm: SocketAddr::new(loopback, 18080),
            speech: SocketAddr::new(loopback, 18081),
        }
    }
}

impl EndpointPolicy {
    pub fn validate(self) -> Result<Self, &'static str> {
        if !self.llm.ip().is_loopback() || !self.speech.ip().is_loopback() {
            return Err("native worker endpoints must remain on loopback");
        }
        if self.llm.port() == self.speech.port() {
            return Err("native workers must use distinct ports");
        }
        Ok(self)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Message {
    pub role: MessageRole,
    pub text: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MessageRole {
    User,
    Assistant,
}

#[derive(Clone, Debug)]
pub struct SessionHistory {
    capacity: usize,
    messages: VecDeque<Message>,
}

impl SessionHistory {
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            messages: VecDeque::with_capacity(capacity),
        }
    }

    pub fn push(&mut self, message: Message) {
        if self.capacity == 0 {
            return;
        }
        if self.messages.len() == self.capacity {
            self.messages.pop_front();
        }
        self.messages.push_back(message);
    }

    pub fn iter(&self) -> impl Iterator<Item = &Message> {
        self.messages.iter()
    }
}

#[derive(Clone, Debug)]
pub struct ClauseChunker {
    buffer: String,
    minimum_chars: usize,
    maximum_words: usize,
}

impl ClauseChunker {
    #[must_use]
    pub fn new(minimum_chars: usize, maximum_words: usize) -> Self {
        Self {
            buffer: String::new(),
            minimum_chars,
            maximum_words,
        }
    }

    pub fn push(&mut self, text: &str) -> Vec<String> {
        self.buffer.push_str(text);
        let mut clauses = Vec::new();

        loop {
            let boundary = self
                .buffer
                .char_indices()
                .find(|(index, character)| {
                    index + character.len_utf8() >= self.minimum_chars
                        && matches!(character, '.' | '!' | '?' | ';' | ':')
                })
                .map(|(index, character)| index + character.len_utf8())
                .or_else(|| {
                    let words = self.buffer.match_indices(char::is_whitespace).count() + 1;
                    (words >= self.maximum_words)
                        .then(|| nth_word_boundary(&self.buffer, self.maximum_words))
                        .flatten()
                });

            let Some(boundary) = boundary else { break };
            let remainder = self.buffer.split_off(boundary);
            let clause = self.buffer.trim().to_owned();
            self.buffer = remainder.trim_start().to_owned();
            if !clause.is_empty() {
                clauses.push(clause);
            }
        }

        clauses
    }

    pub fn finish(&mut self) -> Option<String> {
        let remainder = std::mem::take(&mut self.buffer).trim().to_owned();
        (!remainder.is_empty()).then_some(remainder)
    }
}

fn nth_word_boundary(text: &str, maximum_words: usize) -> Option<usize> {
    text.char_indices()
        .filter(|(_, character)| character.is_whitespace())
        .nth(maximum_words.saturating_sub(1))
        .map(|(index, _)| index)
}

pub trait AsrProvider {
    type Error;

    fn start(&mut self, token: CancellationToken) -> Result<(), Self::Error>;
    fn push_pcm16(&mut self, samples: &[i16], token: CancellationToken) -> Result<(), Self::Error>;
    fn finish(&mut self, token: CancellationToken) -> Result<String, Self::Error>;
}

pub trait LlmProvider {
    type Error;

    fn stream_reply(
        &mut self,
        history: &SessionHistory,
        token: CancellationToken,
        on_text: &mut dyn FnMut(&str),
    ) -> Result<(), Self::Error>;
}

pub trait TtsProvider {
    type Error;

    fn synthesize(
        &mut self,
        clause: &str,
        token: CancellationToken,
        on_pcm16: &mut dyn FnMut(&[i16]),
    ) -> Result<(), Self::Error>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn happy_path_reenters_listening_after_playback() {
        let mut engine = ConversationEngine::default();
        engine.apply(ConversationEvent::StartListening).unwrap();
        engine
            .apply(ConversationEvent::EndOfSpeech {
                transcript: "Hello".to_owned(),
            })
            .unwrap();
        engine
            .apply(ConversationEvent::ClauseReady {
                text: "Hi there.".to_owned(),
            })
            .unwrap();
        let snapshot = engine.apply(ConversationEvent::PlaybackDrained).unwrap();
        assert_eq!(snapshot.state, ConversationState::Listening);
        assert_eq!(snapshot.turn_id, 2);
    }

    #[test]
    fn interrupt_invalidates_inflight_work() {
        let mut engine = ConversationEngine::default();
        engine.apply(ConversationEvent::StartListening).unwrap();
        let token = engine.cancellation_token();
        engine.apply(ConversationEvent::Interrupt).unwrap();
        assert!(!engine.accepts(token));
    }

    #[test]
    fn invalid_transition_preserves_snapshot() {
        let mut engine = ConversationEngine::default();
        let before = engine.snapshot();
        assert!(engine.apply(ConversationEvent::FirstToken).is_err());
        assert_eq!(engine.snapshot(), before);
    }

    #[test]
    fn endpoint_policy_rejects_non_loopback_workers() {
        let policy = EndpointPolicy {
            llm: "0.0.0.0:18080".parse().unwrap(),
            ..EndpointPolicy::default()
        };
        assert!(policy.validate().is_err());
    }

    #[test]
    fn clause_chunker_emits_complete_sentences() {
        let mut chunker = ClauseChunker::new(8, 20);
        assert!(chunker.push("Hello there").is_empty());
        assert_eq!(
            chunker.push(". How are you?"),
            vec!["Hello there.", "How are you?"]
        );
    }

    #[test]
    fn clause_chunker_flushes_remainder() {
        let mut chunker = ClauseChunker::new(8, 20);
        chunker.push("short reply");
        assert_eq!(chunker.finish().as_deref(), Some("short reply"));
        assert_eq!(chunker.finish(), None);
    }

    #[test]
    fn session_history_is_bounded() {
        let mut history = SessionHistory::new(2);
        for text in ["one", "two", "three"] {
            history.push(Message {
                role: MessageRole::User,
                text: text.to_owned(),
            });
        }
        let messages = history
            .iter()
            .map(|message| message.text.as_str())
            .collect::<Vec<_>>();
        assert_eq!(messages, ["two", "three"]);
    }
}

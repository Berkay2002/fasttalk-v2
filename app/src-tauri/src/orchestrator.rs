use crate::native::{
    KOKORO_BASE_URL, LLM_BASE_URL, PreferredTtsBackend, SPEECH_BASE_URL, SPEECH_REALTIME_URL,
};
use crate::smart_turn::SmartTurnDetector;
use fasttalk_audio::AudioEngine;
use fasttalk_conversation::{
    ConversationEngine, ConversationEvent, ConversationState, Message, MessageRole, SessionHistory,
};
use fasttalk_pipeline::{
    AsrEvent, AsrReceiver, AsrSender, CancellationToken, ChatMessage, KokoroClient, LlmClient,
    LlmEvent, MagpieClient, PipelineError, RealtimeAsrClient, TtsEvent,
};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};
use tauri::async_runtime::JoinHandle;
use tauri::{AppHandle, Emitter};
use tokio::sync::mpsc;

const ENDPOINT_SILENCE: Duration = Duration::from_millis(300);
const MAX_ENDPOINT_SILENCE: Duration = Duration::from_millis(1_200);
const AUDIO_POLL_INTERVAL: Duration = Duration::from_millis(10);
const PLAYBACK_DRAIN_GUARD: Duration = Duration::from_millis(30);
const ASR_RECONNECT_DELAYS: [Duration; 4] = [
    Duration::ZERO,
    Duration::from_millis(100),
    Duration::from_millis(250),
    Duration::from_millis(500),
];

pub type SharedEngine = Arc<Mutex<ConversationEngine>>;
pub type SharedAudio = Arc<Mutex<Option<AudioEngine>>>;

pub struct ConversationController {
    cancellation: CancellationToken,
    control: mpsc::UnboundedSender<Control>,
    task: JoinHandle<()>,
    finished: Arc<AtomicBool>,
}

impl ConversationController {
    pub fn interrupt(&self) -> Result<(), &'static str> {
        self.control
            .send(Control::Interrupt)
            .map_err(|_| "conversation task is not running")
    }

    pub async fn stop(self) {
        self.cancellation.cancel();
        let _ = self.task.await;
    }

    pub fn is_finished(&self) -> bool {
        self.finished.load(Ordering::Acquire)
    }
}

pub fn start(
    app: AppHandle,
    engine: SharedEngine,
    audio: SharedAudio,
    tts_backend: PreferredTtsBackend,
    turn_detector: SmartTurnDetector,
) -> ConversationController {
    let cancellation = CancellationToken::new();
    let task_cancellation = cancellation.clone();
    let (control, control_receiver) = mpsc::unbounded_channel();
    let finished = Arc::new(AtomicBool::new(false));
    let task_finished = Arc::clone(&finished);
    let task = tauri::async_runtime::spawn(async move {
        let result = run(
            app.clone(),
            engine.clone(),
            audio.clone(),
            task_cancellation,
            control_receiver,
            tts_backend,
            turn_detector,
        )
        .await;
        finish_audio_session(&audio);
        if let Err(error) = result
            && !matches!(error, PipelineError::Cancelled)
        {
            log::error!("conversation pipeline failed: {error}");
            let _ = apply_event(
                &app,
                &engine,
                ConversationEvent::Fail {
                    message: error.user_message(),
                },
            );
        }
        task_finished.store(true, Ordering::Release);
    });
    ConversationController {
        cancellation,
        control,
        task,
        finished,
    }
}

async fn run(
    app: AppHandle,
    engine: SharedEngine,
    audio: SharedAudio,
    cancellation: CancellationToken,
    mut control: mpsc::UnboundedReceiver<Control>,
    tts_backend: PreferredTtsBackend,
    mut turn_detector: SmartTurnDetector,
) -> Result<(), PipelineError> {
    let asr = RealtimeAsrClient::new(SPEECH_REALTIME_URL)?;
    let (mut asr_sender, mut asr_receiver) = connect_asr(&asr, &cancellation).await?;
    let history = Arc::new(Mutex::new(SessionHistory::new(12)));
    let mut activity = VoiceActivity::default();
    let mut awaiting_commit = false;
    let mut turn: Option<(CancellationToken, JoinHandle<()>)> = None;
    let mut audio_samples = [0.0_f32; 1_600];
    let mut turn_audio = Vec::with_capacity(16_000 * 8);
    let mut interval = tokio::time::interval(AUDIO_POLL_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            _ = cancellation.cancelled() => break,
            command = control.recv() => {
                let Some(Control::Interrupt) = command else { continue };
                interrupt_turn(&app, &engine, &audio, &mut turn)?;
            }
            _ = interval.tick() => {
                reap_finished_turn(&mut turn).await;
                let (sample_count, speech_active, interruption_active) =
                    read_audio(&audio, &mut audio_samples)?;
                if sample_count > 0 && !awaiting_commit {
                    if let Err(error) = asr_sender.send_f32(&audio_samples[..sample_count]).await {
                        if !error.is_recoverable_transport() {
                            return Err(error);
                        }
                        (asr_sender, asr_receiver) = reconnect_asr(
                            &asr,
                            &cancellation,
                            &error,
                        ).await?;
                        reset_asr_state(
                            &app,
                            &engine,
                            &mut activity,
                            &mut awaiting_commit,
                            &mut turn_audio,
                        )?;
                        continue;
                    }
                    turn_audio.extend_from_slice(&audio_samples[..sample_count]);
                }

                let state = snapshot_state(&engine)?;
                let decision = activity.update(
                    speech_active,
                    interruption_active,
                    state,
                    Instant::now(),
                );
                if decision.interrupt {
                    interrupt_turn(&app, &engine, &audio, &mut turn)?;
                }
                let smart_turn_complete = decision.endpoint_check
                    && turn_detector
                        .is_complete(&turn_audio)
                        .map_err(PipelineError::Protocol)?;
                if (smart_turn_complete || decision.force_commit) && !awaiting_commit {
                    if let Err(error) = asr_sender.commit().await {
                        if !error.is_recoverable_transport() {
                            return Err(error);
                        }
                        (asr_sender, asr_receiver) = reconnect_asr(
                            &asr,
                            &cancellation,
                            &error,
                        ).await?;
                        reset_asr_state(
                            &app,
                            &engine,
                            &mut activity,
                            &mut awaiting_commit,
                            &mut turn_audio,
                        )?;
                        continue;
                    }
                    awaiting_commit = true;
                    turn_audio.clear();
                    activity.committed();
                }
            }
            event = asr_receiver.next_event() => {
                let event = match event {
                    Some(Ok(event)) => event,
                    Some(Err(error)) if error.is_recoverable_transport() => {
                        (asr_sender, asr_receiver) = reconnect_asr(
                            &asr,
                            &cancellation,
                            &error,
                        ).await?;
                        reset_asr_state(
                            &app,
                            &engine,
                            &mut activity,
                            &mut awaiting_commit,
                            &mut turn_audio,
                        )?;
                        continue;
                    }
                    Some(Err(error)) => return Err(error),
                    None => {
                        let error = PipelineError::Protocol(
                            "the speech worker closed its realtime stream".to_owned(),
                        );
                        (asr_sender, asr_receiver) = reconnect_asr(
                            &asr,
                            &cancellation,
                            &error,
                        ).await?;
                        reset_asr_state(
                            &app,
                            &engine,
                            &mut activity,
                            &mut awaiting_commit,
                            &mut turn_audio,
                        )?;
                        continue;
                    }
                };
                match event {
                    AsrEvent::SessionReady => {}
                    AsrEvent::Partial(text) => {
                        if snapshot_state(&engine)? == ConversationState::Listening {
                            apply_event(
                                &app,
                                &engine,
                                ConversationEvent::PartialTranscriptDelta { text },
                            )?;
                        }
                    }
                    AsrEvent::Final(transcript) => {
                        let transcript = transcript.trim().to_owned();
                        if !transcript.is_empty()
                            && snapshot_state(&engine)? == ConversationState::Listening
                        {
                            apply_event(
                                &app,
                                &engine,
                                ConversationEvent::EndOfSpeech {
                                    transcript: transcript.clone(),
                                },
                            )?;
                            cancel_turn(&mut turn);
                            push_history(&history, MessageRole::User, transcript)?;
                            turn = Some(spawn_turn(
                                app.clone(),
                                engine.clone(),
                                audio.clone(),
                                history.clone(),
                                tts_backend,
                            ));
                        }
                    }
                    AsrEvent::Committed | AsrEvent::Cleared => {
                        awaiting_commit = false;
                    }
                }
            }
        }
    }

    cancel_turn(&mut turn);
    if let Some((_, task)) = turn.take() {
        let _ = task.await;
    }
    let _ = asr_sender.close().await;
    Ok(())
}

async fn connect_asr(
    client: &RealtimeAsrClient,
    cancellation: &CancellationToken,
) -> Result<(AsrSender, AsrReceiver), PipelineError> {
    tokio::select! {
        _ = cancellation.cancelled() => Err(PipelineError::Cancelled),
        connection = client.connect() => connection,
    }
}

async fn reconnect_asr(
    client: &RealtimeAsrClient,
    cancellation: &CancellationToken,
    cause: &PipelineError,
) -> Result<(AsrSender, AsrReceiver), PipelineError> {
    log::warn!("speech stream interrupted; reconnecting: {cause}");
    let mut last_error = None;
    for (attempt, delay) in ASR_RECONNECT_DELAYS.into_iter().enumerate() {
        if !delay.is_zero() {
            tokio::select! {
                _ = cancellation.cancelled() => return Err(PipelineError::Cancelled),
                _ = tokio::time::sleep(delay) => {}
            }
        }
        match connect_asr(client, cancellation).await {
            Ok(connection) => {
                log::info!("speech stream restored on attempt {}", attempt + 1);
                return Ok(connection);
            }
            Err(PipelineError::Cancelled) => return Err(PipelineError::Cancelled),
            Err(error) => {
                log::warn!("speech reconnect attempt {} failed: {error}", attempt + 1);
                last_error = Some(error);
            }
        }
    }
    if let Some(error) = last_error {
        log::error!("speech reconnect attempts exhausted: {error}");
    }
    Err(PipelineError::Protocol(
        "The local speech connection was interrupted and could not be restored. Stop and start the conversation to retry."
            .to_owned(),
    ))
}

fn reset_asr_state(
    app: &AppHandle,
    engine: &SharedEngine,
    activity: &mut VoiceActivity,
    awaiting_commit: &mut bool,
    turn_audio: &mut Vec<f32>,
) -> Result<(), PipelineError> {
    *activity = VoiceActivity::default();
    *awaiting_commit = false;
    turn_audio.clear();
    if snapshot_state(engine)? == ConversationState::Listening {
        apply_event(app, engine, ConversationEvent::ClearPartialTranscript)?;
    }
    Ok(())
}

fn finish_audio_session(audio: &SharedAudio) {
    let Ok(mut guard) = audio.lock() else {
        log::error!("could not clean up the conversation audio session: audio lock is poisoned");
        return;
    };
    if let Some(audio) = guard.as_mut() {
        audio.cancel_playback();
        audio.end_asr_session();
    }
}

#[derive(Clone, Copy, Debug)]
enum Control {
    Interrupt,
}

fn interrupt_turn(
    app: &AppHandle,
    engine: &SharedEngine,
    audio: &SharedAudio,
    turn: &mut Option<(CancellationToken, JoinHandle<()>)>,
) -> Result<(), PipelineError> {
    if matches!(
        snapshot_state(engine)?,
        ConversationState::Thinking | ConversationState::Speaking | ConversationState::Listening
    ) {
        cancel_turn(turn);
        cancel_playback(audio)?;
        apply_event(app, engine, ConversationEvent::Interrupt)?;
        apply_event(app, engine, ConversationEvent::StartListening)?;
    }
    Ok(())
}

fn spawn_turn(
    app: AppHandle,
    engine: SharedEngine,
    audio: SharedAudio,
    history: Arc<Mutex<SessionHistory>>,
    tts_backend: PreferredTtsBackend,
) -> (CancellationToken, JoinHandle<()>) {
    let cancellation = CancellationToken::new();
    let task_cancellation = cancellation.clone();
    let task = tauri::async_runtime::spawn(async move {
        if let Err(error) = run_turn(
            &app,
            &engine,
            &audio,
            &history,
            task_cancellation,
            tts_backend,
        )
        .await
            && !matches!(error, PipelineError::Cancelled)
        {
            log::error!("conversation turn failed: {error}");
            let _ = apply_event(
                &app,
                &engine,
                ConversationEvent::Fail {
                    message: error.to_string(),
                },
            );
        }
    });
    (cancellation, task)
}

async fn run_turn(
    app: &AppHandle,
    engine: &SharedEngine,
    audio: &SharedAudio,
    history: &Arc<Mutex<SessionHistory>>,
    cancellation: CancellationToken,
    tts_backend: PreferredTtsBackend,
) -> Result<(), PipelineError> {
    let messages = chat_messages(history)?;
    let llm = LlmClient::new(LLM_BASE_URL)?;
    let (events, mut receiver) = mpsc::channel(64);
    let llm_cancellation = cancellation.clone();
    let llm_task = tauri::async_runtime::spawn(async move {
        llm.stream_reply(messages, llm_cancellation, events).await
    });
    let (clauses, mut clause_receiver) = mpsc::channel(8);
    let generation = async {
        let mut saw_first_token = false;
        let mut answer = None;
        while let Some(event) = cancelled_receive(&cancellation, &mut receiver).await? {
            match event {
                LlmEvent::Delta(delta) => {
                    if !saw_first_token {
                        saw_first_token = true;
                        apply_event(app, engine, ConversationEvent::FirstToken)?;
                    }
                    apply_event(
                        app,
                        engine,
                        ConversationEvent::AssistantDelta { text: delta },
                    )?;
                }
                LlmEvent::Clause(clause) => clauses
                    .send(clause)
                    .await
                    .map_err(|_| PipelineError::Cancelled)?,
                LlmEvent::Completed(completed) => {
                    answer = Some(completed);
                    break;
                }
            }
        }
        drop(clauses);
        let generated = llm_task
            .await
            .map_err(|error| PipelineError::Protocol(format!("LLM task failed: {error}")))??;
        Ok::<String, PipelineError>(answer.unwrap_or(generated))
    };
    let speech = async {
        while let Some(clause) = cancelled_receive(&cancellation, &mut clause_receiver).await? {
            apply_event(
                app,
                engine,
                ConversationEvent::ClauseReady {
                    text: clause.clone(),
                },
            )?;
            synthesize_clause(&clause, audio, cancellation.clone(), tts_backend).await?;
        }
        Ok::<(), PipelineError>(())
    };

    let result = tokio::try_join!(generation, speech);
    if result.is_err() {
        cancellation.cancel();
    }
    let (answer, ()) = result?;
    push_history(history, MessageRole::Assistant, answer)?;
    wait_for_playback_drain(audio, &cancellation).await?;
    apply_event(app, engine, ConversationEvent::PlaybackDrained)?;
    Ok(())
}

async fn cancelled_receive<T>(
    cancellation: &CancellationToken,
    receiver: &mut mpsc::Receiver<T>,
) -> Result<Option<T>, PipelineError> {
    tokio::select! {
        _ = cancellation.cancelled() => Err(PipelineError::Cancelled),
        event = receiver.recv() => Ok(event),
    }
}

async fn synthesize_clause(
    clause: &str,
    audio: &SharedAudio,
    cancellation: CancellationToken,
    backend: PreferredTtsBackend,
) -> Result<(), PipelineError> {
    if backend == PreferredTtsBackend::Kokoro {
        return synthesize_with_backend(TtsBackend::Kokoro, clause, audio, cancellation)
            .await
            .map_err(|error| error.source);
    }
    match synthesize_with_backend(TtsBackend::Magpie, clause, audio, cancellation.clone()).await {
        Ok(()) => Ok(()),
        Err(error) if !error.pcm_emitted && !matches!(&error.source, PipelineError::Cancelled) => {
            log::warn!(
                "Magpie failed before producing PCM; retrying clause on CPU Kokoro: {}",
                error.source
            );
            synthesize_with_backend(TtsBackend::Kokoro, clause, audio, cancellation)
                .await
                .map_err(|error| error.source)
        }
        Err(error) => Err(error.source),
    }
}

#[derive(Clone, Copy)]
enum TtsBackend {
    Magpie,
    Kokoro,
}

struct TtsAttemptError {
    source: PipelineError,
    pcm_emitted: bool,
}

async fn synthesize_with_backend(
    backend: TtsBackend,
    clause: &str,
    audio: &SharedAudio,
    cancellation: CancellationToken,
) -> Result<(), TtsAttemptError> {
    let (events, mut receiver) = mpsc::channel(16);
    let text = clause.to_owned();
    let tts_cancellation = cancellation.clone();
    let task = tauri::async_runtime::spawn(async move {
        match backend {
            TtsBackend::Magpie => {
                MagpieClient::new(SPEECH_BASE_URL)?
                    .synthesize(&text, tts_cancellation, events)
                    .await
            }
            TtsBackend::Kokoro => {
                KokoroClient::new(KOKORO_BASE_URL)?
                    .synthesize(&text, tts_cancellation, events)
                    .await
            }
        }
    });

    let mut pcm_emitted = false;
    while let Some(event) = cancelled_receive(&cancellation, &mut receiver)
        .await
        .map_err(|source| TtsAttemptError {
            source,
            pcm_emitted,
        })?
    {
        match event {
            TtsEvent::Pcm48KhzMono(samples) => {
                pcm_emitted = true;
                queue_with_backpressure(audio, &samples, &cancellation)
                    .await
                    .map_err(|source| TtsAttemptError {
                        source,
                        pcm_emitted,
                    })?;
            }
            TtsEvent::Completed => break,
        }
    }
    task.await
        .map_err(|error| TtsAttemptError {
            source: PipelineError::Protocol(format!("TTS task failed: {error}")),
            pcm_emitted,
        })?
        .map_err(|source| TtsAttemptError {
            source,
            pcm_emitted,
        })
}

async fn queue_with_backpressure(
    audio: &SharedAudio,
    samples: &[f32],
    cancellation: &CancellationToken,
) -> Result<(), PipelineError> {
    let mut offset = 0;
    while offset < samples.len() {
        let accepted = {
            let mut guard = audio
                .lock()
                .map_err(|_| PipelineError::Protocol("audio lock is poisoned".to_owned()))?;
            let engine = guard
                .as_mut()
                .ok_or_else(|| PipelineError::Protocol("audio is not running".to_owned()))?;
            engine.queue_playback_partial(&samples[offset..])
        };
        offset += accepted;
        if offset < samples.len() {
            tokio::select! {
                _ = cancellation.cancelled() => return Err(PipelineError::Cancelled),
                _ = tokio::time::sleep(Duration::from_millis(5)) => {}
            }
        }
    }
    Ok(())
}

async fn wait_for_playback_drain(
    audio: &SharedAudio,
    cancellation: &CancellationToken,
) -> Result<(), PipelineError> {
    let mut empty_since = None;
    loop {
        let queued = audio_status(audio)?.queued_playback_samples;
        if queued == 0 {
            let since = empty_since.get_or_insert_with(Instant::now);
            if since.elapsed() >= PLAYBACK_DRAIN_GUARD {
                return Ok(());
            }
        } else {
            empty_since = None;
        }
        tokio::select! {
            _ = cancellation.cancelled() => return Err(PipelineError::Cancelled),
            _ = tokio::time::sleep(Duration::from_millis(5)) => {}
        }
    }
}

fn read_audio(
    audio: &SharedAudio,
    output: &mut [f32],
) -> Result<(usize, bool, bool), PipelineError> {
    let mut guard = audio
        .lock()
        .map_err(|_| PipelineError::Protocol("audio lock is poisoned".to_owned()))?;
    let engine = guard
        .as_mut()
        .ok_or_else(|| PipelineError::Protocol("audio is not running".to_owned()))?;
    let count = engine.read_asr_samples(output);
    let status = engine.status();
    Ok((count, status.speech_active, status.interruption_active))
}

fn audio_status(audio: &SharedAudio) -> Result<fasttalk_audio::AudioStatus, PipelineError> {
    let guard = audio
        .lock()
        .map_err(|_| PipelineError::Protocol("audio lock is poisoned".to_owned()))?;
    guard
        .as_ref()
        .map(AudioEngine::status)
        .ok_or_else(|| PipelineError::Protocol("audio is not running".to_owned()))
}

fn cancel_playback(audio: &SharedAudio) -> Result<(), PipelineError> {
    let guard = audio
        .lock()
        .map_err(|_| PipelineError::Protocol("audio lock is poisoned".to_owned()))?;
    let engine = guard
        .as_ref()
        .ok_or_else(|| PipelineError::Protocol("audio is not running".to_owned()))?;
    engine.cancel_playback();
    Ok(())
}

fn snapshot_state(engine: &SharedEngine) -> Result<ConversationState, PipelineError> {
    engine
        .lock()
        .map(|engine| engine.snapshot().state)
        .map_err(|_| PipelineError::Protocol("conversation lock is poisoned".to_owned()))
}

fn apply_event(
    app: &AppHandle,
    engine: &SharedEngine,
    event: ConversationEvent,
) -> Result<(), PipelineError> {
    let snapshot = engine
        .lock()
        .map_err(|_| PipelineError::Protocol("conversation lock is poisoned".to_owned()))?
        .apply(event)
        .map_err(|error| PipelineError::Protocol(error.to_string()))?;
    app.emit("conversation-snapshot", snapshot)
        .map_err(|error| PipelineError::Protocol(format!("emit conversation state: {error}")))
}

fn chat_messages(history: &Arc<Mutex<SessionHistory>>) -> Result<Vec<ChatMessage>, PipelineError> {
    let history = history
        .lock()
        .map_err(|_| PipelineError::Protocol("history lock is poisoned".to_owned()))?;
    let mut messages = vec![ChatMessage {
        role: "system".to_owned(),
        content: assistant_system_prompt().to_owned(),
    }];
    messages.extend(history.iter().map(|message| {
        ChatMessage {
            role: match message.role {
                MessageRole::User => "user",
                MessageRole::Assistant => "assistant",
            }
            .to_owned(),
            content: message.text.clone(),
        }
    }));
    Ok(messages)
}

fn assistant_system_prompt() -> &'static str {
    "You are FastTalk, a concise local voice assistant for one adult user. Answer lawful factual questions directly, including sensitive historical, political, and religious topics. Do not refuse solely because a topic is disturbing or controversial, or because the user uses profanity. Do not scold the user. Do not end the conversation because of profanity, disagreement, or offensive wording. Correct false premises calmly. Refuse only requests that would meaningfully facilitate imminent violence, abuse, or serious wrongdoing, and keep any refusal brief while offering safe factual help. Respond in natural spoken English without markdown."
}

fn push_history(
    history: &Arc<Mutex<SessionHistory>>,
    role: MessageRole,
    text: String,
) -> Result<(), PipelineError> {
    history
        .lock()
        .map_err(|_| PipelineError::Protocol("history lock is poisoned".to_owned()))?
        .push(Message { role, text });
    Ok(())
}

fn cancel_turn(turn: &mut Option<(CancellationToken, JoinHandle<()>)>) {
    if let Some((cancellation, _)) = turn {
        cancellation.cancel();
    }
}

async fn reap_finished_turn(turn: &mut Option<(CancellationToken, JoinHandle<()>)>) {
    if turn
        .as_ref()
        .is_some_and(|(_, task)| task.inner().is_finished())
        && let Some((_, task)) = turn.take()
    {
        let _ = task.await;
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct VoiceDecision {
    interrupt: bool,
    endpoint_check: bool,
    force_commit: bool,
}

#[derive(Debug, Default)]
struct VoiceActivity {
    interruption_active: bool,
    speech_seen: bool,
    last_active: Option<Instant>,
    endpoint_checked: bool,
}

impl VoiceActivity {
    fn update(
        &mut self,
        speech_active: bool,
        interruption_active: bool,
        state: ConversationState,
        now: Instant,
    ) -> VoiceDecision {
        let interrupt = interruption_active
            && !self.interruption_active
            && matches!(
                state,
                ConversationState::Thinking | ConversationState::Speaking
            );
        if speech_active {
            self.speech_seen = true;
            self.last_active = Some(now);
            self.endpoint_checked = false;
        }
        self.interruption_active = interruption_active;
        let silence = self
            .last_active
            .map(|last_active| now.duration_since(last_active));
        let endpoint_check = self.speech_seen
            && !speech_active
            && !self.endpoint_checked
            && silence.is_some_and(|silence| silence >= ENDPOINT_SILENCE);
        if endpoint_check {
            self.endpoint_checked = true;
        }
        let force_commit = self.speech_seen
            && !speech_active
            && silence.is_some_and(|silence| silence >= MAX_ENDPOINT_SILENCE);
        VoiceDecision {
            interrupt,
            endpoint_check,
            force_commit,
        }
    }

    fn committed(&mut self) {
        self.interruption_active = false;
        self.speech_seen = false;
        self.last_active = None;
        self.endpoint_checked = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assistant_prompt_keeps_sensitive_factual_questions_available() {
        let prompt = assistant_system_prompt();
        assert!(prompt.contains("lawful factual questions"));
        assert!(prompt.contains("sensitive historical"));
        assert!(prompt.contains("profanity"));
        assert!(prompt.contains("Do not end the conversation"));
    }

    #[test]
    fn endpoint_requires_speech_then_sustained_silence() {
        let start = Instant::now();
        let mut activity = VoiceActivity::default();
        assert_eq!(
            activity.update(true, true, ConversationState::Listening, start),
            VoiceDecision::default()
        );
        assert!(
            !activity
                .update(
                    false,
                    false,
                    ConversationState::Listening,
                    start + Duration::from_millis(299)
                )
                .endpoint_check
        );
        assert!(
            activity
                .update(
                    false,
                    false,
                    ConversationState::Listening,
                    start + Duration::from_millis(300)
                )
                .endpoint_check
        );
    }

    #[test]
    fn incomplete_turn_waits_for_more_speech_or_the_safety_timeout() {
        let start = Instant::now();
        let mut activity = VoiceActivity::default();
        activity.update(true, true, ConversationState::Listening, start);
        assert!(
            activity
                .update(
                    false,
                    false,
                    ConversationState::Listening,
                    start + ENDPOINT_SILENCE
                )
                .endpoint_check
        );
        let waiting = activity.update(
            false,
            false,
            ConversationState::Listening,
            start + Duration::from_millis(600),
        );
        assert!(!waiting.endpoint_check);
        assert!(!waiting.force_commit);
        assert!(
            activity
                .update(
                    false,
                    false,
                    ConversationState::Listening,
                    start + MAX_ENDPOINT_SILENCE
                )
                .force_commit
        );
    }

    #[test]
    fn speech_onset_during_playback_interrupts_once() {
        let start = Instant::now();
        let mut activity = VoiceActivity::default();
        assert!(
            activity
                .update(false, true, ConversationState::Speaking, start)
                .interrupt
        );
        assert!(
            !activity
                .update(
                    true,
                    true,
                    ConversationState::Speaking,
                    start + Duration::from_millis(10)
                )
                .interrupt
        );
    }
}

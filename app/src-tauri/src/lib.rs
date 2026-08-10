mod native;
mod orchestrator;

use fasttalk_audio::{AudioConfig, AudioEngine, AudioStatus};
use fasttalk_conversation::{
    ConversationEngine, ConversationEvent, ConversationState, EngineSnapshot,
};
use fasttalk_runtime::WorkerState;
use native::{NativeRuntime, NativeRuntimeStatus};
use orchestrator::{ConversationController, SharedAudio, SharedEngine};
use serde::Serialize;
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, State};
use tauri_plugin_log::{RotationStrategy, Target, TargetKind};

struct AppState {
    engine: SharedEngine,
    audio: SharedAudio,
    runtime: Mutex<NativeRuntime>,
    conversation: Mutex<Option<ConversationController>>,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            engine: Arc::new(Mutex::new(ConversationEngine::default())),
            audio: Arc::new(Mutex::new(None)),
            runtime: Mutex::new(NativeRuntime::for_development_checkout()),
            conversation: Mutex::new(None),
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CommandError {
    code: &'static str,
    message: String,
}

fn lock_engine<'a>(
    state: &'a State<'_, AppState>,
) -> Result<std::sync::MutexGuard<'a, ConversationEngine>, CommandError> {
    state.engine.lock().map_err(|_| CommandError {
        code: "engineUnavailable",
        message: "conversation engine lock is poisoned".to_owned(),
    })
}

#[tauri::command]
fn engine_snapshot(state: State<'_, AppState>) -> Result<EngineSnapshot, CommandError> {
    Ok(lock_engine(&state)?.snapshot())
}

#[tauri::command]
fn engine_dispatch(
    event: ConversationEvent,
    state: State<'_, AppState>,
) -> Result<EngineSnapshot, CommandError> {
    let cancel_audio = matches!(
        event,
        ConversationEvent::Interrupt | ConversationEvent::Fail { .. } | ConversationEvent::Reset
    );
    let snapshot = lock_engine(&state)?
        .apply(event)
        .map_err(|error| CommandError {
            code: "invalidTransition",
            message: error.to_string(),
        })?;
    if cancel_audio && let Some(audio) = lock(&state.audio, "audioUnavailable")?.as_ref() {
        audio.cancel_playback();
    }
    Ok(snapshot)
}

#[tauri::command]
fn audio_start(state: State<'_, AppState>) -> Result<AudioStatus, CommandError> {
    let mut audio = lock(&state.audio, "audioUnavailable")?;
    if audio.is_none() {
        *audio =
            Some(
                AudioEngine::start(AudioConfig::default()).map_err(|error| CommandError {
                    code: "audioStartFailed",
                    message: error.to_string(),
                })?,
            );
        log::info!("native audio started");
    }
    Ok(audio.as_ref().expect("audio initialized above").status())
}

#[tauri::command]
fn audio_status(state: State<'_, AppState>) -> Result<Option<AudioStatus>, CommandError> {
    Ok(lock(&state.audio, "audioUnavailable")?
        .as_ref()
        .map(AudioEngine::status))
}

#[tauri::command]
fn audio_cancel(state: State<'_, AppState>) -> Result<Option<AudioStatus>, CommandError> {
    let audio = lock(&state.audio, "audioUnavailable")?;
    if let Some(audio) = audio.as_ref() {
        audio.cancel_playback();
    }
    Ok(audio.as_ref().map(AudioEngine::status))
}

#[tauri::command]
fn audio_stop(state: State<'_, AppState>) -> Result<(), CommandError> {
    if lock(&state.conversation, "conversationUnavailable")?.is_some() {
        return Err(CommandError {
            code: "conversationActive",
            message: "stop the conversation before stopping audio".to_owned(),
        });
    }
    if let Some(mut audio) = lock(&state.audio, "audioUnavailable")?.take() {
        audio.stop();
        log::info!("native audio stopped");
    }
    Ok(())
}

#[tauri::command]
fn conversation_start(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<EngineSnapshot, CommandError> {
    let mut controller = lock(&state.conversation, "conversationUnavailable")?;
    if controller.is_some() {
        return Ok(lock_engine(&state)?.snapshot());
    }
    let workers = lock(&state.runtime, "runtimeUnavailable")?
        .poll()
        .map_err(runtime_error)?;
    if !workers_ready(&workers) {
        return Err(CommandError {
            code: "workersNotReady",
            message: "native ASR, LLM, and TTS workers are not ready".to_owned(),
        });
    }
    if lock(&state.audio, "audioUnavailable")?.is_none() {
        return Err(CommandError {
            code: "audioNotReady",
            message: "native audio is not running".to_owned(),
        });
    }

    let snapshot = {
        let mut engine = lock_engine(&state)?;
        if engine.snapshot().state != ConversationState::Idle {
            engine
                .apply(ConversationEvent::Reset)
                .map_err(invalid_transition)?;
        }
        engine
            .apply(ConversationEvent::StartListening)
            .map_err(invalid_transition)?
    };
    *controller = Some(orchestrator::start(
        app,
        state.engine.clone(),
        state.audio.clone(),
    ));
    Ok(snapshot)
}

#[tauri::command]
fn conversation_interrupt(state: State<'_, AppState>) -> Result<(), CommandError> {
    let controller = lock(&state.conversation, "conversationUnavailable")?;
    controller
        .as_ref()
        .ok_or_else(|| CommandError {
            code: "conversationInactive",
            message: "conversation is not running".to_owned(),
        })?
        .interrupt()
        .map_err(|message| CommandError {
            code: "conversationUnavailable",
            message: message.to_owned(),
        })
}

#[tauri::command]
async fn conversation_stop(state: State<'_, AppState>) -> Result<EngineSnapshot, CommandError> {
    let controller = { lock(&state.conversation, "conversationUnavailable")?.take() };
    if let Some(controller) = controller {
        controller.stop().await;
    }
    if let Some(audio) = lock(&state.audio, "audioUnavailable")?.as_ref() {
        audio.cancel_playback();
    }
    lock_engine(&state)?
        .apply(ConversationEvent::Reset)
        .map_err(invalid_transition)
}

#[tauri::command]
fn runtime_start(state: State<'_, AppState>) -> Result<NativeRuntimeStatus, CommandError> {
    let status = lock(&state.runtime, "runtimeUnavailable")?
        .start()
        .map_err(runtime_error)?;
    log::info!("native workers started");
    Ok(status)
}

#[tauri::command]
fn runtime_status(state: State<'_, AppState>) -> Result<NativeRuntimeStatus, CommandError> {
    lock(&state.runtime, "runtimeUnavailable")?
        .poll()
        .map_err(runtime_error)
}

#[tauri::command]
fn runtime_stop(state: State<'_, AppState>) -> Result<NativeRuntimeStatus, CommandError> {
    let status = lock(&state.runtime, "runtimeUnavailable")?
        .stop()
        .map_err(runtime_error)?;
    log::info!("native workers stopped");
    Ok(status)
}

fn runtime_error(error: fasttalk_runtime::SupervisorError) -> CommandError {
    CommandError {
        code: "nativeRuntimeFailed",
        message: error.to_string(),
    }
}

fn invalid_transition(error: fasttalk_conversation::InvalidTransition) -> CommandError {
    CommandError {
        code: "invalidTransition",
        message: error.to_string(),
    }
}

fn workers_ready(status: &NativeRuntimeStatus) -> bool {
    status.llm.as_ref().map(|worker| &worker.state) == Some(&WorkerState::Ready)
        && status.speech.as_ref().map(|worker| &worker.state) == Some(&WorkerState::Ready)
}

fn lock<'a, T>(
    mutex: &'a Mutex<T>,
    code: &'static str,
) -> Result<std::sync::MutexGuard<'a, T>, CommandError> {
    mutex.lock().map_err(|_| CommandError {
        code,
        message: "native state lock is poisoned".to_owned(),
    })
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(
            tauri_plugin_log::Builder::new()
                .level(log::LevelFilter::Info)
                .max_file_size(1_000_000)
                .rotation_strategy(RotationStrategy::KeepSome(3))
                .targets([
                    Target::new(TargetKind::Stdout),
                    Target::new(TargetKind::LogDir { file_name: None }),
                ])
                .build(),
        )
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            engine_snapshot,
            engine_dispatch,
            audio_start,
            audio_status,
            audio_cancel,
            audio_stop,
            conversation_start,
            conversation_interrupt,
            conversation_stop,
            runtime_start,
            runtime_status,
            runtime_stop
        ])
        .run(tauri::generate_context!())
        .expect("error while running FastTalk");
}

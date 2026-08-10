mod native;

use fasttalk_audio::{AudioConfig, AudioEngine, AudioStatus};
use fasttalk_conversation::{ConversationEngine, ConversationEvent, EngineSnapshot};
use native::{NativeRuntime, NativeRuntimeStatus};
use serde::Serialize;
use std::sync::Mutex;
use tauri::State;
use tauri_plugin_log::{RotationStrategy, Target, TargetKind};

struct AppState {
    engine: Mutex<ConversationEngine>,
    audio: Mutex<Option<AudioEngine>>,
    runtime: Mutex<NativeRuntime>,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            engine: Mutex::new(ConversationEngine::default()),
            audio: Mutex::new(None),
            runtime: Mutex::new(NativeRuntime::for_development_checkout()),
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
    if let Some(mut audio) = lock(&state.audio, "audioUnavailable")?.take() {
        audio.stop();
        log::info!("native audio stopped");
    }
    Ok(())
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
            runtime_start,
            runtime_status,
            runtime_stop
        ])
        .run(tauri::generate_context!())
        .expect("error while running FastTalk");
}

pub mod native;
mod orchestrator;
mod smart_turn;

use fasttalk_audio::{AudioConfig, AudioDevices, AudioEngine, AudioStatus};
use fasttalk_conversation::{
    ConversationEngine, ConversationEvent, ConversationState, EngineSnapshot,
};
use fasttalk_model_manager::{InstallProgress, ModelManager, ModelStatus, SignedManifest};
use fasttalk_runtime::WorkerState;
use native::{
    ModelBinding, NativeModelPaths, NativeRuntime, NativeRuntimeStatus, RuntimeProfile,
    RuntimeProfileOption, available_runtime_profiles,
};
use orchestrator::{ConversationController, SharedAudio, SharedEngine};
use serde::{Deserialize, Serialize};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_log::{RotationStrategy, Target, TargetKind};

struct AppState {
    engine: SharedEngine,
    audio: SharedAudio,
    runtime: Arc<Mutex<NativeRuntime>>,
    runtime_start_cancelled: Arc<AtomicBool>,
    conversation: Mutex<Option<ConversationController>>,
    models: Arc<ModelManager>,
}

impl AppState {
    fn new(
        legacy_model_root: std::path::PathBuf,
        model_store: std::path::PathBuf,
        runtime_root: std::path::PathBuf,
    ) -> Result<Self, fasttalk_model_manager::ManifestError> {
        let manifest = SignedManifest::verify(
            include_bytes!("../../../config/models.manifest.json").to_vec(),
            include_str!("../../../config/models.manifest.sig"),
            include_str!("../../../config/models.manifest.pub"),
        )?;
        let models = ModelManager::new(legacy_model_root, model_store, manifest)
            .expect("the embedded model-manager HTTP configuration is valid");
        Ok(Self {
            engine: Arc::new(Mutex::new(ConversationEngine::default())),
            audio: Arc::new(Mutex::new(None)),
            runtime: Arc::new(Mutex::new(NativeRuntime::for_root(runtime_root))),
            runtime_start_cancelled: Arc::new(AtomicBool::new(false)),
            conversation: Mutex::new(None),
            models: Arc::new(models),
        })
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CommandError {
    code: &'static str,
    message: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AudioStartRequest {
    input_device_id: Option<String>,
    output_device_id: Option<String>,
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
fn audio_devices() -> Result<AudioDevices, CommandError> {
    AudioEngine::enumerate_devices().map_err(|error| CommandError {
        code: "audioDevicesFailed",
        message: error.to_string(),
    })
}

#[tauri::command]
fn audio_start(
    request: Option<AudioStartRequest>,
    state: State<'_, AppState>,
) -> Result<AudioStatus, CommandError> {
    let mut audio = lock(&state.audio, "audioUnavailable")?;
    if audio.is_none() {
        let request = request.unwrap_or_default();
        let profile = lock(&state.runtime, "runtimeUnavailable")?
            .profile()
            .clone();
        let vad_model_path = binding_model_path(&state.models, &profile.vad)?;
        *audio = Some(
            AudioEngine::start(AudioConfig {
                input_device_id: request.input_device_id,
                output_device_id: request.output_device_id,
                vad_model_path: Some(vad_model_path),
                ..AudioConfig::default()
            })
            .map_err(|error| CommandError {
                code: "audioStartFailed",
                message: error.to_string(),
            })?,
        );
        log::info!("native audio started");
    }
    Ok(audio.as_ref().expect("audio initialized above").status())
}

#[tauri::command]
fn audio_set_muted(muted: bool, state: State<'_, AppState>) -> Result<AudioStatus, CommandError> {
    let audio = lock(&state.audio, "audioUnavailable")?;
    let audio = audio.as_ref().ok_or_else(|| CommandError {
        code: "audioNotReady",
        message: "native audio is not running".to_owned(),
    })?;
    audio.set_muted(muted);
    Ok(audio.status())
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
async fn conversation_start(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<EngineSnapshot, CommandError> {
    {
        let mut conversation = lock(&state.conversation, "conversationUnavailable")?;
        if conversation
            .as_ref()
            .is_some_and(ConversationController::is_finished)
        {
            conversation.take();
        }
    }
    if lock(&state.conversation, "conversationUnavailable")?.is_some() {
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
    {
        let audio = lock(&state.audio, "audioUnavailable")?;
        if audio.is_none() {
            return Err(CommandError {
                code: "audioNotReady",
                message: "native audio is not running".to_owned(),
            });
        }
    }

    let profile = lock(&state.runtime, "runtimeUnavailable")?
        .profile()
        .clone();
    let models = Arc::clone(&state.models);
    let turn_detector = tokio::task::spawn_blocking(move || {
        let path = binding_model_path(&models, &profile.turn_detector)?;
        smart_turn::SmartTurnDetector::new(&path).map_err(|message| CommandError {
            code: "turnDetectorUnavailable",
            message,
        })
    })
    .await
    .map_err(|error| CommandError {
        code: "turnDetectorTaskFailed",
        message: format!("Smart Turn loading task failed: {error}"),
    })??;

    let mut controller = lock(&state.conversation, "conversationUnavailable")?;
    if controller.is_some() {
        return Ok(lock_engine(&state)?.snapshot());
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
    if let Some(audio) = lock(&state.audio, "audioUnavailable")?.as_mut() {
        audio.begin_asr_session();
    }
    *controller = Some(orchestrator::start(
        app,
        state.engine.clone(),
        state.audio.clone(),
        workers.tts_backend,
        turn_detector,
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
    if let Some(audio) = lock(&state.audio, "audioUnavailable")?.as_mut() {
        audio.cancel_playback();
        audio.end_asr_session();
    }
    lock_engine(&state)?
        .apply(ConversationEvent::Reset)
        .map_err(invalid_transition)
}

#[tauri::command]
async fn runtime_start(state: State<'_, AppState>) -> Result<NativeRuntimeStatus, CommandError> {
    let runtime = Arc::clone(&state.runtime);
    let models = Arc::clone(&state.models);
    let cancelled = Arc::clone(&state.runtime_start_cancelled);
    cancelled.store(false, Ordering::Release);
    let profile = lock(&runtime, "runtimeUnavailable")?.profile().clone();
    tokio::task::spawn_blocking(move || {
        let model_paths = native_model_paths(&models, &profile)?;
        if cancelled.load(Ordering::Acquire) {
            return Err(CommandError {
                code: "runtimeStartCancelled",
                message: "Local service startup was cancelled.".to_owned(),
            });
        }
        let mut runtime = lock(&runtime, "runtimeUnavailable")?;
        if cancelled.load(Ordering::Acquire) {
            return Err(CommandError {
                code: "runtimeStartCancelled",
                message: "Local service startup was cancelled.".to_owned(),
            });
        }
        runtime
            .configure_models(model_paths)
            .map_err(runtime_error)?;
        let status = runtime.start().map_err(runtime_error)?;
        log::info!("native workers started");
        Ok(status)
    })
    .await
    .map_err(runtime_task_error)?
}

#[tauri::command]
fn runtime_profiles() -> Vec<RuntimeProfileOption> {
    available_runtime_profiles()
}

#[tauri::command]
fn runtime_select_profile(
    profile_id: String,
    state: State<'_, AppState>,
) -> Result<NativeRuntimeStatus, CommandError> {
    if lock(&state.conversation, "conversationUnavailable")?.is_some() {
        return Err(CommandError {
            code: "conversationActive",
            message: "stop the conversation before changing the language model".to_owned(),
        });
    }
    if lock(&state.audio, "audioUnavailable")?.is_some() {
        return Err(CommandError {
            code: "audioActive",
            message: "stop local services before changing the language model".to_owned(),
        });
    }
    let mut runtime = lock(&state.runtime, "runtimeUnavailable")?;
    runtime.select_profile(&profile_id).map_err(runtime_error)?;
    runtime.poll().map_err(runtime_error)
}

fn native_model_paths(
    models: &ModelManager,
    profile: &RuntimeProfile,
) -> Result<NativeModelPaths, CommandError> {
    let root = |id: &str| {
        models
            .resolved_root(id)
            .map_err(model_error)?
            .ok_or_else(|| CommandError {
                code: "modelMissing",
                message: format!("required model group is not ready: {id}"),
            })
    };
    let qwen = root(&profile.llm.model.group_id)?;
    let asr = root(&profile.asr.group_id)?;
    let magpie = root(&profile.tts.model.group_id)?;
    let nanocodec = root(&profile.codec.group_id)?;
    let kokoro = root(&profile.fallback_tts.group_id)?;
    Ok(NativeModelPaths {
        qwen: qwen.join(&profile.llm.model.artifact),
        asr: asr.join(&profile.asr.artifact),
        magpie: magpie.join(&profile.tts.model.artifact),
        nanocodec: nanocodec.join(&profile.codec.artifact),
        magpie_tokenizer: magpie.join(&profile.tts.tokenizer_artifact),
        kokoro,
    })
}

fn binding_model_path(
    models: &ModelManager,
    binding: &ModelBinding,
) -> Result<std::path::PathBuf, CommandError> {
    let root = models
        .resolved_root(&binding.group_id)
        .map_err(model_error)?
        .ok_or_else(|| CommandError {
            code: "modelMissing",
            message: format!("required model group is not ready: {}", binding.group_id),
        })?;
    Ok(root.join(&binding.artifact))
}

#[tauri::command]
fn runtime_cancel_start(state: State<'_, AppState>) {
    state.runtime_start_cancelled.store(true, Ordering::Release);
}

#[tauri::command]
async fn runtime_status(state: State<'_, AppState>) -> Result<NativeRuntimeStatus, CommandError> {
    let runtime = Arc::clone(&state.runtime);
    tokio::task::spawn_blocking(move || {
        lock(&runtime, "runtimeUnavailable")?
            .poll()
            .map_err(runtime_error)
    })
    .await
    .map_err(runtime_task_error)?
}

#[tauri::command]
async fn runtime_stop(state: State<'_, AppState>) -> Result<NativeRuntimeStatus, CommandError> {
    state.runtime_start_cancelled.store(true, Ordering::Release);
    let runtime = Arc::clone(&state.runtime);
    tokio::task::spawn_blocking(move || {
        let status = lock(&runtime, "runtimeUnavailable")?
            .stop()
            .map_err(runtime_error)?;
        log::info!("native workers stopped");
        Ok(status)
    })
    .await
    .map_err(runtime_task_error)?
}

#[tauri::command]
async fn model_status(state: State<'_, AppState>) -> Result<Vec<ModelStatus>, CommandError> {
    let models = state.models.clone();
    let ids = selected_model_ids(&state)?;
    tokio::task::spawn_blocking(move || models.statuses_for(&ids))
        .await
        .map_err(|error| CommandError {
            code: "modelTaskFailed",
            message: error.to_string(),
        })?
        .map_err(model_error)
}

#[tauri::command]
async fn model_install_all(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<Vec<ModelStatus>, CommandError> {
    let models = state.models.clone();
    let ids = selected_model_ids(&state)?;
    let token = std::env::var("HF_TOKEN").ok();
    let progress_app = app.clone();
    models
        .install_groups(&ids, token.as_deref(), &move |progress: InstallProgress| {
            let _ = progress_app.emit("model-progress", progress);
        })
        .await
        .map_err(model_error)
}

#[tauri::command]
async fn model_import_pack(
    path: String,
    state: State<'_, AppState>,
) -> Result<Vec<ModelStatus>, CommandError> {
    let models = state.models.clone();
    let ids = selected_model_ids(&state)?;
    tokio::task::spawn_blocking(move || models.import_pack(std::path::Path::new(&path)))
        .await
        .map_err(|error| CommandError {
            code: "modelTaskFailed",
            message: error.to_string(),
        })?
        .map_err(model_error)?;
    state.models.statuses_for(&ids).map_err(model_error)
}

#[tauri::command]
async fn model_export_pack(path: String, state: State<'_, AppState>) -> Result<(), CommandError> {
    let models = state.models.clone();
    let ids = selected_model_ids(&state)?;
    tokio::task::spawn_blocking(move || {
        models.export_pack_groups(&ids, std::path::Path::new(&path))
    })
    .await
    .map_err(|error| CommandError {
        code: "modelTaskFailed",
        message: error.to_string(),
    })?
    .map_err(model_error)
}

fn selected_model_ids(state: &State<'_, AppState>) -> Result<Vec<String>, CommandError> {
    let profile = lock(&state.runtime, "runtimeUnavailable")?
        .profile()
        .clone();
    Ok(profile_model_ids(&profile))
}

fn profile_model_ids(profile: &RuntimeProfile) -> Vec<String> {
    let candidates = [
        &profile.llm.model.group_id,
        &profile.asr.group_id,
        &profile.tts.model.group_id,
        &profile.codec.group_id,
        &profile.fallback_tts.group_id,
        &profile.vad.group_id,
        &profile.turn_detector.group_id,
    ];
    let mut ids = Vec::new();
    for id in candidates {
        if !ids.contains(id) {
            ids.push(id.clone());
        }
    }
    ids
}

fn model_error(error: fasttalk_model_manager::ModelManagerError) -> CommandError {
    CommandError {
        code: "modelManagerFailed",
        message: error.to_string(),
    }
}

fn runtime_error(error: fasttalk_runtime::SupervisorError) -> CommandError {
    CommandError {
        code: "nativeRuntimeFailed",
        message: error.to_string(),
    }
}

fn runtime_task_error(error: tokio::task::JoinError) -> CommandError {
    CommandError {
        code: "runtimeTaskFailed",
        message: format!("local service task failed: {error}"),
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
        && status.kokoro.as_ref().map(|worker| &worker.state) == Some(&WorkerState::Ready)
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
        .plugin(tauri_plugin_dialog::init())
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
        .setup(|app| {
            let app_data = app.path().app_local_data_dir()?;
            let model_store = app_data.join("models");
            let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
            let (legacy_model_root, runtime_root) = if cfg!(debug_assertions) {
                (workspace.clone(), workspace)
            } else {
                (app_data, app.path().resource_dir()?)
            };
            app.manage(AppState::new(legacy_model_root, model_store, runtime_root)?);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            engine_snapshot,
            engine_dispatch,
            audio_devices,
            audio_start,
            audio_status,
            audio_set_muted,
            audio_cancel,
            audio_stop,
            conversation_start,
            conversation_interrupt,
            conversation_stop,
            runtime_start,
            runtime_profiles,
            runtime_select_profile,
            runtime_cancel_start,
            runtime_status,
            runtime_stop,
            model_status,
            model_install_all,
            model_import_pack,
            model_export_pack
        ])
        .run(tauri::generate_context!())
        .expect("error while running FastTalk");
}

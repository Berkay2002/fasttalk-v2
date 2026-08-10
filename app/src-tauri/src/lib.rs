use fasttalk_conversation::{ConversationEngine, ConversationEvent, EngineSnapshot};
use serde::Serialize;
use std::sync::Mutex;
use tauri::State;

#[derive(Default)]
struct AppState {
    engine: Mutex<ConversationEngine>,
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
    lock_engine(&state)?
        .apply(event)
        .map_err(|error| CommandError {
            code: "invalidTransition",
            message: error.to_string(),
        })
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![engine_snapshot, engine_dispatch])
        .run(tauri::generate_context!())
        .expect("error while running FastTalk");
}

//! Thin Tauri shell around ledger-core. All logic lives in the core crate;
//! this file only maps commands <-> core calls and streams step events.

use ledger_core::ollama::{ChatMessage, OllamaClient, OllamaConfig};
use ledger_core::orchestrator::{self, StepSink};
use ledger_core::workspace::Workspace;
use serde::Serialize;
use std::path::PathBuf;
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager, State};

pub struct AppState {
    pub ws: Workspace,
    pub history: Vec<ChatMessage>,
    pub cfg: OllamaConfig,
    pub flushed_seq: u32,
}

pub type SharedState = Mutex<AppState>;

#[derive(Serialize, Clone)]
struct StepEvent {
    text: String,
}

struct EmitSink<'a> {
    app: &'a AppHandle,
}
impl<'a> StepSink for EmitSink<'a> {
    fn step(&self, text: &str) {
        let _ = self.app.emit("agent-step", StepEvent { text: text.to_string() });
    }
}

#[derive(Serialize)]
pub struct JournalEntryDto {
    pub seq: u32,
    pub time: String,
    pub tool: String,
    pub summary: String,
    pub ok: bool,
}

#[derive(Serialize)]
pub struct HealthDto {
    pub online: bool,
    pub models: Vec<String>,
    pub configured_model: String,
    pub model_available: bool,
    pub detail: String,
}

#[tauri::command]
fn check_ollama(state: State<SharedState>) -> HealthDto {
    let cfg = state.lock().unwrap().cfg.clone();
    match OllamaClient::new(cfg.clone()).and_then(|c| c.health()) {
        Ok(models) => {
            let model_available = models
                .iter()
                .any(|m| m == &cfg.model || m.starts_with(&format!("{}:", cfg.model)));
            HealthDto {
                online: true,
                model_available,
                detail: if model_available {
                    "ready".into()
                } else {
                    format!("Ollama is running but '{}' isn't pulled. Run: ollama pull {}", cfg.model, cfg.model)
                },
                models,
                configured_model: cfg.model,
            }
        }
        Err(e) => HealthDto {
            online: false,
            models: vec![],
            configured_model: cfg.model,
            model_available: false,
            detail: e.to_string(),
        },
    }
}

#[tauri::command]
fn set_model(state: State<SharedState>, model: String) -> Result<(), String> {
    if model.trim().is_empty() {
        return Err("model name is empty".into());
    }
    state.lock().unwrap().cfg.model = model.trim().to_string();
    Ok(())
}

#[tauri::command]
fn load_files(state: State<SharedState>, paths: Vec<String>) -> Result<serde_json::Value, String> {
    let mut st = state.lock().unwrap();
    let mut loaded = Vec::new();
    for p in paths {
        let summary = st
            .ws
            .load_path(std::path::Path::new(&p))
            .map_err(|e| format!("{p}: {e}"))?;
        loaded.push(summary);
    }
    Ok(serde_json::json!({ "loaded": loaded, "workspace": st.ws.list() }))
}

#[tauri::command]
fn list_workspace(state: State<SharedState>) -> serde_json::Value {
    state.lock().unwrap().ws.list()
}

#[tauri::command]
fn get_journal(state: State<SharedState>) -> Vec<JournalEntryDto> {
    state
        .lock()
        .unwrap()
        .ws
        .audit
        .entries()
        .iter()
        .map(|e| JournalEntryDto {
            seq: e.seq,
            time: e.time.format("%H:%M:%S").to_string(),
            tool: e.tool.clone(),
            summary: e.summary.clone(),
            ok: e.ok,
        })
        .collect()
}

#[tauri::command]
fn export_dir(state: State<SharedState>) -> String {
    state.lock().unwrap().ws.export_dir.display().to_string()
}

/// One chat turn. Blocking (Tauri runs commands off the main thread);
/// progress arrives on the `agent-step` event channel.
#[tauri::command]
fn send_message(
    app: AppHandle,
    state: State<SharedState>,
    text: String,
) -> Result<String, String> {
    let mut st = state.lock().unwrap();
    let client = OllamaClient::new(st.cfg.clone()).map_err(|e| e.to_string())?;
    let sink = EmitSink { app: &app };

    let AppState { ws, history, flushed_seq, .. } = &mut *st;
    let reply = orchestrator::run_turn(ws, &client, history, &text, &sink)
        .map_err(|e| e.to_string())?;

    // Persist the journal after every turn — the paper trail survives crashes.
    let journal_path = ws.export_dir.join("journal.jsonl");
    if ws.audit.flush_jsonl(&journal_path, *flushed_seq).is_ok() {
        *flushed_seq = ws.audit.entries().last().map(|e| e.seq).unwrap_or(*flushed_seq);
    }
    Ok(reply)
}

/// A renderable view of a file/sheet or a derived result, for the viewer grid.
#[tauri::command]
fn get_table(
    state: State<SharedState>,
    result_id: Option<String>,
    file: Option<String>,
    sheet: Option<String>,
    offset: Option<usize>,
    limit: Option<usize>,
) -> Result<serde_json::Value, String> {
    let st = state.lock().unwrap();
    st.ws
        .table_view(
            result_id.as_deref(),
            file.as_deref(),
            sheet.as_deref(),
            offset.unwrap_or(0),
            limit.unwrap_or(200).min(500),
        )
        .map_err(|e| e.to_string())
}

/// Start a fresh conversation: clears chat history and derived results.
/// Loaded source files are kept so the user can keep working on them.
#[tauri::command]
fn new_chat(state: State<SharedState>) {
    let mut st = state.lock().unwrap();
    st.history.clear();
    st.ws.clear_results();
}

/// Preload the model into memory so the first message isn't cold. Best-effort;
/// doesn't hold the app lock during the (possibly long) load.
#[tauri::command]
fn warm_model(state: State<SharedState>) -> Result<(), String> {
    let cfg = state.lock().unwrap().cfg.clone();
    let client = OllamaClient::new(cfg).map_err(|e| e.to_string())?;
    client.warm().map_err(|e| e.to_string())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let export_dir: PathBuf = app
                .path()
                .document_dir()
                .unwrap_or_else(|_| std::env::temp_dir())
                .join("Ledger Local");
            app.manage::<SharedState>(Mutex::new(AppState {
                ws: Workspace::new(export_dir),
                history: Vec::new(),
                cfg: OllamaConfig::default(),
                flushed_seq: 0,
            }));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            check_ollama,
            set_model,
            load_files,
            list_workspace,
            get_journal,
            export_dir,
            send_message,
            get_table,
            new_chat,
            warm_model,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Ledger Local");
}

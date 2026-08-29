mod backend;
mod backend_rpc;
mod workspace_review;

use std::sync::Arc;

use backend::{ConnectionInfo, DesktopBackend};
use serde_json::Value;
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::Mutex;
use workspace_review::WorkspaceReview;

const CODEX_EVENT: &str = "codex-message";

#[derive(Default)]
struct DesktopState {
    backend: Mutex<Option<Arc<DesktopBackend>>>,
}

#[tauri::command]
async fn connect_codex(
    app: AppHandle,
    state: State<'_, DesktopState>,
) -> Result<ConnectionInfo, String> {
    let mut backend = state.backend.lock().await;
    if let Some(backend) = backend.as_ref() {
        return Ok(backend.connection_info());
    }

    let connected = Arc::new(DesktopBackend::spawn().await?);
    forward_codex_events(&app, connected.subscribe());
    let info = connected.connection_info();
    *backend = Some(connected);
    Ok(info)
}

fn forward_codex_events(app: &AppHandle, mut events: tokio::sync::broadcast::Receiver<String>) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            match events.recv().await {
                Ok(line) => {
                    let _ = app.emit(CODEX_EVENT, line);
                }
                Err(_) => {
                    let _ = app.emit(
                        CODEX_EVENT,
                        r#"{"method":"remoteAgent/connectionClosed","params":{}}"#,
                    );
                    return;
                }
            }
        }
    });
}

#[tauri::command]
async fn codex_request(
    method: String,
    params: Value,
    state: State<'_, DesktopState>,
) -> Result<Value, String> {
    connected_backend(&state)
        .await?
        .request(method, params)
        .await
}

#[tauri::command]
async fn codex_respond(
    id: Value,
    result: Value,
    state: State<'_, DesktopState>,
) -> Result<(), String> {
    connected_backend(&state).await?.respond(id, result).await
}

#[tauri::command]
async fn codex_respond_error(
    id: Value,
    error: Value,
    state: State<'_, DesktopState>,
) -> Result<(), String> {
    connected_backend(&state)
        .await?
        .respond_error(id, error)
        .await
}

async fn connected_backend(state: &DesktopState) -> Result<Arc<DesktopBackend>, String> {
    state
        .backend
        .lock()
        .await
        .clone()
        .ok_or_else(|| "Codex is not connected".to_owned())
}

#[tauri::command]
async fn workspace_review(cwd: String) -> Result<WorkspaceReview, String> {
    workspace_review::inspect(cwd).await
}

fn main() {
    tauri::Builder::default()
        .manage(DesktopState::default())
        .invoke_handler(tauri::generate_handler![
            connect_codex,
            codex_request,
            codex_respond,
            codex_respond_error,
            workspace_review
        ])
        .setup(|app| {
            if let Some(window) = app.get_webview_window("main") {
                window.set_focus()?;
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("failed to run Remote Agent Desktop");
}

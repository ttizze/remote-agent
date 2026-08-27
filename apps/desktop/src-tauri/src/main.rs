use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use codex_app_server::{AppServerConfig, ClientInfo, CodexAppServer};
use host_daemon::{
    DesktopProjectStore, HOST_PROJECT_LIST_METHOD, HOST_THREAD_LIST_METHOD,
    HOST_THREAD_READ_METHOD, HOST_THREAD_START_METHOD,
};
use serde::Serialize;
use serde_json::{Map, Value, json};
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::Mutex;

const CODEX_EVENT: &str = "codex-message";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Default)]
struct DesktopState {
    backend: Mutex<Option<DesktopBackend>>,
    next_request_id: AtomicU64,
}

#[derive(Clone)]
struct DesktopBackend {
    app_server: Arc<CodexAppServer>,
    projects: DesktopProjectStore,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ConnectionInfo {
    user_agent: String,
    platform_family: String,
    platform_os: String,
    codex_home: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkspaceReview {
    branch: String,
    additions: u64,
    deletions: u64,
    files: Vec<WorkspaceFileChange>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkspaceFileChange {
    path: String,
    status: &'static str,
}

#[tauri::command]
async fn connect_codex(
    app: AppHandle,
    state: State<'_, DesktopState>,
) -> Result<ConnectionInfo, String> {
    let mut backend = state.backend.lock().await;
    if let Some(backend) = backend.as_ref() {
        return Ok(connection_info(&backend.app_server));
    }

    let app_server = Arc::new(
        CodexAppServer::spawn(AppServerConfig {
            client: ClientInfo {
                name: "remote_agent_desktop".to_owned(),
                title: "Remote Agent Desktop".to_owned(),
                version: env!("CARGO_PKG_VERSION").to_owned(),
            },
            request_timeout: REQUEST_TIMEOUT,
            ..AppServerConfig::default()
        })
        .await
        .map_err(|error| error.to_string())?,
    );
    let projects = DesktopProjectStore::from_environment().map_err(|error| error.to_string())?;
    let mut events = app_server.subscribe();
    let event_app = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            match events.recv().await {
                Ok(line) => {
                    let _ = event_app.emit(CODEX_EVENT, line);
                }
                Err(_) => {
                    let _ = event_app.emit(
                        CODEX_EVENT,
                        r#"{"method":"remoteAgent/connectionClosed","params":{}}"#,
                    );
                    return;
                }
            }
        }
    });

    let info = connection_info(&app_server);
    *backend = Some(DesktopBackend {
        app_server,
        projects,
    });
    Ok(info)
}

#[tauri::command]
async fn codex_request(
    method: String,
    params: Value,
    state: State<'_, DesktopState>,
) -> Result<Value, String> {
    let backend = state
        .backend
        .lock()
        .await
        .clone()
        .ok_or_else(|| "Codex is not connected".to_owned())?;
    let previous_id = state
        .next_request_id
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            current.checked_add(1)
        })
        .map_err(|_| "request id space is exhausted".to_owned())?;
    let id = previous_id + 1;

    if method == HOST_PROJECT_LIST_METHOD {
        return backend
            .projects
            .project_list(&params)
            .await
            .map_err(|error| error.to_string());
    }

    let upstream_method = desktop_upstream_method(&method).unwrap_or(&method);
    let line = serde_json::to_string(&json!({
        "id": id,
        "method": upstream_method,
        "params": params,
    }))
    .map_err(|error| error.to_string())?;
    let response = backend
        .app_server
        .request_raw(&line)
        .await
        .map_err(|error| error.to_string())?;
    let mut result = response_result(&response)?;
    if desktop_upstream_method(&method).is_some() {
        result = backend
            .projects
            .enrich_threads(result)
            .await
            .map_err(|error| error.to_string())?;
    }
    Ok(result)
}

#[tauri::command]
async fn codex_respond(
    id: Value,
    result: Value,
    state: State<'_, DesktopState>,
) -> Result<(), String> {
    let app_server = state
        .backend
        .lock()
        .await
        .as_ref()
        .map(|backend| backend.app_server.clone())
        .ok_or_else(|| "Codex is not connected".to_owned())?;
    let line = serde_json::to_string(&json!({ "id": id, "result": result }))
        .map_err(|error| error.to_string())?;
    app_server
        .send_raw(&line)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn codex_respond_error(
    id: Value,
    error: Value,
    state: State<'_, DesktopState>,
) -> Result<(), String> {
    let app_server = state
        .backend
        .lock()
        .await
        .as_ref()
        .map(|backend| backend.app_server.clone())
        .ok_or_else(|| "Codex is not connected".to_owned())?;
    let line = serde_json::to_string(&json!({ "id": id, "error": error }))
        .map_err(|failure| failure.to_string())?;
    app_server
        .send_raw(&line)
        .await
        .map_err(|failure| failure.to_string())
}

#[tauri::command]
async fn workspace_review(cwd: String) -> Result<WorkspaceReview, String> {
    tokio::task::spawn_blocking(move || collect_workspace_review(PathBuf::from(cwd)))
        .await
        .map_err(|error| format!("failed to inspect workspace: {error}"))?
}

fn collect_workspace_review(cwd: PathBuf) -> Result<WorkspaceReview, String> {
    let cwd = cwd
        .canonicalize()
        .map_err(|error| format!("working directory is unavailable: {error}"))?;
    if !cwd.is_dir() {
        return Err("working directory is not a directory".to_owned());
    }
    run_git(&cwd, &["rev-parse", "--is-inside-work-tree"])?;
    let branch = run_git(&cwd, &["symbolic-ref", "--quiet", "--short", "HEAD"])
        .or_else(|_| run_git(&cwd, &["rev-parse", "--short", "HEAD"]))?
        .trim()
        .to_owned();
    let status = run_git_bytes(&cwd, &["status", "--porcelain=v1", "-z"])?;
    let files = parse_git_status(&status);
    let numstat = run_git(&cwd, &["diff", "--numstat", "HEAD", "--"])
        .or_else(|_| run_git(&cwd, &["diff", "--numstat", "--"]))?;
    let (additions, deletions) = parse_numstat(&numstat);
    Ok(WorkspaceReview {
        branch,
        additions,
        deletions,
        files,
    })
}

fn run_git(cwd: &Path, args: &[&str]) -> Result<String, String> {
    let output = run_git_output(cwd, args)?;
    String::from_utf8(output.stdout).map_err(|_| "git returned non-UTF-8 output".to_owned())
}

fn run_git_bytes(cwd: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    Ok(run_git_output(cwd, args)?.stdout)
}

fn run_git_output(cwd: &Path, args: &[&str]) -> Result<std::process::Output, String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .map_err(|error| format!("failed to run git: {error}"))?;
    if output.status.success() {
        return Ok(output);
    }
    let message = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    Err(if message.is_empty() {
        "git command failed".to_owned()
    } else {
        message
    })
}

fn parse_git_status(output: &[u8]) -> Vec<WorkspaceFileChange> {
    let entries = output.split(|byte| *byte == 0).collect::<Vec<_>>();
    let mut files = Vec::new();
    let mut index = 0;
    while index < entries.len() && files.len() < 200 {
        let entry = entries[index];
        if entry.len() < 4 {
            index += 1;
            continue;
        }
        let code = &entry[..2];
        let path = String::from_utf8_lossy(&entry[3..]).into_owned();
        let status = if code == b"??" {
            "untracked"
        } else if code.contains(&b'D') {
            "deleted"
        } else if code.contains(&b'A') {
            "added"
        } else if code.contains(&b'R') || code.contains(&b'C') {
            index += 1;
            "renamed"
        } else {
            "modified"
        };
        files.push(WorkspaceFileChange { path, status });
        index += 1;
    }
    files
}

fn parse_numstat(output: &str) -> (u64, u64) {
    output.lines().fold((0, 0), |(added, deleted), line| {
        let mut fields = line.split('\t');
        let line_added = fields
            .next()
            .and_then(|value| value.parse().ok())
            .unwrap_or(0);
        let line_deleted = fields
            .next()
            .and_then(|value| value.parse().ok())
            .unwrap_or(0);
        (added + line_added, deleted + line_deleted)
    })
}

fn desktop_upstream_method(method: &str) -> Option<&'static str> {
    match method {
        HOST_THREAD_LIST_METHOD => Some("thread/list"),
        HOST_THREAD_READ_METHOD => Some("thread/read"),
        HOST_THREAD_START_METHOD => Some("thread/start"),
        _ => None,
    }
}

fn connection_info(app_server: &CodexAppServer) -> ConnectionInfo {
    let response = app_server.initialize_response();
    ConnectionInfo {
        user_agent: response.user_agent.clone(),
        platform_family: response.platform_family.clone(),
        platform_os: response.platform_os.clone(),
        codex_home: response.codex_home.to_string_lossy().into_owned(),
    }
}

fn response_result(line: &str) -> Result<Value, String> {
    let response: Map<String, Value> =
        serde_json::from_str(line).map_err(|error| format!("invalid Codex response: {error}"))?;
    if let Some(error) = response.get("error") {
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("Codex rejected the request");
        return Err(message.to_owned());
    }
    response
        .get("result")
        .cloned()
        .ok_or_else(|| "Codex response is missing result".to_owned())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_only_desktop_thread_methods() {
        assert_eq!(
            desktop_upstream_method(HOST_THREAD_LIST_METHOD),
            Some("thread/list")
        );
        assert_eq!(
            desktop_upstream_method(HOST_THREAD_READ_METHOD),
            Some("thread/read")
        );
        assert_eq!(
            desktop_upstream_method(HOST_THREAD_START_METHOD),
            Some("thread/start")
        );
        assert_eq!(desktop_upstream_method("turn/start"), None);
    }

    #[test]
    fn extracts_result_and_remote_error() {
        assert_eq!(
            response_result(r#"{"id":1,"result":{"value":7}}"#).unwrap(),
            json!({"value": 7})
        );
        assert_eq!(
            response_result(r#"{"id":1,"error":{"code":-1,"message":"nope"}}"#).unwrap_err(),
            "nope"
        );
    }
}

use agent_peer::home;
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{env, os::unix::fs::PermissionsExt, path::PathBuf};
use tokio::process::Command;

pub fn command(provider: &str, session: Option<&str>, model: Option<&str>) -> Result<Command> {
    let binary = match env::var_os(format!("AGENT_PEER_{}_BIN", provider.to_uppercase()))
        .filter(|s| !s.is_empty())
    {
        Some(binary) => PathBuf::from(binary),
        None => {
            let mut candidates = Vec::new();
            if provider == "codex" {
                candidates.extend(["ChatGPT", "Codex"].map(|app| {
                    PathBuf::from(format!("/Applications/{app}.app/Contents/Resources/codex"))
                }));
            }
            candidates.extend(
                env::split_paths(&env::var_os("PATH").unwrap_or_default())
                    .map(|p| p.join(provider)),
            );
            candidates.extend(home().ok().map(|p| p.join(".local/bin").join(provider)));
            candidates
                .into_iter()
                .find(|p| {
                    p.metadata()
                        .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
                })
                .with_context(|| format!("CLI not found: {provider}"))?
                .canonicalize()?
        }
    };
    let mut command = Command::new(binary);
    if provider == "claude" {
        command
            .arg("-p")
            .args(["--output-format", "json"])
            .args(["--tools", "Read,Glob,Grep"])
            .args(["--allowedTools", "Read,Glob,Grep"])
            .args(["--permission-mode", "dontAsk"])
            .args(["--disable-slash-commands", "--strict-mcp-config"])
            .args(["--mcp-config", r#"{"mcpServers":{}}"#])
            .args(["--settings", r#"{"disableAllHooks":true}"#]);
    } else {
        command
            .args([
                "exec",
                "--json",
                "--sandbox",
                "read-only",
                "--skip-git-repo-check",
            ])
            .args(["-c", r#"approval_policy="never""#]);
    }
    if let Some(id) = session {
        command.args([
            if provider == "claude" {
                "--resume"
            } else {
                "resume"
            },
            id,
        ]);
    }
    if let Some(model) = model {
        command.args(["--model", model]);
    }
    if provider == "codex" {
        command.arg("-");
    }
    Ok(command)
}

pub fn decode(provider: &str, output: &str, success: bool) -> Result<Value> {
    if provider == "claude" {
        let result: Value = serde_json::from_str(output)?;
        ensure!(
            result["type"] == "result",
            "Claude did not return a final result"
        );
        let denied: Vec<_> = result["permission_denials"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|item| &item["tool_name"])
            .collect();
        let status = if !success || result["is_error"] == true {
            "error"
        } else if denied.is_empty() {
            "ok"
        } else {
            "blocked"
        };
        return Ok(json!({"session_id": result["session_id"], "status": status,
            "text": result.get("result").unwrap_or(&json!("")),
            "errors": result.get("errors").unwrap_or(&json!([])), "denied_tools": denied}));
    }
    let mut session = Value::Null;
    let (mut messages, mut errors, mut stream_errors) = (Vec::new(), Vec::new(), Vec::new());
    let mut complete = false;
    for line in output.lines().filter(|s| !s.trim().is_empty()) {
        let event: Value = serde_json::from_str(line)?;
        match event["type"].as_str().unwrap_or_default() {
            "thread.started" if session.is_null() => session = event["thread_id"].clone(),
            "item.completed" if event["item"]["type"] == "agent_message" => messages.push(
                event["item"]["text"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
            ),
            "turn.completed" => complete = true,
            "turn.failed" => errors.push(
                event["error"]["message"]
                    .as_str()
                    .unwrap_or("Codex turn failed")
                    .to_owned(),
            ),
            "error" => stream_errors.push(
                event["message"]
                    .as_str()
                    .unwrap_or("Codex error")
                    .to_owned(),
            ),
            _ => {}
        }
    }
    if !complete && errors.is_empty() {
        errors = stream_errors;
        if errors.is_empty() {
            errors.push("No completed turn received".into());
        }
    }
    Ok(json!({"session_id": session,
        "status": if success && complete && errors.is_empty() { "ok" } else { "error" },
        "text": messages.join("\n\n"), "errors": errors, "denied_tools": []}))
}

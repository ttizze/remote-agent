use std::{path::Path, process::Stdio, time::Duration};

use agent_core::peer::{JsonlReader, JsonlWriter};
use serde_json::{Value, json};
use tokio::{
    io::AsyncReadExt,
    process::{Child, ChildStdin, ChildStdout, Command},
};

pub(super) struct Process {
    child: Child,
    input: Option<JsonlWriter<ChildStdin>>,
    output: JsonlReader<ChildStdout>,
    stderr: tokio::task::JoinHandle<String>,
}

impl Process {
    pub(super) async fn start(
        program: &Path,
        cwd: &Path,
        session: Option<(&str, bool)>,
        model: Option<&str>,
        effort: Option<&str>,
    ) -> Result<(Self, Value), String> {
        let mut command = Command::new(program);
        command.current_dir(cwd).args([
            "-p",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--verbose",
            "--include-partial-messages",
            "--permission-prompt-tool",
            "stdio",
        ]);
        if let Some((session, resume)) = session {
            command
                .arg(if resume { "--resume" } else { "--session-id" })
                .arg(session);
        }
        if let Some(model) = model {
            command.arg("--model").arg(model);
        }
        if let Some(effort) = effort {
            command.arg("--effort").arg(effort);
        }
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| {
                format!(
                    "Claude Codeを起動できません（{}）: {error}",
                    program.display()
                )
            })?;
        let mut stderr = child.stderr.take().ok_or("Claude Code stderr is missing")?;
        let stderr = tokio::spawn(async move {
            let mut tail = Vec::new();
            let mut buffer = [0; 4096];
            while let Ok(length) = stderr.read(&mut buffer).await {
                if length == 0 {
                    break;
                }
                tail.extend_from_slice(&buffer[..length]);
                if tail.len() > 8192 {
                    tail.drain(..tail.len() - 8192);
                }
            }
            String::from_utf8_lossy(&tail).into_owned()
        });
        let mut process = Self {
            input: Some(JsonlWriter::new(
                child.stdin.take().ok_or("Claude Code stdin is missing")?,
            )),
            output: JsonlReader::new(child.stdout.take().ok_or("Claude Code stdout is missing")?),
            child,
            stderr,
        };
        let initialized = tokio::time::timeout(Duration::from_secs(30), async {
            process.write(&json!({"type":"control_request", "request_id":"initialize", "request":{"subtype":"initialize"}})).await?;
            loop {
                let message = process.read().await?.ok_or("Claude Code exited before initialization")?;
                if message["type"] == "control_response" && message["response"]["request_id"] == "initialize" {
                    if message["response"]["subtype"] != "success" {
                        return Err(format!("Claude Code initialization failed: {}", message["response"]["error"]));
                    }
                    return Ok(message["response"]["response"].clone());
                }
            }
        }).await.map_err(|_| "Claude Codeの初期化がタイムアウトしました。".to_owned())?;
        match initialized {
            Ok(result) => Ok((process, result)),
            Err(error) => {
                let detail = process.finish().await.err().unwrap_or_default();
                Err(format!("{error}. {detail}"))
            }
        }
    }

    pub(super) async fn write(&mut self, value: &Value) -> Result<(), String> {
        self.input
            .as_mut()
            .ok_or("Claude Code input is closed")?
            .write_line(&value.to_string())
            .await
            .map_err(|error| error.to_string())
    }

    pub(super) async fn read(&mut self) -> Result<Option<Value>, String> {
        self.output
            .read_line()
            .await
            .map_err(|error| error.to_string())?
            .map(|line| {
                serde_json::from_str(&line)
                    .map_err(|error| format!("invalid Claude Code message: {error}"))
            })
            .transpose()
    }

    pub(super) async fn finish(self) -> Result<(), String> {
        let Self {
            mut child,
            input,
            mut output,
            mut stderr,
        } = self;
        drop(input);
        let finished = tokio::time::timeout(Duration::from_secs(10), async {
            tokio::try_join!(
                async { child.wait().await.map_err(|error| error.to_string()) },
                async {
                    while output
                        .read_line()
                        .await
                        .map_err(|error| error.to_string())?
                        .is_some()
                    {}
                    Ok::<_, String>(())
                },
                async { (&mut stderr).await.map_err(|error| error.to_string()) }
            )
        })
        .await;
        let (status, (), stderr_text) = match finished {
            Ok(result) => result?,
            Err(_) => {
                let _ = child.kill().await;
                stderr.abort();
                return Err("Claude Code did not exit after closing its input".into());
            }
        };
        if !status.success() {
            return Err(format!(
                "Claude Code exited with {status}: {}",
                stderr_text.trim()
            ));
        }
        Ok(())
    }
}

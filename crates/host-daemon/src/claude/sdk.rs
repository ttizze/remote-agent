//! Embedded, lockfile-pinned SDK, run under the Host's process supervisor.
use super::control::ClaudeProgram;
use crate::conversation::ProcessSpec;
use agent_domain::Driver;
use agent_providers::{read_frame, write_frame};
use tokio::io::BufReader;
use serde_json::{Value, json};
use std::{io, path::Path, process::Stdio};

impl ClaudeProgram {
    pub(crate) async fn sdk_requests(
        &self,
        home: &Path,
        cwd: &Path,
        requests: Vec<Value>,
    ) -> io::Result<Value> {
        let spec = self.sdk_process(home, cwd).await?;
        let mut child = bex_process::command(&spec.program)?
            .args(spec.args)
            .env_clear()
            .envs(spec.env)
            .current_dir(spec.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let mut writer = child.stdin.take().ok_or_else(|| io::Error::other("Claude SDK stdin missing"))?;
        let mut reader = BufReader::new(child.stdout.take().ok_or_else(|| io::Error::other("Claude SDK stdout missing"))?);
        let mut line = Vec::new();
        let result = tokio::time::timeout(std::time::Duration::from_secs(30), async {
            let mut result = Value::Null;
            for (id, request) in requests.into_iter().enumerate() {
                let id = id.to_string();
                write_frame(&mut writer, &json!({"type":"control_request", "request_id":id, "request":request})).await?;
                loop {
                    let frame = read_frame(&mut reader, &mut line).await?.ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "Claude SDK closed before replying"))?;
                    if frame["type"] != "control_response" || frame["response"]["request_id"] != id
                    {
                        continue;
                    }
                    if frame["response"]["subtype"] == "error" {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            frame["response"]["error"]
                                .as_str()
                                .unwrap_or("Claude SDK rejected request"),
                        ));
                    }
                    result = frame["response"]["response"].clone();
                    break;
                }
            }
            Ok(result)
        })
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "Claude SDK request timed out"));
        drop(writer);
        drop(reader);
        // Closing the supervisor's lifetime pipe terminates the whole Node/CLI tree.
        tokio::spawn(async move {
            let _ = child.wait().await;
        });
        result?
    }

    pub(crate) async fn sdk_process(
        &self,
        credentials_home: &Path,
        cwd: &Path,
    ) -> io::Result<ProcessSpec> {
        let sdk = include_bytes!("sdk/sdk.mjs");
        let hash = ring::digest::digest(&ring::digest::SHA256, sdk);
        let key = hash
            .as_ref()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let path = self
            .config_home
            .join("remote-agent-sdk")
            .join(format!("{}-{key}", agent_providers::CLAUDE_SDK_VERSION))
            .join("sdk.mjs");
        let destination = path.clone();
        tokio::task::spawn_blocking(move || {
            if std::fs::read(&destination).ok().as_deref() != Some(sdk.as_slice()) {
                crate::platform::save_private_bytes(&destination, sdk).map_err(io::Error::other)?;
            }
            Ok::<_, io::Error>(())
        })
        .await
        .map_err(io::Error::other)??;
        let executable = std::env::current_exe()?;
        let bundled_node = executable
            .parent()
            .unwrap_or(Path::new("."))
            .join(format!("node{}", std::env::consts::EXE_SUFFIX));
        Ok(ProcessSpec {
            driver: Driver::Claude,
            program: if bundled_node.is_file() {
                bundled_node
            } else {
                "node".into()
            },
            args: vec![
                "--input-type=module".into(),
                "--eval".into(),
                include_str!("sdk/bridge.mjs").into(),
                path.to_string_lossy().into_owned(),
                self.program.to_string_lossy().into_owned(),
            ],
            env: self.environment(credentials_home),
            clear_env: true,
            cwd: cwd.to_owned(),
        })
    }
}

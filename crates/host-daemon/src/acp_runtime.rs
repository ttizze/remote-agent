//! Small ACP stdio session owner used by registry probes and future provider
//! adapters. JSON-RPC correlation belongs to the shared transport peer; this
//! type owns the child lifetime and ACP handshake around it.

use agent_transport::peer::{JsonlReader, RpcPeer};
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::Path, process::Stdio, sync::Arc, time::Duration};
use tokio::process::{Child, Command};

pub(crate) struct AcpSessionRuntime {
    peer: Arc<RpcPeer>,
    child: tokio::sync::Mutex<Child>,
}

impl AcpSessionRuntime {
    pub(crate) async fn spawn(
        command: &str,
        args: &[String],
        environment: &BTreeMap<String, String>,
        cwd: Option<&Path>,
    ) -> Result<Self, String> {
        if command.trim().is_empty() || command.len() > 1_024 {
            return Err("ACP command is invalid".into());
        }
        let mut child = Command::new(command);
        child
            .args(args)
            .envs(environment)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        if let Some(cwd) = cwd {
            child.current_dir(cwd);
        }
        let mut child = child
            .spawn()
            .map_err(|error| format!("ACP agent could not start: {error}"))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "ACP agent stdin is unavailable".to_owned())?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "ACP agent stdout is unavailable".to_owned())?;
        let peer = RpcPeer::open(
            JsonlReader::with_max_message_bytes(stdout, 4 * 1024 * 1024),
            stdin,
            Some(Duration::from_secs(30)),
            16,
        )
        .map_err(|error| format!("ACP transport could not start: {error}"))?;
        Ok(Self {
            peer: Arc::new(peer),
            child: tokio::sync::Mutex::new(child),
        })
    }

    pub(crate) async fn initialize(&self) -> Result<Value, String> {
        let result = self
            .peer
            .request::<_, Value>(
                "initialize",
                &json!({
                    "protocolVersion": 2,
                    "clientInfo": {"name": "remote-agent", "version": env!("CARGO_PKG_VERSION")},
                    "clientCapabilities": {
                        "fs": {"readTextFile": false, "writeTextFile": false},
                        "terminal": false,
                        "auth": {"terminal": false}
                    }
                }),
            )
            .await
            .map_err(|error| format!("ACP initialize failed: {error}"))?;
        self.peer
            .send_raw(r#"{"jsonrpc":"2.0","method":"initialized","params":{}}"#)
            .await
            .map_err(|error| format!("ACP initialized notification failed: {error}"))?;
        Ok(result.value)
    }

    pub(crate) async fn request(&self, method: &str, params: &Value) -> Result<Value, String> {
        self.peer
            .request::<_, Value>(method, params)
            .await
            .map(|reply| reply.value)
            .map_err(|error| format!("ACP request {method} failed: {error}"))
    }

    pub(crate) async fn new_session(&self, cwd: &Path) -> Result<Value, String> {
        self.request(
            "session/new",
            &json!({
                "cwd": cwd.to_string_lossy(),
                "mcpServers": []
            }),
        )
        .await
    }

    pub(crate) async fn shutdown(&self) {
        let _ = self.peer.close().await;
        let mut child = self.child.lock().await;
        let _ = child.kill().await;
        let _ = child.wait().await;
    }
}

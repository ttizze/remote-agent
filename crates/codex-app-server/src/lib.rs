mod executable;
mod platform;

use std::{env, io, path::PathBuf, process::Stdio, time::Duration};

use agent_transport::peer::{PeerError, PeerEvent, RpcMessage, RpcPeer, RpcResponse};
use serde::{Deserialize, Serialize};
use tokio::{process::Child, sync::broadcast};

#[derive(Debug, Clone)]
pub struct AppServerConfig {
    pub program: PathBuf,
    pub codex_home: Option<PathBuf>,
    pub config_overrides: Vec<String>,
}

impl Default for AppServerConfig {
    fn default() -> Self {
        Self {
            program: PathBuf::from(executable::DEFAULT_CODEX_PROGRAM),
            codex_home: None,
            config_overrides: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResponse {
    pub user_agent: String,
    pub codex_home: PathBuf,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("failed to resolve Codex executable {program}: {source}")]
    ResolveExecutable {
        program: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("Codex executable was not found on PATH: {0}")]
    ExecutableNotFound(PathBuf),
    #[error("failed to start Codex App Server: {0}")]
    Spawn(#[source] io::Error),
    #[error("Codex App Server did not expose {0}")]
    MissingPipe(&'static str),
    #[error("Codex App Server I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error(transparent)]
    Peer(#[from] PeerError),
    #[error("Codex App Server lifecycle method {method} is managed by spawn")]
    LifecycleManaged { method: String },
}

/// The Codex executable `spawn` would run for `program`.
pub fn resolve_executable(program: &std::path::Path) -> Result<PathBuf, Error> {
    executable::resolve(program)
}

/// A ready, initialized Codex App Server process.
pub struct CodexAppServer {
    child: tokio::sync::Mutex<Child>,
    peer: RpcPeer,
    initialize_response: InitializeResponse,
}

impl CodexAppServer {
    pub async fn spawn(config: AppServerConfig) -> Result<Self, Error> {
        let executable = executable::resolve(&config.program)?;
        let mut command = bex_process::command(&executable).map_err(Error::Spawn)?;
        if let Some(home) = &config.codex_home {
            command.env("CODEX_HOME", home);
        }
        for value in &config.config_overrides {
            command.arg("-c").arg(value);
        }
        let mut child = command
            .arg("app-server")
            .arg("--listen")
            .arg("stdio://")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(Error::Spawn)?;

        let stdin = child.stdin.take().ok_or(Error::MissingPipe("stdin"))?;
        let stdout = child.stdout.take().ok_or(Error::MissingPipe("stdout"))?;
        let peer = RpcPeer::open(
            agent_transport::peer::JsonlReader::new(stdout),
            stdin,
            Some(Duration::from_secs(30)),
            1024,
        )?;
        let initialize_response = peer
            .request(
                "initialize",
                &InitializeParams {
                    client_info: ClientInfo {
                        name: "remote_agent_host",
                        title: "Remote Agent Host",
                        version: env!("CARGO_PKG_VERSION"),
                    },
                    capabilities: Capabilities {
                        experimental_api: true,
                    },
                },
            )
            .await?
            .value;
        peer.send_raw(r#"{"method":"initialized","params":{}}"#)
            .await?;

        Ok(Self {
            child: tokio::sync::Mutex::new(child),
            peer,
            initialize_response,
        })
    }

    pub fn initialize_response(&self) -> &InitializeResponse {
        &self.initialize_response
    }

    /// Ordered Codex messages, response markers, and connection termination.
    pub fn subscribe(&self) -> broadcast::Receiver<PeerEvent> {
        self.peer.subscribe()
    }

    /// Sends one raw JSON-RPC request to Codex. The request's original id is
    /// restored on the raw response returned to the caller.
    pub async fn request_raw(&self, line: &str) -> Result<String, Error> {
        ensure_public_line(line)?;
        Ok(self.peer.request_raw(line).await?.value)
    }

    /// Shared peer correlation and typed payloads.
    pub async fn request<P: Serialize, T: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        params: &P,
    ) -> Result<RpcResponse<T>, Error> {
        Ok(self.request_sequenced(method, params).await?.value)
    }

    pub async fn request_sequenced<P: Serialize, T: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        params: &P,
    ) -> Result<agent_transport::peer::Reply<RpcResponse<T>>, Error> {
        ensure_public_method(method)?;
        Ok(self.peer.request_envelope(method, params).await?)
    }

    /// Sends a raw Codex notification or response exactly as supplied after
    /// validating its JSON-RPC envelope. Raw requests must use
    /// [`Self::request_raw`] so their ids can be correlated.
    pub async fn send_raw(&self, line: &str) -> Result<(), Error> {
        ensure_public_line(line)?;
        self.peer.send_raw(line).await.map_err(Into::into)
    }

    /// Stop this backend without consuming other Host services that share it.
    pub async fn shutdown(&self) -> Result<(), Error> {
        let mut child = self.child.lock().await;
        self.peer.close().await?;
        child.wait().await?;
        Ok(())
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct InitializeParams {
    client_info: ClientInfo,
    capabilities: Capabilities,
}
#[derive(Serialize)]
struct ClientInfo {
    name: &'static str,
    title: &'static str,
    version: &'static str,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Capabilities {
    experimental_api: bool,
}

fn ensure_public_method(method: &str) -> Result<(), Error> {
    if matches!(method, "initialize" | "initialized") {
        return Err(Error::LifecycleManaged {
            method: method.to_owned(),
        });
    }
    Ok(())
}

fn ensure_public_line(line: &str) -> Result<(), Error> {
    let message = RpcMessage::parse(line)
        .map_err(|error| Error::Peer(PeerError::InvalidMessage(error.to_string())))?;
    if let Some(method) = message.method() {
        ensure_public_method(method)?;
    }
    Ok(())
}

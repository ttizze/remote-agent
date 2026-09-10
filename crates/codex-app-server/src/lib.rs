mod executable;
mod platform;
mod schema;

use std::{collections::HashSet, env, io, path::PathBuf, process::Stdio, time::Duration};

use agent_core::peer::{
    PeerError, PeerEvent, RpcPeer, RpcResponse, classify_message, request_line,
};
use serde::{Deserialize, Serialize};
use tokio::{
    process::{Child, Command},
    sync::broadcast,
};

const MAX_SCHEMA_BYTES: u64 = schema::MAX_SCHEMA_BYTES;

#[derive(Debug, Clone)]
pub struct AppServerConfig {
    pub program: PathBuf,
    pub client: ClientInfo,
    pub request_timeout: Duration,
    pub codex_home: Option<PathBuf>,
    pub config_overrides: Vec<String>,
}

impl Default for AppServerConfig {
    fn default() -> Self {
        Self {
            program: PathBuf::from(executable::DEFAULT_CODEX_PROGRAM),
            client: ClientInfo {
                name: "remote_agent_host".to_owned(),
                title: "Remote Agent Host".to_owned(),
                version: env!("CARGO_PKG_VERSION").to_owned(),
            },
            request_timeout: Duration::from_secs(30),
            codex_home: None,
            config_overrides: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientInfo {
    pub name: String,
    pub title: String,
    pub version: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResponse {
    pub user_agent: String,
    pub platform_family: String,
    pub platform_os: String,
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
    #[error("failed to create private schema directory: {0}")]
    CreateSchemaDirectory(#[source] io::Error),
    #[error("failed to run Codex schema generator: {0}")]
    SchemaGenerator(#[source] io::Error),
    #[error("Codex schema generator exited unsuccessfully: {0}")]
    SchemaGeneratorFailed(std::process::ExitStatus),
    #[error("failed to inspect generated Codex schema: {0}")]
    ReadSchema(#[source] io::Error),
    #[error("generated Codex ClientRequest schema exceeds {MAX_SCHEMA_BYTES} bytes")]
    SchemaTooLarge,
    #[error("generated Codex ClientRequest schema is invalid JSON: {0}")]
    InvalidSchema(#[source] serde_json::Error),
    #[error("installed Codex schema is missing required method {0}")]
    MissingRequiredMethod(&'static str),
    #[error("failed to start Codex App Server: {0}")]
    Spawn(#[source] io::Error),
    #[error("Codex App Server did not expose {0}")]
    MissingPipe(&'static str),
    #[error("Codex App Server I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("invalid Codex App Server JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Peer(#[from] PeerError),
    #[error("Codex App Server lifecycle method {method} is managed by spawn")]
    LifecycleManaged { method: String },
    #[error("Codex App Server response to {method} had an unexpected shape: {reason}")]
    UnexpectedResponse { method: String, reason: String },
}

/// A ready, initialized Codex App Server process.
pub struct CodexAppServer {
    child: Child,
    peer: RpcPeer,
    initialize_response: InitializeResponse,
    supported_methods: HashSet<String>,
}

impl CodexAppServer {
    pub async fn spawn(config: AppServerConfig) -> Result<Self, Error> {
        let executable = executable::resolve(&config.program)?;
        let supported_methods =
            schema::generate_and_validate(&executable, config.request_timeout).await?;
        let mut command = Command::new(&executable);
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
            .kill_on_drop(true)
            .spawn()
            .map_err(Error::Spawn)?;

        let stdin = child.stdin.take().ok_or(Error::MissingPipe("stdin"))?;
        let stdout = child.stdout.take().ok_or(Error::MissingPipe("stdout"))?;
        let peer = RpcPeer::open(
            agent_core::peer::JsonlReader::new(stdout),
            stdin,
            Some(config.request_timeout),
            1024,
        )?;
        let initialize_response = peer
            .request(
                "initialize",
                &InitializeParams {
                    client_info: &config.client,
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
            child,
            peer,
            initialize_response,
            supported_methods,
        })
    }

    pub fn initialize_response(&self) -> &InitializeResponse {
        &self.initialize_response
    }

    pub fn supported_methods(&self) -> impl Iterator<Item = &str> {
        self.supported_methods.iter().map(String::as_str)
    }

    /// Ordered Codex messages, response markers, and connection termination.
    pub fn subscribe(&self) -> broadcast::Receiver<PeerEvent> {
        self.peer.subscribe()
    }

    /// Sends one raw JSON-RPC request to Codex. The request's original id is
    /// restored on the raw response returned to the caller.
    pub async fn request_raw(&self, line: &str) -> Result<String, Error> {
        let message = classify_message(line)
            .map_err(|error| Error::Peer(PeerError::InvalidMessage(error.to_string())))?;
        if let Some(method) = message.method() {
            ensure_public_method(method)?;
        }
        Ok(self.peer.request_raw(line).await?.value)
    }

    /// Shared peer correlation and typed payloads; preserve upstream extensions.
    pub async fn request<P: Serialize, T: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        params: &P,
    ) -> Result<RpcResponse<T>, Error> {
        ensure_public_method(method)?;
        let response = self
            .peer
            .request_raw(&request_line(method, params)?)
            .await?;
        RpcResponse::parse(&response.value).map_err(|error| Error::UnexpectedResponse {
            method: method.into(),
            reason: error.to_string(),
        })
    }

    /// Sends a raw Codex notification or response exactly as supplied after
    /// validating its JSON-RPC envelope. Raw requests must use
    /// [`Self::request_raw`] so their ids can be correlated.
    pub async fn send_raw(&self, line: &str) -> Result<(), Error> {
        ensure_public_send_method(line)?;
        self.peer.send_raw(line).await.map_err(Into::into)
    }

    pub async fn shutdown(mut self) -> Result<(), Error> {
        self.child.start_kill()?;
        self.child.wait().await?;
        Ok(())
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct InitializeParams<'a> {
    client_info: &'a ClientInfo,
    capabilities: Capabilities,
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

fn ensure_public_send_method(line: &str) -> Result<(), Error> {
    let message = classify_message(line)
        .map_err(|error| Error::Peer(PeerError::InvalidMessage(error.to_string())))?;
    if let Some(method) = message.method() {
        ensure_public_method(method)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn initialize_params_advertise_the_experimental_api_capability() {
        let client = ClientInfo {
            name: "test-client".to_owned(),
            title: "Test Client".to_owned(),
            version: "1.0.0".to_owned(),
        };

        assert_eq!(
            serde_json::to_value(InitializeParams {
                client_info: &client,
                capabilities: Capabilities {
                    experimental_api: true
                }
            })
            .unwrap(),
            json!({
                "clientInfo": {
                    "name": "test-client",
                    "title": "Test Client",
                    "version": "1.0.0",
                },
                "capabilities": {
                    "experimentalApi": true,
                },
            })
        );
    }
}

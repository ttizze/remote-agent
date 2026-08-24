mod executable;
mod peer;
mod schema;
mod wire;

pub use wire::{RawResponse, RequestId, ServerEvent, ServerResponse};

use std::{collections::HashSet, env, io, path::PathBuf, process::Stdio, time::Duration};

use peer::RpcPeer;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
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
    #[error("secure random generation failed while creating schema directory")]
    SchemaRandom,
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
    #[error("Codex App Server connection closed: {0}")]
    ConnectionClosed(String),
    #[error("Codex App Server request {method} timed out")]
    RequestTimeout { method: String },
    #[error("Codex App Server lifecycle method {method} is managed by spawn")]
    LifecycleManaged { method: String },
    #[error("Codex App Server rejected {method}: {detail}")]
    Remote {
        method: String,
        detail: Box<RemoteError>,
    },
    #[error("Codex App Server response to {method} had an unexpected shape: {reason}")]
    UnexpectedResponse { method: String, reason: String },
}

#[derive(Debug)]
pub struct RemoteError {
    pub code: Value,
    pub message: String,
    pub data: Option<Value>,
    /// Error members other than the standard JSON-RPC fields.
    pub additional_fields: Map<String, Value>,
    /// Members at the top level of the upstream JSON-RPC response.
    pub response_extensions: Map<String, Value>,
}

impl std::fmt::Display for RemoteError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "({}): {}", self.code, self.message)
    }
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
        let mut child = Command::new(&executable)
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
        let peer = RpcPeer::open(stdout, stdin, config.request_timeout);
        let initialize_response = peer
            .request("initialize", initialize_params(&config.client))
            .await?;
        peer.notify("initialized", json!({})).await?;

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

    pub fn subscribe(&self) -> broadcast::Receiver<ServerEvent> {
        self.peer.subscribe()
    }

    /// Sends an arbitrary Codex request without imposing a local result schema.
    pub async fn request_json(
        &self,
        method: &str,
        params: Value,
        extensions: Map<String, Value>,
    ) -> Result<Value, Error> {
        self.request_json_with_extensions(method, params, extensions)
            .await
            .map(|response| response.result)
    }

    /// Sends an arbitrary Codex request while retaining unknown members at
    /// the top level of the upstream JSON-RPC response.
    pub async fn request_json_with_extensions(
        &self,
        method: &str,
        params: Value,
        extensions: Map<String, Value>,
    ) -> Result<RawResponse, Error> {
        ensure_public_method(method)?;
        self.peer
            .request_json_with_extensions(method, params, extensions)
            .await
    }

    pub async fn notify_json(
        &self,
        method: &str,
        params: Value,
        extensions: Map<String, Value>,
    ) -> Result<(), Error> {
        ensure_public_method(method)?;
        self.peer.notify_json(method, params, extensions).await
    }

    pub async fn respond_json(
        &self,
        id: RequestId,
        response: ServerResponse,
        extensions: Map<String, Value>,
    ) -> Result<(), Error> {
        self.peer.respond_json(id, response, extensions).await
    }

    pub async fn shutdown(mut self) -> Result<(), Error> {
        self.child.start_kill()?;
        self.child.wait().await?;
        Ok(())
    }
}

fn initialize_params(client: &ClientInfo) -> Value {
    json!({
        "clientInfo": client,
        "capabilities": {
            "experimentalApi": true,
        },
    })
}

fn ensure_public_method(method: &str) -> Result<(), Error> {
    if matches!(method, "initialize" | "initialized") {
        return Err(Error::LifecycleManaged {
            method: method.to_owned(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initialize_params_advertise_the_experimental_api_capability() {
        let client = ClientInfo {
            name: "test-client".to_owned(),
            title: "Test Client".to_owned(),
            version: "1.0.0".to_owned(),
        };

        assert_eq!(
            initialize_params(&client),
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

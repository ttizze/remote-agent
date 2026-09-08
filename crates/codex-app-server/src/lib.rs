mod executable;
mod peer;
mod schema;

use std::{collections::HashSet, env, io, path::PathBuf, process::Stdio, time::Duration};

use host_protocol::{RpcMessageKind, classify_message, raw_object};
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
    #[error("invalid Codex App Server JSONL message: {0}")]
    InvalidMessage(String),
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
        let peer = RpcPeer::open(stdout, stdin, config.request_timeout);
        let initialize_response = parse_initialize_response(
            &peer
                .request_raw(&initialize_request(&config.client))
                .await?,
        )?;
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

    /// Receives raw Codex-originated notification and request lines. The
    /// trailing JSONL delimiter is removed by the reader, but the JSON text
    /// itself is otherwise unchanged.
    pub fn subscribe(&self) -> broadcast::Receiver<String> {
        self.peer.subscribe()
    }

    /// Sends one raw JSON-RPC request to Codex. The request's original id is
    /// restored on the raw response returned to the caller.
    pub async fn request_raw(&self, line: &str) -> Result<String, Error> {
        let message =
            classify_message(line).map_err(|error| Error::InvalidMessage(error.to_string()))?;
        if let Some(method) = message.method() {
            ensure_public_method(method)?;
        }
        self.peer.request_raw(line).await
    }

    /// Sends a raw Codex notification or response exactly as supplied after
    /// validating its JSON-RPC envelope. Raw requests must use
    /// [`Self::request_raw`] so their ids can be correlated.
    pub async fn send_raw(&self, line: &str) -> Result<(), Error> {
        ensure_public_send_method(line)?;
        self.peer.send_raw(line).await
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

fn ensure_public_send_method(line: &str) -> Result<(), Error> {
    let message =
        classify_message(line).map_err(|error| Error::InvalidMessage(error.to_string()))?;
    if let Some(method) = message.method() {
        ensure_public_method(method)?;
    }
    Ok(())
}

fn initialize_request(client: &ClientInfo) -> String {
    serde_json::to_string(&json!({
        "id": 0,
        "method": "initialize",
        "params": initialize_params(client),
    }))
    .expect("initialize request contains only serializable values")
}

fn parse_initialize_response(line: &str) -> Result<InitializeResponse, Error> {
    let message = classify_message(line).map_err(|error| Error::UnexpectedResponse {
        method: "initialize".to_owned(),
        reason: error.to_string(),
    })?;
    if message.kind() != RpcMessageKind::Response {
        return Err(Error::UnexpectedResponse {
            method: "initialize".to_owned(),
            reason: "message was not a response".to_owned(),
        });
    }
    let object = raw_object(line).map_err(|error| Error::UnexpectedResponse {
        method: "initialize".to_owned(),
        reason: error.to_string(),
    })?;
    if let Some(error) = object.get("error") {
        let error: Value = serde_json::from_str(error.get())?;
        let (code, message, data, additional_fields) = remote_error_fields(&error);
        return Err(Error::Remote {
            method: "initialize".to_owned(),
            detail: Box::new(RemoteError {
                code,
                message,
                data,
                additional_fields,
                response_extensions: Map::new(),
            }),
        });
    }
    let Some(result) = object.get("result") else {
        return Err(Error::UnexpectedResponse {
            method: "initialize".to_owned(),
            reason: "response had neither result nor error".to_owned(),
        });
    };
    serde_json::from_str(result.get()).map_err(|error| Error::UnexpectedResponse {
        method: "initialize".to_owned(),
        reason: error.to_string(),
    })
}

fn remote_error_fields(error: &Value) -> (Value, String, Option<Value>, Map<String, Value>) {
    let Some(object) = error.as_object() else {
        return (
            json!(-1),
            "unknown App Server error".to_owned(),
            None,
            Map::from_iter([(String::from("raw"), error.clone())]),
        );
    };

    let code = object.get("code").cloned().unwrap_or_else(|| json!(-1));
    let message = object
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("unknown App Server error")
        .to_owned();
    let data = object.get("data").cloned();
    let additional_fields = object
        .iter()
        .filter(|(key, _)| !matches!(key.as_str(), "code" | "message" | "data"))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    (code, message, data, additional_fields)
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

use std::{
    collections::HashSet,
    env, fs,
    future::Future,
    io,
    path::{Path, PathBuf},
    process::{ExitStatus, Stdio},
    time::Duration,
};

use ring::rand::{SecureRandom, SystemRandom};
use serde_json::Value;
use tokio::process::Command;
use tokio::time::timeout;

use crate::Error;

pub(crate) const MAX_SCHEMA_BYTES: u64 = 4 * 1024 * 1024;
const REQUIRED_BASELINE_METHODS: &[&str] =
    &["initialize", "thread/list", "thread/start", "thread/read"];

pub(crate) async fn generate_and_validate(
    executable: &Path,
    request_timeout: Duration,
) -> Result<HashSet<String>, Error> {
    let directory = PrivateSchemaDirectory::create()?;
    let mut command = Command::new(executable);
    command
        .arg("app-server")
        .arg("generate-json-schema")
        .arg("--out")
        .arg(directory.path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .kill_on_drop(true);
    let status = status_with_deadline(command.status(), request_timeout)
        .await
        .map_err(Error::SchemaGenerator)?;
    if !status.success() {
        return Err(Error::SchemaGeneratorFailed(status));
    }

    let schema_path = directory.path().join("ClientRequest.json");
    let metadata = fs::metadata(&schema_path).map_err(Error::ReadSchema)?;
    if metadata.len() > MAX_SCHEMA_BYTES {
        return Err(Error::SchemaTooLarge);
    }
    let bytes = fs::read(&schema_path).map_err(Error::ReadSchema)?;
    let schema: Value = serde_json::from_slice(&bytes).map_err(Error::InvalidSchema)?;
    let supported_methods = collect_request_methods(&schema);
    for required in REQUIRED_BASELINE_METHODS {
        if !supported_methods.contains(*required) {
            return Err(Error::MissingRequiredMethod(required));
        }
    }
    Ok(supported_methods)
}

async fn status_with_deadline<F>(status: F, deadline: Duration) -> io::Result<ExitStatus>
where
    F: Future<Output = io::Result<ExitStatus>>,
{
    match timeout(deadline, status).await {
        Ok(result) => result,
        Err(_) => Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "Codex schema generator timed out",
        )),
    }
}

fn collect_request_methods(schema: &Value) -> HashSet<String> {
    fn visit(value: &Value, methods: &mut HashSet<String>) {
        match value {
            Value::Object(object) => {
                if let Some(method_values) = object
                    .get("properties")
                    .and_then(Value::as_object)
                    .and_then(|properties| properties.get("method"))
                    .and_then(Value::as_object)
                    .and_then(|method| method.get("enum"))
                    .and_then(Value::as_array)
                {
                    methods.extend(
                        method_values
                            .iter()
                            .filter_map(Value::as_str)
                            .map(str::to_owned),
                    );
                }
                for child in object.values() {
                    visit(child, methods);
                }
            }
            Value::Array(array) => {
                for child in array {
                    visit(child, methods);
                }
            }
            _ => {}
        }
    }

    let mut methods = HashSet::new();
    if let Some(requests) = schema.get("oneOf").and_then(Value::as_array) {
        for request in requests {
            visit(request, &mut methods);
        }
    }
    methods
}

struct PrivateSchemaDirectory {
    path: PathBuf,
}

impl PrivateSchemaDirectory {
    fn create() -> Result<Self, Error> {
        for _ in 0..16 {
            let mut random = [0_u8; 16];
            SystemRandom::new()
                .fill(&mut random)
                .map_err(|_| Error::SchemaRandom)?;
            let suffix = random
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>();
            let path = env::temp_dir().join(format!("remote-agent-codex-schema-{suffix}"));
            let mut builder = fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            match builder.create(&path) {
                Ok(()) => return Ok(Self { path }),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(Error::CreateSchemaDirectory(error)),
            }
        }
        Err(Error::CreateSchemaDirectory(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not allocate a unique schema directory",
        )))
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for PrivateSchemaDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn pending_schema_generator_status_maps_deadline_to_timeout_error() {
        let pending = std::future::pending::<io::Result<ExitStatus>>();

        let error = status_with_deadline(pending, Duration::ZERO)
            .await
            .expect_err("pending status should hit the deadline");

        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    }

    #[test]
    fn extracts_only_request_methods_from_generated_schema_shape() {
        let schema = json!({
            "definitions": {
                "misleading": {
                    "properties": { "method": { "enum": ["not/a/request"] } }
                }
            },
            "oneOf": [
                {
                    "properties": {
                        "method": { "enum": ["initialize"] },
                        "params": { "type": "object" }
                    }
                },
                {
                    "properties": {
                        "method": { "enum": ["thread/list"] }
                    }
                }
            ]
        });

        let methods = collect_request_methods(&schema);
        assert_eq!(methods.len(), 2);
        assert!(methods.contains("initialize"));
        assert!(methods.contains("thread/list"));
        assert!(!methods.contains("not/a/request"));
    }
}

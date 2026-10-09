use std::{path::Path, process::Stdio, time::Duration};

use agent_transport::peer::{JsonlReader, JsonlWriter};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::{
    io::AsyncReadExt,
    process::{Child, ChildStdin, ChildStdout},
};

pub(super) fn executable(
    program: &Path,
    path: Option<&std::ffi::OsStr>,
) -> Result<std::path::PathBuf, String> {
    #[cfg(windows)]
    let program = if program.extension().is_none() {
        std::borrow::Cow::Owned(program.with_extension("exe"))
    } else {
        std::borrow::Cow::Borrowed(program)
    };
    let found = if program.components().count() > 1 {
        program.is_file().then(|| program.to_path_buf())
    } else {
        path.and_then(|path| {
            std::env::split_paths(path)
                .map(|directory| directory.join(program.as_os_str()))
                .find(|candidate| candidate.is_file())
        })
    }
    .ok_or_else(|| format!("executable is missing: {}", program.display()))?;
    std::path::absolute(found).map_err(|error| error.to_string())
}

pub(super) fn runtime() -> Result<(std::path::PathBuf, std::path::PathBuf), String> {
    let path = std::env::var_os("PATH");
    let node = std::env::var_os("BEX_NODE");
    let host = std::env::current_exe().map_err(|error| error.to_string())?;
    runtime_from(&host, node.as_deref(), path.as_deref())
}

fn runtime_from(
    host: &Path,
    node: Option<&std::ffi::OsStr>,
    path: Option<&std::ffi::OsStr>,
) -> Result<(std::path::PathBuf, std::path::PathBuf), String> {
    let companion = bex_process::companion_path(host, "bex-claude-sdk.mjs")
        .map_err(|error| error.to_string())?;
    let directory = companion.parent().expect("companion has a parent");
    // macOS treats Contents/MacOS as nested code. JavaScript belongs in the
    // sealed Resources directory, rather than alongside its Mach-O executables.
    let (default_node, bridge) = if directory.ends_with("Contents/MacOS") {
        (
            directory.join(format!("node{}", std::env::consts::EXE_SUFFIX)),
            directory
                .parent()
                .expect("Contents exists")
                .join("Resources/bex-claude-sdk.mjs"),
        )
    } else {
        (std::path::PathBuf::from("node"), companion)
    };
    let node = executable(node.map(Path::new).unwrap_or(&default_node), path)?;
    if !bridge.is_file() {
        return Err("bex-claude-sdk.mjs must be installed with the Host".into());
    }
    Ok((node, bridge))
}

pub(super) struct Process {
    capacity: Option<tokio::sync::OwnedSemaphorePermit>,
    child: Child,
    input: JsonlWriter<ChildStdin>,
    output: JsonlReader<ChildStdout>,
    stderr: tokio::task::JoinHandle<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mac_sdk_uses_bundled_node_without_path_and_respects_explicit_overrides() {
        let root = tempfile::tempdir().unwrap();
        let executables = root.path().join("Bex Dev.app/Contents/MacOS");
        let resources = root.path().join("Bex Dev.app/Contents/Resources");
        std::fs::create_dir_all(&executables).unwrap();
        std::fs::create_dir_all(&resources).unwrap();
        let host = executables.join("host-daemon");
        let bundled = executables.join(format!("node{}", std::env::consts::EXE_SUFFIX));
        let bridge = resources.join("bex-claude-sdk.mjs");
        std::fs::write(&bundled, b"bundled runtime").unwrap();
        std::fs::write(&bridge, b"SDK bridge").unwrap();
        assert_eq!(
            runtime_from(&host, None, None).unwrap(),
            (bundled.clone(), bridge.clone())
        );

        let path_directory = root.path().join("installed");
        std::fs::create_dir(&path_directory).unwrap();
        let installed = path_directory.join(format!("node{}", std::env::consts::EXE_SUFFIX));
        std::fs::write(&installed, b"installed runtime").unwrap();
        let path = std::env::join_paths([&path_directory]).unwrap();
        assert_eq!(runtime_from(&host, None, Some(&path)).unwrap().0, bundled);
        assert_eq!(
            runtime_from(&host, Some(installed.as_os_str()), None)
                .unwrap()
                .0,
            installed
        );
        let missing = path_directory.join("missing-node");
        assert!(runtime_from(&host, Some(missing.as_os_str()), Some(&path)).is_err());
        std::fs::remove_file(&bundled).unwrap();
        assert!(runtime_from(&host, None, Some(&path)).is_err());

        let standalone = path_directory.join("host-daemon");
        let standalone_bridge = path_directory.join("bex-claude-sdk.mjs");
        std::fs::write(&standalone_bridge, b"SDK bridge").unwrap();
        for host in [&standalone, &path_directory.join("deps/claude-test")] {
            assert_eq!(
                runtime_from(host, None, Some(&path)).unwrap(),
                (installed.clone(), standalone_bridge.clone())
            );
        }
        assert!(runtime_from(&standalone, None, None).is_err());
        std::fs::remove_file(path_directory.join("bex-claude-sdk.mjs")).unwrap();
        assert!(
            runtime_from(&standalone, None, Some(&path))
                .unwrap_err()
                .contains("bex-claude-sdk.mjs")
        );
    }
}

#[derive(Debug, Serialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub(crate) enum Input {
    Usage,
    Message {
        id: String,
        content: Value,
    },
    Answer {
        request_id: String,
        response: Option<Value>,
        error: Option<String>,
    },
    Interrupt {
        request_id: String,
    },
}

#[derive(Debug, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub(super) enum Event {
    Usage {
        usage: Option<Value>,
        error: Option<String>,
    },
    Ready {
        initialized: Value,
    },
    Message {
        message: Value,
    },
    Request {
        request_id: String,
        request: SdkRequest,
    },
    RequestCancelled {
        request_id: String,
    },
    Interrupted {
        request_id: String,
        error: Option<String>,
    },
    Error {
        message: String,
    },
}

#[derive(Debug, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub(crate) enum SdkRequest {
    Tool {
        tool_name: String,
        input: Value,
        tool_use_id: Option<String>,
    },
    Elicitation {
        server_name: String,
        message: String,
        mode: Option<String>,
        url: Option<String>,
        requested_schema: Option<Value>,
    },
}

impl Process {
    pub(super) async fn start(
        program: &Path,
        config_home: &Path,
        credentials_home: &Path,
        cwd: &Path,
        session: Option<(&str, bool)>,
        model: Option<(&str, Option<&str>)>,
        browser: Option<Value>,
    ) -> Result<(Self, Value), String> {
        let path = std::env::var_os("PATH");
        let program = executable(program, path.as_deref())?;
        let (node, bridge) = runtime()?;
        let mut command = bex_process::command(&node).map_err(|error| error.to_string())?;
        // Account changes must not replace skills, settings, plugins or history.
        command
            .env("CLAUDE_CONFIG_DIR", config_home)
            .env("CLAUDE_SECURESTORAGE_CONFIG_DIR", credentials_home)
            .env("CLAUDE_CODE_SDK_READS_SESSION_STATE", "1")
            .env_remove("ANTHROPIC_API_KEY")
            .env_remove("ANTHROPIC_AUTH_TOKEN")
            .env_remove("CLAUDE_CODE_OAUTH_TOKEN")
            .env_remove("CLAUDE_CODE_OAUTH_REFRESH_TOKEN")
            .current_dir(cwd)
            .arg(bridge);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().map_err(|error| {
            format!("Claude SDKを起動できません（{}）: {error}", node.display())
        })?;
        let mut stderr = child.stderr.take().ok_or("Claude SDK stderr is missing")?;
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
            capacity: None,
            input: JsonlWriter::new(child.stdin.take().ok_or("Claude SDK stdin is missing")?),
            output: JsonlReader::new(child.stdout.take().ok_or("Claude SDK stdout is missing")?),
            child,
            stderr,
        };
        let initialized: Result<Value, String> = tokio::time::timeout(Duration::from_secs(30), async {
            process.input.write_line(&json!({"type":"initialize", "program":program,"cwd":cwd,
                "sessionId":session.map(|(id,_)|id),"resume":session.is_some_and(|(_,resume)|resume),
                "model":model.map(|(model,_)|model),"effort":model.and_then(|(_,effort)|effort),"browser":browser}).to_string())
                .await.map_err(|error|error.to_string())?;
            match process.read().await?.ok_or("Claude SDK exited before initialization")? {
                Event::Ready { initialized } => Ok(initialized),
                Event::Error { message } => Err(message),
                _ => Err("Claude SDK emitted an event before initialization".into()),
            }
        }).await.map_err(|_| "Claude SDKの初期化がタイムアウトしました。".to_owned())?;
        match initialized {
            Ok(result) => Ok((process, result)),
            Err(error) => {
                let detail = process.finish().await.err().unwrap_or_default();
                Err(format!("{error}. {detail}"))
            }
        }
    }

    pub(super) fn retain_capacity(&mut self, permit: tokio::sync::OwnedSemaphorePermit) {
        self.capacity = Some(permit);
    }

    pub(super) async fn write(&mut self, value: &Input) -> Result<(), String> {
        self.input
            .write_line(&serde_json::to_string(value).map_err(|error| error.to_string())?)
            .await
            .map_err(|error| error.to_string())
    }

    pub(super) async fn read(&mut self) -> Result<Option<Event>, String> {
        self.output
            .read_line()
            .await
            .map_err(|error| error.to_string())?
            .map(|line| {
                serde_json::from_str(&line)
                    .map_err(|error| format!("invalid Claude SDK event: {error}"))
            })
            .transpose()
    }

    pub(super) async fn finish(self) -> Result<(), String> {
        let Self {
            capacity: _capacity,
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
                let _ = child.wait().await;
                stderr.abort();
                return Err("Claude SDK did not exit after closing its input".into());
            }
        };
        if !status.success() {
            return Err(format!(
                "Claude SDK exited with {status}: {}",
                stderr_text.trim()
            ));
        }
        Ok(())
    }
}

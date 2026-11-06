use std::{path::Path, process::Stdio, time::Duration};

use agent_transport::peer::{JsonlReader, JsonlWriter};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::{
    io::AsyncReadExt,
    process::{Child, ChildStdin, ChildStdout, Command},
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
                .flat_map(|directory| {
                    let direct = directory.join(program.as_os_str());
                    // npm's Windows shim launches this native binary. Resolve
                    // it directly so the SDK never needs a command shell.
                    let npm = (cfg!(windows) && program.as_os_str() == "claude.exe").then(|| {
                        directory.join("node_modules/@anthropic-ai/claude-code/bin/claude.exe")
                    });
                    std::iter::once(direct).chain(npm)
                })
                .find(|candidate| candidate.is_file())
        })
    }
    .ok_or_else(|| format!("executable is missing: {}", program.display()))?;
    std::path::absolute(found).map_err(|error| error.to_string())
}

pub(super) fn runtime() -> Result<(std::path::PathBuf, std::path::PathBuf), String> {
    let path = std::env::var_os("PATH");
    let node = std::env::var_os("AGENT_NODE");
    let directory = agent_process::runtime_directory().map_err(|error| error.to_string())?;
    runtime_from(&directory, node.as_deref(), path.as_deref())
}

fn runtime_from(
    directory: &Path,
    node: Option<&std::ffi::OsStr>,
    path: Option<&std::ffi::OsStr>,
) -> Result<(std::path::PathBuf, std::path::PathBuf), String> {
    // macOS treats Contents/MacOS as nested code. JavaScript belongs in the
    // sealed Resources directory, rather than alongside its Mach-O executables.
    let (default_node, bridge) = if directory.ends_with("Contents/MacOS") {
        (
            agent_local_host::bundled_node_path(directory).expect("Mac bundle has a runtime path"),
            directory
                .parent()
                .expect("Contents exists")
                .join("Resources/agent-claude-sdk.mjs"),
        )
    } else {
        (
            std::path::PathBuf::from("node"),
            directory.join("agent-claude-sdk.mjs"),
        )
    };
    let node = executable(node.map(Path::new).unwrap_or(&default_node), path)?;
    if !bridge.is_file() {
        return Err("agent-claude-sdk.mjs must be installed with the Host".into());
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

pub(super) struct Probe {
    pub(super) initialized: Value,
    pub(super) usage: Result<Value, String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn windows_npm_claude_is_resolved_without_running_a_shim() {
        let root = tempfile::tempdir().unwrap();
        let binary = root
            .path()
            .join("node_modules/@anthropic-ai/claude-code/bin/claude.exe");
        std::fs::create_dir_all(binary.parent().unwrap()).unwrap();
        std::fs::write(&binary, b"native CLI").unwrap();
        let path = std::env::join_paths([root.path()]).unwrap();
        assert_eq!(
            executable(Path::new("claude"), Some(&path)).unwrap(),
            binary
        );
        assert!(executable(Path::new("node"), Some(&path)).is_err());
        let explicit = root.path().join("missing/claude.exe");
        assert!(executable(&explicit, Some(&path)).is_err());
        let direct = root.path().join("claude.exe");
        std::fs::write(&direct, b"native install").unwrap();
        assert_eq!(
            executable(Path::new("claude"), Some(&path)).unwrap(),
            direct
        );
    }

    fn home_variables(homes: Option<(&Path, &Path)>) -> Vec<(String, Option<String>)> {
        let mut command = Command::new("claude");
        set_homes(&mut command, homes);
        command
            .as_std()
            .get_envs()
            .map(|(key, value)| {
                (
                    key.to_string_lossy().into_owned(),
                    value.map(|value| value.to_string_lossy().into_owned()),
                )
            })
            .collect()
    }

    #[test]
    fn inherited_native_home_keeps_the_users_own_claude_login() {
        let native = Path::new("/home/user/.claude");
        let account = Path::new("/state/claude/accounts/4c5b");
        assert_eq!(homes(native, true, native), None);
        assert!(home_variables(homes(native, true, native)).is_empty());

        // Added accounts still share skills and history but keep their credentials.
        assert_eq!(homes(native, true, account), Some((native, account)));
        assert_eq!(
            home_variables(homes(native, true, account)),
            [
                (
                    "CLAUDE_CONFIG_DIR".into(),
                    Some(native.display().to_string())
                ),
                (
                    "CLAUDE_SECURESTORAGE_CONFIG_DIR".into(),
                    Some(account.display().to_string())
                ),
            ]
        );

        // An explicitly chosen native home is not Claude Code's default.
        assert_eq!(homes(native, false, native), Some((native, native)));
    }

    #[test]
    fn mac_sdk_uses_bundled_node_without_path_and_respects_explicit_overrides() {
        let root = tempfile::tempdir().unwrap();
        let executables = root.path().join("Bex Dev.app/Contents/MacOS");
        let resources = root.path().join("Bex Dev.app/Contents/Resources");
        std::fs::create_dir_all(&executables).unwrap();
        std::fs::create_dir_all(&resources).unwrap();
        let bundled = root
            .path()
            .join("Bex Dev.app/Contents/Helpers/Bex Node.app/Contents/MacOS")
            .join(format!("node{}", std::env::consts::EXE_SUFFIX));
        std::fs::create_dir_all(bundled.parent().unwrap()).unwrap();
        let bridge = resources.join("agent-claude-sdk.mjs");
        std::fs::write(&bundled, b"bundled runtime").unwrap();
        std::fs::write(&bridge, b"SDK bridge").unwrap();
        assert_eq!(
            runtime_from(&executables, None, None).unwrap(),
            (bundled.clone(), bridge.clone())
        );

        let path_directory = root.path().join("installed");
        std::fs::create_dir(&path_directory).unwrap();
        let installed = path_directory.join(format!("node{}", std::env::consts::EXE_SUFFIX));
        std::fs::write(&installed, b"installed runtime").unwrap();
        let path = std::env::join_paths([&path_directory]).unwrap();
        assert_eq!(
            runtime_from(&executables, None, Some(&path)).unwrap().0,
            bundled
        );
        assert_eq!(
            runtime_from(&executables, Some(installed.as_os_str()), None)
                .unwrap()
                .0,
            installed
        );
        let missing = path_directory.join("missing-node");
        assert!(runtime_from(&executables, Some(missing.as_os_str()), Some(&path)).is_err());
        std::fs::remove_file(&bundled).unwrap();
        assert!(runtime_from(&executables, None, Some(&path)).is_err());

        let standalone_bridge = path_directory.join("agent-claude-sdk.mjs");
        std::fs::write(&standalone_bridge, b"SDK bridge").unwrap();
        assert_eq!(
            runtime_from(&path_directory, None, Some(&path)).unwrap(),
            (installed.clone(), standalone_bridge.clone())
        );
        assert!(runtime_from(&path_directory, None, None).is_err());
        std::fs::remove_file(path_directory.join("agent-claude-sdk.mjs")).unwrap();
        assert!(
            runtime_from(&path_directory, None, Some(&path))
                .unwrap_err()
                .contains("agent-claude-sdk.mjs")
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
    Forked {
        session_id: String,
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

/// Configuration and credential homes for a Claude Code process, or `None` to
/// keep the Host's own environment. Claude Code keys its credentials on these
/// variables, so naming even its default directory hides the user's login.
pub(super) fn homes<'a>(
    native_home: &'a Path,
    native_inherited: bool,
    credentials_home: &'a Path,
) -> Option<(&'a Path, &'a Path)> {
    (!native_inherited || credentials_home != native_home)
        .then_some((native_home, credentials_home))
}

pub(super) fn set_homes(command: &mut Command, homes: Option<(&Path, &Path)>) {
    if let Some((config_home, credentials_home)) = homes {
        command
            .env("CLAUDE_CONFIG_DIR", config_home)
            .env("CLAUDE_SECURESTORAGE_CONFIG_DIR", credentials_home);
    }
}

impl Process {
    pub(super) async fn probe(
        program: &Path,
        homes: Option<(&Path, &Path)>,
        cwd: &Path,
    ) -> Result<Probe, String> {
        let (mut process, initialized) = Self::start(program, homes, cwd, None, None, None).await?;
        // A short-lived probe must let an in-flight OAuth refresh finish before
        // closing the CLI. Usage failures do not discard initialization metadata.
        let usage = process.usage().await;
        process.finish().await?;
        Ok(Probe { initialized, usage })
    }

    pub(super) async fn start(
        program: &Path,
        homes: Option<(&Path, &Path)>,
        cwd: &Path,
        session: Option<(&str, bool)>,
        model: Option<(&str, Option<&str>)>,
        browser: Option<Value>,
    ) -> Result<(Self, Value), String> {
        let path = std::env::var_os("PATH");
        let program = executable(program, path.as_deref())?;
        let mut process = Self::spawn(homes, cwd)?;
        let initialized: Result<Value, String> = tokio::time::timeout(Duration::from_secs(30), async {
            process.input.write_line(&json!({"type":"initialize", "program":program,"cwd":cwd,
                "sessionId":session.map(|(id,_)|id),"resume":session.is_some_and(|(_,resume)|resume),
                "model":model.map(|(model,_)|model),"effort":model.and_then(|(_,effort)|effort),"browser":browser,
                "probe":session.is_none()}).to_string())
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

    /// Copy a native session through its last message `up_to` into a new one.
    pub(super) async fn fork(
        homes: Option<(&Path, &Path)>,
        cwd: &Path,
        session: &str,
        up_to: &str,
    ) -> Result<String, String> {
        let mut process = Self::spawn(homes, cwd)?;
        let forked = tokio::time::timeout(Duration::from_secs(30), async {
            process
                .input
                .write_line(
                    &json!({"type":"fork","sessionId":session,"cwd":cwd,"upToMessageId":up_to})
                        .to_string(),
                )
                .await
                .map_err(|error| error.to_string())?;
            match process
                .read()
                .await?
                .ok_or("Claude SDK exited before forking")?
            {
                Event::Forked { session_id } => Ok(session_id),
                Event::Error { message } => Err(message),
                _ => Err("Claude SDK emitted an unexpected fork event".into()),
            }
        })
        .await
        .map_err(|_| "Claude の会話の分岐がタイムアウトしました。".to_owned())?;
        let finished = process.finish().await;
        let forked = forked?;
        finished?;
        Ok(forked)
    }

    fn spawn(homes: Option<(&Path, &Path)>, cwd: &Path) -> Result<Self, String> {
        let (node, bridge) = runtime()?;
        let mut command = agent_process::command(&node).map_err(|error| error.to_string())?;
        // Account changes must not replace skills, settings, plugins or history.
        set_homes(&mut command, homes);
        command
            .env("CLAUDE_CODE_SDK_READS_SESSION_STATE", "1")
            .env_remove("ANTHROPIC_API_KEY")
            .env_remove("ANTHROPIC_AUTH_TOKEN")
            .env_remove("CLAUDE_CODE_OAUTH_TOKEN")
            .env_remove("CLAUDE_CODE_OAUTH_REFRESH_TOKEN")
            .current_dir(cwd)
            .arg(bridge);
        Self::spawn_command(command)
            .map_err(|error| format!("Claude SDKを起動できません（{}）: {error}", node.display()))
    }

    pub(super) fn spawn_command(mut command: Command) -> Result<Self, String> {
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().map_err(|error| error.to_string())?;
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
        Ok(Self {
            capacity: None,
            input: JsonlWriter::new(child.stdin.take().ok_or("Claude SDK stdin is missing")?),
            output: JsonlReader::new(child.stdout.take().ok_or("Claude SDK stdout is missing")?),
            child,
            stderr,
        })
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

    async fn usage(&mut self) -> Result<Value, String> {
        tokio::time::timeout(Duration::from_secs(30), async {
            self.write(&Input::Usage).await?;
            while let Some(event) = self.read().await? {
                match event {
                    Event::Error { message } => return Err(message),
                    Event::Usage {
                        usage: Some(usage),
                        error: None,
                    } => return Ok(usage),
                    Event::Usage { error, .. } => {
                        return Err(error.unwrap_or_else(|| "Claude usage unavailable".into()));
                    }
                    _ => {}
                }
            }
            Err("Claude exited".into())
        })
        .await
        .map_err(|_| "Claude の利用状況の取得がタイムアウトしました。".to_owned())?
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

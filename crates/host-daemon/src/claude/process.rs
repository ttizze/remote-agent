use std::{path::Path, process::Stdio, time::Duration};

use agent_transport::peer::{JsonlReader, JsonlWriter};
use serde_json::{Value, json};
use tokio::{
    io::AsyncReadExt,
    process::{Child, ChildStdin, ChildStdout},
};

pub(super) struct Process {
    capacity: Option<tokio::sync::OwnedSemaphorePermit>,
    child: Child,
    input: JsonlWriter<ChildStdin>,
    output: JsonlReader<ChildStdout>,
    stderr: tokio::task::JoinHandle<String>,
}

pub(super) fn command(
    program: &Path,
    environment: &[(String, String)],
    config_home: &Path,
    credentials_home: &Path,
) -> std::io::Result<tokio::process::Command> {
    let mut command = bex_process::command(program)?;
    command
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("ANTHROPIC_AUTH_TOKEN")
        .env_remove("CLAUDE_CODE_OAUTH_TOKEN")
        .env_remove("CLAUDE_CODE_OAUTH_REFRESH_TOKEN")
        .envs(environment.iter().map(|(name, value)| (name, value)))
        .env("CLAUDE_CONFIG_DIR", config_home)
        .env("CLAUDE_SECURESTORAGE_CONFIG_DIR", credentials_home);
    Ok(command)
}

pub(super) async fn version(
    program: &Path,
    environment: &[(String, String)],
    native_home: &Path,
) -> Result<Option<semver::Version>, String> {
    let mut command = command(program, environment, native_home, native_home)
        .map_err(|error| error.to_string())?;
    command
        .arg("--version")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().map_err(|error| error.to_string())?;
    // Keep the supervisor's lifetime pipe open until the version probe ends.
    // On timeout its EOF terminates and reaps the entire owned process group.
    let lifetime = child.stdin.take();
    let output = tokio::time::timeout(Duration::from_secs(4), child.wait_with_output()).await;
    drop(lifetime);
    let output = output
        .map_err(|_| "Claude version probe timed out")?
        .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err("Claude version probe failed".into());
    }
    let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(parse_version(&text))
}

// Match T3's first three-part CLI version, independent of surrounding labels
// and prerelease text. Manifest bounds use full semver precedence separately.
fn parse_version(output: &str) -> Option<semver::Version> {
    static PATTERN: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let pattern = PATTERN.get_or_init(|| {
        regex::Regex::new(r"\bv?([0-9]+)\.([0-9]+)\.([0-9]+)\b").expect("constant version pattern")
    });
    let value = pattern.captures(output)?;
    Some(semver::Version::new(
        value[1].parse().ok()?,
        value[2].parse().ok()?,
        value[3].parse().ok()?,
    ))
}

impl Process {
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn start(
        environment: &[(String, String)],
        launch_args: &[String],
        program: &Path,
        config_home: &Path,
        credentials_home: &Path,
        cwd: &Path,
        session: Option<(&str, bool)>,
        model: Option<&super::catalog::Query>,
        browser: Option<Value>,
    ) -> Result<(Self, Value), String> {
        let launch_args =
            settings_args(launch_args, cwd, model.map(|query| &query.settings)).await?;
        let mut command = command(program, environment, config_home, credentials_home)
            .map_err(|error| error.to_string())?;
        // Account changes must not replace skills, settings, plugins or history.
        command
            .env("CLAUDE_CODE_SDK_READS_SESSION_STATE", "1")
            .current_dir(cwd)
            .args([
                "-p",
                "--input-format",
                "stream-json",
                "--output-format",
                "stream-json",
                "--verbose",
                "--include-partial-messages",
                "--replay-user-messages",
                "--permission-prompt-tool",
                "stdio",
            ]);
        let position = launch_args
            .iter()
            .position(|arg| arg == "--")
            .unwrap_or(launch_args.len());
        let (options, positional) = launch_args.split_at(position);
        command.args(options);
        if let Some(browser) = browser {
            command
                .arg("--mcp-config")
                .arg(json!({"mcpServers":{"bex_browser":browser}}).to_string());
        }
        if let Some((session, resume)) = session {
            command
                .arg(if resume { "--resume" } else { "--session-id" })
                .arg(session);
        }
        if let Some(model) = model {
            command.arg("--model").arg(&model.model);
            if let Some(effort) = &model.effort {
                command.arg("--effort").arg(effort);
            }
        }
        command.args(positional);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().map_err(|error| {
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
            capacity: None,
            input: JsonlWriter::new(child.stdin.take().ok_or("Claude Code stdin is missing")?),
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

    pub(super) fn retain_capacity(&mut self, permit: tokio::sync::OwnedSemaphorePermit) {
        self.capacity = Some(permit);
    }

    pub(super) async fn write(&mut self, value: &Value) -> Result<(), String> {
        self.input
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

// The instance's auto-compact setting and user CLI settings share one flag.
// Resolve files at the native process's cwd and retain the user's other keys.
async fn settings_args(
    args: &[String],
    cwd: &Path,
    overrides: Option<&std::collections::BTreeMap<String, bool>>,
) -> Result<Vec<String>, String> {
    let mut retained = Vec::new();
    let mut sources = Vec::new();
    let mut remaining = args.iter();
    while let Some(arg) = remaining.next() {
        if arg == "--" {
            retained.push(arg.clone());
            retained.extend(remaining.cloned());
            break;
        }
        if arg == "--settings" {
            sources.push(
                remaining
                    .next()
                    .ok_or("Claude settings flag requires a value")?
                    .clone(),
            );
        } else if let Some(value) = arg.strip_prefix("--settings=") {
            sources.push(value.to_owned());
        } else {
            retained.push(arg.clone());
        }
    }
    if sources.len() <= 1 && overrides.is_none_or(|settings| settings.is_empty()) {
        // Preserve native CLI validation and file loading for unmodified flags.
        return Ok(args.to_vec());
    }
    let mut settings = serde_json::Map::new();
    for source in sources {
        let value = match serde_json::from_str::<Value>(&source) {
            Ok(value) => value,
            Err(_) => {
                let bytes = tokio::fs::read(cwd.join(source))
                    .await
                    .map_err(|_| "Claude settings file cannot be read")?;
                serde_json::from_slice(&bytes)
                    .map_err(|_| "Claude settings file contains invalid JSON")?
            }
        };
        let Value::Object(fields) = value else {
            return Err("Claude settings must be a JSON object".into());
        };
        settings.extend(fields);
    }
    if let Some(overrides) = overrides {
        settings.extend(
            overrides
                .iter()
                .map(|(key, value)| (key.clone(), json!(value))),
        );
    }
    // Insert before a positional delimiter, if one was supplied.
    let position = retained
        .iter()
        .position(|arg| arg == "--")
        .unwrap_or(retained.len());
    retained.splice(
        position..position,
        ["--settings".into(), Value::Object(settings).to_string()],
    );
    Ok(retained)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_version_matches_t3_core_version_and_ignores_surrounding_labels() {
        for output in [
            "2.1.280 (Claude Code)",
            "claude v2.1.280-beta.1",
            "version:2.1.280+build",
            "2.1.280\n2.1.284",
            "02.01.280",
        ] {
            assert_eq!(parse_version(output), Some(semver::Version::new(2, 1, 280)));
        }
        for output in ["unknown", "2.1", "build2.1.280x", "v2.1.x"] {
            assert_eq!(parse_version(output), None);
        }
    }

    #[tokio::test]
    async fn query_settings_override_native_flags_without_losing_other_keys() {
        let root = tempfile::tempdir().unwrap();
        let args = vec![
            "--settings".into(),
            r#"{"fastMode":true,"autoCompactWindow":500000,"permissions":{"allow":["Read"]}}"#
                .into(),
            "--".into(),
            "prompt".into(),
        ];
        let overrides = std::collections::BTreeMap::from([
            ("fastMode".into(), false),
            ("alwaysThinkingEnabled".into(), true),
        ]);
        let actual = settings_args(&args, root.path(), Some(&overrides))
            .await
            .unwrap();
        assert_eq!(actual[0], "--settings");
        assert_eq!(
            serde_json::from_str::<Value>(&actual[1]).unwrap(),
            json!({
                "fastMode":false, "alwaysThinkingEnabled":true, "autoCompactWindow":500000,
                "permissions":{"allow":["Read"]}
            })
        );
        assert_eq!(&actual[2..], &["--", "prompt"]);
        let actual = settings_args(&[], root.path(), Some(&overrides))
            .await
            .unwrap();
        assert_eq!(actual.len(), 2);
        assert_eq!(
            serde_json::from_str::<Value>(&actual[1]).unwrap(),
            json!({"fastMode":false,"alwaysThinkingEnabled":true})
        );
    }

    #[test]
    fn configured_environment_cannot_replace_bound_history_or_account_storage() {
        let environment = vec![
            ("CLAUDE_CONFIG_DIR".into(), "/other-history".into()),
            (
                "CLAUDE_SECURESTORAGE_CONFIG_DIR".into(),
                "/other-account".into(),
            ),
            ("ANTHROPIC_API_KEY".into(), "placeholder".into()),
            ("EXAMPLE_VALUE".into(), "public-placeholder".into()),
        ];
        let command = command(
            Path::new("unstarted-fixture"),
            &environment,
            Path::new("/bound-history"),
            Path::new("/selected-account"),
        )
        .unwrap();
        let variables: std::collections::BTreeMap<_, _> = command.as_std().get_envs().collect();
        let value = |name: &str| variables.get(std::ffi::OsStr::new(name)).copied().flatten();
        assert_eq!(
            value("CLAUDE_CONFIG_DIR"),
            Some(std::ffi::OsStr::new("/bound-history"))
        );
        assert_eq!(
            value("CLAUDE_SECURESTORAGE_CONFIG_DIR"),
            Some(std::ffi::OsStr::new("/selected-account"))
        );
        assert_eq!(
            value("ANTHROPIC_API_KEY"),
            Some(std::ffi::OsStr::new("placeholder"))
        );
        assert_eq!(
            value("EXAMPLE_VALUE"),
            Some(std::ffi::OsStr::new("public-placeholder"))
        );
        for name in [
            "ANTHROPIC_AUTH_TOKEN",
            "CLAUDE_CODE_OAUTH_TOKEN",
            "CLAUDE_CODE_OAUTH_REFRESH_TOKEN",
        ] {
            assert_eq!(variables.get(std::ffi::OsStr::new(name)), Some(&None));
        }
    }

    #[tokio::test]
    async fn instance_settings_merge_with_inline_and_cwd_relative_files_before_positional_args() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("native.json"),
            r#"{"permissions":{"allow":["Read"]},"autoCompactWindow":100000}"#,
        )
        .unwrap();
        for source in [
            "native.json",
            r#"{"permissions":{"allow":["Read"]},"autoCompactWindow":100000}"#,
        ] {
            let args = vec![
                "--settings".into(),
                source.into(),
                "--effort".into(),
                "high".into(),
                "--settings={\"autoCompactWindow\":500000}".into(),
                "--".into(),
                "--settings".into(),
            ];
            let actual = settings_args(&args, root.path(), None).await.unwrap();
            assert_eq!(&actual[..2], &["--effort", "high"]);
            assert_eq!(actual[2], "--settings");
            assert_eq!(
                serde_json::from_str::<Value>(&actual[3]).unwrap(),
                json!({"permissions":{"allow":["Read"]},"autoCompactWindow":500000})
            );
            assert_eq!(&actual[4..], &["--", "--settings"]);
        }
        let single = vec!["--settings=native.json".into()];
        assert_eq!(
            settings_args(&single, root.path(), None).await.unwrap(),
            single
        );
        for first in ["missing.json", "[]", "false"] {
            let invalid = vec![
                "--settings".into(),
                first.into(),
                "--settings".into(),
                "{}".into(),
            ];
            assert!(settings_args(&invalid, root.path(), None).await.is_err());
        }
        assert!(
            settings_args(&["--settings".into()], root.path(), None)
                .await
                .is_err()
        );
    }
}

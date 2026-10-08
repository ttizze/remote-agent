//! Git for the long operations: a timeout, bounded output, the output lines
//! as they arrive, and the hooks a commit runs, read from Git's trace.
use agent_protocol::vcs::OutputStream;
use anyhow::{Context as _, Result, anyhow};
use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt as _, BufReader};

pub(super) const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
pub(super) const DEFAULT_MAX_OUTPUT_BYTES: usize = 1_000_000;
const TRACE_POLL: Duration = Duration::from_millis(50);

/// Variables that keep a background remote operation from waiting on a
/// prompt nobody can answer.
pub(super) const NON_INTERACTIVE_ENV: [(&str, &str); 5] = [
    ("GCM_INTERACTIVE", "never"),
    ("GIT_ASKPASS", ""),
    ("GIT_TERMINAL_PROMPT", "0"),
    ("SSH_ASKPASS", ""),
    ("SSH_ASKPASS_REQUIRE", "never"),
];

/// What a running command reports as it goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Progress {
    Output {
        stream: OutputStream,
        line: String,
    },
    HookStarted {
        hook_name: String,
    },
    HookFinished {
        hook_name: String,
        exit_code: Option<i32>,
        duration_ms: Option<u64>,
    },
}

pub(super) struct Execute<'a> {
    pub cwd: &'a Path,
    pub args: &'a [&'a str],
    pub env: &'a [(&'a str, &'a str)],
    /// `None` runs without a deadline.
    pub timeout: Option<Duration>,
    pub max_output_bytes: usize,
    /// Receives the output lines, and the hooks when `trace_hooks` is set.
    pub progress: Option<&'a mut (dyn FnMut(Progress) + Send)>,
    pub trace_hooks: bool,
}

impl<'a> Execute<'a> {
    pub(super) fn new(cwd: &'a Path, args: &'a [&'a str]) -> Self {
        Self {
            cwd,
            args,
            env: &[],
            timeout: Some(DEFAULT_TIMEOUT),
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
            progress: None,
            trace_hooks: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Executed {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
    pub stdout_truncated: bool,
}

impl Executed {
    pub(super) fn ok(&self) -> bool {
        self.code == 0
    }
}

/// One `GIT_TRACE2_EVENT` record of a hook child.
#[derive(serde::Deserialize)]
struct TraceRecord {
    event: String,
    #[serde(default)]
    child_class: Option<String>,
    #[serde(default)]
    child_id: Option<serde_json::Value>,
    #[serde(default)]
    hook_name: Option<String>,
    #[serde(default)]
    code: Option<i64>,
}

/// Follows the hook children in a trace file as it grows.
struct HookTrace {
    file: tempfile::NamedTempFile,
    processed: usize,
    remainder: String,
    started: std::collections::HashMap<String, (String, Instant)>,
}

impl HookTrace {
    fn new() -> Result<Self> {
        Ok(Self {
            file: tempfile::Builder::new()
                .prefix("git-trace2-")
                .suffix(".json")
                .tempfile()?,
            processed: 0,
            remainder: String::new(),
            started: Default::default(),
        })
    }

    fn read(&mut self, progress: &mut (dyn FnMut(Progress) + Send)) {
        let Ok(contents) = std::fs::read(self.file.path()) else {
            return;
        };
        if contents.len() <= self.processed {
            return;
        }
        let appended = String::from_utf8_lossy(&contents[self.processed..]).into_owned();
        self.processed = contents.len();
        let combined = format!("{}{appended}", self.remainder);
        let mut lines: Vec<&str> = combined.split('\n').collect();
        self.remainder = lines.pop().unwrap_or_default().to_owned();
        for line in lines {
            self.line(line.trim_end_matches('\r'), progress);
        }
    }

    fn line(&mut self, line: &str, progress: &mut (dyn FnMut(Progress) + Send)) {
        let line = line.trim();
        if line.is_empty() {
            return;
        }
        let Ok(record) = serde_json::from_str::<TraceRecord>(line) else {
            return;
        };
        if record.child_class.as_deref() != Some("hook") {
            return;
        }
        let key = match &record.child_id {
            Some(serde_json::Value::Number(number)) => number.to_string(),
            Some(serde_json::Value::String(text)) => text.clone(),
            _ => match record.hook_name.as_deref().map(str::trim) {
                Some(name) if !name.is_empty() => name.to_owned(),
                _ => return,
            },
        };
        let started = self.started.get(&key).cloned();
        let hook_name = record
            .hook_name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
            .or_else(|| started.as_ref().map(|(name, _)| name.clone()));
        let Some(hook_name) = hook_name else {
            return;
        };
        match record.event.as_str() {
            "child_start" => {
                self.started
                    .insert(key, (hook_name.clone(), Instant::now()));
                progress(Progress::HookStarted { hook_name });
            }
            "child_exit" => {
                self.started.remove(&key);
                let (hook_name, duration_ms) = match started {
                    Some((name, at)) => (name, Some(at.elapsed().as_millis() as u64)),
                    None => (hook_name, None),
                };
                progress(Progress::HookFinished {
                    hook_name,
                    exit_code: record.code.and_then(|code| i32::try_from(code).ok()),
                    duration_ms,
                });
            }
            _ => {}
        }
    }
}

async fn read_lines(
    pipe: impl tokio::io::AsyncRead + Unpin,
    stream: OutputStream,
    sender: tokio::sync::mpsc::UnboundedSender<(OutputStream, Vec<u8>)>,
) {
    let mut reader = BufReader::new(pipe);
    let mut line = Vec::new();
    loop {
        line.clear();
        match reader.read_until(b'\n', &mut line).await {
            Ok(0) | Err(_) => return,
            Ok(_) => {
                if sender.send((stream, line.clone())).is_err() {
                    return;
                }
            }
        }
    }
}

/// Runs Git in `cwd` and waits for it; the command is killed when the timeout
/// passes. Output is kept up to the limit and reported line by line.
pub(super) async fn execute(input: Execute<'_>) -> Result<Executed> {
    let Execute {
        cwd,
        args,
        env,
        timeout,
        max_output_bytes,
        mut progress,
        trace_hooks,
    } = input;
    let mut trace = if trace_hooks && progress.is_some() {
        Some(HookTrace::new()?)
    } else {
        None
    };
    let mut command = tokio::process::Command::new("git");
    command
        .arg("--no-optional-locks")
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    for (key, value) in env {
        command.env(key, value);
    }
    if let Some(trace) = &trace {
        command
            .env("GIT_TRACE2_EVENT", trace.file.path())
            .env("GIT_TRACE2_EVENT_NESTING", "5");
    }
    let mut child = command.spawn().context("failed to run git")?;
    let (sender, mut lines) = tokio::sync::mpsc::unbounded_channel();
    let stdout_task = child
        .stdout
        .take()
        .map(|pipe| tokio::spawn(read_lines(pipe, OutputStream::Stdout, sender.clone())));
    let stderr_task = child
        .stderr
        .take()
        .map(|pipe| tokio::spawn(read_lines(pipe, OutputStream::Stderr, sender)));
    let deadline = timeout.map(|timeout| tokio::time::Instant::now() + timeout);
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    let mut stdout_truncated = false;
    let mut status = None;
    let mut collect =
        |stream: OutputStream,
         line: Vec<u8>,
         progress: &mut Option<&mut (dyn FnMut(Progress) + Send)>| {
            let buffer = match stream {
                OutputStream::Stdout => &mut stdout,
                OutputStream::Stderr => &mut stderr,
            };
            if buffer.len() + line.len() <= max_output_bytes {
                buffer.extend_from_slice(&line);
            } else if stream == OutputStream::Stdout {
                stdout_truncated = true;
            }
            if let Some(progress) = progress.as_mut() {
                let text = String::from_utf8_lossy(&line).trim_end().to_owned();
                if !text.trim().is_empty() {
                    progress(Progress::Output { stream, line: text });
                }
            }
        };
    let mut ticker = tokio::time::interval(TRACE_POLL);
    // Reads until both pipes close; the exit status may arrive first.
    loop {
        let timed_out = async {
            match deadline {
                Some(deadline) => tokio::time::sleep_until(deadline).await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            biased;
            line = lines.recv() => match line {
                Some((stream, line)) => collect(stream, line, &mut progress),
                None => break,
            },
            exited = child.wait(), if status.is_none() => {
                status = Some(exited?);
            }
            _ = ticker.tick() => {
                if let (Some(trace), Some(progress)) = (trace.as_mut(), progress.as_mut()) {
                    trace.read(*progress);
                }
            }
            _ = timed_out => {
                let _ = child.kill().await;
                if let Some(task) = stdout_task { task.abort(); }
                if let Some(task) = stderr_task { task.abort(); }
                return Err(anyhow!(
                    "Git command timed out after {}s in {}.",
                    timeout.map_or(0, |t| t.as_secs()),
                    cwd.display()
                ));
            }
        }
    }
    let status = match status {
        Some(status) => status,
        None => child.wait().await?,
    };
    if let Some(task) = stdout_task {
        let _ = task.await;
    }
    if let Some(task) = stderr_task {
        let _ = task.await;
    }
    if let (Some(trace), Some(progress)) = (trace.as_mut(), progress.as_mut()) {
        trace.read(*progress);
        let open: Vec<(String, Instant)> = trace.started.drain().map(|(_, v)| v).collect();
        for (hook_name, at) in open {
            progress(Progress::HookFinished {
                hook_name,
                exit_code: None,
                duration_ms: Some(at.elapsed().as_millis() as u64),
            });
        }
    }
    Ok(Executed {
        code: status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
        stdout_truncated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn output_lines_arrive_in_order_and_the_result_keeps_them() {
        let directory = tempfile::tempdir().unwrap();
        let mut seen = vec![];
        let mut progress = |event: Progress| seen.push(event);
        let executed = execute(Execute {
            progress: Some(&mut progress),
            ..Execute::new(directory.path(), &["--version"])
        })
        .await
        .unwrap();
        assert!(executed.ok());
        assert!(executed.stdout.starts_with("git version"));
        assert_eq!(seen.len(), 1);
        assert!(matches!(
            &seen[0],
            Progress::Output { stream: OutputStream::Stdout, line } if line.starts_with("git version")
        ));
    }

    #[tokio::test]
    async fn a_hook_shows_up_as_started_output_and_finished() {
        let directory = tempfile::tempdir().unwrap();
        let cwd = directory.path();
        let setup = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .args(args)
                .current_dir(cwd)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .output()
                .unwrap();
            assert!(output.status.success(), "git {args:?}: {output:?}");
        };
        setup(&["init", "--quiet"]);
        setup(&["config", "user.email", "test@test.com"]);
        setup(&["config", "user.name", "Test"]);
        setup(&["config", "commit.gpgsign", "false"]);
        let hooks = cwd.join(".git").join("hooks");
        std::fs::create_dir_all(&hooks).unwrap();
        let hook = hooks.join("pre-commit");
        std::fs::write(
            &hook,
            "#!/bin/sh\necho checking formatting\necho lint >&2\nexit 0\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        std::fs::write(cwd.join("a.txt"), "a\n").unwrap();
        setup(&["add", "a.txt"]);
        let mut seen = vec![];
        let mut progress = |event: Progress| seen.push(event);
        let executed = execute(Execute {
            progress: Some(&mut progress),
            trace_hooks: true,
            ..Execute::new(cwd, &["commit", "-m", "Add a"])
        })
        .await
        .unwrap();
        assert!(executed.ok(), "{executed:?}");
        let kinds: Vec<String> = seen
            .iter()
            .map(|event| match event {
                Progress::Output { stream, line } => format!("{stream:?}:{line}"),
                Progress::HookStarted { hook_name } => format!("start:{hook_name}"),
                Progress::HookFinished {
                    hook_name,
                    exit_code,
                    ..
                } => format!("finish:{hook_name}:{exit_code:?}"),
            })
            .collect();
        assert!(kinds.contains(&"start:pre-commit".to_owned()), "{kinds:?}");
        assert!(
            kinds.contains(&"Stdout:checking formatting".to_owned()),
            "{kinds:?}"
        );
        assert!(kinds.contains(&"Stderr:lint".to_owned()), "{kinds:?}");
        assert!(
            kinds.contains(&"finish:pre-commit:Some(0)".to_owned()),
            "{kinds:?}"
        );
    }

    #[tokio::test]
    async fn a_timeout_kills_the_command() {
        let directory = tempfile::tempdir().unwrap();
        let error = execute(Execute {
            timeout: Some(Duration::from_millis(100)),
            ..Execute::new(directory.path(), &["-c", "alias.wait=!sleep 5", "wait"])
        })
        .await
        .unwrap_err();
        assert!(error.to_string().contains("timed out"), "{error}");
    }
}

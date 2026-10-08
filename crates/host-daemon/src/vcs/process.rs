//! Git for the long operations: a timeout, bounded output, the output lines
//! as they arrive, and the hooks a commit runs, read from Git's trace.
use agent_protocol::vcs::OutputStream;
use anyhow::{Context as _, Result, anyhow};
use std::io::{Read as _, Seek as _, SeekFrom};
use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncReadExt as _};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

pub(super) const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
pub(super) const DEFAULT_MAX_OUTPUT_BYTES: usize = 1_000_000;
const TRACE_POLL: Duration = Duration::from_millis(50);
const OUTPUT_CHANNEL_CAPACITY: usize = 64;
const READ_CHUNK_BYTES: usize = 8 * 1024;
const MAX_LINE_BYTES: usize = 64 * 1024;
const TRACE_READ_CHUNK_BYTES: usize = 64 * 1024;

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
    pub progress: Option<&'a mut dyn FnMut(Progress)>,
    pub trace_hooks: bool,
    /// Cancellation owned by the process runner. The runner kills and waits
    /// for the child, then joins its pipe readers before returning.
    pub cancel: Option<CancellationToken>,
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
            cancel: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Executed {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
    pub stdout_truncated: bool,
}

#[derive(Debug, thiserror::Error)]
#[error("external command cancelled")]
pub(crate) struct CommandCancelled;

#[derive(Debug, thiserror::Error)]
#[error("external command timed out after {timeout:?}")]
pub(crate) struct CommandTimedOut {
    pub timeout: Duration,
}

impl Executed {
    pub(crate) fn ok(&self) -> bool {
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

    fn read(&mut self, progress: &mut dyn FnMut(Progress)) {
        let Ok(mut file) = std::fs::File::open(self.file.path()) else {
            return;
        };
        let Ok(length) = file.metadata().map(|metadata| metadata.len()) else {
            return;
        };
        if length < self.processed as u64 {
            // Git can replace a trace file between polls. Treat that as a
            // fresh stream rather than indexing past the new file.
            self.processed = 0;
            self.remainder.clear();
            self.started.clear();
        }
        if length <= self.processed as u64
            || file
                .seek(SeekFrom::Start(self.processed as u64))
                .is_err()
        {
            return;
        }
        let mut bytes = Vec::with_capacity(TRACE_READ_CHUNK_BYTES);
        if file
            .take(TRACE_READ_CHUNK_BYTES as u64)
            .read_to_end(&mut bytes)
            .is_err()
        {
            return;
        }
        self.processed = self.processed.saturating_add(bytes.len());
        let appended = String::from_utf8_lossy(&bytes);
        if self.remainder.len() > MAX_LINE_BYTES {
            self.remainder.clear();
        }
        let combined = format!("{}{appended}", self.remainder);
        let mut lines: Vec<&str> = combined.split('\n').collect();
        self.remainder = lines.pop().unwrap_or_default().to_owned();
        if self.remainder.len() > MAX_LINE_BYTES {
            self.remainder.clear();
        }
        for line in lines {
            if line.len() <= MAX_LINE_BYTES {
                self.line(line.trim_end_matches('\r'), progress);
            }
        }
    }

    fn line(&mut self, line: &str, progress: &mut dyn FnMut(Progress)) {
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
    mut pipe: impl AsyncRead + Unpin,
    stream: OutputStream,
    sender: tokio::sync::mpsc::Sender<(OutputStream, Vec<u8>)>,
    cancel: CancellationToken,
) {
    let mut chunk = [0_u8; READ_CHUNK_BYTES];
    let mut line = Vec::with_capacity(MAX_LINE_BYTES.min(READ_CHUNK_BYTES));
    let mut truncated = false;
    loop {
        let read = tokio::select! {
            _ = cancel.cancelled() => return,
            result = pipe.read(&mut chunk) => match result {
                Ok(0) | Err(_) => {
                    if !line.is_empty() || truncated {
                        let _ = send_line(&sender, stream, finish_line(&mut line, &mut truncated), &cancel).await;
                    }
                    return;
                }
                Ok(read) => read,
            },
        };
        for byte in &chunk[..read] {
            if *byte == b'\n' {
                let output = finish_line(&mut line, &mut truncated);
                if !send_line(&sender, stream, output, &cancel).await {
                    return;
                }
            } else if line.len() < MAX_LINE_BYTES {
                line.push(*byte);
            } else {
                // Keep consuming until the newline while retaining only a
                // bounded prefix. A command that never emits a newline can
                // therefore never grow this task's memory without limit.
                truncated = true;
            }
        }
    }
}

fn finish_line(line: &mut Vec<u8>, truncated: &mut bool) -> Vec<u8> {
    if *truncated {
        const MARKER: &[u8] = b"...[truncated]";
        let keep = MAX_LINE_BYTES.saturating_sub(MARKER.len());
        line.truncate(keep);
        line.extend_from_slice(MARKER);
    }
    line.push(b'\n');
    let output = std::mem::take(line);
    *truncated = false;
    output
}

async fn send_line(
    sender: &tokio::sync::mpsc::Sender<(OutputStream, Vec<u8>)>,
    stream: OutputStream,
    line: Vec<u8>,
    cancel: &CancellationToken,
) -> bool {
    tokio::select! {
        _ = cancel.cancelled() => false,
        result = sender.send((stream, line)) => result.is_ok(),
    }
}

async fn join_reader(task: &mut Option<JoinHandle<()>>) {
    if let Some(task) = task.take() {
        let _ = task.await;
    }
}

async fn stop_process(
    child: &mut tokio::process::Child,
    stdout_task: &mut Option<JoinHandle<()>>,
    stderr_task: &mut Option<JoinHandle<()>>,
    cancel: &CancellationToken,
) {
    // Wake readers blocked on the bounded channel before waiting for them.
    // The child is still killed and awaited even if it has already exited;
    // this closes the ownership window before the action permit is released.
    cancel.cancel();
    let _ = child.kill().await;
    let _ = child.wait().await;
    if let Some(task) = stdout_task.take() {
        task.abort();
        let _ = task.await;
    }
    if let Some(task) = stderr_task.take() {
        task.abort();
        let _ = task.await;
    }
}

/// Runs an arbitrary executable with the same bounded, cancellation-safe
/// ownership used by Git. GitHub's CLI uses this so dropping an action future
/// can never leave a child or its pipe readers detached.
pub(crate) async fn execute_program(
    program: &Path,
    cwd: &Path,
    args: &[&str],
    env: &[(&str, &str)],
    timeout: Option<Duration>,
    max_output_bytes: usize,
    cancel: Option<CancellationToken>,
) -> Result<Executed> {
    run_command(
        program,
        cwd,
        args,
        env,
        timeout,
        max_output_bytes,
        cancel,
        None,
        false,
        false,
    )
    .await
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
        progress,
        trace_hooks,
        cancel,
    } = input;
    run_command(
        Path::new("git"),
        cwd,
        args,
        env,
        timeout,
        max_output_bytes,
        cancel,
        progress,
        trace_hooks,
        true,
    )
    .await
}

async fn run_command(
    program: &Path,
    cwd: &Path,
    args: &[&str],
    env: &[(&str, &str)],
    timeout: Option<Duration>,
    max_output_bytes: usize,
    mut cancel: Option<CancellationToken>,
    mut progress: Option<&mut dyn FnMut(Progress)>,
    trace_hooks: bool,
    prefix_git_options: bool,
) -> Result<Executed> {
    if cancel.as_ref().is_some_and(CancellationToken::is_cancelled) {
        return Err(anyhow!(CommandCancelled));
    }
    let mut trace = if trace_hooks && progress.is_some() {
        Some(HookTrace::new()?)
    } else {
        None
    };
    let mut command = tokio::process::Command::new(program);
    if prefix_git_options {
        command.arg("--no-optional-locks");
    }
    command
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
    let mut child = command.spawn().context("failed to run command")?;
    let cancellation_enabled = cancel.is_some();
    let process_cancel = cancel.take().unwrap_or_else(CancellationToken::new);
    let reader_cancel = CancellationToken::new();
    let (sender, mut lines) = tokio::sync::mpsc::channel(OUTPUT_CHANNEL_CAPACITY);
    let stdout_task = child
        .stdout
        .take()
        .map(|pipe| tokio::spawn(read_lines(
            pipe,
            OutputStream::Stdout,
            sender.clone(),
            reader_cancel.clone(),
        )));
    let stderr_task = child
        .stderr
        .take()
        .map(|pipe| tokio::spawn(read_lines(
            pipe,
            OutputStream::Stderr,
            sender,
            reader_cancel.clone(),
        )));
    let mut stdout_task = stdout_task;
    let mut stderr_task = stderr_task;
    let deadline = timeout.map(|timeout| tokio::time::Instant::now() + timeout);
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    let mut stdout_truncated = false;
    let mut status = None;
    let mut collect = |stream: OutputStream, line: Vec<u8>, progress: &mut Option<&mut dyn FnMut(Progress)>| {
        let buffer = match stream {
            OutputStream::Stdout => &mut stdout,
            OutputStream::Stderr => &mut stderr,
        };
        let remaining = max_output_bytes.saturating_sub(buffer.len());
        let retained = remaining.min(line.len());
        buffer.extend_from_slice(&line[..retained]);
        if retained < line.len() && stream == OutputStream::Stdout {
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
            _ = timed_out => {
                stop_process(
                    &mut child,
                    &mut stdout_task,
                    &mut stderr_task,
                    &reader_cancel,
                )
                .await;
                return Err(anyhow!(CommandTimedOut {
                    timeout: timeout.unwrap_or_default(),
                }));
            }
            _ = process_cancel.cancelled(), if cancellation_enabled => {
                stop_process(
                    &mut child,
                    &mut stdout_task,
                    &mut stderr_task,
                    &reader_cancel,
                )
                .await;
                return Err(anyhow!(CommandCancelled));
            }
            line = lines.recv() => match line {
                Some((stream, line)) => collect(stream, line, &mut progress),
                None => break,
            },
            exited = child.wait(), if status.is_none() => {
                match exited {
                    Ok(exited) => status = Some(exited),
                    Err(error) => {
                        stop_process(
                            &mut child,
                            &mut stdout_task,
                            &mut stderr_task,
                            &reader_cancel,
                        )
                        .await;
                        return Err(error.into());
                    }
                }
            }
            _ = ticker.tick() => {
                if let (Some(trace), Some(progress)) = (trace.as_mut(), progress.as_mut()) {
                    trace.read(*progress);
                }
            }
        }
    }
    let status = match status {
        Some(status) => status,
        None => match child.wait().await {
            Ok(status) => status,
            Err(error) => {
                stop_process(
                    &mut child,
                    &mut stdout_task,
                    &mut stderr_task,
                    &reader_cancel,
                )
                .await;
                return Err(error.into());
            }
        },
    };
    join_reader(&mut stdout_task).await;
    join_reader(&mut stderr_task).await;
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
        std::fs::write(&hook, "#!/bin/sh\necho checking formatting\necho lint >&2\nexit 0\n")
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
        assert!(kinds.contains(&"Stdout:checking formatting".to_owned()), "{kinds:?}");
        assert!(kinds.contains(&"Stderr:lint".to_owned()), "{kinds:?}");
        assert!(kinds.contains(&"finish:pre-commit:Some(0)".to_owned()), "{kinds:?}");
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

    #[cfg(unix)]
    #[tokio::test]
    async fn cancellation_kills_and_joins_an_active_fake_git_process() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let executable = directory.path().join("git");
        std::fs::write(
            &executable,
            "#!/bin/sh\nprintf 'started\\n'\nwhile :; do sleep 1; done\n",
        )
        .unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut paths = vec![directory.path().to_path_buf()];
        if let Some(existing) = std::env::var_os("PATH") {
            paths.extend(std::env::split_paths(&existing));
        }
        let path = std::env::join_paths(paths).unwrap();
        let path = path.to_string_lossy().into_owned();
        let env = [("PATH", path.as_str())];
        let cancel = CancellationToken::new();
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let mut ready_tx = Some(ready_tx);
        let mut progress = move |event: Progress| {
            if matches!(event, Progress::Output { line, .. } if line == "started") {
                if let Some(ready_tx) = ready_tx.take() {
                    let _ = ready_tx.send(());
                }
            }
        };
        let mut running = Box::pin(execute(Execute {
            env: &env,
            progress: Some(&mut progress),
            cancel: Some(cancel.clone()),
            timeout: Some(Duration::from_secs(5)),
            ..Execute::new(directory.path(), &[])
        }));
        tokio::time::timeout(Duration::from_secs(1), ready_rx)
            .await
            .expect("fake git did not start")
            .expect("fake git readiness signal was dropped");
        cancel.cancel();
        let error = tokio::time::timeout(Duration::from_secs(2), &mut running)
            .await
            .expect("cancellation left the process owned forever")
            .expect_err("a cancelled process must return an error");
        assert!(error.downcast_ref::<CommandCancelled>().is_some(), "{error}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_newline_free_fake_output_is_bounded() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let executable = directory.path().join("git");
        std::fs::write(
            &executable,
            "#!/bin/sh\nprintf 'started\\n'\nhead -c 2000000 /dev/zero\n",
        )
        .unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut paths = vec![directory.path().to_path_buf()];
        if let Some(existing) = std::env::var_os("PATH") {
            paths.extend(std::env::split_paths(&existing));
        }
        let path = std::env::join_paths(paths).unwrap();
        let path = path.to_string_lossy().into_owned();
        let env = [("PATH", path.as_str())];
        let mut seen = vec![];
        let mut progress = |event: Progress| seen.push(event);
        let executed = execute(Execute {
            env: &env,
            max_output_bytes: 1024,
            progress: Some(&mut progress),
            ..Execute::new(directory.path(), &[])
        })
        .await
        .unwrap();
        assert!(executed.ok(), "{executed:?}");
        assert!(executed.stdout_truncated);
        assert!(seen.iter().all(|event| match event {
            Progress::Output { line, .. } => line.len() <= MAX_LINE_BYTES,
            _ => true,
        }));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_deadline_wins_over_continuous_newline_output() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let executable = directory.path().join("git");
        std::fs::write(&executable, "#!/bin/sh\nwhile :; do printf 'x\\n'; done\n").unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut paths = vec![directory.path().to_path_buf()];
        if let Some(existing) = std::env::var_os("PATH") {
            paths.extend(std::env::split_paths(&existing));
        }
        let path = std::env::join_paths(paths).unwrap();
        let path = path.to_string_lossy().into_owned();
        let env = [("PATH", path.as_str())];
        let started = std::time::Instant::now();
        let error = execute(Execute {
            env: &env,
            timeout: Some(Duration::from_millis(100)),
            max_output_bytes: 1024,
            ..Execute::new(directory.path(), &[])
        })
        .await
        .expect_err("a continuous output producer must hit its deadline");
        assert!(error.downcast_ref::<CommandTimedOut>().is_some(), "{error}");
        assert!(started.elapsed() < Duration::from_secs(2), "{error}");
    }
}

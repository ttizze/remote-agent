//! Project setup scripts, run as supervised processes in the thread's
//! workspace until they exit or the thread's terminals are cleaned up.
use agent_domain::ThreadId;
use agent_protocol::models::ProjectScript;
use agent_runtime::{SetupEvent, SetupProgress};
use futures_util::future::BoxFuture;
use std::sync::LazyLock;
use std::{collections::HashMap, path::Path, process::Stdio, sync::Mutex};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::{sync::oneshot, task::JoinHandle};
use tokio_util::sync::CancellationToken;

const OUTPUT_LINE_MAX_LENGTH: usize = 400;
/// A partial line longer than this is a byte stream; only its tail is kept.
const PARTIAL_LINE_MAX_LENGTH: usize = 4_096;

/// The first script that runs on worktree creation.
pub(crate) fn setup_script(scripts: &[ProjectScript]) -> Option<&ProjectScript> {
    scripts.iter().find(|script| script.run_on_worktree_create)
}

static TERMINAL_CONTROL: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(
        r"\x1b\[[0-9;?]*[ -/]*[@-~]|\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)|\x1b[()][A-Za-z0-9]|\x1b[=>]|[\x00-\x08\x0b\x0c\x0e-\x1f\x7f]",
    )
    .expect("pattern compiles")
});

/// Strips terminal control sequences and drops empty lines, capping length.
fn output_line(raw: &str) -> Option<String> {
    let cleaned = TERMINAL_CONTROL.replace_all(raw, "");
    let cleaned = cleaned.trim_end();
    (!cleaned.is_empty()).then(|| {
        let mut units = 0;
        cleaned
            .chars()
            .take_while(|c| {
                units += c.len_utf16();
                units <= OUTPUT_LINE_MAX_LENGTH
            })
            .collect()
    })
}

/// Splits output on `\r\n`, `\r` or `\n`; an installer's redrawn progress line
/// becomes a short line of its own.
async fn forward_lines(mut output: impl AsyncRead + Unpin, progress: SetupProgress) {
    let mut pending = String::new();
    let mut buffer = [0u8; 8192];
    loop {
        let read = match output.read(&mut buffer).await {
            Ok(0) | Err(_) => break,
            Ok(read) => read,
        };
        pending.push_str(&String::from_utf8_lossy(&buffer[..read]));
        let mut lines: Vec<String> = pending
            .split("\r\n")
            .flat_map(|part| part.split(['\r', '\n']))
            .map(str::to_owned)
            .collect();
        pending = lines.pop().unwrap_or_default();
        if pending.len() > PARTIAL_LINE_MAX_LENGTH {
            let mut start = pending.len() - PARTIAL_LINE_MAX_LENGTH;
            while !pending.is_char_boundary(start) {
                start += 1;
            }
            pending = pending[start..].to_owned();
        }
        for line in lines.iter().filter_map(|line| output_line(line)) {
            progress.report(SetupEvent::Output(line));
        }
    }
}

struct Running {
    stop: CancellationToken,
    task: JoinHandle<()>,
}

pub(crate) struct SetupScripts {
    /// The program and leading arguments; the script's command follows them.
    shell: Vec<String>,
    running: Mutex<HashMap<ThreadId, Vec<Running>>>,
}

impl Default for SetupScripts {
    fn default() -> Self {
        let shell: &[&str] = if cfg!(windows) {
            &["cmd.exe", "/C"]
        } else {
            &[
                "/bin/sh",
                "-c",
                "exec \"${SHELL:-/bin/sh}\" -l -c \"$1\"",
                "sh",
            ]
        };
        Self::with_shell(shell.iter().map(|part| (*part).to_owned()).collect())
    }
}

impl Drop for SetupScripts {
    fn drop(&mut self) {
        for running in self.running.get_mut().unwrap().values().flatten() {
            running.stop.cancel();
        }
    }
}

/// Resolves with the script's exit code, `None` when it was stopped. Dropping it
/// before the script exits stops the script.
pub(crate) type Completion = BoxFuture<'static, Option<i32>>;

impl SetupScripts {
    fn with_shell(shell: Vec<String>) -> Self {
        Self {
            shell,
            running: Mutex::default(),
        }
    }

    /// Starts `script` in `cwd`. An observed run forwards the script's output
    /// lines and returns its completion.
    pub(crate) fn start(
        &self,
        thread: &ThreadId,
        script: &ProjectScript,
        project_root: &str,
        cwd: &str,
        observe: Option<SetupProgress>,
    ) -> Result<Option<Completion>, String> {
        let failed = |operation: &str| {
            format!(
                "Project setup script operation '{operation}' failed for thread '{thread}' in '{cwd}'."
            )
        };
        let mut command =
            bex_process::command(Path::new(&self.shell[0])).map_err(|_| failed("openTerminal"))?;
        let output = || {
            if observe.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            }
        };
        command
            .args(&self.shell[1..])
            .arg(&script.command)
            .current_dir(cwd)
            .env("PROJECT_ROOT", project_root)
            .env("WORKTREE_PATH", cwd)
            // Nobody can answer a terminal's color probes while the script runs.
            .env("COLORTERM", "")
            .env("NO_COLOR", "1")
            .env("FORCE_COLOR", "0")
            .stdin(Stdio::piped())
            .stdout(output())
            .stderr(output());
        let mut child = command.spawn().map_err(|_| failed("openTerminal"))?;
        let readers: Vec<JoinHandle<()>> = match &observe {
            Some(progress) => [
                child
                    .stdout
                    .take()
                    .map(|out| tokio::spawn(forward_lines(out, progress.clone()))),
                child
                    .stderr
                    .take()
                    .map(|err| tokio::spawn(forward_lines(err, progress.clone()))),
            ]
            .into_iter()
            .flatten()
            .collect(),
            None => vec![],
        };
        // Closing the supervisor's input stops the script and its descendants.
        let input = child.stdin.take();
        let stop = CancellationToken::new();
        let (exited, exit) = oneshot::channel();
        let stopped = stop.clone();
        let task = tokio::spawn(async move {
            let status = tokio::select! {
                status = child.wait() => Some(status),
                _ = stopped.cancelled() => None,
            };
            let code = match status {
                Some(status) => status.ok().and_then(|status| status.code()),
                None => {
                    drop(input);
                    let _ = child.wait().await;
                    None
                }
            };
            for reader in readers {
                let _ = reader.await;
            }
            let _ = exited.send(code);
        });
        {
            let mut running = self.running.lock().unwrap();
            running.retain(|_, scripts| {
                scripts.retain(|script| !script.task.is_finished());
                !scripts.is_empty()
            });
            running.entry(thread.clone()).or_default().push(Running {
                stop: stop.clone(),
                task,
            });
        }
        Ok(observe.map(|_| -> Completion {
            Box::pin(async move {
                let cancel = stop.drop_guard();
                let code = exit.await.ok().flatten();
                cancel.disarm();
                code
            })
        }))
    }

    /// Stops the thread's scripts and waits until they exited.
    pub(crate) async fn stop(&self, thread: &ThreadId) {
        let scripts = self
            .running
            .lock()
            .unwrap()
            .remove(thread)
            .unwrap_or_default();
        for script in scripts {
            script.stop.cancel();
            let _ = script.task.await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_protocol::models::ProjectScriptIcon;
    #[cfg(unix)]
    use std::sync::Arc;
    #[cfg(unix)]
    use std::time::Duration;

    fn script(id: &str, command: &str, setup: bool) -> ProjectScript {
        ProjectScript {
            id: id.into(),
            name: id.into(),
            command: command.into(),
            icon: ProjectScriptIcon::Configure,
            run_on_worktree_create: setup,
            run_async: None,
            preview_url: None,
            auto_open_preview: None,
        }
    }

    #[cfg(unix)]
    fn scripts() -> SetupScripts {
        SetupScripts::with_shell(vec!["/bin/sh".into(), "-c".into()])
    }

    #[cfg(unix)]
    fn lines() -> (SetupProgress, Arc<Mutex<Vec<String>>>) {
        let lines = Arc::new(Mutex::new(vec![]));
        let seen = lines.clone();
        let progress = SetupProgress::new(move |event| {
            if let SetupEvent::Output(line) = event {
                seen.lock().unwrap().push(line);
            }
        });
        (progress, lines)
    }

    #[cfg(unix)]
    async fn until(what: &str, check: impl Fn() -> bool) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while !check() {
            assert!(tokio::time::Instant::now() < deadline, "timed out: {what}");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    #[test]
    fn the_first_script_run_on_worktree_creation_is_the_setup() {
        let scripts = [
            script("lint", "vp lint", false),
            script("setup", "vp install", true),
            script("seed", "vp seed", true),
        ];
        assert_eq!(setup_script(&scripts).unwrap().id, "setup");
        assert_eq!(setup_script(&scripts[..1]), None);
    }

    // ProjectSetupScriptRunner.ts stripTerminalControl and the output line filter.
    #[test]
    fn output_lines_drop_terminal_control_and_stay_bounded() {
        assert_eq!(
            output_line("\x1b[32mok\x1b[0m done \x07  ").as_deref(),
            Some("ok done")
        );
        assert_eq!(output_line("\x1b]0;title\x07"), None);
        assert_eq!(output_line(&"x".repeat(500)).unwrap().len(), 400);
    }

    // ProjectSetupScriptRunner.test.ts: the script runs in the worktree with the
    // project and worktree paths and without color.
    #[cfg(unix)]
    #[tokio::test]
    async fn runs_the_script_in_the_worktree_with_its_environment() {
        let directory = tempfile::tempdir().unwrap();
        let worktree = dunce::canonicalize(directory.path()).unwrap();
        let cwd = worktree.to_str().unwrap();
        let thread = ThreadId::new("thread-1").unwrap();
        let command = "printf '%s|%s|%s|%s|%s|%s' \"$PWD\" \"$PROJECT_ROOT\" \
                       \"$WORKTREE_PATH\" \"${COLORTERM-unset}\" \"$NO_COLOR\" \
                       \"$FORCE_COLOR\" > environment.txt";
        let runner = scripts();
        let (progress, _) = lines();
        let completion = runner
            .start(
                &thread,
                &script("setup", command, true),
                "/repo",
                cwd,
                Some(progress),
            )
            .unwrap()
            .unwrap();
        assert_eq!(completion.await, Some(0));
        assert_eq!(
            std::fs::read_to_string(worktree.join("environment.txt")).unwrap(),
            format!("{cwd}|/repo|{cwd}||1|0")
        );
    }

    // ThreadLaunchService.ts: an observed setup reports its exit code and output.
    #[cfg(unix)]
    #[tokio::test]
    async fn an_observed_setup_reports_its_exit_code_and_output_lines() {
        let directory = tempfile::tempdir().unwrap();
        let cwd = directory.path().to_str().unwrap();
        let thread = ThreadId::new("thread-1").unwrap();
        let runner = scripts();
        let (progress, lines) = lines();
        let command = "printf 'one\\r\\ntwo\\rthree\\n'; echo four >&2; exit 3";
        let completion = runner
            .start(
                &thread,
                &script("setup", command, true),
                "/repo",
                cwd,
                Some(progress),
            )
            .unwrap()
            .unwrap();
        assert_eq!(completion.await, Some(3));
        let mut seen = lines.lock().unwrap().clone();
        seen.sort();
        assert_eq!(seen, ["four", "one", "three", "two"]);
        // An unobserved start returns no completion.
        assert!(
            runner
                .start(
                    &thread,
                    &script("setup", "exit 3", true),
                    "/repo",
                    cwd,
                    None
                )
                .unwrap()
                .is_none()
        );
    }

    // ProjectSetupScriptRunner.ts: an unobserved or asynchronous script keeps running
    // after the agent starts, until the thread's terminals close.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_started_script_runs_until_the_threads_terminals_close() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().to_path_buf();
        let cwd = root.to_str().unwrap();
        let thread = ThreadId::new("thread-1").unwrap();
        let other = ThreadId::new("thread-2").unwrap();
        let runner = scripts();
        let long = "touch started; sleep 30; touch finished";
        runner
            .start(&thread, &script("setup", long, true), "/repo", cwd, None)
            .unwrap();
        until("the script starts", || root.join("started").exists()).await;
        runner.stop(&other).await;
        tokio::time::timeout(Duration::from_secs(10), runner.stop(&thread))
            .await
            .expect("the script stops");
        assert!(!root.join("finished").exists());
        assert!(runner.running.lock().unwrap().is_empty());
    }

    // A preparation cancelled while it waits stops the script.
    #[cfg(unix)]
    #[tokio::test]
    async fn dropping_an_observed_setup_stops_the_script() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().to_path_buf();
        let cwd = root.to_str().unwrap();
        let thread = ThreadId::new("thread-1").unwrap();
        let runner = scripts();
        let long = "touch started; sleep 30; touch finished";
        let (progress, _) = lines();
        let waiting = runner
            .start(
                &thread,
                &script("setup", long, true),
                "/repo",
                cwd,
                Some(progress),
            )
            .unwrap()
            .unwrap();
        tokio::select! {
            _ = waiting => panic!("the script does not finish"),
            _ = until("the script starts", || root.join("started").exists()) => {}
        }
        let task = runner.running.lock().unwrap().remove(&thread).unwrap();
        tokio::time::timeout(
            Duration::from_secs(10),
            task.into_iter().next().unwrap().task,
        )
        .await
        .expect("the script stops")
        .unwrap();
        assert!(!root.join("finished").exists());
    }
}

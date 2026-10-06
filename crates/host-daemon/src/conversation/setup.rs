//! Project setup scripts (T3 `ProjectSetupScriptRunner`), run as supervised
//! processes in the thread's workspace until they exit or the thread's terminals
//! are cleaned up.
use agent_domain::ThreadId;
use agent_protocol::models::ProjectScript;
use std::{collections::HashMap, path::Path, process::Stdio, sync::Mutex};
use tokio::{sync::oneshot, task::JoinHandle};
use tokio_util::sync::CancellationToken;

/// T3 `setupProjectScript`: the first script that runs on worktree creation.
pub(crate) fn setup_script(scripts: &[ProjectScript]) -> Option<&ProjectScript> {
    scripts.iter().find(|script| script.run_on_worktree_create)
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

impl SetupScripts {
    fn with_shell(shell: Vec<String>) -> Self {
        Self {
            shell,
            running: Mutex::default(),
        }
    }

    /// Starts `script` in `cwd`. With `wait` it returns once the script exited and
    /// fails unless it exited with 0; dropping the wait stops the script.
    pub(crate) async fn run(
        &self,
        thread: &ThreadId,
        script: &ProjectScript,
        project_root: &str,
        cwd: &str,
        wait: bool,
    ) -> Result<(), String> {
        let failed = |operation: &str| {
            format!(
                "Project setup script operation '{operation}' failed for thread '{thread}' in '{cwd}'."
            )
        };
        let mut command =
            bex_process::command(Path::new(&self.shell[0])).map_err(|_| failed("openTerminal"))?;
        command
            .args(&self.shell[1..])
            .arg(&script.command)
            .current_dir(cwd)
            .env("T3CODE_PROJECT_ROOT", project_root)
            .env("T3CODE_WORKTREE_PATH", cwd)
            // Nobody can answer a terminal's color probes while the script runs.
            .env("COLORTERM", "")
            .env("NO_COLOR", "1")
            .env("FORCE_COLOR", "0")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut child = command.spawn().map_err(|_| failed("openTerminal"))?;
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
        if !wait {
            return Ok(());
        }
        let cancel = stop.drop_guard();
        let code = exit.await.ok().flatten();
        cancel.disarm();
        match code {
            Some(0) => Ok(()),
            Some(code) => Err(format!("Setup script exited with {code}.")),
            None => Err("Setup script exited with no exit code.".into()),
        }
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

    // ProjectSetupScriptRunner.test.ts: the script runs in the worktree with the
    // project and worktree paths and without color.
    #[cfg(unix)]
    #[tokio::test]
    async fn runs_the_script_in_the_worktree_with_its_environment() {
        let directory = tempfile::tempdir().unwrap();
        let worktree = dunce::canonicalize(directory.path()).unwrap();
        let cwd = worktree.to_str().unwrap();
        let thread = ThreadId::new("thread-1").unwrap();
        let command = "printf '%s|%s|%s|%s|%s|%s' \"$PWD\" \"$T3CODE_PROJECT_ROOT\" \
                       \"$T3CODE_WORKTREE_PATH\" \"${COLORTERM-unset}\" \"$NO_COLOR\" \
                       \"$FORCE_COLOR\" > environment.txt";
        let runner = scripts();
        runner
            .run(&thread, &script("setup", command, true), "/repo", cwd, true)
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(worktree.join("environment.txt")).unwrap(),
            format!("{cwd}|/repo|{cwd}||1|0")
        );
    }

    // ThreadLaunchService.ts: an awaited setup fails with its exit code.
    #[cfg(unix)]
    #[tokio::test]
    async fn an_awaited_setup_fails_with_its_exit_code() {
        let directory = tempfile::tempdir().unwrap();
        let cwd = directory.path().to_str().unwrap();
        let thread = ThreadId::new("thread-1").unwrap();
        let runner = scripts();
        assert_eq!(
            runner
                .run(
                    &thread,
                    &script("setup", "exit 3", true),
                    "/repo",
                    cwd,
                    true
                )
                .await,
            Err("Setup script exited with 3.".into())
        );
        // Without waiting, only the start counts.
        assert_eq!(
            runner
                .run(
                    &thread,
                    &script("setup", "exit 3", true),
                    "/repo",
                    cwd,
                    false
                )
                .await,
            Ok(())
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
            .run(&thread, &script("setup", long, true), "/repo", cwd, false)
            .await
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
    async fn dropping_an_awaited_setup_stops_the_script() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().to_path_buf();
        let cwd = root.to_str().unwrap();
        let thread = ThreadId::new("thread-1").unwrap();
        let runner = scripts();
        let long = "touch started; sleep 30; touch finished";
        let setup = script("setup", long, true);
        let waiting = runner.run(&thread, &setup, "/repo", cwd, true);
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

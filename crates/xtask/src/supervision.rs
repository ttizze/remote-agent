//! Own child groups through completion, cancellation and shutdown.
use crate::Result;
#[cfg(unix)]
use nix::{errno::Errno, sys::signal::Signal};
use process_wrap::tokio::{ChildWrapper, CommandWrap, KillOnDrop};
use std::{ffi::OsString, fs::File, io, path::Path, process::Output, time::Duration};
use tokio::{io::AsyncReadExt, process::Command, sync::watch};

pub fn cancellation() -> watch::Receiver<bool> {
    let (sender, receiver) = watch::channel(false);
    tokio::spawn(async move {
        async fn signal() {
            #[cfg(unix)]
            {
                let Ok(mut terminate) =
                    tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                else {
                    return;
                };
                tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = terminate.recv() => {} }
            }
            #[cfg(not(unix))]
            {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
        tokio::select! {
            _ = signal() => {},
            _ = sender.closed() => return,
        }
        let _ = sender.send(true);
    });
    receiver
}

pub async fn cancelled(mut receiver: watch::Receiver<bool>) {
    while !*receiver.borrow_and_update() {
        if receiver.changed().await.is_err() {
            return;
        }
    }
}

pub fn interrupted() -> Box<dyn std::error::Error + Send + Sync> {
    io::Error::new(io::ErrorKind::Interrupted, "interrupted").into()
}

pub struct Child {
    process: Box<dyn ChildWrapper>,
    #[cfg(unix)]
    group: nix::unistd::Pid,
}

pub enum Io<'a> {
    Capture,
    Inherit,
    Log(&'a File),
}

impl Child {
    pub fn spawn(command: Command) -> Result<Self> {
        let mut command = CommandWrap::from(command);
        #[cfg(unix)]
        command.wrap(process_wrap::tokio::ProcessGroup::leader());
        #[cfg(windows)]
        command.wrap(process_wrap::tokio::JobObject);
        command.wrap(KillOnDrop);
        let process = command.spawn()?;
        #[cfg(unix)]
        let group =
            nix::unistd::Pid::from_raw(process.id().ok_or("missing child PID")?.try_into()?);
        Ok(Self {
            process,
            #[cfg(unix)]
            group,
        })
    }

    pub fn try_wait(&mut self) -> Result<Option<std::process::ExitStatus>> {
        Ok(self.process.try_wait()?)
    }

    #[cfg(test)]
    pub(crate) fn take_stdin(&mut self) -> Option<tokio::process::ChildStdin> {
        self.process.stdin().take()
    }

    pub async fn stop(&mut self, host: bool, grace: Duration) -> Result<()> {
        // Only the Host receives SIGINT. Its supervisors must remain alive to
        // observe lifetime-pipe closure and stop their separate provider groups.
        #[cfg(unix)]
        let signal = if host {
            self.process
                .try_inner_child()
                .ok_or("missing native child")?
                .signal(Signal::SIGINT as i32)
        } else {
            self.process.signal(Signal::SIGTERM as i32)
        };
        #[cfg(not(unix))]
        let signal = {
            let _ = host;
            self.process.start_kill()
        };
        if let Err(error) = signal
            && !self.missing(&error).await?
        {
            return Err(error.into());
        }
        let deadline = tokio::time::Instant::now() + grace;
        let _ = tokio::time::timeout_at(deadline, self.process.wait()).await;
        #[cfg(unix)]
        while tokio::time::Instant::now() < deadline {
            match nix::sys::signal::killpg(self.group, None) {
                Ok(()) => tokio::time::sleep(Duration::from_millis(10)).await,
                Err(error) if self.missing(&io::Error::from(error)).await? => break,
                Err(error) => return Err(error.into()),
            }
        }
        if let Err(error) = self.process.start_kill()
            && !self.missing(&error).await?
        {
            return Err(error.into());
        }
        self.process.wait().await?;
        Ok(())
    }

    async fn missing(&self, error: &io::Error) -> Result<bool> {
        #[cfg(unix)]
        {
            if error.raw_os_error() == Some(Errno::ESRCH as i32) {
                return Ok(true);
            }
            #[cfg(target_os = "macos")]
            if error.raw_os_error() == Some(Errno::EPERM as i32) {
                // An orphaned zombie group can reject signals on macOS before
                // launchd reaps it. Ignore EPERM only after verifying that no
                // live group member remains; genuine permission errors fail.
                let mut command = Command::new("ps");
                command.args(["-A", "-o", "pgid=,stat="]).kill_on_drop(true);
                let output =
                    tokio::time::timeout(Duration::from_secs(5), command.output()).await??;
                if !output.status.success() {
                    return Err("Could not inspect stopped process group".into());
                }
                return Ok(!live_group(
                    std::str::from_utf8(&output.stdout)?,
                    self.group.as_raw(),
                )?);
            }
            Ok(false)
        }
        #[cfg(not(unix))]
        {
            Ok(error.kind() == io::ErrorKind::NotFound)
        }
    }

    pub async fn output(
        &mut self,
        cancel: &watch::Receiver<bool>,
        timeout: Duration,
        shutdown_grace: Duration,
    ) -> Result<Output> {
        async fn read(pipe: Option<impl tokio::io::AsyncRead + Unpin>) -> io::Result<Vec<u8>> {
            let mut bytes = Vec::new();
            if let Some(mut pipe) = pipe {
                pipe.read_to_end(&mut bytes).await?;
            }
            Ok(bytes)
        }
        let stdout = self.process.stdout().take();
        let stderr = self.process.stderr().take();
        let result = tokio::select! {
            result = tokio::time::timeout(timeout, async {
                let (status, stdout, stderr) = tokio::try_join!(self.process.wait(), read(stdout), read(stderr))?;
                Ok::<_, io::Error>(Output { status, stdout, stderr })
            }) => result.map_err(|error| Box::new(error) as Box<dyn std::error::Error + Send + Sync>).and_then(|result| result.map_err(Into::into)),
            _ = cancelled(cancel.clone()) => Err(interrupted()),
        };
        self.stop(false, shutdown_grace).await?;
        result
    }
}

#[cfg(any(target_os = "macos", all(test, unix)))]
fn live_group(processes: &str, group: i32) -> Result<bool> {
    for line in processes.lines().filter(|line| !line.trim().is_empty()) {
        let mut fields = line.split_whitespace();
        let id: i32 = fields.next().ok_or("missing process group")?.parse()?;
        let state = fields.next().ok_or("missing process state")?;
        if id == group && !state.starts_with('Z') {
            return Ok(true);
        }
    }
    Ok(false)
}

impl Drop for Child {
    fn drop(&mut self) {
        let _ = self.process.start_kill();
    }
}

pub async fn run(
    args: &[OsString],
    cwd: &Path,
    io: Io<'_>,
    cancel: &watch::Receiver<bool>,
    timeout: Duration,
) -> Result<Output> {
    if *cancel.borrow() {
        return Err(interrupted());
    }
    let (program, arguments) = args.split_first().ok_or("empty command")?;
    let mut command = Command::new(program);
    command
        .args(arguments)
        .current_dir(cwd)
        .stdin(std::process::Stdio::null());
    match io {
        Io::Log(log) => {
            command.stdout(log.try_clone()?).stderr(log.try_clone()?);
        }
        Io::Capture => {
            command
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped());
        }
        Io::Inherit => {}
    }
    let output = Child::spawn(command)?
        .output(cancel, timeout, Duration::from_secs(10))
        .await?;
    if !output.status.success() {
        return Err(format!(
            "{} failed: {}{}",
            program.to_string_lossy(),
            output.status,
            if output.stderr.is_empty() {
                String::new()
            } else {
                format!("\n{}", String::from_utf8_lossy(&output.stderr))
            }
        )
        .into());
    }
    Ok(output)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::test_support::Fixture;
    use tokio::io::{AsyncBufReadExt, BufReader};

    #[test]
    fn orphaned_zombies_are_not_live_group_members() {
        assert!(!live_group("42 Z+\n42 Z\n7 S\n", 42).unwrap());
        assert!(live_group("42 Z\n42 S+\n", 42).unwrap());
        assert!(!live_group("7 R\n", 42).unwrap());
        assert!(live_group("42\n", 42).is_err());
        assert!(live_group("unknown S\n", 42).is_err());
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn permission_errors_require_confirmation_of_group_exit() {
        let mut command = Command::new("sh");
        command
            .args(["-c", "echo ready; read hold"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped());
        let mut child = Child::spawn(command).unwrap();
        let mut stdout = BufReader::new(child.process.stdout().take().unwrap());
        let mut line = String::new();
        tokio::time::timeout(Duration::from_secs(5), stdout.read_line(&mut line))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(line.trim(), "ready");
        assert!(
            !child
                .missing(&io::Error::from_raw_os_error(Errno::EPERM as i32))
                .await
                .unwrap()
        );
        child.stop(false, Duration::from_secs(10)).await.unwrap();
        assert!(
            child
                .missing(&io::Error::from_raw_os_error(Errno::EPERM as i32))
                .await
                .unwrap()
        );
        assert!(
            !child
                .missing(&io::Error::from_raw_os_error(Errno::EACCES as i32))
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn timeout_stops_descendants_and_closes_their_pipes() {
        let mut command = Command::new("sh");
        command
            .args(["-c", "sleep 30 & echo ready; wait"])
            .stdout(std::process::Stdio::piped());
        let mut child = Child::spawn(command).unwrap();
        let mut pipe = BufReader::new(child.process.stdout().take().unwrap());
        let mut line = String::new();
        tokio::time::timeout(Duration::from_secs(5), pipe.read_line(&mut line))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(line.trim(), "ready");
        let (_sender, cancel) = watch::channel(false);
        assert!(
            child
                .output(&cancel, Duration::from_millis(20), Duration::from_secs(10))
                .await
                .is_err()
        );
        let mut rest = Vec::new();
        tokio::time::timeout(Duration::from_secs(1), pipe.read_to_end(&mut rest))
            .await
            .unwrap()
            .unwrap();
    }

    async fn ready(child: &mut Child, root: &Path) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while !root.join("ready").exists() {
            assert!(child.try_wait().unwrap().is_none());
            assert!(tokio::time::Instant::now() < deadline);
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    #[tokio::test]
    async fn cancellation_waits_for_provider_descendant_shutdown() {
        let fixture = Fixture::new().await;
        let root = fixture.project("provider").await;
        let mut command = Command::new(Fixture::executable(&root));
        command.arg("parent").arg(&root);
        let mut child = Child::spawn(command).unwrap();
        ready(&mut child, &root).await;
        let (sender, cancel) = watch::channel(false);
        sender.send(true).unwrap();
        let error = child
            .output(&cancel, Duration::from_secs(5), Duration::from_secs(10))
            .await
            .unwrap_err();
        assert_eq!(
            error.downcast_ref::<io::Error>().unwrap().kind(),
            io::ErrorKind::Interrupted
        );
        assert!(child.try_wait().unwrap().is_some());
        assert!(root.join("stopped").exists());
    }

    #[tokio::test]
    async fn host_shutdown_closes_supervisor_lifetime_pipe_without_interrupting_it() {
        let fixture = Fixture::new().await;
        let root = fixture.project("host").await;
        let mut command = Command::new(Fixture::executable(&root));
        command.arg("host").arg(&root);
        let mut child = Child::spawn(command).unwrap();
        ready(&mut child, &root).await;
        child.stop(true, Duration::from_secs(10)).await.unwrap();
        assert!(child.try_wait().unwrap().unwrap().success());
        assert!(root.join("closed").exists());
        assert!(!root.join("interrupted").exists());
    }

    #[tokio::test]
    async fn cancellation_honors_the_controller_cleanup_deadline() {
        let fixture = Fixture::new().await;
        let root = fixture.project("controller").await;
        for (mode, grace, cleaned) in [
            ("slow", Duration::from_millis(20), false),
            ("controller", Duration::from_secs(2), true),
        ] {
            let records = root.join(mode);
            std::fs::create_dir(&records).unwrap();
            let mut command = Command::new(Fixture::executable(&root));
            command.arg(mode).arg(&records);
            let mut child = Child::spawn(command).unwrap();
            ready(&mut child, &records).await;
            let (sender, cancel) = watch::channel(false);
            sender.send(true).unwrap();
            let error = child
                .output(&cancel, Duration::from_secs(5), grace)
                .await
                .unwrap_err();
            assert_eq!(
                error.downcast_ref::<io::Error>().unwrap().kind(),
                io::ErrorKind::Interrupted
            );
            assert_eq!(records.join("cleaned").exists(), cleaned);
            if cleaned {
                assert!(records.join("stopped").exists());
                assert!(child.try_wait().unwrap().unwrap().success());
            }
        }
    }
}

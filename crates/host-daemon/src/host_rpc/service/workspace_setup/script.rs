//! Setup scripts use the same supervised PTY lifetime and terminal UI as shells.
use super::*;
use agent_process::PtyEvent;
use tokio_util::sync::CancellationToken;

enum Event {
    Output(Vec<u8>),
    Exited(i32),
    Failed(String),
}
#[derive(Default)]
struct Events {
    pending: std::sync::Mutex<std::collections::VecDeque<Event>>,
    changed: tokio::sync::Notify,
}
impl Events {
    fn send(&self, event: Event) {
        let mut pending = self.pending.lock().unwrap();
        if matches!(event, Event::Output(_))
            && pending.len() >= 32
            && let Some(index) = pending
                .iter()
                .position(|event| matches!(event, Event::Output(_)))
        {
            pending.remove(index);
        }
        pending.push_back(event);
        self.changed.notify_one();
    }
    async fn recv(&self) -> Event {
        loop {
            let changed = self.changed.notified();
            if let Some(event) = self.pending.lock().unwrap().pop_front() {
                return event;
            }
            changed.await;
        }
    }
}

impl HostRpcService {
    pub(in crate::host_rpc) async fn run_setup_script(
        &self,
        target: &SessionRef,
        generation: Uuid,
        (cwd, project_cwd): (&str, &str),
        script: Option<agent_protocol::workspace::SetupScript>,
        cancel: CancellationToken,
    ) -> Result<(), Failure> {
        let Some(script) = script else {
            self.setup_update(
                target,
                generation,
                SetupChange::Stage {
                    id: SetupStageId::SetupScript,
                    status: SetupStageStatus::Skipped,
                    exit_code: None,
                },
            )
            .await?;
            return Ok(());
        };
        let handle = format!("setup-{}", uuid::Uuid::new_v4());
        self.setup_update(
            target,
            generation,
            SetupChange::Script {
                handle: handle.clone(),
            },
        )
        .await?;
        let owner = self.inner.router.open_session();
        let receiver = Arc::new(Events::default());
        let sender = receiver.clone();
        let observer = Arc::new(move |event: &PtyEvent| {
            let event = match event {
                PtyEvent::Output { data } => Event::Output(data.clone()),
                PtyEvent::Exited { code } => Event::Exited(i32::try_from(*code).unwrap_or(1)),
                PtyEvent::Failed { message } => Event::Failed(message.clone()),
                _ => return,
            };
            sender.send(event);
        });
        #[cfg(windows)]
        let arguments = vec![
            "powershell.exe".into(),
            "-NoLogo".into(),
            "-NoProfile".into(),
            "-Command".into(),
            script.command.clone(),
        ];
        #[cfg(not(windows))]
        let arguments = vec![
            "/bin/sh".into(),
            "-c".into(),
            r#"exec "${SHELL:-/bin/sh}" -lic "$1""#.into(),
            "setup-script".into(),
            script.command.clone(),
        ];
        let launch = crate::terminals::Launch {
            directory: cwd.into(),
            arguments,
            environment: vec![
                ("BEX_PROJECT_ROOT".into(), Some(project_cwd.to_owned())),
                ("BEX_WORKTREE_PATH".into(), Some(cwd.to_owned())),
                ("COLORTERM".into(), Some(String::new())),
                ("NO_COLOR".into(), Some("1".into())),
                ("FORCE_COLOR".into(), Some("0".into())),
            ],
        };
        self.inner
            .terminals
            .start_observed(
                self.inner.router.clone(),
                owner.id(),
                handle.clone(),
                launch,
                op::TerminalSize {
                    rows: 24,
                    cols: 100,
                },
                Some(observer),
            )
            .await
            .map_err(|error| Failure::new("setup_script_failed", error))?;
        let service = self.clone();
        let id = target.clone();
        let completion = async move {
            let mut owner = owner;
            let mut owner_open = true;
            let mut buffer = Vec::new();
            loop {
                tokio::select! {
                    message = owner.recv(), if owner_open => { owner_open = message.is_some(); }
                    _ = cancel.cancelled() => {
                        service.inner.terminals.stop_setup(&handle).await;
                        return Err(Failure::new("preparation_interrupted", "Setup script cancelled"));
                    }
                    event = receiver.recv() => match event {
                        Event::Output(bytes) => service.setup_output(&id, generation, &mut buffer, &bytes).await?,
                        Event::Exited(code) => {
                            service.setup_output(&id, generation, &mut buffer, b"\n").await?;
                            service.setup_update(&id, generation, SetupChange::Stage {
                                id: SetupStageId::SetupScript,
                                status: if code == 0 { SetupStageStatus::Done } else { SetupStageStatus::Failed },
                                exit_code: Some(code),
                            }).await?;
                            if code != 0 && !script.asynchronous { return Err(Failure::new("setup_script_failed", format!("Setup script exited with {code}"))); }
                            return Ok(());
                        }
                        Event::Failed(message) => return Err(Failure::new("setup_script_failed", message)),
                    }
                }
            }
        };
        if script.asynchronous {
            let service = self.clone();
            let id = target.clone();
            tokio::spawn(async move {
                if let Err(error) = completion.await {
                    let _ = service
                        .setup_update(
                            &id,
                            generation,
                            SetupChange::Stage {
                                id: SetupStageId::SetupScript,
                                status: SetupStageStatus::Failed,
                                exit_code: None,
                            },
                        )
                        .await;
                    tracing::warn!(target:"agent",operation="workspace.script",%error);
                }
            });
            Ok(())
        } else {
            completion.await
        }
    }

    async fn setup_output(
        &self,
        id: &SessionRef,
        generation: Uuid,
        buffer: &mut Vec<u8>,
        bytes: &[u8],
    ) -> Result<(), Failure> {
        buffer.extend_from_slice(bytes);
        let Some(end) = buffer
            .iter()
            .rposition(|byte| matches!(byte, b'\r' | b'\n'))
            .map(|at| at + 1)
        else {
            if buffer.len() > 64 * 1024 {
                buffer.truncate(64 * 1024);
            }
            return Ok(());
        };
        let lines = String::from_utf8_lossy(&buffer[..end])
            .lines()
            .map(clean_line)
            .filter(|line| !line.is_empty())
            .collect::<Vec<_>>();
        self.setup_update(id, generation, SetupChange::Output(lines))
            .await?;
        buffer.drain(..end);
        Ok(())
    }
}

fn clean_line(line: &str) -> String {
    static ANSI: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    ANSI.get_or_init(|| {
        regex::Regex::new(r"\x1b(?:\[[0-?]*[ -/]*[@-~]|\][^\x07]*(?:\x07|\x1b\\))").unwrap()
    })
    .replace_all(line, "")
    .chars()
    .filter(|character| !character.is_control())
    .take(400)
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn noisy_script_preview_retains_recent_output_and_never_drops_the_exit_result() {
        let events = Events::default();
        for index in 0..40 {
            events.send(Event::Output(vec![index]));
        }
        events.send(Event::Exited(7));
        for index in 8..40 {
            let Event::Output(bytes) = events.recv().await else {
                panic!("recent output must precede exit")
            };
            assert_eq!(bytes, [index]);
        }
        assert!(matches!(events.recv().await, Event::Exited(7)));
    }

    #[test]
    fn preview_removes_terminal_control_sequences_and_bounds_unicode_lines() {
        assert_eq!(clean_line("\x1b[31mfailed\x1b[0m\r"), "failed");
        assert_eq!(clean_line(&"あ".repeat(450)).chars().count(), 400);
    }
}

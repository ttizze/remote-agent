use super::*;

pub use agent_protocol::operations::{StartAccountTerminal, StartTerminal};

/// An automatic view mount must not restart a shell admitted by another
/// owner. Explicit reconnects continue to use StartTerminal.
pub(crate) fn terminal_is_attached(
    terminals: &BTreeMap<String, Arc<Terminal>>,
    handle: &str,
    cwd: &str,
    command: Option<&str>,
) -> bool {
    terminals.get(handle).is_some_and(|terminal| {
        matches!(&terminal.target, crate::state::TerminalTarget::Shell { cwd: current, command: existing }
            if current == cwd && existing.as_deref() == command)
            && matches!(
                terminal.phase,
                TerminalPhase::Starting | TerminalPhase::Running | TerminalPhase::Suspended
            )
    })
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunTerminalScript {
    pub handle: String,
    pub cwd: String,
    pub command: String,
}

const SCRIPT_TERMINAL_SIZE: rpc::TerminalSize = rpc::TerminalSize {
    cols: 120,
    rows: 30,
};

impl Operation for RunTerminalScript {
    no_input!();
    type Output = ();
    const STALE_POLICY: StalePolicy = StalePolicy::Apply;

    fn navigation_bound(&self) -> bool {
        false
    }

    fn scheduling(&self) -> Scheduling {
        Scheduling::Terminal {
            handle: self.handle.clone(),
            starts: true,
        }
    }

    fn prepare(&mut self, snapshot: &mut Snapshot, _pane_id: &str) -> Result<(), String> {
        if terminal_is_attached(&snapshot.terminals, &self.handle, &self.cwd, None) {
            return Ok(());
        }
        prepare_terminal_start(
            snapshot,
            &self.handle,
            crate::state::TerminalTarget::Shell {
                command: None,
                cwd: self.cwd.clone(),
            },
            SCRIPT_TERMINAL_SIZE,
        )
    }

    async fn run(&self, _: (), context: &mut Execution<'_>) -> Result<(), PeerError> {
        context
            .call(&StartTerminal {
                handle: self.handle.clone(),
                command: None,
                cwd: self.cwd.clone(),
                size: SCRIPT_TERMINAL_SIZE,
            })
            .await?;
        // T3 projectScriptTerminalInput suppresses shell history by default and
        // submits once with CR. Keep the command out of retained tab state.
        WriteTerminal {
            handle: self.handle.clone(),
            data: format!(" {}\r", self.command).into_bytes(),
        }
        .run((), context)
        .await
    }

    fn apply(self, snapshot: &mut Snapshot, _pane_id: &str, _: ()) -> Vec<Effect> {
        complete_terminal_start(snapshot, &self.handle)
    }
}

impl Operation for StartTerminal {
    fn navigation_bound(&self) -> bool {
        false
    }
    rpc_operation!();
    fn scheduling(&self) -> Scheduling {
        Scheduling::Terminal {
            handle: self.handle.clone(),
            starts: true,
        }
    }
    fn prepare(&mut self, snapshot: &mut Snapshot, _pane_id: &str) -> Result<(), String> {
        prepare_terminal_start(
            snapshot,
            &self.handle,
            crate::state::TerminalTarget::Shell {
                command: self.command.clone(),
                cwd: self.cwd.clone(),
            },
            self.size,
        )
    }
    fn apply(self, snapshot: &mut Snapshot, _pane_id: &str, _output: Self::Output) -> Vec<Effect> {
        complete_terminal_start(snapshot, &self.handle)
    }
    const STALE_POLICY: StalePolicy = StalePolicy::Apply;
}

impl Operation for StartAccountTerminal {
    fn navigation_bound(&self) -> bool {
        false
    }
    rpc_operation!();
    fn scheduling(&self) -> Scheduling {
        Scheduling::Terminal {
            handle: self.handle.clone(),
            starts: true,
        }
    }
    fn prepare(&mut self, snapshot: &mut Snapshot, _pane_id: &str) -> Result<(), String> {
        prepare_terminal_start(
            snapshot,
            &self.handle,
            crate::state::TerminalTarget::Account {
                provider: self.provider,
                id: self.id.clone(),
            },
            self.size,
        )
    }
    fn apply(self, snapshot: &mut Snapshot, _pane_id: &str, _output: Self::Output) -> Vec<Effect> {
        complete_terminal_start(snapshot, &self.handle)
    }
    const STALE_POLICY: StalePolicy = StalePolicy::Apply;
}

fn prepare_terminal_start(
    snapshot: &mut Snapshot,
    handle: &str,
    target: crate::state::TerminalTarget,
    size: agent_protocol::operations::TerminalSize,
) -> Result<(), String> {
    if snapshot
        .terminals
        .get(handle)
        .is_some_and(|terminal| terminal.phase == TerminalPhase::Starting)
    {
        return Err("terminal is already starting".into());
    }
    let sequence = snapshot
        .terminals
        .get(handle)
        .map_or(0, |terminal| terminal.sequence);
    Arc::make_mut(&mut snapshot.terminals).insert(
        handle.into(),
        Arc::new(Terminal {
            target,
            size,
            phase: TerminalPhase::Starting,
            output: VecDeque::new(),
            sequence,
        }),
    );
    Ok(())
}

fn complete_terminal_start(snapshot: &mut Snapshot, handle: &str) -> Vec<Effect> {
    if snapshot.terminals.get(handle).is_some_and(|terminal| {
        matches!(
            terminal.phase,
            TerminalPhase::Starting | TerminalPhase::Suspended
        )
    }) && let Some(terminal) = shared_mut(&mut snapshot.terminals, handle)
    {
        terminal.phase = TerminalPhase::Running;
    }
    Vec::new()
}

pub use agent_protocol::operations::ResizeTerminal;

impl Operation for ResizeTerminal {
    fn navigation_bound(&self) -> bool {
        false
    }
    rpc_operation!();
    fn apply(self, snapshot: &mut Snapshot, _pane_id: &str, _output: Self::Output) -> Vec<Effect> {
        if let Some(terminal) = shared_mut(&mut snapshot.terminals, &self.handle) {
            terminal.size = self.size;
        }
        Vec::new()
    }
    fn scheduling(&self) -> Scheduling {
        Scheduling::Terminal {
            handle: self.handle.clone(),
            starts: false,
        }
    }
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WriteTerminal {
    pub handle: String,
    pub data: Vec<u8>,
}
impl rpc::RpcMethod for WriteTerminal {
    agent_protocol::operations::rpc_contract!(WriteTerminal);
    fn params(
        &self,
    ) -> Result<<Self::Contract as crate::protocol::contracts::Contract>::Params, PeerError> {
        Ok(rpc::TerminalWrite {
            process_handle: self.handle.clone(),
            data: self.data.clone(),
        })
    }
}

impl Operation for WriteTerminal {
    fn navigation_bound(&self) -> bool {
        false
    }
    no_input!();
    fn scheduling(&self) -> Scheduling {
        Scheduling::Terminal {
            handle: self.handle.clone(),
            starts: false,
        }
    }
    type Output = ();

    async fn run(
        &self,
        _: Self::Input,
        context: &mut Execution<'_>,
    ) -> Result<Self::Output, PeerError> {
        // The actor sends each paste chunk in order.
        for chunk in self.data.chunks(16 * 1024) {
            context
                .call(&WriteTerminal {
                    handle: self.handle.clone(),
                    data: chunk.to_vec(),
                })
                .await?;
        }
        Ok(())
    }
}

pub use agent_protocol::operations::DetachTerminal;

impl Operation for DetachTerminal {
    fn navigation_bound(&self) -> bool {
        false
    }
    rpc_operation!();
    fn scheduling(&self) -> Scheduling {
        Scheduling::Terminal {
            handle: self.handle.clone(),
            starts: false,
        }
    }
    fn prepare(&mut self, snapshot: &mut Snapshot, _pane_id: &str) -> Result<(), String> {
        if let Some(terminal) = shared_mut(&mut snapshot.terminals, &self.handle)
            && matches!(
                terminal.phase,
                TerminalPhase::Starting | TerminalPhase::Running | TerminalPhase::Suspended
            )
        {
            terminal.phase = TerminalPhase::Detached;
        }
        Ok(())
    }
}
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KillTerminal {
    #[serde(rename = "processHandle")]
    pub handle: String,
}
rpc::rpc_method!(KillTerminal, KillTerminal, |self| rpc::TerminalKill {
    process_handle: self.handle.clone()
});
impl Operation for KillTerminal {
    fn navigation_bound(&self) -> bool {
        false
    }
    rpc_operation!();
    fn scheduling(&self) -> Scheduling {
        Scheduling::Terminal {
            handle: self.handle.clone(),
            starts: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn automatic_mounts_only_reuse_the_same_attached_shell() {
        let handle = "shell";
        for (phase, attached) in [
            (TerminalPhase::Starting, true),
            (TerminalPhase::Running, true),
            (TerminalPhase::Suspended, true),
            (TerminalPhase::Detached, false),
            (TerminalPhase::Exited(0), false),
            (TerminalPhase::Failed("refused".into()), false),
        ] {
            let terminal = Arc::new(Terminal {
                target: crate::state::TerminalTarget::Shell {
                    cwd: "/fixture".into(),
                    command: None,
                },
                size: SCRIPT_TERMINAL_SIZE,
                phase,
                sequence: 0,
                output: VecDeque::new(),
            });
            let terminals = BTreeMap::from([(handle.into(), terminal)]);
            assert_eq!(
                terminal_is_attached(&terminals, handle, "/fixture", None),
                attached
            );
            assert!(!terminal_is_attached(&terminals, "other", "/fixture", None));
            assert!(!terminal_is_attached(
                &terminals,
                handle,
                "/different",
                None
            ));
            assert!(!terminal_is_attached(
                &terminals,
                handle,
                "/fixture",
                Some("printf once")
            ));
        }
    }

    #[test]
    fn closing_a_terminal_releases_only_its_state_and_kills_owned_processes() {
        for (phase, kills) in [
            (TerminalPhase::Starting, true),
            (TerminalPhase::Running, true),
            (TerminalPhase::Suspended, true),
            (TerminalPhase::Failed("startup failed".into()), true),
            (TerminalPhase::Exited(0), false),
            (TerminalPhase::Detached, false),
        ] {
            let terminal = Arc::new(Terminal {
                target: crate::state::TerminalTarget::Shell {
                    command: None,
                    cwd: "/fixture".into(),
                },
                size: agent_protocol::operations::TerminalSize { rows: 24, cols: 80 },
                phase,
                sequence: 0,
                output: VecDeque::new(),
            });
            let before = Snapshot {
                terminals: Arc::new(BTreeMap::from([
                    ("closing".into(), terminal.clone()),
                    ("other".into(), terminal.clone()),
                ])),
                ..Default::default()
            };
            let (closed, effects) = reduce(
                &before,
                "",
                Event::from(Intent::CloseTerminal {
                    handle: "closing".into(),
                }),
            );
            assert!(!closed.terminals.contains_key("closing"));
            assert!(Arc::ptr_eq(&closed.terminals["other"], &terminal));
            assert_eq!(effects.len(), usize::from(kills));
            assert_eq!(before.terminals.len(), 2);
            let (closed_again, effects) = reduce(
                &closed,
                "",
                Event::from(Intent::CloseTerminal {
                    handle: "closing".into(),
                }),
            );
            assert_eq!(closed_again.terminals, closed.terminals);
            assert!(effects.is_empty());
        }
    }
}

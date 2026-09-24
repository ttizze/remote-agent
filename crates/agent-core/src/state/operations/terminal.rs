use super::*;

pub use agent_protocol::operations::StartTerminal;

impl Operation for StartTerminal {
    rpc_operation!();
    fn terminal_handle(&self) -> Option<&str> {
        Some(&self.handle)
    }
    fn prepare(&mut self, snapshot: &mut Snapshot) -> Result<(), String> {
        if snapshot
            .terminals
            .get(&self.handle)
            .is_some_and(|terminal| terminal.phase == TerminalPhase::Starting)
        {
            return Err("terminal is already starting".into());
        }
        let sequence = snapshot
            .terminals
            .get(&self.handle)
            .map_or(0, |terminal| terminal.sequence);
        Arc::make_mut(&mut snapshot.terminals).insert(
            self.handle.clone(),
            Arc::new(Terminal {
                cwd: self.cwd.clone(),
                size: self.size,
                phase: TerminalPhase::Starting,
                output: VecDeque::new(),
                sequence,
            }),
        );
        Ok(())
    }
    fn apply(self, snapshot: &mut Snapshot, _output: Self::Output) -> Vec<Effect> {
        if snapshot
            .terminals
            .get(&self.handle)
            .is_some_and(|terminal| {
                matches!(
                    terminal.phase,
                    TerminalPhase::Starting | TerminalPhase::Suspended
                )
            })
            && let Some(terminal) = shared_mut(&mut snapshot.terminals, &self.handle)
        {
            terminal.phase = TerminalPhase::Running;
        }
        Vec::new()
    }
    const STALE_POLICY: StalePolicy = StalePolicy::Apply;
}

pub use agent_protocol::operations::ResizeTerminal;

impl Operation for ResizeTerminal {
    rpc_operation!();
    fn apply(self, snapshot: &mut Snapshot, _output: Self::Output) -> Vec<Effect> {
        if let Some(terminal) = shared_mut(&mut snapshot.terminals, &self.handle) {
            terminal.size = self.size;
        }
        Vec::new()
    }
    fn terminal_handle(&self) -> Option<&str> {
        Some(&self.handle)
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
    fn terminal_handle(&self) -> Option<&str> {
        Some(&self.handle)
    }
    type Output = ();

    async fn run(&self, context: &mut Execution<'_>) -> Result<Self::Output, PeerError> {
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
    rpc_operation!();
    fn terminal_handle(&self) -> Option<&str> {
        Some(&self.handle)
    }
    fn prepare(&mut self, snapshot: &mut Snapshot) -> Result<(), String> {
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
    rpc_operation!();
    fn terminal_handle(&self) -> Option<&str> {
        Some(&self.handle)
    }
}

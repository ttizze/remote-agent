use super::*;

pub use crate::client::StartTerminal;
rpc::rpc_method!(
    StartTerminal,
    crate::models::Empty,
    "host/terminal/start",
    StartTerminal,
    |self| self.clone()
);

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
    const APPLY_WHEN_STALE: bool = true;
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResizeTerminal {
    #[serde(rename = "processHandle")]
    pub handle: String,
    pub size: rpc::TerminalSize,
}
rpc::rpc_method!(
    ResizeTerminal,
    crate::models::Empty,
    "process/resizePty",
    ResizeTerminal,
    |self| self.clone()
);

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
    type Output = crate::models::Empty;
    const METHOD: &'static str = "process/writeStdin";
    fn request(&self) -> Result<crate::protocol::Call, PeerError> {
        Ok(crate::protocol::Call::WriteTerminal(rpc::TerminalWrite {
            process_handle: self.handle.clone(),
            data: self.data.clone(),
        }))
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

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DetachTerminal {
    #[serde(rename = "processHandle")]
    pub handle: String,
}
rpc::rpc_method!(
    DetachTerminal,
    crate::models::Empty,
    "host/terminal/detach",
    DetachTerminal,
    |self| self.clone()
);
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
rpc::rpc_method!(
    KillTerminal,
    crate::models::Empty,
    "process/kill",
    KillTerminal,
    |self| rpc::TerminalKill {
        process_handle: self.handle.clone()
    }
);
impl Operation for KillTerminal {
    rpc_operation!();
    fn terminal_handle(&self) -> Option<&str> {
        Some(&self.handle)
    }
}

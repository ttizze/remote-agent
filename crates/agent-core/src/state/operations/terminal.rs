use super::*;

pub use crate::client::StartTerminal;
rpc::rpc_method!(StartTerminal, Map<String, Value>, "host/terminal/start");

impl Operation for StartTerminal {
    rpc_operation!();
    fn terminal_handle(&self) -> Option<&str> {
        Some(&self.handle)
    }
    fn prepare(&mut self, snapshot: &mut Snapshot) -> Result<(), String> {
        if snapshot.terminals.contains_key(&self.handle) {
            return Err("terminal handle is already in use".into());
        }
        Arc::make_mut(&mut snapshot.terminals).insert(
            self.handle.clone(),
            Arc::new(Terminal {
                phase: TerminalPhase::Starting,
                output: VecDeque::new(),
                sequence: 0,
            }),
        );
        Ok(())
    }
    const ORDERED: bool = true;
    fn apply(self, snapshot: &mut Snapshot, _output: Self::Output) -> Vec<Effect> {
        if snapshot
            .terminals
            .get(&self.handle)
            .is_some_and(|terminal| terminal.phase == TerminalPhase::Starting)
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
rpc::rpc_method!(ResizeTerminal, Map<String, Value>, "process/resizePty");

impl Operation for ResizeTerminal {
    rpc_operation!();
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
    type Output = Map<String, Value>;
    const METHOD: &'static str = TerminalInput::METHOD;
    fn serialize_params<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        TerminalInput(&self.handle, &self.data).serialize(serializer)
    }
}
struct TerminalInput<'a>(&'a str, &'a [u8]);
impl Serialize for TerminalInput<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut params = serializer.serialize_struct("WriteTerminal", 2)?;
        params.serialize_field("processHandle", self.0)?;
        params.serialize_field("deltaBase64", &Base64Bytes(self.1))?;
        params.end()
    }
}
rpc::rpc_method!(TerminalInput<'_>, Map<String, Value>, "process/writeStdin");

impl Operation for WriteTerminal {
    fn terminal_handle(&self) -> Option<&str> {
        Some(&self.handle)
    }
    type Output = ();

    async fn run(&self, context: &mut Execution<'_>) -> Result<Self::Output, PeerError> {
        // The actor serializes every chunk of one paste; serialization encodes into the JSON buffer.
        for chunk in self.data.chunks(16 * 1024) {
            context.call(&TerminalInput(&self.handle, chunk)).await?;
        }
        Ok(())
    }
}

use super::*;
pub use agent_protocol::automations::Request as Automations;

impl Operation for Automations {
    rpc_operation!();
    fn key(&self) -> Option<OperationKey> {
        Some(OperationKey::Automations {
            session: self.session.clone(),
        })
    }
    fn prepare(&mut self, _: &mut Snapshot, _: &str) -> Result<(), String> {
        self.validate().map_err(str::to_owned)
    }
    fn outcome(output: &mut Self::Output) -> Outcome {
        Outcome::Automations {
            details: std::mem::take(output),
        }
    }
}

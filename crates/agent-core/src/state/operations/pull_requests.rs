use super::*;
pub use agent_protocol::pull_requests::Request as PullRequests;

impl Operation for PullRequests {
    rpc_operation!();
    fn key(&self) -> Option<OperationKey> {
        Some(OperationKey::PullRequests {
            session: self.session.clone(),
        })
    }
    fn prepare(&mut self, _: &mut Snapshot, _: &str) -> Result<(), String> {
        self.validate().map_err(str::to_owned)
    }
    fn outcome(output: &mut Self::Output) -> Outcome {
        Outcome::PullRequests {
            details: std::mem::take(output),
        }
    }
}

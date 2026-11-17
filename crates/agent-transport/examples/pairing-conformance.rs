//! Finite conformance with the Lean specification; run `just lean-pairing`.
//! This calls production authorization and commits each update just as the Host
//! does serially. It does not exercise persistence, locks, or network sessions.
use agent_transport::transport::{Identity, NodeId, TransportError, Trust, authorize};
use serde::Deserialize;
use std::{error::Error, fs::File, io::BufRead};

#[derive(Deserialize)]
struct Snapshot {
    allowed: Vec<usize>,
    invitations: Vec<(u128, u64)>,
}
impl Snapshot {
    fn trust(&self, nodes: &[NodeId; 3]) -> Trust {
        Trust {
            allowed: self.allowed.iter().map(|&index| nodes[index]).collect(),
            invitations: self
                .invitations
                .iter()
                .map(|&(token, expiry)| (uuid::Uuid::from_u128(token), expiry))
                .collect(),
        }
    }
}

#[derive(Deserialize)]
struct Request {
    node: usize,
    invitation: Option<u128>,
    now: u64,
}

#[derive(Deserialize)]
struct Step {
    request: Request,
    outcome: String,
    state: Snapshot,
}

#[derive(Deserialize)]
struct Trace {
    initial: Snapshot,
    steps: Vec<Step>,
}

fn main() -> Result<(), Box<dyn Error>> {
    let path = std::env::args().nth(1).ok_or("expected Lean trace file")?;
    let nodes = std::array::from_fn(|_| Identity::generate().node_id());
    let mut traces = 0;
    let mut requests = 0;
    for line in std::io::BufReader::new(File::open(path)?).lines() {
        let trace: Trace = serde_json::from_str(&line?)?;
        let mut trust = trace.initial.trust(&nodes);
        for (index, step) in trace.steps.into_iter().enumerate() {
            let request = step.request;
            let outcome = match authorize(
                &trust,
                nodes[request.node],
                request.invitation.map(uuid::Uuid::from_u128),
                request.now,
            ) {
                Ok(Some(next)) => {
                    trust = next;
                    "paired"
                }
                Ok(None) => "existing",
                Err(TransportError::Unauthorized) => "denied",
                Err(error) => return Err(error.into()),
            };
            assert_eq!(outcome, step.outcome, "trace {traces}, step {index}");
            assert!(
                trust == step.state.trust(&nodes),
                "state differs at trace {traces}, step {index}"
            );
            requests += 1;
        }
        traces += 1;
    }
    assert!(traces > 0, "Lean generated no traces");
    println!("Lean/Rust conformance: {traces} traces, {requests} requests matched");
    Ok(())
}

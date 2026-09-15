//! Host-owned routing for authenticated clients and independent agent backends.
//! Backend adapters translate their protocols into the shared conversation API.

mod providers;
pub(crate) mod routing;
mod service;
mod thread_watch;

pub use routing::{HostSession, SessionId};
pub use service::HostRpcService;

use agent_core::peer::RpcMessageError;
use serde::Serialize;
use serde_json::value::RawValue;

#[derive(Debug, Serialize, thiserror::Error)]
#[serde(untagged)]
pub(crate) enum Failure {
    #[error("{message}")]
    Host { code: &'static str, message: String },
    #[error("{0}")]
    Upstream(Box<RawValue>),
}
impl From<RpcMessageError> for Failure {
    fn from(error: RpcMessageError) -> Self {
        Self::new("invalid_params", error)
    }
}

impl From<crate::DesktopProjectError> for Failure {
    fn from(error: crate::DesktopProjectError) -> Self {
        Self::new("desktop_project_state_unavailable", error)
    }
}

impl Failure {
    pub(crate) fn new(code: &'static str, error: impl std::fmt::Display) -> Self {
        Self::Host {
            code,
            message: error.to_string(),
        }
    }
}
pub(crate) async fn run_handler<
    P,
    R,
    E: std::fmt::Display,
    F: Future<Output = Result<R, String>>,
>(
    params: Result<P, E>,
    code: &'static str,
    run: impl FnOnce(P) -> F,
) -> Result<R, Failure> {
    let params = params.map_err(|error| Failure::new(code, error))?;
    run(params).await.map_err(|error| Failure::new(code, error))
}

fn invalid_message(error: impl std::fmt::Display) -> String {
    format!("invalid raw JSONL message: {error}")
}

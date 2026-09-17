//! Host-owned routing for authenticated clients and independent agent backends.
//! Backend adapters translate their protocols into the shared conversation API.

mod codex;
pub(crate) mod routing;
mod service;
mod session_actor;

pub use routing::{HostSession, SessionId};
pub use service::HostRpcService;

use agent_core::{client as op, models::Empty};
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(tag = "method", content = "params")]
pub(crate) enum AccountRequest {
    #[serde(rename = "host/account/list")]
    List(Empty),
    #[serde(rename = "host/account/select")]
    Select(op::SelectAccount),
    #[serde(rename = "host/account/logout")]
    Logout(op::LogoutAccount),
    #[serde(rename = "host/account/login/start")]
    LoginStart(op::StartAccountLogin),
    #[serde(rename = "host/account/login/submit")]
    LoginSubmit(op::SubmitAccountLogin),
    #[serde(rename = "host/account/login/status")]
    LoginStatus(op::ReadAccountLogin),
    #[serde(rename = "host/account/login/cancel")]
    LoginCancel(op::CancelAccountLogin),
}

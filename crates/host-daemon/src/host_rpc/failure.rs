//! Host RPC faults. Delivery evidence is independent of provider error text.
use agent_transport::peer::{Delivery, RpcMessageError};
use codex_app_server::Error as AppServerError;
use serde::Serialize;
use serde_json::value::RawValue;

#[derive(Debug, Serialize, thiserror::Error)]
#[serde(untagged)]
pub(super) enum Failure {
    #[error("{message}")]
    Host {
        code: &'static str,
        message: String,
        delivery: Delivery,
    },
    #[error("{provider_error}")]
    Upstream {
        #[serde(rename = "providerError")]
        provider_error: Box<RawValue>,
        message: String,
        delivery: Delivery,
    },
}
impl From<RpcMessageError> for Failure {
    fn from(error: RpcMessageError) -> Self {
        Self::new("invalid_params", error)
    }
}

impl From<serde_json::Error> for Failure {
    fn from(error: serde_json::Error) -> Self {
        Self::new("invalid_params", error)
    }
}

impl From<AppServerError> for Failure {
    fn from(error: AppServerError) -> Self {
        Self::unknown("codex_unavailable", error)
    }
}

impl Failure {
    pub(super) fn before_submission(mut self) -> Self {
        match &mut self {
            Self::Host { delivery, .. } | Self::Upstream { delivery, .. } => {
                *delivery = Delivery::NotSent
            }
        }
        self
    }
    pub(super) fn delivery(&self) -> Delivery {
        match self {
            Self::Host { delivery, .. } | Self::Upstream { delivery, .. } => *delivery,
        }
    }

    pub(super) fn upstream(provider_error: Box<RawValue>) -> Self {
        let value: serde_json::Value =
            serde_json::from_str(provider_error.get()).unwrap_or_default();
        let message = value["message"]
            .as_str()
            .filter(|message| !message.trim().is_empty())
            .map(agent_transport::diagnostics::sanitize)
            .unwrap_or_else(|| "接続先で操作に失敗しました。もう一度お試しください。".into());
        Self::Upstream {
            message,
            provider_error,
            delivery: Delivery::Unknown,
        }
    }
    pub(super) fn unknown(code: &'static str, error: impl std::fmt::Display) -> Self {
        Self::Host {
            code,
            message: format!("{error:#}"),
            delivery: Delivery::Unknown,
        }
    }
    pub(super) fn new(code: &'static str, error: impl std::fmt::Display) -> Self {
        Self::Host {
            code,
            message: format!("{error:#}"),
            delivery: Delivery::NotSent,
        }
    }
}

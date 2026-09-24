use serde::Serialize;
/// Delivery evidence is independent of provider availability and display text.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Delivery {
    NotSent,
    #[default]
    Unknown,
}
impl Delivery {
    pub fn from_error(error: &serde_json::value::RawValue) -> Self {
        #[derive(serde::Deserialize)]
        struct Evidence {
            #[serde(default)]
            delivery: Delivery,
        }
        serde_json::from_str::<Evidence>(error.get()).map_or(Self::Unknown, |value| value.delivery)
    }
}

#[cfg(test)]
mod delivery_tests {
    use super::*;
    #[test]
    fn delivery_evidence_is_independent_of_messages_and_defaults_to_unknown() {
        for message in [
            "rejected",
            "submission outcome unknown: detail",
            "任意の診断情報",
        ] {
            for delivery in [Delivery::NotSent, Delivery::Unknown] {
                let raw = serde_json::value::to_raw_value(
                    &serde_json::json!({"message":message,"delivery":delivery}),
                )
                .unwrap();
                assert_eq!(Delivery::from_error(&raw), delivery);
            }
            let raw =
                serde_json::value::to_raw_value(&serde_json::json!({"message":message})).unwrap();
            assert_eq!(Delivery::from_error(&raw), Delivery::Unknown);
        }
    }
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum PeerError {
    #[error("invalid RPC message: {0}")]
    InvalidMessage(String),
    #[error("Invalid {method} response: {reason}")]
    InvalidResponse {
        method: String,
        reason: String,
        raw: String,
        sequence: Option<u64>,
    },
    #[error("connection closed: {0}")]
    ConnectionClosed(String),
    #[error("request {method} timed out")]
    RequestTimeout { method: String },
    #[error("request ID space exhausted")]
    RequestIdExhausted,
    #[error("remote RPC error: {error}")]
    Remote {
        error: String,
        delivery: Delivery,
        sequence: Option<u64>,
    },
}

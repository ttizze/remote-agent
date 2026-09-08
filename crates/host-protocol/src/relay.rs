use std::fmt;

use serde::{Deserialize, Serialize};
use url::Url;

/// Relay credentials authorize only resource use. SSH independently authorizes
/// all application access. Serialize only to a pairing payload or secure store.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayEndpoint {
    pub relay_url: String,
    pub relay_token: String,
    pub runner_id: String,
}

impl fmt::Debug for RelayEndpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RelayEndpoint")
            .field("runner_id", &self.runner_id)
            .field("relay_url", &self.relay_url)
            .field("relay_token", &"[redacted]")
            .finish()
    }
}

impl RelayEndpoint {
    pub fn validate(&self) -> Result<(), RelayEndpointError> {
        self.parsed_url()?;
        if self.runner_id.is_empty()
            || self.runner_id.len() > 512
            || self.runner_id.chars().any(char::is_control)
        {
            return Err(RelayEndpointError(
                "runner ID must contain 1 to 512 non-control bytes",
            ));
        }
        if self.relay_token.is_empty() || self.relay_token.len() > 512 {
            return Err(RelayEndpointError(
                "relay token must contain 1 to 512 bytes",
            ));
        }
        Ok(())
    }

    pub fn socket_url(&self, role: &str) -> Result<Url, RelayEndpointError> {
        self.validate()?;
        let mut url = self.parsed_url()?;
        url.query_pairs_mut()
            .append_pair("vsn", "2.0.0")
            .append_pair("token", &self.relay_token)
            .append_pair("runner_id", &self.runner_id)
            .append_pair("role", role);
        Ok(url)
    }

    fn parsed_url(&self) -> Result<Url, RelayEndpointError> {
        let url =
            Url::parse(&self.relay_url).map_err(|_| RelayEndpointError("invalid relay URL"))?;
        let authority = self
            .relay_url
            .split_once("://")
            .map(|(_, rest)| rest.split(['/', '?', '#']).next().unwrap_or_default())
            .unwrap_or_default();
        if !matches!(url.scheme(), "ws" | "wss")
            || url.host_str().is_none()
            || authority.is_empty()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
            || self
                .relay_url
                .chars()
                .any(|c| c.is_whitespace() || c.is_control())
        {
            return Err(RelayEndpointError(
                "relay URL must be ws:// or wss:// with a host and no userinfo or fragment",
            ));
        }
        if url
            .query_pairs()
            .any(|(key, _)| matches!(key.as_ref(), "token" | "runner_id" | "role" | "vsn"))
        {
            return Err(RelayEndpointError(
                "relay URL must not contain authentication or protocol query parameters",
            ));
        }
        Ok(url)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct RelayEndpointError(&'static str);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credentials_are_encoded_once_and_url_cannot_override_them() {
        let mut endpoint = RelayEndpoint {
            relay_url: "ws://localhost/socket/websocket?region=local".into(),
            relay_token: "a?b&c".into(),
            runner_id: "runner/one".into(),
        };
        let url = endpoint.socket_url("mobile").unwrap();
        assert_eq!(
            url.query_pairs().find(|(k, _)| k == "token").unwrap().1,
            "a?b&c"
        );
        assert!(!format!("{endpoint:?}").contains("a?b&c"));
        for bad in [
            "ws:///socket",
            "https://localhost/socket",
            "ws://localhost/socket?token=other",
            "ws://user:pass@localhost/socket",
            "ws://localhost/socket#secret",
        ] {
            endpoint.relay_url = bad.into();
            assert!(endpoint.validate().is_err(), "{bad}");
        }
    }
}

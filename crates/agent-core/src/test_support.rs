//! Explicit Host registry fixtures, independent of model identifiers and cached history.
use agent_protocol::providers::{ProviderAvailability, ProviderInstance, ProviderRef};
use std::sync::Arc;

pub(crate) fn instances() -> Arc<Vec<ProviderInstance>> {
    Arc::new(
        [
            ("codex", "codex", "Codex"),
            ("claude", "claudeAgent", "Claude"),
        ]
        .into_iter()
        .map(|(id, driver, name)| ProviderInstance {
            reference: ProviderRef {
                instance_id: id.parse().unwrap(),
                driver: driver.parse().unwrap(),
            },
            display_name: name.into(),
            availability: ProviderAvailability::Ready,
            capabilities: Default::default(),
            requires_account: true,
        })
        .collect(),
    )
}

use agent_core::state::{
    Snapshot,
    operations::{LoadModels, Operation},
};
use agent_protocol::{operations::ModelPage, session::ProviderKind};

// A fixture corpus contains every provider; production publishes each independently.
pub(crate) fn apply_catalog(snapshot: &mut Snapshot, mut catalog: ModelPage) {
    let mut errors = catalog.provider_errors.take().unwrap_or_default();
    for provider in [
        ProviderKind::Codex,
        ProviderKind::Claude,
        ProviderKind::DeepSeek,
    ] {
        let data = catalog
            .data
            .iter()
            .filter(|model| model.model.provider == provider)
            .cloned()
            .collect();
        let provider_errors = errors
            .remove(provider.key())
            .map(|error| [(provider.key().into(), error)].into_iter().collect());
        LoadModels { provider }.apply(
            snapshot,
            "",
            ModelPage {
                data,
                next_cursor: None,
                provider_errors,
            },
        );
    }
}

use super::*;

use agent_protocol::composer::{ComposerCandidate, ComposerCatalog};
pub use agent_protocol::operations::LoadComposerCatalog;
use agent_protocol::session::ProviderKind;

pub(crate) fn prefetch_composer_catalog(snapshot: &mut Snapshot) -> Option<Effect> {
    if !snapshot.connected
        || snapshot.navigation.draft_key.is_empty()
        || snapshot
            .composer_catalog
            .as_ref()
            .is_some_and(|catalog| catalog.cwd == snapshot.navigation.cwd)
    {
        return None;
    }
    let mut operation = LoadComposerCatalog {
        cwd: snapshot.navigation.cwd.clone(),
    };
    operation.prepare(snapshot).unwrap();
    Some(Effect::execute(operation))
}

impl Operation for LoadComposerCatalog {
    type Input = Arc<ComposerCatalog>;
    fn capture(&self, snapshot: &Snapshot) -> Result<Self::Input, PeerError> {
        snapshot
            .composer_catalog
            .clone()
            .ok_or_else(|| PeerError::InvalidMessage("catalog load is not prepared".into()))
    }
    const BACKGROUND: bool = true;
    type Output = (Arc<ComposerCatalog>, ComposerCatalog);
    // Unrelated epoch changes do not invalidate metadata for the same catalog.
    const STALE_POLICY: StalePolicy = StalePolicy::Apply;
    fn prepare(&mut self, snapshot: &mut Snapshot) -> Result<(), String> {
        let candidates = snapshot
            .composer_catalog
            .as_ref()
            .filter(|catalog| catalog.cwd == self.cwd)
            .map(|catalog| catalog.candidates.clone())
            .unwrap_or_default();
        snapshot.composer_catalog = Some(Arc::new(ComposerCatalog {
            cwd: self.cwd.clone(),
            loading: true,
            candidates,
            ..Default::default()
        }));
        Ok(())
    }
    async fn run(
        &self,
        source: Self::Input,
        context: &mut Execution<'_>,
    ) -> Result<Self::Output, PeerError> {
        let catalog = context
            .call(self)
            .await
            .unwrap_or_else(|_| unavailable_catalog(&self.cwd, &source.candidates));
        Ok((source, catalog))
    }
    fn rejected_output(&self, source: Self::Input) -> Option<Self::Output> {
        let catalog = unavailable_catalog(&self.cwd, &source.candidates);
        Some((source, catalog))
    }
    fn apply(self, snapshot: &mut Snapshot, (source, catalog): Self::Output) -> Vec<Effect> {
        if snapshot.navigation.cwd == self.cwd
            && snapshot
                .composer_catalog
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, &source))
        {
            snapshot.composer_catalog = Some(Arc::new(catalog));
        }
        Vec::new()
    }
}

fn unavailable_catalog(cwd: &str, candidates: &[ComposerCandidate]) -> ComposerCatalog {
    ComposerCatalog {
        cwd: cwd.into(),
        candidates: candidates.to_vec(),
        errors: [ProviderKind::Codex, ProviderKind::Claude]
            .into_iter()
            .map(|provider| {
                (
                    provider,
                    vec!["候補を取得できませんでした。再度 @ または / を入力してください。".into()],
                )
            })
            .collect(),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    proptest::proptest! {
        #[test]
        fn catalog_replies_belong_to_their_loading_source(
            same_source in proptest::bool::ANY,
            same_cwd in proptest::bool::ANY,
            connected in proptest::bool::ANY,
        ) {
            let source = Arc::new(ComposerCatalog { cwd: "/first".into(), loading: true, ..Default::default() });
            let current = if same_source { source.clone() } else { Arc::new((*source).clone()) };
            let mut snapshot = Snapshot {
                connected,
                composer_catalog: if connected { Some(current.clone()) } else { None },
                navigation: Arc::new(Navigation { cwd: if same_cwd { "/first" } else { "/second" }.into(), ..Default::default() }),
                ..Default::default()
            };
            let before = snapshot.composer_catalog.clone();
            let output = ComposerCatalog { cwd: "/first".into(), ..Default::default() };
            // A reply survives unrelated navigation epochs only while the
            // exact loading catalog still owns it. Account/reconnect resets
            // replace that owner even when all catalog values are identical.
            LoadComposerCatalog { cwd: "/first".into() }.complete(&mut snapshot, (source, output), false).unwrap();
            if same_source && same_cwd && connected {
                proptest::prop_assert!(!snapshot.composer_catalog.as_ref().unwrap().loading);
            } else {
                proptest::prop_assert_eq!(snapshot.composer_catalog, before);
            }
        }
    }
}

use super::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadComposerCatalog {
    pub cwd: String,
}
impl Operation for LoadComposerCatalog {
    type Output = crate::composer::ComposerCatalog;
    fn prepare(&mut self, snapshot: &mut Snapshot) -> Result<(), String> {
        snapshot.composer_catalog = Some(Arc::new(Self::Output {
            cwd: self.cwd.clone(),
            loading: true,
            ..Default::default()
        }));
        Ok(())
    }
    async fn run(&self, context: &mut Execution<'_>) -> Result<Self::Output, PeerError> {
        Ok(context.call(self).await.unwrap_or_else(|_| Self::Output {
            cwd: self.cwd.clone(),
            errors: vec!["候補を取得できませんでした。再度 @ または / を入力してください。".into()],
            ..Default::default()
        }))
    }
    fn apply(self, snapshot: &mut Snapshot, catalog: Self::Output) -> Vec<Effect> {
        if snapshot.navigation.cwd == self.cwd {
            snapshot.composer_catalog = Some(Arc::new(catalog));
        }
        Vec::new()
    }
}

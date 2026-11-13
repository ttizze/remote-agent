//! Started providers, providers that failed to start, and reads fanned out
//! across all of them. One provider's failure never hides the others.
use super::{Agent, Backend, Configuration, Discovery, Failure, Identity, Provider};
use crate::dictation::Dictation;
use agent_protocol::{
    composer::{ComposerCandidate, ComposerCatalog},
    operations as op,
    session::ProviderKind,
};
use std::{
    collections::{BTreeMap, HashMap},
    path::Path,
    path::PathBuf,
    sync::Arc,
};

// Finish discovery before the client's request deadline, even when a native
// provider is still initializing. Healthy providers must remain usable.
const MODEL_LIST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

fn ordered_candidates(mut candidates: Vec<ComposerCandidate>) -> Vec<ComposerCandidate> {
    let key = |candidate: &ComposerCandidate| {
        (
            candidate.invocation.name.to_lowercase(),
            candidate.invocation.path.clone(),
            candidate.invocation.provider,
        )
    };
    if candidates.len() > 1 && !candidates.iter().map(key).is_sorted() {
        candidates.sort_by_cached_key(key);
    }
    candidates.dedup_by(|a, b| a.invocation == b.invocation);
    candidates
}

pub(crate) struct Registry {
    agents: HashMap<ProviderKind, Arc<dyn Provider>>,
    startup_errors: BTreeMap<ProviderKind, Failure>,
}

impl Registry {
    /// Also returns the dictation backend supplied by the backends, if any.
    pub(crate) fn new(backends: impl IntoIterator<Item = Backend>) -> (Self, Dictation) {
        let mut agents = HashMap::new();
        let mut startup_errors = BTreeMap::new();
        let mut dictation = Dictation::default();
        for backend in backends {
            match backend.agent {
                Ok(agent) => {
                    agents.insert(backend.provider, agent);
                }
                Err(error) => {
                    startup_errors.insert(backend.provider, error);
                }
            }
            if let Some(backend) = backend.dictation {
                dictation = backend;
            }
        }
        (
            Self {
                agents,
                startup_errors,
            },
            dictation,
        )
    }

    fn provider(&self, provider: ProviderKind) -> Result<Arc<dyn Provider>, Failure> {
        self.agents.get(&provider).cloned().ok_or_else(|| {
            self.startup_errors
                .get(&provider)
                .cloned()
                .unwrap_or_else(|| {
                    Failure::new(
                        "provider_unavailable",
                        format!("{provider:?} is unavailable"),
                    )
                })
        })
    }

    pub(crate) fn get(&self, provider: ProviderKind) -> Result<Arc<dyn Agent>, Failure> {
        self.provider(provider).map(|agent| agent as Arc<dyn Agent>)
    }
    pub(crate) fn identity(&self, provider: ProviderKind) -> Result<Arc<dyn Identity>, Failure> {
        self.provider(provider)
            .map(|agent| agent as Arc<dyn Identity>)
    }
    pub(crate) fn configuration(
        &self,
        provider: ProviderKind,
    ) -> Result<Arc<dyn Configuration>, Failure> {
        self.provider(provider)
            .map(|agent| agent as Arc<dyn Configuration>)
    }

    pub(crate) fn discovery(&self, provider: ProviderKind) -> Result<Arc<dyn Discovery>, Failure> {
        self.provider(provider)
            .map(|agent| agent as Arc<dyn Discovery>)
    }

    /// Started providers in provider order.
    pub(crate) fn all(&self) -> Vec<(ProviderKind, Arc<dyn Provider>)> {
        let mut agents: Vec<_> = self.agents.iter().map(|(p, a)| (*p, a.clone())).collect();
        agents.sort_by_key(|(p, _)| *p);
        agents
    }

    pub(crate) fn startup_errors(&self) -> impl Iterator<Item = (ProviderKind, &Failure)> {
        self.startup_errors
            .iter()
            .map(|(provider, error)| (*provider, error))
    }

    /// Startup and one read failure per provider share the same provider ordering.
    pub(crate) fn error_summary(&self, errors: BTreeMap<ProviderKind, String>) -> Option<String> {
        let errors = self
            .startup_errors()
            .map(|(provider, error)| (provider, error.to_string()))
            .chain(errors)
            .collect::<BTreeMap<_, _>>();
        (!errors.is_empty()).then(|| errors.into_values().collect::<Vec<_>>().join("\n"))
    }

    pub(crate) async fn accounts(&self) -> op::Accounts {
        let mut combined = op::Accounts {
            accounts: Vec::new(),
            selected: HashMap::new(),
            errors: self
                .startup_errors()
                .map(|(provider, error)| (provider, error.to_string()))
                .collect(),
        };
        let listings = futures_util::future::join_all(self.all().into_iter().map(
            |(provider, agent)| async move { (provider, Identity::list(agent.as_ref()).await) },
        ))
        .await;
        for (provider, result) in listings {
            match result {
                Ok(accounts) => {
                    combined.accounts.extend(accounts.accounts);
                    combined.selected.extend(accounts.selected);
                    combined.errors.extend(accounts.errors);
                }
                Err(error) => {
                    combined.errors.insert(provider, error.to_string());
                }
            }
        }
        combined
    }

    pub(crate) async fn catalog(&self, cwd: &str) -> ComposerCatalog {
        let results = futures_util::future::join_all(
            self.all()
                .into_iter()
                .map(|(_, agent)| async move { agent.catalog(cwd).await }),
        )
        .await;
        let mut catalog = ComposerCatalog {
            cwd: cwd.to_owned(),
            ..Default::default()
        };
        for (provider, error) in self.startup_errors.iter() {
            catalog.errors.insert(*provider, vec![error.to_string()]);
        }
        for result in results {
            catalog.candidates.extend(result.candidates);
            catalog.errors.extend(result.errors);
        }
        catalog.candidates = ordered_candidates(catalog.candidates);
        catalog
    }

    pub(crate) async fn models(&self, params: &op::ListModels) -> Result<op::ModelPage, Failure> {
        // A Host may configure only some of the client's known providers.
        if !self.agents.contains_key(&params.provider)
            && !self.startup_errors.contains_key(&params.provider)
        {
            return Ok(op::ModelPage::default());
        }
        let result = tokio::time::timeout(MODEL_LIST_TIMEOUT, async {
            self.configuration(params.provider)?.models(params).await
        })
        .await
        .unwrap_or_else(|_| {
            Err(Failure::new(
                "provider_timeout",
                "モデル一覧の取得がタイムアウトしました。もう一度お試しください。",
            ))
        });
        match result {
            Ok(page) => Ok(page),
            Err(error) => Ok(op::ModelPage {
                provider_errors: Some(
                    [(params.provider.key().into(), serde_json::to_value(error)?)]
                        .into_iter()
                        .collect(),
                ),
                ..Default::default()
            }),
        }
    }

    /// Identifies the native storage the providers read, independent of path aliases.
    pub(crate) fn storage_scope(&self) -> Result<String, serde_json::Error> {
        storage_scope(self.all().into_iter().filter_map(|(p, a)| {
            a.history_scope_directory()
                .map(|path| (p, canonical_storage_path(path)))
        }))
    }

    pub(crate) fn data_recipients(&self) -> Vec<String> {
        self.all()
            .into_iter()
            .filter(|(_, a)| a.availability().is_ok())
            .map(|(_, agent)| agent.data_recipient().into())
            .collect()
    }

    pub(crate) fn errors(&self) -> serde_json::Value {
        let mut errors: serde_json::Map<_, _> = self
            .all()
            .into_iter()
            .filter_map(|(p, a)| {
                a.availability().err().map(|e| {
                    (
                        p.key().to_owned(),
                        serde_json::to_value(e).expect("failure serializes"),
                    )
                })
            })
            .collect();
        errors.extend(self.startup_errors.iter().map(|(provider, error)| {
            (
                provider.key().to_owned(),
                serde_json::to_value(error).expect("failure serializes"),
            )
        }));
        serde_json::Value::Object(errors)
    }

    pub(crate) async fn discard_workspace_processes(
        &self,
        directory: &Path,
    ) -> Result<(), Failure> {
        for (_, agent) in self.all() {
            agent.discard_workspace_processes(directory).await?;
        }
        Ok(())
    }

    pub(crate) async fn shutdown(&self) {
        for (_, agent) in self.all() {
            agent.shutdown().await;
        }
    }
}

fn storage_scope(
    areas: impl IntoIterator<Item = (ProviderKind, PathBuf)>,
) -> Result<String, serde_json::Error> {
    let areas: BTreeMap<_, _> = areas
        .into_iter()
        .map(|(provider, path)| (provider.key().to_owned(), path))
        .collect();
    let bytes = serde_json::to_vec(&areas)?;
    let digest = ring::digest::digest(&ring::digest::SHA256, &bytes);
    Ok(digest
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

// Resolve existing ancestors too: a newly created native directory must not
// change a scope merely because /var is a symlink to /private/var on macOS.
fn canonical_storage_path(path: &Path) -> PathBuf {
    if let Ok(path) = dunce::canonicalize(path) {
        return path;
    }
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) if !parent.as_os_str().is_empty() => {
            canonical_storage_path(parent).join(name)
        }
        _ => std::env::current_dir().unwrap_or_default().join(path),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_candidates_keep_unicode_name_path_provider_order_and_stable_ties() {
        use ProviderKind::{Claude, Codex};
        use agent_protocol::composer::{
            Invocation,
            InvocationKind::{Plugin, Skill},
        };
        let candidate =
            |provider, kind, name: &str, path: &str, description: &str| ComposerCandidate {
                invocation: Invocation {
                    provider,
                    kind,
                    name: name.into(),
                    path: path.into(),
                },
                description: description.into(),
            };
        let candidates = vec![
            candidate(Codex, Skill, "Beta", "/b", "beta"),
            candidate(Claude, Skill, "alpha", "/a", "claude"),
            candidate(Codex, Skill, "ALPHA", "/z", "uppercase"),
            candidate(Codex, Skill, "alpha", "/a", "first alpha"),
            candidate(Codex, Skill, "alpha", "/a", "ignored adjacent duplicate"),
            candidate(Codex, Plugin, "alpha", "/a", "different kind"),
            candidate(Codex, Skill, "alpha", "/a", "kept separated duplicate"),
            candidate(Codex, Skill, "Alpha", "/a", "different original case"),
            candidate(Codex, Skill, "ístanbul", "/a", "accent"),
            candidate(Codex, Skill, "İ", "/a", "unicode uppercase"),
            candidate(Codex, Skill, "i\u{307}", "/a", "unicode lowercase"),
        ];
        let expected = [3, 5, 6, 7, 1, 2, 0, 9, 10, 8]
            .into_iter()
            .map(|index| candidates[index].clone())
            .collect::<Vec<_>>();
        assert_eq!(ordered_candidates(candidates), expected);
        assert_eq!(ordered_candidates(expected.clone()), expected);
        assert_eq!(ordered_candidates(Vec::new()), Vec::new());
        assert_eq!(
            ordered_candidates(vec![expected[0].clone()]),
            vec![expected[0].clone()]
        );
    }

    #[tokio::test]
    async fn startup_errors_stay_ordered_and_account_errors_keep_their_provider() {
        for providers in [
            [
                ProviderKind::DeepSeek,
                ProviderKind::Claude,
                ProviderKind::Codex,
            ],
            [
                ProviderKind::Claude,
                ProviderKind::Codex,
                ProviderKind::DeepSeek,
            ],
            [
                ProviderKind::Codex,
                ProviderKind::DeepSeek,
                ProviderKind::Claude,
            ],
        ] {
            let (registry, _) = Registry::new(providers.map(|provider| {
                Backend::unavailable(provider, format!("{} startup failed", provider.key()))
            }));
            let expected = "codex startup failed\nclaude startup failed\ndeepseek startup failed";
            assert_eq!(
                registry
                    .startup_errors()
                    .map(|(_, error)| error.to_string())
                    .collect::<Vec<_>>()
                    .join("\n"),
                expected
            );
            assert_eq!(
                registry.accounts().await.errors,
                providers
                    .map(|provider| (provider, format!("{} startup failed", provider.key())))
                    .into()
            );
        }
    }

    #[test]
    fn storage_scope_has_a_fixed_encoding() {
        let areas = [
            (ProviderKind::Codex, "/fixtures/codex".into()),
            (ProviderKind::Claude, "/fixtures/claude".into()),
        ];
        assert_eq!(
            storage_scope(areas).unwrap(),
            "8170d6192a00957e1b3b4a3da16963137015f8d36f1efaa8b8b347090f78476b",
        );
    }

    proptest::proptest! {
        #[test]
        fn storage_scope_ignores_input_order_but_tracks_provider_paths(
            codex in "[a-zA-Z0-9/_ .あ界é-]{1,64}",
            claude in "[a-zA-Z0-9/_ .あ界é-]{1,64}",
        ) {
            let codex = std::path::PathBuf::from(codex);
            let claude = std::path::PathBuf::from(claude);
            let original = storage_scope([
                (ProviderKind::Codex, codex.clone()),
                (ProviderKind::Claude, claude.clone()),
            ]).unwrap();
            proptest::prop_assert_eq!(&original, &storage_scope([
                (ProviderKind::Claude, claude.clone()),
                (ProviderKind::Codex, codex.clone()),
            ]).unwrap());
            for areas in [
                [(ProviderKind::Codex, codex.join("different")), (ProviderKind::Claude, claude.clone())],
                [(ProviderKind::Codex, codex.clone()), (ProviderKind::Claude, claude.join("different"))],
            ] {
                proptest::prop_assert_ne!(&original, &storage_scope(areas).unwrap());
            }
            proptest::prop_assert_ne!(&original, &storage_scope([
                (ProviderKind::Codex, codex),
            ]).unwrap());
        }
    }

    #[test]
    fn creating_native_storage_does_not_change_its_identity() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("native").join("projects");
        let before = canonical_storage_path(&path);
        std::fs::create_dir_all(&path).unwrap();
        let scope = storage_scope([(ProviderKind::Codex, before)]).unwrap();
        assert_eq!(
            scope,
            storage_scope([(ProviderKind::Codex, canonical_storage_path(&path)),]).unwrap()
        );
        #[cfg(unix)]
        {
            let alias = root.path().join("alias");
            std::os::unix::fs::symlink(root.path(), &alias).unwrap();
            assert_eq!(
                scope,
                storage_scope([(
                    ProviderKind::Codex,
                    canonical_storage_path(&alias.join("native/projects"))
                ),])
                .unwrap()
            );
        }
    }
}

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

#[cfg(test)]
mod tests;

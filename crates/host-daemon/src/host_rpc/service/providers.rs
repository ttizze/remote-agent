//! Registry ownership: reconcile saved instances before exposing their resources.
use super::*;
use agent_protocol::providers::*;
use std::path::{Path, PathBuf};

pub(super) struct Registered {
    pub(super) generation: uuid::Uuid,
    pub(super) config: ProviderConfig,
    pub(super) backend: Result<Arc<dyn Agent>, Failure>,
}

pub(super) fn default_config(
    driver: &str,
    program: Option<&Path>,
    home: Option<&Path>,
) -> anyhow::Result<ProviderConfig> {
    let mut config = serde_json::Map::new();
    if let Some(program) = program {
        config.insert(
            "binaryPath".into(),
            program.to_string_lossy().to_string().into(),
        );
    }
    if let Some(home) = home {
        config.insert("homePath".into(), home.to_string_lossy().to_string().into());
    }
    Ok(ProviderConfig {
        driver: driver.parse()?,
        display_name: None,
        accent_color: None,
        environment: Vec::new(),
        enabled: true,
        config: config.into(),
    })
}

#[derive(Default, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct CliConfig {
    binary_path: String,
    home_path: String,
    shadow_home_path: String,
    launch_args: String,
    auto_compact_window: String,
}

// Preserve case-sensitive instance identity on case-insensitive filesystems,
// and avoid reserved Windows filenames without restricting protocol slugs.
fn private_storage_component(identifier: &str) -> String {
    identifier
        .bytes()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn configured_path(value: &str, user_home: Option<&Path>) -> anyhow::Result<Option<PathBuf>> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    if value == "~" || value.starts_with("~/") {
        let home = user_home.ok_or_else(|| anyhow::anyhow!("user home is unavailable"))?;
        return Ok(Some(home.join(value.strip_prefix("~/").unwrap_or(""))));
    }
    Ok(Some(PathBuf::from(value)))
}

fn native_home(
    configured: &str,
    environment_home: Option<&str>,
    inherited_home: Option<&Path>,
    user_home: Option<&Path>,
    default_directory: &str,
) -> anyhow::Result<PathBuf> {
    if let Some(path) = configured_path(configured, user_home)? {
        return Ok(path);
    }
    // An explicitly empty environment value clears the inherited override.
    let overridden = environment_home
        .map(Path::new)
        .or(inherited_home)
        .filter(|path| !path.as_os_str().is_empty());
    overridden
        .map(Path::to_owned)
        .or_else(|| user_home.map(|home| home.join(default_directory)))
        .ok_or_else(|| anyhow::anyhow!("provider home is unavailable"))
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ModelCursor {
    resources: BTreeMap<ProviderInstanceId, uuid::Uuid>,
    remaining: BTreeMap<ProviderInstanceId, Option<String>>,
}

impl HostRpcService {
    pub(super) async fn model_page(
        &self,
        params: &op::ListModels,
    ) -> Result<op::ModelPage, Failure> {
        let (resources, descriptors, backends) = {
            let instances = self
                .inner
                .instances
                .read()
                .unwrap_or_else(|error| error.into_inner());
            let resources: BTreeMap<_, _> = instances
                .iter()
                .map(|(id, instance)| (id.clone(), instance.generation))
                .collect();
            let descriptors = instances
                .iter()
                .map(|(id, instance)| instance.descriptor(id))
                .collect();
            let backends: indexmap::IndexMap<_, _> = instances
                .iter()
                .map(|(id, instance)| (id.clone(), instance.backend.clone()))
                .collect();
            (resources, descriptors, backends)
        };
        let remaining = if let Some(cursor) = &params.cursor {
            let cursor: ModelCursor = serde_json::from_str(cursor)?;
            if cursor.resources != resources
                || cursor
                    .remaining
                    .keys()
                    .any(|id| !resources.contains_key(id))
            {
                return Err(Failure::new(
                    "provider_catalog_changed",
                    "provider catalog changed; reload models",
                ));
            }
            cursor.remaining
        } else {
            resources.keys().cloned().map(|id| (id, None)).collect()
        };
        let mut page = op::ModelPage {
            data: Vec::new(),
            instances: descriptors,
            next_cursor: None,
            provider_errors: None,
        };
        let mut next = BTreeMap::new();
        for (id, backend) in backends {
            let Some(cursor) = remaining.get(&id) else {
                continue;
            };
            let result = match backend {
                Ok(agent) => {
                    agent
                        .models(&op::ListModels {
                            cursor: cursor.clone(),
                            ..params.clone()
                        })
                        .await
                }
                Err(error) => Err(error),
            };
            match result {
                Ok(result) => {
                    if result
                        .data
                        .iter()
                        .any(|model| model.model.instance_id != id)
                    {
                        return Err(Failure::new(
                            "invalid_provider_models",
                            "model catalog belongs to another instance",
                        ));
                    }
                    page.data.extend(result.data);
                    if let Some(cursor) = result.next_cursor {
                        next.insert(id, Some(cursor));
                    }
                }
                Err(error) => {
                    page.provider_errors
                        .get_or_insert_default()
                        .insert(id.to_string(), serde_json::to_value(error)?);
                }
            }
        }
        let current: BTreeMap<_, _> = self
            .inner
            .instances
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .iter()
            .map(|(id, instance)| (id.clone(), instance.generation))
            .collect();
        if current != resources {
            return Err(Failure::new(
                "provider_catalog_changed",
                "provider catalog changed; reload models",
            ));
        }
        if !next.is_empty() {
            page.next_cursor = Some(serde_json::to_string(&ModelCursor {
                resources,
                remaining: next,
            })?);
        }
        Ok(page)
    }

    /// Production loads persisted settings before any provider process starts.
    pub async fn configure_providers(
        &self,
        directory: PathBuf,
        codex: PathBuf,
        codex_home: Option<PathBuf>,
        claude: PathBuf,
        claude_home: Option<PathBuf>,
    ) -> anyhow::Result<()> {
        self.inner
            .account_directory
            .set(directory)
            .map_err(|_| anyhow::anyhow!("provider storage is already configured"))?;
        let defaults = vec![
            ConfiguredProvider {
                instance_id: "codex".parse()?,
                config: default_config("codex", Some(&codex), codex_home.as_deref())?,
            },
            ConfiguredProvider {
                instance_id: "claudeAgent".parse()?,
                config: default_config("claudeAgent", Some(&claude), claude_home.as_deref())?,
            },
        ];
        self.inner.conversations.initialize_providers(defaults)?;
        let _access = self.inner.provider_access.lock().await;
        self.reconcile_providers().await?;
        Ok(())
    }

    async fn build_provider(
        &self,
        id: &ProviderInstanceId,
        config: &ProviderConfig,
    ) -> Result<Arc<dyn Agent>, Failure> {
        if !config.enabled {
            return Err(Failure::new(
                "provider_disabled",
                "provider instance is disabled",
            ));
        }
        let reference = ProviderRef {
            instance_id: id.clone(),
            driver: config.driver.clone(),
        };
        if !matches!(config.driver.as_str(), "codex" | "claudeAgent") {
            return Err(Failure::new(
                "provider_unavailable",
                "this build does not include the configured driver",
            ));
        }
        let settings: CliConfig = serde_json::from_value(config.config.clone()).map_err(|_| {
            Failure::new(
                "invalid_provider_config",
                "driver configuration has invalid field types",
            )
        })?;
        let mut launch_args = shell_words::split(&settings.launch_args).map_err(|_| {
            Failure::new(
                "invalid_provider_config",
                "launch arguments contain unmatched quotes",
            )
        })?;
        let environment: Vec<_> = config
            .environment
            .iter()
            .map(|entry| (entry.name.clone(), entry.value.clone()))
            .collect();
        let user_dirs = directories::BaseDirs::new();
        let user_home = user_dirs.as_ref().map(|dirs| dirs.home_dir());
        let (home_variable, default_directory, default_program) = match config.driver.as_str() {
            "codex" => ("CODEX_HOME", ".codex", "codex"),
            "claudeAgent" => ("CLAUDE_CONFIG_DIR", ".claude", "claude"),
            _ => unreachable!("driver availability checked above"),
        };
        let inherited_home = std::env::var_os(home_variable);
        let environment_home = environment.iter().rev().find(|(name, _)| {
            if cfg!(windows) {
                name.eq_ignore_ascii_case(home_variable)
            } else {
                name == home_variable
            }
        });
        let home = native_home(
            &settings.home_path,
            environment_home.map(|(_, value)| value.as_str()),
            inherited_home.as_deref().map(Path::new),
            user_home,
            default_directory,
        )
        .map_err(|error| Failure::new("invalid_provider_config", error))?;
        let program = configured_path(&settings.binary_path, user_home)
            .map_err(|error| Failure::new("invalid_provider_config", error))?
            .unwrap_or_else(|| default_program.into());
        let directory = self
            .inner
            .account_directory
            .get()
            .cloned()
            .unwrap_or_else(|| {
                self.inner
                    .projects
                    .path()
                    .parent()
                    .unwrap_or(Path::new("."))
                    .to_owned()
            })
            .join("provider-instances")
            .join(private_storage_component(id.as_str()))
            .join(private_storage_component(config.driver.as_str()));
        match config.driver.as_str() {
            "codex" => {
                let effective_home = match configured_path(&settings.shadow_home_path, user_home)
                    .map_err(|error| Failure::new("invalid_provider_config", error))?
                {
                    Some(shadow) => {
                        super::super::codex_home::materialize(&home, &shadow)
                            .map_err(|error| Failure::new("invalid_provider_config", error))?;
                        Some(shadow)
                    }
                    None => Some(home.clone()),
                };
                let runtime = codex_app_server::AppServerConfig {
                    program,
                    codex_home: effective_home,
                    environment,
                    launch_args,
                    ..Default::default()
                };
                let process = CodexAppServer::spawn(runtime.clone())
                    .await
                    .map(Arc::new)
                    .map_err(|error| error.to_string());
                let adapter = Arc::new(super::super::codex::Codex::new(
                    reference,
                    process.clone(),
                    Some(home),
                ));
                if process.is_ok() {
                    adapter
                        .enable_accounts(directory.join("accounts"), runtime)
                        .await
                        .map_err(|error| Failure::new("account_operation_failed", error))?;
                }
                // Only the configured first Codex instance supplies dictation.
                // Reconciliation selects this resource after all changes commit.
                Ok(adapter)
            }
            "claudeAgent" => {
                if !settings.auto_compact_window.is_empty() {
                    let window = settings
                        .auto_compact_window
                        .parse::<u32>()
                        .ok()
                        .filter(|window| (100_000..=1_000_000).contains(window))
                        .ok_or_else(|| {
                            Failure::new(
                                "invalid_provider_config",
                                "auto compact window must be 100000–1000000",
                            )
                        })?;
                    launch_args.extend([
                        "--settings".to_owned(),
                        serde_json::json!({"autoCompactWindow":window}).to_string(),
                    ]);
                }
                crate::claude::Claude::load(
                    reference,
                    program,
                    directory,
                    Some(home),
                    environment,
                    launch_args,
                )
                .await
                .map(|adapter| Arc::new(adapter) as Arc<dyn Agent>)
                .map_err(|error| Failure::new("provider_unavailable", error))
            }
            _ => unreachable!("driver availability checked above"),
        }
    }

    async fn reconcile_providers(&self) -> Result<bool, Failure> {
        let desired = self
            .inner
            .conversations
            .provider_settings()
            .map_err(|error| Failure::new("provider_settings_unavailable", error))?;
        let mut retired = Vec::new();
        let mut starting = Vec::new();
        let mut configuration_changed = false;
        {
            let mut instances = self
                .inner
                .instances
                .write()
                .unwrap_or_else(|error| error.into_inner());
            let mut previous = std::mem::take(&mut *instances);
            for saved in &desired.instances {
                if let Some(mut live) = previous.shift_remove(&saved.instance_id) {
                    configuration_changed |= live.config != saved.config;
                    if live.config.driver == saved.config.driver
                        && live.config.enabled == saved.config.enabled
                        && live.config.environment == saved.config.environment
                        && live.config.config == saved.config.config
                    {
                        live.config = saved.config.clone();
                        instances.insert(saved.instance_id.clone(), live);
                        continue;
                    }
                    retired.push((saved.instance_id.clone(), live));
                }
                instances.insert(
                    saved.instance_id.clone(),
                    Registered {
                        generation: uuid::Uuid::new_v4(),
                        config: saved.config.clone(),
                        backend: Err(Failure::new(
                            "provider_starting",
                            "provider instance is starting",
                        )),
                    },
                );
                starting.push(saved);
            }
            retired.extend(previous);
        }
        let resources_changed = !retired.is_empty() || !starting.is_empty();
        if !configuration_changed && !resources_changed {
            return Ok(false);
        }
        self.inner.router.broadcast(
            agent_protocol::protocol::Notification::ProviderSettingsChanged {
                revision: desired.revision,
            },
        );
        if !resources_changed {
            return Ok(false);
        }
        let mut stop_error = None;
        for (id, instance) in retired {
            if let Err(error) = self
                .inner
                .router
                .fail_provider(&id, "provider instance configuration changed")
            {
                stop_error.get_or_insert_with(|| Failure::new("provider_stop_failed", error));
            }
            self.inner.catalog_errors.write().unwrap().remove(&id);
            if let Ok(agent) = instance.backend {
                agent.shutdown().await;
            }
        }
        let had_starting = !starting.is_empty();
        if let Some(error) = stop_error {
            let mut instances = self
                .inner
                .instances
                .write()
                .unwrap_or_else(|error| error.into_inner());
            for saved in &starting {
                if let Some(instance) = instances.get_mut(&saved.instance_id) {
                    instance.generation = uuid::Uuid::new_v4();
                    instance.backend = Err(error.clone());
                }
            }
        } else {
            let started =
                futures_util::future::join_all(starting.into_iter().map(|saved| async move {
                    let backend = self.build_provider(&saved.instance_id, &saved.config).await;
                    (saved, backend)
                }))
                .await;
            let mut instances = self
                .inner
                .instances
                .write()
                .unwrap_or_else(|error| error.into_inner());
            for (saved, backend) in started {
                instances.insert(
                    saved.instance_id.clone(),
                    Registered {
                        generation: uuid::Uuid::new_v4(),
                        config: saved.config.clone(),
                        backend,
                    },
                );
            }
        }
        let dictation = self
            .agents()
            .into_iter()
            .find_map(|(_, agent)| agent.dictation_backend());
        self.inner
            .dictation
            .replace_backend(dictation.ok_or_else(|| "no available Codex instance".to_owned()));
        self.start_event_pumps();
        if had_starting {
            self.inner.router.broadcast(
                agent_protocol::protocol::Notification::ProviderSettingsChanged {
                    revision: desired.revision,
                },
            );
        }
        Ok(true)
    }

    pub(super) async fn update_provider_instance(
        &self,
        request: &UpdateProviderInstance,
    ) -> Result<ProviderSettings, Failure> {
        let _access = self.inner.provider_access.lock().await;
        // Synchronize catalog scans with registry changes. Native body reads retain their source scope.
        let _catalog = self.inner.catalog_import.lock().await;
        let response = self
            .inner
            .conversations
            .update_provider(request)
            .map_err(|error| Failure::new("provider_settings_conflict", error))?;
        let changed = self.reconcile_providers().await?;
        drop(_catalog);
        if !changed {
            return Ok(response);
        }
        let service = self.clone();
        tokio::spawn(async move {
            if let Err(error) = service.refresh_import().await {
                tracing::warn!(target: "bex", operation = "history.import", code = error.code, message = %error);
            }
        });
        Ok(response)
    }
}

impl Registered {
    fn descriptor(&self, id: &ProviderInstanceId) -> ProviderInstance {
        let availability = if !self.config.enabled {
            ProviderAvailability::Disabled
        } else {
            match self
                .backend
                .as_ref()
                .map_err(Clone::clone)
                .and_then(|agent| agent.availability().map(|_| agent))
            {
                Ok(_) => ProviderAvailability::Ready,
                Err(error) if error.code == "provider_starting" => ProviderAvailability::Starting,
                Err(error) => ProviderAvailability::Unavailable {
                    reason: error.to_string(),
                },
            }
        };
        let driver = self.config.driver.clone();
        let display_name =
            self.config
                .display_name
                .clone()
                .unwrap_or_else(|| match driver.as_str() {
                    "codex" => "Codex".into(),
                    "claudeAgent" => "Claude".into(),
                    name => name.into(),
                });
        ProviderInstance {
            reference: ProviderRef {
                instance_id: id.clone(),
                driver,
            },
            display_name,
            availability,
            capabilities: self
                .backend
                .as_ref()
                .map(|agent| host_capabilities(agent.capabilities()))
                .unwrap_or_default(),
            requires_account: matches!(self.config.driver.as_str(), "codex" | "claudeAgent"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn private_instance_storage_keeps_case_distinct_and_avoids_reserved_names() {
        let upper = private_storage_component("Codex");
        let lower = private_storage_component("codex");
        assert!(!upper.eq_ignore_ascii_case(&lower));
        let root = tempfile::tempdir().unwrap();
        for identifier in ["CON", "con", "COM1", "LPT9", &"a".repeat(64)] {
            std::fs::create_dir(root.path().join(private_storage_component(identifier))).unwrap();
        }
        std::fs::create_dir(root.path().join(&upper)).unwrap();
        std::fs::create_dir(root.path().join(&lower)).unwrap();
        assert_ne!(
            root.path().join(&upper).canonicalize().unwrap(),
            root.path().join(&lower).canonicalize().unwrap()
        );
        let codex = root
            .path()
            .join(&upper)
            .join(private_storage_component("codex"));
        let claude = root
            .path()
            .join(&upper)
            .join(private_storage_component("claudeAgent"));
        std::fs::create_dir(&codex).unwrap();
        std::fs::create_dir(&claude).unwrap();
        assert_ne!(
            codex.canonicalize().unwrap(),
            claude.canonicalize().unwrap()
        );
    }

    proptest! {
        #[test]
        fn distinct_slugs_never_share_a_private_storage_name(
            left in "[A-Za-z][A-Za-z0-9_-]{0,63}",
            right in "[A-Za-z][A-Za-z0-9_-]{0,63}",
        ) {
            let encoded_left = private_storage_component(&left);
            let encoded_right = private_storage_component(&right);
            prop_assert_eq!(encoded_left.eq_ignore_ascii_case(&encoded_right), left == right);
        }
    }

    #[test]
    fn configured_paths_and_native_homes_follow_instance_overrides() {
        let user_home = Path::new("/example-user");
        let inherited = Path::new("/inherited");
        assert_eq!(
            configured_path(" ~/bin/codex ", Some(user_home)).unwrap(),
            Some(user_home.join("bin/codex"))
        );
        assert!(configured_path("~/bin/codex", None).is_err());
        assert_eq!(
            configured_path(" custom-cli ", None).unwrap(),
            Some("custom-cli".into())
        );
        for directory in [".codex", ".claude"] {
            let resolve = |configured, environment| {
                native_home(
                    configured,
                    environment,
                    Some(inherited),
                    Some(user_home),
                    directory,
                )
                .unwrap()
            };
            assert_eq!(
                resolve("~/explicit", Some("/instance")),
                user_home.join("explicit")
            );
            assert_eq!(resolve("", Some("/instance")), Path::new("/instance"));
            assert_eq!(resolve("", None), inherited);
            assert_eq!(resolve("", Some("")), user_home.join(directory));
            assert_eq!(
                native_home("", None, None, Some(user_home), directory).unwrap(),
                user_home.join(directory)
            );
            assert!(native_home("", None, None, None, directory).is_err());
        }
    }

    #[tokio::test]
    async fn model_cursors_reject_replaced_and_unrecognized_instances() {
        let root = tempfile::tempdir().unwrap();
        let service =
            HostRpcService::new(ProjectStore::new(root.path().join("projects.json"))).unwrap();
        service
            .inner
            .conversations
            .initialize_providers(vec![ConfiguredProvider {
                instance_id: "work".parse().unwrap(),
                config: default_config("FutureDriver", None, None).unwrap(),
            }])
            .unwrap();
        service.reconcile_providers().await.unwrap();
        let resources = service
            .inner
            .instances
            .read()
            .unwrap()
            .iter()
            .map(|(id, instance)| (id.clone(), instance.generation))
            .collect();
        let mut cursor = ModelCursor {
            resources,
            remaining: [("work".parse().unwrap(), None)].into(),
        };
        let params = |cursor: &ModelCursor| op::ListModels {
            limit: 1,
            cursor: Some(serde_json::to_string(cursor).unwrap()),
        };
        let page = service.model_page(&params(&cursor)).await.unwrap();
        assert_eq!(page.instances.len(), 1);
        assert!(page.data.is_empty());
        assert!(page.provider_errors.unwrap().contains_key("work"));
        cursor.remaining.insert("missing".parse().unwrap(), None);
        assert_eq!(
            service.model_page(&params(&cursor)).await.unwrap_err().code,
            "provider_catalog_changed"
        );
        cursor
            .remaining
            .remove(&"missing".parse::<ProviderInstanceId>().unwrap());
        service
            .inner
            .instances
            .write()
            .unwrap()
            .get_mut(&"work".parse::<ProviderInstanceId>().unwrap())
            .unwrap()
            .generation = uuid::Uuid::new_v4();
        assert_eq!(
            service.model_page(&params(&cursor)).await.unwrap_err().code,
            "provider_catalog_changed"
        );
        service.inner.instances.write().unwrap().clear();
        assert_eq!(
            service.model_page(&params(&cursor)).await.unwrap_err().code,
            "provider_catalog_changed"
        );
    }

    #[tokio::test]
    async fn instance_resource_changes_replace_only_their_generation() {
        for field in ["driver", "enabled", "environment", "config"] {
            let root = tempfile::tempdir().unwrap();
            let service =
                HostRpcService::new(ProjectStore::new(root.path().join("projects.json"))).unwrap();
            let config = default_config("FutureDriver", None, None).unwrap();
            let work: ProviderInstanceId = "work".parse().unwrap();
            let personal: ProviderInstanceId = "personal".parse().unwrap();
            service
                .inner
                .conversations
                .initialize_providers(
                    [work.clone(), personal.clone()]
                        .into_iter()
                        .map(|instance_id| ConfiguredProvider {
                            instance_id,
                            config: config.clone(),
                        })
                        .collect(),
                )
                .unwrap();
            assert!(service.reconcile_providers().await.unwrap());
            let generations: BTreeMap<_, _> = service
                .inner
                .instances
                .read()
                .unwrap()
                .iter()
                .map(|(id, live)| (id.clone(), live.generation))
                .collect();
            let mut changed = config;
            match field {
                "driver" => changed.driver = "OtherDriver".parse().unwrap(),
                "enabled" => changed.enabled = false,
                "environment" => changed.environment.push(EnvironmentVariable {
                    name: "MODE".into(),
                    value: "test".into(),
                    sensitive: false,
                    value_redacted: false,
                }),
                "config" => changed.config = serde_json::json!({"binaryPath":"other-cli"}),
                _ => unreachable!(),
            }
            service
                .inner
                .conversations
                .update_provider(&UpdateProviderInstance {
                    operation_id: uuid::Uuid::new_v4(),
                    revision: 0,
                    mutation: ProviderMutation::Upsert {
                        instance_id: work.clone(),
                        config: changed.clone(),
                    },
                })
                .unwrap();
            assert!(
                service.reconcile_providers().await.unwrap(),
                "{field} must replace native resources"
            );
            let instances = service.inner.instances.read().unwrap();
            assert_ne!(instances[&work].generation, generations[&work], "{field}");
            assert_eq!(
                instances[&personal].generation, generations[&personal],
                "{field}"
            );
            assert_eq!(instances[&work].config, changed, "{field}");
        }
    }

    #[tokio::test]
    async fn changed_instances_retire_independently_and_unknown_configs_remain_visible() {
        let root = tempfile::tempdir().unwrap();
        let service =
            HostRpcService::new(ProjectStore::new(root.path().join("projects.json"))).unwrap();
        let config = default_config("codex", None, None).unwrap();
        let mut originals = Vec::new();
        let mut saved = Vec::new();
        for id in ["personal", "work"] {
            let reference = ProviderRef {
                instance_id: id.parse().unwrap(),
                driver: "codex".parse().unwrap(),
            };
            let agent: Arc<dyn Agent> = Arc::new(super::super::super::codex::Codex::new(
                reference.clone(),
                Err("isolated native resource".into()),
                Some(root.path().join(id)),
            ));
            service.inner.instances.write().unwrap().insert(
                reference.instance_id.clone(),
                Registered {
                    generation: uuid::Uuid::new_v4(),
                    config: config.clone(),
                    backend: Ok(agent.clone()),
                },
            );
            saved.push(ConfiguredProvider {
                instance_id: reference.instance_id,
                config: config.clone(),
            });
            originals.push(agent);
        }
        service
            .inner
            .conversations
            .initialize_providers(saved)
            .unwrap();
        assert!(!service.reconcile_providers().await.unwrap());
        for original in &originals {
            assert!(Arc::ptr_eq(
                &service.agent(&original.reference().instance_id).unwrap(),
                original
            ));
        }
        let personal: ProviderInstanceId = "personal".parse().unwrap();
        let generation = service.inner.instances.read().unwrap()[&personal].generation;
        let mut metadata = config.clone();
        metadata.display_name = Some("Personal account".into());
        metadata.accent_color = Some("#aa5500".into());
        let mut listener = service.open_session();
        service
            .inner
            .conversations
            .update_provider(&UpdateProviderInstance {
                operation_id: uuid::Uuid::new_v4(),
                revision: 0,
                mutation: ProviderMutation::Upsert {
                    instance_id: personal.clone(),
                    config: metadata,
                },
            })
            .unwrap();
        assert!(
            !service.reconcile_providers().await.unwrap(),
            "display metadata must not restart native resources or import history"
        );
        assert!(Arc::ptr_eq(
            &service.agent(&personal).unwrap(),
            &originals[0]
        ));
        assert_eq!(
            service.inner.instances.read().unwrap()[&personal].generation,
            generation
        );
        let notification = tokio::time::timeout(std::time::Duration::from_secs(1), listener.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            agent_protocol::protocol::decode::<agent_protocol::protocol::Notification>(
                &notification
            )
            .unwrap(),
            agent_protocol::protocol::Notification::ProviderSettingsChanged { revision: 1 }
        ));
        assert_eq!(
            service
                .model_page(&op::ListModels {
                    cursor: None,
                    limit: 10
                })
                .await
                .unwrap()
                .instances[0]
                .display_name,
            "Personal account"
        );
        let mut unknown = default_config("FutureDriver", None, None).unwrap();
        unknown.display_name = Some("Experimental".into());
        unknown.config = serde_json::json!([{"unknown":true}, "preserved"]);
        service
            .inner
            .conversations
            .update_provider(&UpdateProviderInstance {
                operation_id: uuid::Uuid::new_v4(),
                revision: 1,
                mutation: ProviderMutation::Upsert {
                    instance_id: "work".parse().unwrap(),
                    config: unknown.clone(),
                },
            })
            .unwrap();
        assert!(service.reconcile_providers().await.unwrap());
        assert!(Arc::ptr_eq(
            &service.agent(&"personal".parse().unwrap()).unwrap(),
            &originals[0]
        ));
        assert!(service.agent(&"work".parse().unwrap()).is_err());
        let manifest = service
            .model_page(&op::ListModels {
                cursor: None,
                limit: 10,
            })
            .await
            .unwrap()
            .instances;
        let changed = manifest
            .iter()
            .find(|entry| entry.reference.instance_id.as_str() == "work")
            .unwrap();
        assert_eq!(changed.reference.driver.as_str(), "FutureDriver");
        assert_eq!(changed.display_name, "Experimental");
        assert!(matches!(
            changed.availability,
            ProviderAvailability::Unavailable { .. }
        ));
        let session = service.open_session();
        let cwd = root.path().to_string_lossy().into_owned();
        let reply = service
            .dispatch(
                session.id(),
                &Call::ComposerCatalog(op::LoadComposerCatalog { cwd: cwd.clone() }),
            )
            .await
            .unwrap();
        let agent_protocol::protocol::Response::Success { result: catalog } =
            agent_protocol::protocol::decode::<
                agent_protocol::protocol::Response<agent_protocol::composer::ComposerCatalog>,
            >(&reply.initial)
            .unwrap()
        else {
            panic!("missing composer catalog")
        };
        assert_eq!(catalog.cwd, cwd);
        assert!(catalog.errors.contains_key(&"work".parse().unwrap()));
        assert_eq!(
            service
                .inner
                .conversations
                .provider_settings()
                .unwrap()
                .instances[1]
                .config,
            unknown
        );
        assert!(!service.reconcile_providers().await.unwrap());
        service
            .inner
            .conversations
            .update_provider(&UpdateProviderInstance {
                operation_id: uuid::Uuid::new_v4(),
                revision: 2,
                mutation: ProviderMutation::Remove {
                    instance_id: "work".parse().unwrap(),
                },
            })
            .unwrap();
        assert!(service.reconcile_providers().await.unwrap());
        assert_eq!(
            service
                .model_page(&op::ListModels {
                    cursor: None,
                    limit: 10
                })
                .await
                .unwrap()
                .instances
                .len(),
            1
        );
        assert!(Arc::ptr_eq(
            &service.agent(&"personal".parse().unwrap()).unwrap(),
            &originals[0]
        ));
    }

    #[tokio::test]
    async fn resolved_native_resources_and_late_events_cannot_follow_a_replaced_home() {
        let root = tempfile::tempdir().unwrap();
        let service =
            HostRpcService::new(ProjectStore::new(root.path().join("projects.json"))).unwrap();
        let reference = ProviderRef {
            instance_id: "work".parse().unwrap(),
            driver: "codex".parse().unwrap(),
        };
        let config = default_config("codex", None, None).unwrap();
        let original: Arc<dyn Agent> = Arc::new(super::super::super::codex::Codex::new(
            reference.clone(),
            Err("isolated original".into()),
            Some(root.path().join("before")),
        ));
        service.inner.instances.write().unwrap().insert(
            reference.instance_id.clone(),
            Registered {
                generation: uuid::Uuid::new_v4(),
                config: config.clone(),
                backend: Ok(original.clone()),
            },
        );
        let native = NativeIdentity {
            provider: reference.clone(),
            id: "native-id".into(),
        };
        let before = native_storage_scope(original.storage_directory()).unwrap();
        let target = service.inner.conversations.bind(&native, &before).unwrap();
        service
            .inner
            .conversations
            .rename(&target, "Original", false)
            .unwrap();
        let (resolved, captured) = service.native_session(&target).unwrap();
        let replacement: Arc<dyn Agent> = Arc::new(super::super::super::codex::Codex::new(
            reference.clone(),
            Err("isolated replacement".into()),
            Some(root.path().join("after")),
        ));
        service.inner.instances.write().unwrap().insert(
            reference.instance_id.clone(),
            Registered {
                generation: uuid::Uuid::new_v4(),
                config,
                backend: Ok(replacement.clone()),
            },
        );
        assert!(Arc::ptr_eq(&captured, &original));
        assert_eq!(resolved, native);
        assert_eq!(
            native_storage_scope(captured.storage_directory()).unwrap(),
            before
        );
        assert!(service.native_session(&target).is_err());
        let change = |name: &str| super::super::super::agent::AgentChange::Renamed {
            session: agent_protocol::session::SessionRef::new("native-id".into()).unwrap(),
            name: name.into(),
        };
        assert!(
            service
                .apply_agent_change(&original, change("Stale event"))
                .is_err()
        );
        assert_eq!(
            service
                .inner
                .conversations
                .open_thread(&target, 5, false)
                .unwrap()
                .thread
                .name
                .as_deref(),
            Some("Original")
        );
        service
            .apply_agent_change(&replacement, change("Current event"))
            .unwrap();
        let after = native_storage_scope(replacement.storage_directory()).unwrap();
        let current = service.inner.conversations.bind(&native, &after).unwrap();
        assert_ne!(target, current);
        assert_eq!(
            service
                .inner
                .conversations
                .open_thread(&current, 5, false)
                .unwrap()
                .thread
                .name
                .as_deref(),
            Some("Current event")
        );
        assert_eq!(
            service
                .inner
                .conversations
                .open_thread(&target, 5, false)
                .unwrap()
                .thread
                .name
                .as_deref(),
            Some("Original")
        );
        let client = service.open_session();
        let reply = service
            .dispatch(
                client.id(),
                &Call::OpenSession(agent_protocol::session::OpenSession {
                    session: target.clone(),
                    limit: 5,
                    include_activity: false,
                }),
            )
            .await
            .unwrap();
        let agent_protocol::protocol::Response::Success { result: opened } =
            agent_protocol::protocol::decode::<
                agent_protocol::protocol::Response<agent_protocol::session::OpenedSession>,
            >(&reply.initial)
            .unwrap()
        else {
            panic!("cached conversation must remain readable")
        };
        let cached = agent_protocol::session::Capabilities {
            rename: true,
            ..Default::default()
        };
        assert_eq!(opened.response.thread.capabilities, Some(cached));
        service.inner.instances.write().unwrap().clear();
        let page = service
            .host_title_list(ListQuery {
                project_limit: 10,
                chat_limit: 10,
                ..Default::default()
            })
            .await
            .unwrap();
        let retained = page
            .data
            .iter()
            .find(|thread| thread.id.as_ref() == Some(&target))
            .unwrap();
        assert_eq!(retained.name.as_deref(), Some("Original"));
        assert_eq!(retained.capabilities, Some(cached));
    }
}

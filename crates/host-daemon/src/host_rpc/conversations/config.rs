//! Settings and mutation receipts commit in the conversation database transaction.
use super::*;
use agent_protocol::providers::*;
use std::collections::BTreeSet;

fn merge_config(
    previous: Option<&ProviderConfig>,
    mut next: ProviderConfig,
) -> Result<ProviderConfig> {
    validate_environment(&next.environment).map_err(anyhow::Error::msg)?;
    for variable in &mut next.environment {
        if variable.value_redacted {
            let saved = previous
                .and_then(|config| {
                    config.environment.iter().find(|saved| {
                        saved.name == variable.name && saved.sensitive && !saved.value_redacted
                    })
                })
                .context("redacted environment variable has no saved value")?;
            variable.value = saved.value.clone();
            variable.value_redacted = false;
        }
    }
    ensure!(
        next.display_name
            .as_ref()
            .is_none_or(|name| !name.is_empty() && name.trim() == name),
        "display name must be nonempty without surrounding whitespace"
    );
    ensure!(
        next.accent_color
            .as_ref()
            .is_none_or(|color| !color.is_empty() && color.trim() == color),
        "accent color must be nonempty without surrounding whitespace"
    );
    Ok(next)
}

impl Conversations {
    pub(in crate::host_rpc) fn provider_settings(&self) -> Result<ProviderSettings> {
        read_settings(&self.lock())
    }
    pub(in crate::host_rpc) fn initialize_providers(
        &self,
        instances: Vec<ConfiguredProvider>,
    ) -> Result<ProviderSettings> {
        let mut connection = self.lock();
        let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        if !tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM provider_settings)",
            [],
            |row| row.get::<_, bool>(0),
        )? {
            let mut names = BTreeSet::new();
            for instance in &instances {
                ensure!(
                    names.insert(&instance.instance_id),
                    "initial provider instance is duplicated"
                );
                merge_config(None, instance.config.clone())?;
            }
            tx.execute(
                "INSERT INTO provider_settings(singleton, revision, body) VALUES(1, 0, ?1)",
                [serde_json::to_string(&instances)?],
            )?;
        }
        let settings = read_settings(&tx)?;
        tx.commit()?;
        Ok(settings)
    }
    pub(in crate::host_rpc) fn update_provider(
        &self,
        request: &UpdateProviderInstance,
    ) -> Result<ProviderSettings> {
        let mut connection = self.lock();
        let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let payload = serde_json::to_string(request)?;
        if let Some((previous, result)) = tx
            .query_row(
                "SELECT request, response FROM provider_mutations WHERE operation_id=?1",
                [request.operation_id.to_string()],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?
        {
            ensure!(
                previous == payload,
                "provider operation ID was reused for a different mutation"
            );
            return serde_json::from_str(&result).context("provider mutation receipt is invalid");
        }
        let mut settings = read_settings(&tx)?;
        ensure!(
            request.revision == settings.revision,
            "provider settings changed; reload before editing"
        );
        let previous_instances = settings.instances.clone();
        match &request.mutation {
            ProviderMutation::Create {
                instance_id,
                config,
            }
            | ProviderMutation::Upsert {
                instance_id,
                config,
            } => {
                let previous = settings
                    .instances
                    .iter()
                    .position(|entry| &entry.instance_id == instance_id);
                ensure!(
                    !matches!(request.mutation, ProviderMutation::Create { .. })
                        || previous.is_none(),
                    "provider instance already exists"
                );
                let config = merge_config(
                    previous.map(|index| &settings.instances[index].config),
                    config.clone(),
                )?;
                let entry = ConfiguredProvider {
                    instance_id: instance_id.clone(),
                    config,
                };
                if let Some(index) = previous {
                    settings.instances[index] = entry;
                } else {
                    settings.instances.push(entry);
                }
            }
            ProviderMutation::Remove { instance_id } => {
                ensure!(
                    settings
                        .instances
                        .iter()
                        .any(|entry| &entry.instance_id == instance_id),
                    "provider instance does not exist"
                );
                settings
                    .instances
                    .retain(|entry| &entry.instance_id != instance_id);
            }
        }
        if settings.instances != previous_instances {
            settings.revision = settings
                .revision
                .checked_add(1)
                .context("provider settings revision overflow")?;
        }
        tx.execute("INSERT INTO provider_settings(singleton, revision, body) VALUES(1, ?1, ?2) ON CONFLICT(singleton) DO UPDATE SET revision=excluded.revision, body=excluded.body", params![settings.revision, serde_json::to_string(&settings.instances)?])?;
        let redacted = settings.redacted();
        tx.execute(
            "INSERT INTO provider_mutations(operation_id, request, response) VALUES(?1, ?2, ?3)",
            params![
                request.operation_id.to_string(),
                payload,
                serde_json::to_string(&redacted)?
            ],
        )?;
        tx.commit()?;
        Ok(redacted)
    }
}

fn read_settings(connection: &Connection) -> Result<ProviderSettings> {
    let saved = connection
        .query_row(
            "SELECT revision, body FROM provider_settings WHERE singleton=1",
            [],
            |row| Ok((row.get(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;
    match saved {
        Some((revision, body)) => Ok(ProviderSettings {
            revision,
            instances: serde_json::from_str(&body)?,
        }),
        None => Ok(ProviderSettings {
            revision: 0,
            instances: Vec::new(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn configured_instance_order_survives_updates_and_restart() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("ordered.sqlite");
        let db = Conversations::open(&path).unwrap();
        let original: Vec<ConfiguredProvider> = ["zulu", "alpha", "middle"]
            .into_iter()
            .map(|id| ConfiguredProvider {
                instance_id: id.parse().unwrap(),
                config: config(),
            })
            .collect();
        db.initialize_providers(original.clone()).unwrap();
        let mut renamed = original[1].config.clone();
        renamed.display_name = Some("Renamed".into());
        db.update_provider(&update(
            0,
            ProviderMutation::Upsert {
                instance_id: "alpha".parse().unwrap(),
                config: renamed,
            },
        ))
        .unwrap();
        db.update_provider(&update(
            1,
            ProviderMutation::Create {
                instance_id: "before".parse().unwrap(),
                config: config(),
            },
        ))
        .unwrap();
        db.update_provider(&update(
            2,
            ProviderMutation::Remove {
                instance_id: "middle".parse().unwrap(),
            },
        ))
        .unwrap();
        drop(db);
        let saved = Conversations::open(&path)
            .unwrap()
            .provider_settings()
            .unwrap();
        assert_eq!(
            saved
                .instances
                .iter()
                .map(|instance| instance.instance_id.as_str())
                .collect::<Vec<_>>(),
            ["zulu", "alpha", "before"]
        );
        assert_eq!(
            saved.instances[1].config.display_name.as_deref(),
            Some("Renamed")
        );
    }

    fn config() -> ProviderConfig {
        ProviderConfig {
            driver: "FutureDriver".parse().unwrap(),
            display_name: Some("Work".into()),
            accent_color: Some("#123456".into()),
            enabled: true,
            environment: vec![EnvironmentVariable {
                name: "EXAMPLE_VALUE".into(),
                value: "placeholder".into(),
                sensitive: true,
                value_redacted: false,
            }],
            config: serde_json::json!({"unknown": {"values": [1, false, null]}}),
        }
    }
    fn update(revision: u64, mutation: ProviderMutation) -> UpdateProviderInstance {
        UpdateProviderInstance {
            operation_id: uuid::Uuid::new_v4(),
            revision,
            mutation,
        }
    }
    fn store(root: &tempfile::TempDir) -> Conversations {
        Conversations::open(&root.path().join("history.sqlite")).unwrap()
    }

    #[test]
    fn unknown_configuration_secrets_and_idempotent_receipts_survive_restart() {
        let root = tempfile::tempdir().unwrap();
        let db = store(&root);
        let original = config();
        let request = update(
            0,
            ProviderMutation::Create {
                instance_id: "work".parse().unwrap(),
                config: original.clone(),
            },
        );
        let receipt = db.update_provider(&request).unwrap();
        assert_eq!(receipt.revision, 1);
        assert!(receipt.instances[0].config.environment[0].value.is_empty());
        assert!(receipt.instances[0].config.environment[0].value_redacted);
        assert_eq!(
            db.provider_settings().unwrap().instances[0].config,
            original
        );
        assert_eq!(db.update_provider(&request).unwrap(), receipt);
        let mut replacement = receipt.instances[0].config.clone();
        replacement.display_name = Some("Updated".into());
        let changed = update(
            1,
            ProviderMutation::Upsert {
                instance_id: "work".parse().unwrap(),
                config: replacement,
            },
        );
        assert_eq!(db.update_provider(&changed).unwrap().revision, 2);
        assert_eq!(
            db.provider_settings().unwrap().instances[0]
                .config
                .environment,
            original.environment
        );
        drop(db);
        let reopened = store(&root);
        assert_eq!(reopened.update_provider(&request).unwrap(), receipt);
        assert_eq!(reopened.provider_settings().unwrap().revision, 2);
        assert_eq!(
            reopened.provider_settings().unwrap().instances[0]
                .config
                .config,
            original.config
        );
        let mut collision = request.clone();
        collision.mutation = ProviderMutation::Remove {
            instance_id: "work".parse().unwrap(),
        };
        assert!(reopened.update_provider(&collision).is_err());
        assert!(
            reopened
                .update_provider(&update(1, collision.mutation))
                .is_err()
        );
        assert_eq!(reopened.provider_settings().unwrap().revision, 2);
    }

    #[test]
    fn redacted_values_require_a_saved_sensitive_variable_with_the_same_name() {
        let original = config();
        let redacted = ProviderSettings {
            revision: 0,
            instances: vec![ConfiguredProvider {
                instance_id: "work".parse().unwrap(),
                config: original.clone(),
            }],
        }
        .redacted()
        .instances
        .remove(0)
        .config;
        let mut previous = original.clone();
        previous.environment[0].sensitive = false;
        assert!(merge_config(Some(&previous), redacted.clone()).is_err());
        previous.environment[0].sensitive = true;
        previous.environment[0].name = "OTHER_VALUE".into();
        previous.environment[0].value = "other-placeholder".into();
        assert!(merge_config(Some(&previous), redacted.clone()).is_err());
        previous.environment.push(original.environment[0].clone());
        let saved = previous.clone();
        let restored = merge_config(Some(&previous), redacted).unwrap();
        assert_eq!(restored.environment, original.environment);
        assert_eq!(previous, saved);
    }

    #[test]
    fn initialization_and_no_op_updates_preserve_settings_and_revision() {
        let root = tempfile::tempdir().unwrap();
        let db = store(&root);
        let entries = vec![ConfiguredProvider {
            instance_id: "work".parse().unwrap(),
            config: config(),
        }];
        let initial = db.initialize_providers(entries.clone()).unwrap();
        assert_eq!(db.initialize_providers(Vec::new()).unwrap(), initial);
        let redacted = initial.clone().redacted();
        let no_op = update(
            0,
            ProviderMutation::Upsert {
                instance_id: "work".parse().unwrap(),
                config: redacted.instances[0].config.clone(),
            },
        );
        assert_eq!(db.update_provider(&no_op).unwrap(), redacted);
        assert_eq!(db.provider_settings().unwrap(), initial);
        assert!(
            db.update_provider(&update(
                0,
                ProviderMutation::Create {
                    instance_id: "work".parse().unwrap(),
                    config: config()
                }
            ))
            .is_err()
        );
        assert!(
            db.update_provider(&update(
                0,
                ProviderMutation::Remove {
                    instance_id: "missing".parse().unwrap()
                }
            ))
            .is_err()
        );
        let removed = db
            .update_provider(&update(
                0,
                ProviderMutation::Remove {
                    instance_id: "work".parse().unwrap(),
                },
            ))
            .unwrap();
        assert_eq!(removed.revision, 1);
        assert!(removed.instances.is_empty());
    }

    #[test]
    fn invalid_environment_edits_are_rejected_without_writing_or_consuming_the_operation_id() {
        let root = tempfile::tempdir().unwrap();
        for invalid in ["", "1VALUE", "BAD-NAME", "BAD NAME", "é", &"a".repeat(129)] {
            let mut edited = config();
            edited.environment[0].name = invalid.into();
            assert!(merge_config(None, edited).is_err());
        }
        for edit in 0..5 {
            let path = root.path().join(format!("invalid-{edit}.sqlite"));
            let db = Conversations::open(&path).unwrap();
            let mut edited = config();
            match edit {
                0 => edited.environment.push(edited.environment[0].clone()),
                1 => edited.environment[0].value.push('\0'),
                2 => {
                    edited.environment[0].value_redacted = true;
                    edited.environment[0].value.clear();
                }
                3 => edited.environment[0].value_redacted = true,
                4 => edited.display_name = Some(" bad ".into()),
                _ => unreachable!(),
            }
            let mut request = update(
                0,
                ProviderMutation::Create {
                    instance_id: "work".parse().unwrap(),
                    config: edited,
                },
            );
            assert!(db.update_provider(&request).is_err());
            assert!(db.provider_settings().unwrap().instances.is_empty());
            request.mutation = ProviderMutation::Create {
                instance_id: "work".parse().unwrap(),
                config: config(),
            };
            assert_eq!(db.update_provider(&request).unwrap().revision, 1);
            db.update_provider(&update(
                1,
                ProviderMutation::Remove {
                    instance_id: "work".parse().unwrap(),
                },
            ))
            .unwrap();
        }
    }

    proptest! {
        #[test]
        fn redaction_preserves_host_values_without_mutating_inputs(value in "[^\\x00]{0,128}", name in "[A-Za-z_][A-Za-z0-9_]{0,127}") {
            let mut original = config();
            original.environment[0].name = name;
            original.environment[0].value = value;
            let saved = original.clone();
            let redacted = ProviderSettings { revision: 0, instances: vec![ConfiguredProvider { instance_id: "work".parse().unwrap(), config: original.clone() }] }.redacted();
            let restored = merge_config(Some(&original), redacted.instances[0].config.clone()).unwrap();
            prop_assert_eq!(restored, saved.clone());
            prop_assert_eq!(original, saved);
        }
    }
}

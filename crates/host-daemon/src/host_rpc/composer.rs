//! Normalize Codex's catalog at the provider boundary.
use super::codex::Codex;
use agent_protocol::composer::{ComposerCandidate, ComposerCatalog, Invocation, InvocationKind};
use serde::Deserialize;
use serde_json::json;

#[derive(Deserialize)]
struct Skills {
    data: Vec<SkillGroup>,
}
#[derive(Deserialize)]
struct SkillGroup {
    skills: Vec<Skill>,
    errors: Vec<serde_json::Value>,
}
#[derive(Deserialize)]
struct Skill {
    name: String,
    path: String,
    description: String,
    enabled: bool,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Plugins {
    marketplaces: Vec<Marketplace>,
    marketplace_load_errors: Vec<serde_json::Value>,
}
#[derive(Deserialize)]
struct Marketplace {
    plugins: Vec<Plugin>,
}
#[derive(Deserialize)]
struct Plugin {
    id: String,
    name: String,
    installed: bool,
    enabled: bool,
    availability: String,
    interface: Option<PluginInterface>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PluginInterface {
    display_name: Option<String>,
    short_description: Option<String>,
}

impl Codex {
    pub(super) async fn composer_catalog(&self, cwd: &str) -> ComposerCatalog {
        let cwds: Vec<&str> = if cwd.is_empty() { vec![] } else { vec![cwd] };
        let params = json!({"cwds":cwds});
        let (skills, plugins) = tokio::join!(
            self.request::<_, Skills>("skills/list", &params),
            self.request::<_, Plugins>("plugin/list", &params)
        );
        let mut catalog = ComposerCatalog {
            cwd: cwd.into(),
            ..Default::default()
        };
        match skills {
            Ok(skills) => {
                for group in skills.data {
                    if !group.errors.is_empty() {
                        catalog
                            .errors
                            .entry(self.reference.instance_id.clone())
                            .or_default()
                            .push("一部のスキルを読み込めませんでした".into());
                    }
                    catalog
                        .candidates
                        .extend(group.skills.into_iter().filter(|s| s.enabled).map(|s| {
                            ComposerCandidate {
                                invocation: Invocation {
                                    instance_id: self.reference.instance_id.clone(),
                                    kind: InvocationKind::Skill,
                                    name: s.name,
                                    path: s.path,
                                },
                                description: s.description,
                            }
                        }));
                }
            }
            Err(_) => catalog
                .errors
                .entry(self.reference.instance_id.clone())
                .or_default()
                .push("スキルを取得できませんでした".into()),
        }
        match plugins {
            Ok(plugins) => {
                if !plugins.marketplace_load_errors.is_empty() {
                    catalog
                        .errors
                        .entry(self.reference.instance_id.clone())
                        .or_default()
                        .push("一部のプラグインを読み込めませんでした".into());
                }
                catalog.candidates.extend(
                    plugins
                        .marketplaces
                        .into_iter()
                        .flat_map(|m| m.plugins)
                        .filter(|p| p.installed && p.enabled && p.availability == "AVAILABLE")
                        .map(|p| {
                            let (name, description) =
                                p.interface.map_or((p.name.clone(), String::new()), |i| {
                                    (
                                        i.display_name.unwrap_or(p.name),
                                        i.short_description.unwrap_or_default(),
                                    )
                                });
                            ComposerCandidate {
                                invocation: Invocation {
                                    instance_id: self.reference.instance_id.clone(),
                                    kind: InvocationKind::Plugin,
                                    name,
                                    path: format!("plugin://{}", p.id),
                                },
                                description,
                            }
                        }),
                );
            }
            Err(_) => catalog
                .errors
                .entry(self.reference.instance_id.clone())
                .or_default()
                .push("プラグインを取得できませんでした".into()),
        }
        catalog
    }
}

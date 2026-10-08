//! Host-owned Agent Client Protocol registry management.
//!
//! Search, preparation, probing, and removal stay as core intents.  This
//! surface only owns the input widgets and dispatches those typed operations.

use super::{Row, notice, section};
use crate::app::{
    Desktop,
    ui::{color, icon, tint},
};
use agent_core::state::Intent;
use gpui_kit::{
    component::{
        Disableable, Sizable,
        button::{Button, ButtonVariants},
        h_flex,
        input::Input,
    },
    *,
};
use std::collections::BTreeSet;

impl Desktop {
    pub(super) fn render_acp_registry(
        &mut self,
        _: &mut Window,
        cx: &mut Context<Desktop>,
    ) -> AnyElement {
        let registry = &self.snapshot.acp_registry;
        let query = self.settings.providers.acp_query.clone();
        let search_pending = registry.search_pending;
        let search = Button::new("acp-registry-search")
            .primary()
            .small()
            .label(if search_pending {
                "Searching…"
            } else {
                "Search"
            })
            .disabled(search_pending)
            .on_click(cx.listener(|view, _, _, cx| {
                let query = view
                    .settings
                    .providers
                    .acp_query
                    .read(cx)
                    .value()
                    .trim()
                    .to_owned();
                view.perform(Intent::SearchAcpRegistry { query });
            }));
        let mut rows = vec![
            Row::new("Find an ACP agent")
                .description("Search the credential-free registry, then prepare and inspect an agent before using it.")
                .control(
                    h_flex()
                        .gap_2()
                        .child(Input::new(&query).small().aria_label("ACP registry search"))
                        .child(search),
                )
                .render(),
        ];

        if let Some(error) = registry.error.clone() {
            rows.push(
                Row::new("Registry error")
                    .description(error)
                    .status(
                        div()
                            .text_color(color("errorForeground"))
                            .child(icon("circle-alert")),
                    )
                    .render(),
            );
        }

        let result_ids: BTreeSet<_> = registry
            .results
            .as_ref()
            .map(|result| result.agents.iter().map(|agent| agent.id.clone()).collect())
            .unwrap_or_default();
        if let Some(result) = &registry.results {
            if result.agents.is_empty() {
                rows.push(notice("No ACP agents matched this search.").into_any_element());
            }
            for (index, agent) in result.agents.iter().enumerate() {
                let agent_id = agent.id.clone();
                let prepared = registry.prepared.get(&agent.id);
                let probe = registry.probes.get(&agent.id);
                let preparing = registry.prepare_pending.as_deref() == Some(agent.id.as_str());
                let probing = registry.probe_pending.as_deref() == Some(agent.id.as_str());
                let uninstalling = registry.uninstall_pending.as_deref() == Some(agent.id.as_str());
                let prepare_id = agent.id.clone();
                let probe_id = agent.id.clone();
                let uninstall_id = agent.id.clone();
                let cwd = self.snapshot.cwd();
                let prepare = Button::new(("acp-prepare", index))
                    .outline()
                    .xsmall()
                    .label(if preparing { "Preparing…" } else { "Prepare" })
                    .disabled(preparing || prepared.is_some())
                    .on_click(cx.listener(move |view, _, _, _| {
                        view.perform(Intent::PrepareAcpAgent {
                            agent_id: prepare_id.clone(),
                        });
                    }));
                let probe_button = Button::new(("acp-probe", index))
                    .outline()
                    .xsmall()
                    .label(if probing { "Probing…" } else { "Probe" })
                    .disabled(probing || prepared.is_none())
                    .on_click(cx.listener(move |view, _, _, _| {
                        view.perform(Intent::ProbeAcpAgent {
                            agent_id: probe_id.clone(),
                            cwd: cwd.clone(),
                        });
                    }));
                let uninstall = Button::new(("acp-uninstall", index))
                    .ghost()
                    .xsmall()
                    .label(if uninstalling {
                        "Removing…"
                    } else {
                        "Remove"
                    })
                    .disabled(uninstalling || prepared.is_none())
                    .on_click(cx.listener(move |view, _, _, _| {
                        view.perform(Intent::UninstallAcpAgent {
                            agent_id: uninstall_id.clone(),
                        });
                    }));
                let mut details = format!("{} · {}", agent.id, agent.version);
                if let Some(license) = &agent.license {
                    details.push_str(&format!(" · {license}"));
                }
                if let Some(prepared) = prepared {
                    details.push_str(&format!(
                        " · prepared {} ({})",
                        prepared.version, prepared.distribution
                    ));
                }
                let probe_detail = probe.map(|probe| {
                    format!(
                        "{} · {} model{} · {} auth method{}",
                        if probe.ready { "Ready" } else { "Not ready" },
                        probe.models.len(),
                        if probe.models.len() == 1 { "" } else { "s" },
                        probe.auth_methods.len(),
                        if probe.auth_methods.len() == 1 {
                            ""
                        } else {
                            "s"
                        },
                    )
                });
                let description = if agent.description.trim().is_empty() {
                    probe_detail.unwrap_or(details)
                } else if let Some(probe_detail) = probe_detail {
                    format!("{} · {probe_detail}", agent.description)
                } else {
                    format!("{} · {details}", agent.description)
                };
                rows.push(
                    Row::new(agent.name.clone())
                        .description(description)
                        .control(
                            h_flex()
                                .gap_2()
                                .child(prepare)
                                .child(probe_button)
                                .child(uninstall),
                        )
                        .render(),
                );
            }
        }
        for (index, (agent_id, prepared)) in registry
            .prepared
            .iter()
            .filter(|(agent_id, _)| !result_ids.contains((*agent_id).as_str()))
            .enumerate()
        {
            let probe_id = agent_id.clone();
            let uninstall_id = agent_id.clone();
            let cwd = self.snapshot.cwd();
            let probing = registry.probe_pending.as_deref() == Some(agent_id.as_str());
            let uninstalling = registry.uninstall_pending.as_deref() == Some(agent_id.as_str());
            let probe = Button::new(("acp-prepared-probe", index))
                .outline()
                .xsmall()
                .label(if probing { "Probing…" } else { "Probe" })
                .disabled(probing)
                .on_click(cx.listener(move |view, _, _, _| {
                    view.perform(Intent::ProbeAcpAgent {
                        agent_id: probe_id.clone(),
                        cwd: cwd.clone(),
                    });
                }));
            let uninstall = Button::new(("acp-prepared-uninstall", index))
                .ghost()
                .xsmall()
                .label(if uninstalling {
                    "Removing…"
                } else {
                    "Remove"
                })
                .disabled(uninstalling)
                .on_click(cx.listener(move |view, _, _, _| {
                    view.perform(Intent::UninstallAcpAgent {
                        agent_id: uninstall_id.clone(),
                    });
                }));
            let probe_detail = registry
                .probes
                .get(agent_id)
                .map(|probe| format!(" · {}", if probe.ready { "ready" } else { "not ready" }));
            rows.push(
                Row::new(agent_id.clone())
                    .description(format!(
                        "Prepared {} ({}){}",
                        prepared.version,
                        prepared.distribution,
                        probe_detail.unwrap_or_default(),
                    ))
                    .control(h_flex().gap_2().child(probe).child(uninstall))
                    .render(),
            );
        }

        section(
            Some("ACP registry".into()),
            Some(
                div()
                    .text_xs()
                    .text_color(tint("textMuted", 0.8))
                    .child("Install and verify Agent Client Protocol providers from the connected Host.")
                    .into_any_element(),
            ),
            None,
            rows,
        )
        .into_any_element()
    }
}

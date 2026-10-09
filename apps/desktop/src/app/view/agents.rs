use super::*;

impl Desktop {
    pub(super) fn agents_panel(&self) -> AnyElement {
        let agents = self.snapshot.agent_panel();
        let working = agents.iter().filter(|agent| agent.active).count();
        v_flex()
            .debug_selector(|| "agents-panel".into())
            .size_full()
            .min_h_0()
            .gap_3()
            .p_4()
            .child(
                h_flex()
                    .gap_2()
                    .child(IconName::Network)
                    .child(div().flex_1().font_semibold().child("エージェント"))
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(appearance::MUTED))
                            .child(format!("{working} 件が実行中")),
                    ),
            )
            .child(
                div()
                    .id("agents-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(
                        v_flex()
                            .gap_2()
                            .when(agents.is_empty(), |list| {
                                list.child(
                                    div()
                                        .text_sm()
                                        .text_color(rgb(appearance::MUTED))
                                        .child("サブエージェントが起動するとここに表示されます。"),
                                )
                            })
                            .children(agents.into_iter().map(|agent| {
                                v_flex()
                                    .id(SharedString::from(agent.key))
                                    .debug_selector(|| "agent-row".into())
                                    .ml(px(agent.depth as f32 * 16.))
                                    .gap_1()
                                    .p_3()
                                    .rounded(px(8.))
                                    .border_1()
                                    .border_color(rgb(appearance::BORDER))
                                    .bg(rgb(appearance::RAISED))
                                    .child(
                                        h_flex()
                                            .h(px(20.))
                                            .gap_2()
                                            .when(agent.active, |row| {
                                                row.child(spinner::Spinner::new().small())
                                            })
                                            .child(
                                                div()
                                                    .flex_1()
                                                    .min_w_0()
                                                    .text_sm()
                                                    .text_ellipsis()
                                                    .child(agent.title),
                                            )
                                            .when(agent.unread, |row| {
                                                row.child(
                                                    div()
                                                        .size(px(6.))
                                                        .rounded_full()
                                                        .bg(rgb(0xffffff)),
                                                )
                                            })
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .text_color(rgb(appearance::MUTED))
                                                    .child(agent.status),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .h(px(20.))
                                            .text_sm()
                                            .text_ellipsis()
                                            .text_color(rgb(appearance::MUTED))
                                            .child(agent.detail.unwrap_or_default()),
                                    )
                                    .child(
                                        div()
                                            .h(px(16.))
                                            .text_xs()
                                            .text_color(rgb(appearance::MUTED))
                                            .child(agent.model.unwrap_or_default()),
                                    )
                            })),
                    ),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;
    use gpui::{TestAppContext, size};

    #[gpui::test]
    fn agents_open_to_the_right_without_changing_parent_or_draft(cx: &mut TestAppContext) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        cx.update(|cx| {
            crate::appearance::init(cx);
            cx.set_global(Runtime {
                handle: runtime.handle().clone(),
                connections: Arc::default(),
                closing: tokio_util::task::TaskTracker::new(),
                logging_error: None,
            });
        });
        let (view, window) = cx.add_window_view(|window,cx| {
            let mut desktop = Desktop::new(Mode::Main,window,cx);
            desktop.onboarding = false;
            desktop.panel_open = false;
            let parent = agent_protocol::session::SessionRef { provider:agent_protocol::session::ProviderKind::Codex, id:"parent".into() };
            let snapshot = Arc::make_mut(&mut desktop.snapshot);
            snapshot.threads = Some(Arc::new(serde_json::from_value(serde_json::json!({
                "data":[{"id":{"provider":"codex","id":"parent"},"name":"Parent"}],
                "projects":[],"hasMore":false,"hasMoreProjects":false,"projectPages":{},"limit":5,})).unwrap()));
            Arc::make_mut(&mut snapshot.conversations).insert(
                agent_protocol::session::SessionRef { provider: parent.provider, id:"child".into() },
                Arc::new(serde_json::from_value(serde_json::json!({"id":{"provider":"codex","id":"child"},"parentId":{"provider":"codex","id":"parent"},"name":"Review","status":"running"})).unwrap())
            );
            Arc::make_mut(&mut snapshot.navigation).thread_id = Some(parent.clone());
            Arc::make_mut(&mut snapshot.navigation).draft_key = parent.clone().into();
            Arc::make_mut(&mut snapshot.drafts).insert(parent.into(),Arc::new(Draft { text:"Keep my draft".into(),..Default::default() }));
            desktop
        });
        window.simulate_resize(size(px(1600.), px(900.)));
        window.run_until_parked();
        assert!(window.debug_bounds("agents-panel").is_none());
        let toggle = window.debug_bounds("agents-toggle").unwrap().center();
        window.simulate_click(toggle, Modifiers::default());
        window.run_until_parked();
        let agents = window.debug_bounds("agents-panel").unwrap();
        let chat = window.debug_bounds("conversation-chat").unwrap();
        assert!(
            agents.left() >= chat.right(),
            "agents must be to the right of the parent conversation"
        );
        let row_before = window.debug_bounds("agent-row").unwrap();
        view.update(window, |desktop, cx| {
            assert!(desktop.panel == Panel::Agents);
            assert_eq!(desktop.selected().unwrap().id, "parent");
            assert_eq!(desktop.draft().text, "Keep my draft");
            assert_eq!(desktop.snapshot.thread_list().unwrap().threads.len(), 1);
            assert!(desktop.snapshot.agent_panel()[0].active);
            let snapshot = Arc::make_mut(&mut desktop.snapshot);
            let child = snapshot.agent_panel()[0].key.clone();
            Arc::make_mut(&mut snapshot.activity).active.insert(
                agent_protocol::session::SessionRef {
                    provider: agent_protocol::session::ProviderKind::Codex,
                    id: "child".into(),
                },
                false,
            );
            assert_eq!(snapshot.agent_panel()[0].key, child);
            assert!(!snapshot.agent_panel()[0].active);
            cx.notify();
        });
        window.run_until_parked();
        assert_eq!(window.debug_bounds("agent-row").unwrap(), row_before);
        let close = window.debug_bounds("panel-toggle").unwrap().center();
        window.simulate_click(close, Modifiers::default());
        window.run_until_parked();
        assert!(window.debug_bounds("agents-panel").is_none());
        assert!(window.debug_bounds("conversation-chat").is_some());
        view.update(window, |desktop, _| {
            assert_eq!(desktop.selected().unwrap().id, "parent");
            assert_eq!(desktop.draft().text, "Keep my draft");
        });
    }
}

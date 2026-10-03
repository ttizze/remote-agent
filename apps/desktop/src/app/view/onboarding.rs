use super::*;
use agent_core::presentation::connections::{AgentAvailability, ConnectionAgent};

impl Desktop {
    pub(super) fn connection_agent_controls(
        &self,
        agents: &[ConnectionAgent],
        cx: &Context<Self>,
    ) -> Div {
        let login_provider = self.account_provider();
        let login = self.snapshot.account.login.is_some();
        let disabled = !self.snapshot.connected || self.account_busy || login;
        let mut body = v_flex().gap_2().child(
            div()
                .text_xs()
                .text_color(rgb(0x949494))
                .child("この環境で使えるAI"),
        );
        for agent in agents {
            let provider = agent.provider;
            if login && provider == login_provider {
                body = body.child(self.account_login_controls(Some(&agent.name), cx));
                continue;
            }
            let pending = self.account_busy && provider == login_provider;
            let mut row = h_flex()
                .items_center()
                .gap_3()
                .py_1()
                .child(div().flex_1().child(agent.name.clone()))
                .child(
                    div()
                        .text_sm()
                        .text_color(if agent.availability == AgentAvailability::Ready {
                            rgb(0x88c9a0)
                        } else {
                            rgb(0x949494)
                        })
                        .child(if pending {
                            "接続中…".into()
                        } else {
                            agent.label.clone()
                        }),
                );
            if matches!(
                agent.availability,
                AgentAvailability::LoginRequired | AgentAvailability::Unavailable
            ) {
                let id = format!("connection-agent-{}", agent.name);
                row = row.child(
                    self.button(id.clone(), "ログイン", cx, move |view, _, _| {
                        view.account_operation(Intent::StartAccountLogin(op::StartAccountLogin {
                            provider,
                        }));
                    })
                    .debug_selector(move || id.clone())
                    .small()
                    .ghost()
                    .disabled(disabled),
                );
            }
            body = body.child(row);
        }
        body
    }

    fn complete_onboarding(&mut self) {
        if !self.snapshot.connection_setup().can_start {
            return;
        }
        let directory = match platform::state_dir() {
            Ok(directory) => directory,
            Err(error) => {
                self.set_error(error);
                return;
            }
        };
        self.effect(
            async move {
                tokio::fs::create_dir_all(&directory)
                    .await
                    .map_err(|error| error.to_string())?;
                tokio::fs::write(directory.join("onboarding.completed"), b"completed\n")
                    .await
                    .map_err(|error| error.to_string())
            },
            Update::OnboardingCompleted,
        );
    }

    pub(super) fn onboarding_view(&self, window: &Window, cx: &mut Context<Self>) -> Div {
        let setup = self.snapshot.connection_setup();
        let current = self.remote.as_ref().map(|host| host.id.as_str());
        let connected = self.snapshot.connected;
        let agent_controls = self.connection_agent_controls(&setup.agents, cx);
        let choices = self.hosts.as_ref().map(|hosts| {
            hosts.update(cx, |hosts, cx| {
                hosts.connection_choices(
                    ConnectionLayout::Onboarding,
                    current,
                    connected,
                    agent_controls,
                    cx,
                )
            })
        });
        let checking = setup
            .agents
            .iter()
            .any(|agent| agent.availability == AgentAvailability::Checking);
        let mut body = v_flex()
            .flex_shrink_0()
            .gap_5()
            .p_6()
            .child(
                v_flex()
                    .gap_2()
                    .child(
                        div()
                            .text_size(px(24.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("どこで作業しますか？"),
                    )
                    .child(
                        div()
                            .text_color(rgb(0x949494))
                            .child("手元のPCでも、クラウドでも。いつもの環境でAIを使えます。"),
                    ),
            )
            .children(choices);
        if !self.error.is_empty() {
            body = body.child(
                div()
                    .text_color(rgb(0xff8e86))
                    .child(error_message(&self.error)),
            );
        }
        let start_label = if self.connecting || !connected {
            "接続中…"
        } else if setup.can_start {
            "この環境で始める"
        } else if checking {
            "AIを確認中…"
        } else {
            "AIにログインしてください"
        };
        let footer = h_flex()
            .flex_shrink_0()
            .items_center()
            .gap_4()
            .px_6()
            .py_4()
            .border_t_1()
            .border_color(rgb(0x292929))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_xs()
                    .text_color(rgb(0x949494))
                    .child("既存の会話とプロジェクトは自動で表示されます。"),
            )
            .child(
                self.button("onboarding-start", start_label, cx, |view, _, _| {
                    view.complete_onboarding();
                })
                .debug_selector(|| "onboarding-start".into())
                .primary()
                .icon(IconName::ArrowRight)
                .disabled(self.connecting || !setup.can_start),
            );
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .px_6()
            .py_8()
            .bg(rgb(0x101010))
            .text_color(rgb(0xececec))
            .text_size(px(14.))
            .child(
                v_flex()
                    .w_full()
                    .max_w(px(768.))
                    .max_h(window.viewport_size().height - px(80.))
                    .id("onboarding-scroll")
                    .overflow_y_scroll()
                    .rounded(px(18.))
                    .border_1()
                    .border_color(rgb(0x292929))
                    .bg(rgb(0x141414))
                    .child(
                        h_flex()
                            .flex_shrink_0()
                            .items_center()
                            .gap_2()
                            .px_6()
                            .pt_5()
                            .pb_3()
                            .child(
                                div()
                                    .text_size(px(23.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("Bex"),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .text_color(rgb(0x949494))
                                    .child("セットアップ"),
                            )
                            .child(
                                self.icon_button(
                                    "onboarding-refresh",
                                    IconName::RotateCw,
                                    "接続とAIの状態を再確認",
                                    cx,
                                    |view, _, cx| {
                                        if view.snapshot.connected {
                                            view.refresh_accounts_and_models();
                                        } else {
                                            view.connect(None);
                                        }
                                        if let Some(hosts) = &view.hosts {
                                            hosts.update(cx, |hosts, _| hosts.refresh());
                                        }
                                    },
                                )
                                .disabled(self.connecting),
                            ),
                    )
                    .child(body)
                    .child(footer),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::{Desktop, Hosts, Mode, Snapshot, Tab};
    use crate::{Runtime, store_session::StoreSession};
    use agent_core::store::Store;
    use gpui_kit as gpui;
    use gpui_kit::{
        AppContext, Context, Entity, IntoElement, Modifiers, Render, TestAppContext, Window,
    };
    use std::sync::Arc;

    struct SetupView(Entity<Desktop>);

    impl Render for SetupView {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            self.0
                .update(cx, |view, cx| view.onboarding_view(window, cx))
        }
    }

    #[gpui::test]
    fn onboarding_login_stays_in_the_environment_card_and_requires_a_code(cx: &mut TestAppContext) {
        // Keep external connection and authentication tasks parked.
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let app_runtime = Runtime {
            handle: runtime.handle().clone(),
            connections: Arc::default(),
            closing: tokio_util::task::TaskTracker::new(),
            logging_error: None,
        };
        let session = runtime.block_on(async {
            let (send, receive) = async_channel::unbounded();
            tokio::spawn(StoreSession::publish(
                Ok(Arc::new(Store::offline(Snapshot::default()))),
                app_runtime.clone(),
                send,
                Some,
                |_| None,
            ));
            receive.recv().await.unwrap().unwrap().unwrap()
        });
        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.set_global(app_runtime);
        });
        let (view, window) = cx.add_window_view(|window, cx| {
            let desktop = cx.new(|cx| Desktop::new(Mode::SideChat {
                remote: None, cwd: "/fixture".into(),
            }, window, cx));
            desktop.update(cx, |view, cx| {
                view.hosts = Some(cx.new(|cx| Hosts::new(window, cx)));
                view.session = Some(session);
                view.connecting = false;
                let snapshot = Arc::make_mut(&mut view.snapshot);
                snapshot.connected = true;
                snapshot.models = Arc::new(serde_json::from_value(serde_json::json!([
                    {"id":"gpt","model":{"provider":"codex","id":"gpt"},"displayName":"GPT","defaultReasoningEffort":"","supportedReasoningEfforts":[]},
                    {"id":"sonnet","model":{"provider":"claude","id":"sonnet"},"displayName":"Sonnet","defaultReasoningEffort":"","supportedReasoningEfforts":[]}
                ])).unwrap());
                Arc::make_mut(&mut snapshot.account).accounts = Some(Arc::new(serde_json::from_value(serde_json::json!({"accounts":[]})).unwrap()));
            });
            cx.observe(&desktop, |_, _, cx| cx.notify()).detach();
            SetupView(desktop)
        });
        window.run_until_parked();
        let start = window.debug_bounds("onboarding-start").unwrap();
        window.simulate_click(start.center(), Modifiers::default());
        window.update(|_, cx| assert!(view.read(cx).0.read(cx).tab == Tab::Chat));
        let login = window.debug_bounds("connection-agent-Claude Code").unwrap();
        window.simulate_click(login.center(), Modifiers::default());
        window.update(|_, cx| {
            let desktop = view.read(cx).0.read(cx);
            assert!(desktop.tab == Tab::Chat);
            assert_eq!(
                desktop.model_provider,
                Some(agent_protocol::session::ProviderKind::Claude)
            );
            assert!(desktop.account_busy);
        });
        window.update(|_, cx| {
            view.read(cx).0.clone().update(cx, |view, cx| {
                view.account_busy = false;
                view.account_polling = true;
                Arc::make_mut(&mut Arc::make_mut(&mut view.snapshot).account).login =
                    Some(Arc::new(agent_protocol::operations::AccountLogin {
                        login_id: "fixture-login".into(),
                        requires_code_submission: true,
                        user_code: String::new(),
                        verification_url: "https://example.invalid/login".into(),
                    }));
                cx.notify();
            })
        });
        window.run_until_parked();
        assert!(
            window
                .debug_bounds("connection-agent-Claude Code")
                .is_none()
        );
        assert!(window.debug_bounds("connection-agent-Codex").is_some());
        assert!(window.debug_bounds("account-open-login").is_some());
        let submit = window.debug_bounds("account-submit-code").unwrap();
        window.simulate_click(submit.center(), Modifiers::default());
        window.update(|_, cx| assert!(!view.read(cx).0.read(cx).account_busy));
        window.update(|window, cx| {
            view.read(cx).0.clone().update(cx, |view, cx| {
                view.account_code
                    .update(cx, |input, cx| input.set_value("fixture-code", window, cx));
            })
        });
        window.run_until_parked();
        window.simulate_click(submit.center(), Modifiers::default());
        window.update(|_, cx| {
            let desktop = view.read(cx).0.read(cx);
            assert!(desktop.tab == Tab::Chat);
            assert!(desktop.account_busy);
            assert!(desktop.account_code.read(cx).value().is_empty());
        });
        window.update(|_, cx| {
            view.read(cx).0.clone().update(cx, |view, cx| {
                view.account_busy = false;
                cx.notify();
            })
        });
        window.run_until_parked();
        let cancel = window.debug_bounds("account-cancel-login").unwrap();
        window.simulate_click(cancel.center(), Modifiers::default());
        window.update(|_, cx| {
            let desktop = view.read(cx).0.read(cx);
            assert!(desktop.tab == Tab::Chat);
            assert!(desktop.account_busy);
        });
    }
}

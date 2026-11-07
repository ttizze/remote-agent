impl Preferences {
    pub(super) fn connection_agent_controls(
        &self,
        agents: &[ConnectionAgent],
        cx: &Context<Self>,
    ) -> Div {
        let language = crate::localization::language(cx);
        let login_provider = self.account_provider();
        let login = self.snapshot.catalog.account.login.is_some();
        let disabled = !self.snapshot.connection.connected || self.account.busy || login;
        let mut body = v_flex().gap_2().child(
            div()
                .text_xs()
                .text_color(rgb(appearance::MUTED))
                .child(language.text("この環境で使えるAI")),
        );
        for agent in agents {
            let provider = agent.provider;
            if login && Some(provider) == login_provider {
                body = body.child(self.account_login_controls(Some(&agent.name), cx));
                continue;
            }
            let pending = self.account.busy && Some(provider) == login_provider;
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
                            rgb(appearance::MUTED)
                        })
                        .child(if pending {
                            language.text("接続中…").into()
                        } else {
                            agent_core::localization::localized_ui_text(
                                language,
                                agent.label.clone(),
                            )
                        }),
                );
            if !agent.enabled {
                row = row.child(
                    button(
                        format!("connection-provider-enable-{}", provider.key()),
                        language.text("オンにする"),
                        cx,
                        move |view, _, _| {
                            view.dispatch(Intent::SetProviderEnabled(op::SetProviderEnabled {
                                provider,
                                enabled: true,
                            }));
                        },
                    )
                    .small()
                    .ghost()
                    .disabled(
                        disabled
                            || self
                                .snapshot
                                .operation_running(op::OperationKey::ProviderSettings),
                    ),
                );
            } else if matches!(
                agent.availability,
                AgentAvailability::LoginRequired | AgentAvailability::Unavailable
            ) {
                let id = format!("connection-agent-{}", agent.name);
                row = row.child(
                    button(
                        id.clone(),
                        language.text("ログイン"),
                        cx,
                        move |view, window, cx| {
                            view.start_provider_setup(provider, window, cx);
                        },
                    )
                    .debug_selector(move || id.clone())
                    .small()
                    .ghost()
                    .disabled(disabled),
                );
            }
            let mut controls = v_flex().gap_1().child(row);
            if let Some(error) = &agent.error {
                controls = controls.child(
                    h_flex()
                        .gap_3()
                        .items_center()
                        .child(
                            div()
                                .flex_1()
                                .text_xs()
                                .text_color(rgb(0xffc773))
                                .child(error_view(error, language)),
                        )
                        .child(
                            button(
                                format!("connection-agent-retry-{}", provider.key()),
                                language.text("再確認"),
                                cx,
                                |view, _, _| view.refresh_accounts_and_models(),
                            )
                            .ghost()
                            .small()
                            .disabled(disabled),
                        ),
                );
            }
            body = body.child(controls);
        }
        body
    }
}
use super::*;
use agent_core::presentation::connections::{AgentAvailability, ConnectionAgent};
use gpui_kit::component::checkbox::Checkbox;

impl Desktop {
    pub(in crate::app) fn complete_onboarding(&mut self) {
        if !self.onboarding {
            self.import_projects = false;
            return;
        }
        if !self.snapshot.connection.connected {
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
        let language = crate::localization::language(cx);
        if self.import_projects {
            return self.project_import_view(window, cx);
        }
        let setup = self.snapshot.connection_setup();
        let current = self.remote.as_ref().map(|host| host.id.as_str());
        let connected = self.snapshot.connection.connected;
        let agent_controls = self.preferences.update(cx, |view, cx| {
            view.connection_agent_controls(&setup.agents, cx)
        });
        let choices = self.hosts.as_ref().map(|hosts| {
            hosts.update(cx, |hosts, cx| {
                hosts.connection_choices(
                    ConnectionLayout::Onboarding,
                    current,
                    connected,
                    self.snapshot.connection_enabled(),
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
            .child(super::settings::language_settings(cx, |view, error| {
                view.set_error(error)
            }))
            .child(
                v_flex()
                    .gap_2()
                    .child(
                        div()
                            .text_size(px(24.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(language.text("どこで作業しますか？")),
                    )
                    .child(div().text_color(rgb(appearance::MUTED)).child(
                        language.text("手元のPCでも、クラウドでも。いつもの環境でAIを使えます。"),
                    )),
            )
            .children(choices);
        body = body
            .child(self.error_notices(ErrorScope::Projects, cx))
            .child(self.error_notices(ErrorScope::Settings, cx))
            .child(self.error_notices(ErrorScope::Application, cx));
        let start_label = if self.connecting || !connected {
            language.text("接続中…")
        } else if setup.can_start {
            language.text("プロジェクトを選ぶ")
        } else if checking {
            language.text("AIを確認中…")
        } else {
            language.text("AIにログインしてください")
        };
        let footer = h_flex()
            .flex_shrink_0()
            .items_center()
            .gap_4()
            .px_6()
            .py_4()
            .border_t_1()
            .border_color(rgb(appearance::BORDER))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_xs()
                    .text_color(rgb(appearance::MUTED))
                    .child(language.text("次に、既存のプロジェクトを選んで取り込めます。")),
            )
            .children((!setup.can_start).then(|| {
                button(
                    "onboarding-skip-agents",
                    language.text("AIは後で設定"),
                    cx,
                    |view, _, _| {
                        view.open_project_import();
                    },
                )
                .debug_selector(|| "onboarding-skip-agents".into())
                .ghost()
                .disabled(self.connecting || !connected)
            }))
            .child(
                button("onboarding-start", start_label, cx, |view, _, _| {
                    view.open_project_import();
                })
                .debug_selector(|| "onboarding-start".into())
                .primary()
                .icon(IconName::ArrowRight)
                .disabled(self.connecting || !setup.can_start),
            );
        self.onboarding_frame(body, footer, window, cx)
    }

    fn onboarding_frame(
        &self,
        body: Div,
        footer: Div,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let language = crate::localization::language(cx);
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .px_6()
            .py_8()
            .bg(rgb(appearance::BACKGROUND))
            .text_color(rgb(appearance::FOREGROUND))
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
                    .border_color(rgb(appearance::BORDER))
                    .bg(rgb(appearance::RAISED))
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
                                    .child("bex"),
                            )
                            .child(div().flex_1().text_color(rgb(appearance::MUTED)).child(
                                if self.import_projects {
                                    if self.onboarding {
                                        language.text("2 / 2 プロジェクトの取り込み")
                                    } else {
                                        language.text("プロジェクトの取り込み")
                                    }
                                } else {
                                    language.text("1 / 2 環境とAI")
                                },
                            ))
                            .child(
                                icon_button(
                                    "onboarding-refresh",
                                    IconName::RotateCw,
                                    if self.import_projects {
                                        language.text("既存のプロジェクトを再検索")
                                    } else {
                                        language.text("接続とAIの状態を再確認")
                                    },
                                    cx,
                                    |view, _, cx| {
                                        if view.import_projects {
                                            view.local_errors.remove(&ErrorScope::Projects);
                                            view.dispatch(Intent::ScanProjects(
                                                op::ScanProjects {},
                                            ));
                                            return;
                                        }
                                        if view.snapshot.connection.connected {
                                            view.preferences.read(cx).refresh_accounts_and_models();
                                        } else {
                                            view.connect(None, None);
                                        }
                                        if let Some(hosts) = &view.hosts {
                                            hosts.update(cx, |hosts, _| hosts.refresh());
                                        }
                                    },
                                )
                                .disabled(self.connecting || self.snapshot.project_import_busy()),
                            ),
                    )
                    .child(body)
                    .child(footer),
            )
    }

    pub(in crate::app) fn open_project_import(&mut self) {
        if !self.snapshot.connection.connected {
            return;
        }
        self.import_projects = true;
        self.local_errors.remove(&ErrorScope::Projects);
        self.dispatch(Intent::ScanProjects(op::ScanProjects {}));
    }

    fn project_import_view(&self, window: &Window, cx: &mut Context<Self>) -> Div {
        let language = crate::localization::language(cx);
        let state = &self.snapshot.project_import;
        let busy = self.snapshot.project_import_busy();
        let mut body = v_flex()
            .gap_4()
            .p_6()
            .child(section_heading(
                language.text("これまでのプロジェクトを引き継ぐ"),
                language
                    .text("CodexとClaude Codeの履歴から選べます。元の会話をそのまま再開できます。"),
            ))
            .child(div().text_sm().text_color(rgb(appearance::MUTED)).child(
                language.text(
                    "最近30日以内に使い、会話が3件以上あるGitリポジトリを初期選択しています。",
                ),
            ));
        if self
            .snapshot
            .operation_running(op::OperationKey::ProjectScan)
        {
            body = body.child(language.text("既存のプロジェクトを探しています…"));
        }
        body = body
            .child(self.error_notices(ErrorScope::Projects, cx))
            .child(self.error_notices(ErrorScope::Settings, cx))
            .child(self.error_notices(ErrorScope::Application, cx));
        if let Some(scan) = &state.scan {
            body = body.child(
                h_flex()
                    .gap_2()
                    .child(
                        button(
                            "import-select-all",
                            language.text("すべて選択"),
                            cx,
                            |view, _, _| {
                                view.dispatch(Intent::SelectAllImportProjects { selected: true })
                            },
                        )
                        .ghost()
                        .small()
                        .disabled(busy),
                    )
                    .child(
                        button(
                            "import-select-none",
                            language.text("選択を解除"),
                            cx,
                            |view, _, _| {
                                view.dispatch(Intent::SelectAllImportProjects { selected: false })
                            },
                        )
                        .ghost()
                        .small()
                        .disabled(busy),
                    ),
            );
            if scan.projects.is_empty() {
                body = body.child(language.text(
                    "取り込めるプロジェクトは見つかりませんでした。後から設定でも取り込めます。",
                ));
            }
            for (index, project) in scan.projects.iter().enumerate() {
                let selected = state.selected.contains(&project.path);
                let path = project.path.clone();
                let selector = format!("import-project-{index}");
                let providers = project
                    .providers
                    .iter()
                    .map(|provider| provider.key())
                    .collect::<Vec<_>>()
                    .join(" / ");
                body = body.child(
                    v_flex()
                        .gap_1()
                        .p_3()
                        .rounded_md()
                        .border_1()
                        .border_color(rgb(appearance::BORDER))
                        .child(
                            h_flex()
                                .gap_2()
                                .items_center()
                                .child(
                                    div().debug_selector(move || selector.clone()).child(
                                        Checkbox::new(("import-project", index))
                                            .label(project.name.clone())
                                            .checked(selected || project.registered)
                                            .disabled(busy || project.registered)
                                            .on_click(cx.listener(
                                                move |view, selected: &bool, _, _| {
                                                    view.dispatch(Intent::SelectImportProject {
                                                        path: path.clone(),
                                                        selected: *selected,
                                                    });
                                                },
                                            )),
                                    ),
                                )
                                .child(div().flex_1())
                                .child(div().text_xs().text_color(rgb(appearance::MUTED)).child(
                                    if project.registered {
                                        language.text("取り込み済み").into()
                                    } else {
                                        agent_core::localization::localized_ui_format(
                                            language,
                                            "{0}件の会話 · {1}".into(),
                                            vec![
                                                project.conversation_count.to_string(),
                                                providers.to_string(),
                                            ],
                                        )
                                    },
                                )),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(rgb(appearance::MUTED))
                                .child(project.path.clone()),
                        ),
                );
            }
            for (provider, warning) in &scan.warnings {
                body = body.child(
                    div()
                        .text_sm()
                        .text_color(rgb(0xffc773))
                        .child(format!("{provider}: {warning}")),
                );
            }
        }
        if let Some(result) = &state.result {
            if !result.imported.is_empty() {
                body = body.child(agent_core::localization::localized_ui_format(
                    language,
                    "{0}件のプロジェクトを取り込みました。".into(),
                    vec![result.imported.len().to_string()],
                ));
            }
            for (provider, warning) in &result.warnings {
                body = body.child(
                    div()
                        .text_sm()
                        .text_color(rgb(0xffc773))
                        .child(format!("{provider}: {warning}")),
                );
            }
            for (path, error) in &result.errors {
                body = body.child(
                    div()
                        .text_sm()
                        .text_color(rgb(0xff8e86))
                        .child(format!("{path}: {error}")),
                );
            }
        }
        let importing = self
            .snapshot
            .operation_running(op::OperationKey::ProjectImport);
        let footer = h_flex()
            .gap_3()
            .items_center()
            .px_6()
            .py_4()
            .border_t_1()
            .border_color(rgb(appearance::BORDER))
            .child(
                button("import-back", language.text("戻る"), cx, |view, _, _| {
                    view.import_projects = false
                })
                .ghost()
                .debug_selector(|| "import-back".into())
                .disabled(importing),
            )
            .child(div().flex_1())
            .child(
                button(
                    "import-skip",
                    if self.onboarding {
                        language.text("今はスキップ")
                    } else {
                        language.text("閉じる")
                    },
                    cx,
                    |view, _, _| view.complete_onboarding(),
                )
                .debug_selector(|| "import-skip".into())
                .ghost()
                .disabled(importing),
            )
            .child(
                button(
                    "import-selected",
                    if busy {
                        language.text("読み込み中…").into()
                    } else {
                        agent_core::localization::localized_ui_format(
                            language,
                            "{0}件を取り込む".into(),
                            vec![state.selected.len().to_string()],
                        )
                    },
                    cx,
                    |view, _, _| {
                        view.perform(
                            Intent::ImportProjects(op::ImportProjects {}),
                            OperationCompletion::ProjectImport,
                        );
                    },
                )
                .debug_selector(|| "import-selected".into())
                .primary()
                .disabled(busy || state.selected.is_empty() || !self.snapshot.connection.connected),
            );
        self.onboarding_frame(body, footer, window, cx)
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
            sessions: Default::default(),
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
            crate::appearance::init(cx);
            cx.set_global(app_runtime);
        });
        let (view, window) = cx.add_window_view(|window, cx| {
            let desktop = cx.new(|cx| Desktop::new(Mode::Fixture {
                remote: None, cwd: "/fixture".into(),
            }, window, cx));
            desktop.update(cx, |view, cx| {
                view.hosts = Some(cx.new(|cx| Hosts::new(window, cx)));
                view.session = Some(session);
                view.connecting = false;
                let snapshot = Arc::make_mut(&mut view.snapshot);
                snapshot.connection.connected = true;
                snapshot.catalog.models = Arc::new(serde_json::from_value(serde_json::json!([
                    {"id":"gpt","model":{"provider":"codex","id":"gpt"},"displayName":"GPT", "settings": {"defaultReasoningEffort":"","supportedReasoningEfforts":[]}},
                    {"id":"sonnet","model":{"provider":"claude","id":"sonnet"},"displayName":"Sonnet", "settings": {"defaultReasoningEffort":"","supportedReasoningEfforts":[]}}
                ])).unwrap());
                Arc::make_mut(&mut snapshot.catalog.account).accounts = Some(Arc::new(serde_json::from_value(serde_json::json!({"accounts":[],"selected":{}})).unwrap()));
            });
            cx.observe(&desktop, |_, _, cx| cx.notify()).detach();
            let preferences = desktop.read(cx).preferences.clone();
            cx.observe(&preferences, |_, _, cx| cx.notify()).detach();
            SetupView(desktop)
        });
        window.run_until_parked();
        let start = window.debug_bounds("onboarding-start").unwrap();
        window.simulate_click(start.center(), Modifiers::default());
        window.update(|_, cx| assert!(view.read(cx).0.read(cx).tab == Tab::Chat));
        window.update(|_, cx| assert!(!view.read(cx).0.read(cx).import_projects));
        let later = window.debug_bounds("onboarding-skip-agents").unwrap();
        window.simulate_click(later.center(), Modifiers::default());
        window.run_until_parked();
        window.update(|_, cx| assert!(view.read(cx).0.read(cx).import_projects));
        let back = window.debug_bounds("import-back").unwrap();
        window.simulate_click(back.center(), Modifiers::default());
        window.run_until_parked();
        let login = window.debug_bounds("connection-agent-Claude Code").unwrap();
        window.simulate_click(login.center(), Modifiers::default());
        window.update(|_, cx| {
            let desktop = view.read(cx).0.read(cx);
            assert!(desktop.tab == Tab::Chat);
            assert_eq!(
                desktop.preferences.read(cx).model_controls.provider,
                Some(agent_protocol::session::ProviderKind::Claude)
            );
            assert!(desktop.preferences.read(cx).account.busy);
        });
        window.update(|_, cx| {
            view.read(cx).0.clone().update(cx, |view, cx| {
                view.preferences.update(cx, |preferences, _| {
                    preferences.account.busy = false;
                });
                view.preferences.update(cx, |preferences, _| {
                    preferences.account.polling = true;
                });
                Arc::make_mut(&mut Arc::make_mut(&mut view.snapshot).catalog.account).login =
                    Some(Arc::new(agent_protocol::operations::AccountLogin {
                        provider: agent_protocol::session::ProviderKind::Claude,
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
        window.update(|_, cx| assert!(!view.read(cx).0.read(cx).preferences.read(cx).account.busy));
        window.update(|window, cx| {
            view.read(cx).0.clone().update(cx, |view, cx| {
                view.preferences.update(cx, |preferences, cx| {
                    preferences
                        .account
                        .code
                        .update(cx, |input, cx| input.set_value("fixture-code", window, cx));
                });
            })
        });
        window.run_until_parked();
        window.simulate_click(submit.center(), Modifiers::default());
        window.update(|_, cx| {
            let desktop = view.read(cx).0.read(cx);
            assert!(desktop.tab == Tab::Chat);
            assert!(desktop.preferences.read(cx).account.busy);
            assert!(
                desktop
                    .preferences
                    .read(cx)
                    .account
                    .code
                    .read(cx)
                    .value()
                    .is_empty()
            );
        });
        window.update(|_, cx| {
            view.read(cx).0.clone().update(cx, |view, cx| {
                view.preferences.update(cx, |preferences, _| {
                    preferences.account.busy = false;
                });
                cx.notify();
            })
        });
        window.run_until_parked();
        let cancel = window.debug_bounds("account-cancel-login").unwrap();
        window.simulate_click(cancel.center(), Modifiers::default());
        window.update(|_, cx| {
            let desktop = view.read(cx).0.read(cx);
            assert!(desktop.tab == Tab::Chat);
            assert!(desktop.preferences.read(cx).account.busy);
        });
        window.update(|_, cx| {
            view.read(cx).0.clone().update(cx, |view, cx| {
                view.preferences.update(cx, |preferences, _| {
                    preferences.account.busy = false;
                });
                let account = Arc::make_mut(&mut Arc::make_mut(&mut view.snapshot).catalog.account);
                account.login = None;
                account.accounts = Some(Arc::new(serde_json::from_value(serde_json::json!({
                    "accounts":[{"id":"native","provider":"codex"}], "selected":{"codex":"native"}
                })).unwrap()));
                cx.notify();
            });
        });
        window.run_until_parked();
        let next = window.debug_bounds("onboarding-start").unwrap();
        window.simulate_click(next.center(), Modifiers::default());
        window.run_until_parked();
        window.update(|_, cx| assert!(view.read(cx).0.read(cx).import_projects));
        assert!(window.debug_bounds("import-selected").is_some());
        let skip = window.debug_bounds("import-skip").unwrap();
        window.simulate_click(skip.center(), Modifiers::default());
        window.update(|_, cx| assert!(!view.read(cx).0.read(cx).import_projects));
    }
}

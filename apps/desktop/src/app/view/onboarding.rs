use super::*;
use agent_core::presentation::connections::AgentAvailability;

impl Desktop {
    pub(in crate::app) fn setup_agent(
        &mut self,
        provider: agent_protocol::session::ProviderKind,
        availability: AgentAvailability,
    ) {
        self.open_settings();
        self.model_provider = Some(provider);
        if availability == AgentAvailability::LoginRequired {
            self.account_operation(Intent::StartAccountLogin(op::StartAccountLogin {
                provider,
            }));
        }
    }

    fn complete_onboarding(&mut self) {
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
        let choices = self.hosts.as_ref().map(|hosts| {
            hosts.update(cx, |hosts, cx| {
                hosts.connection_choices(
                    ConnectionLayout::Onboarding,
                    current,
                    connected,
                    &setup.agents,
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
            "AIを設定"
        };
        let can_start = setup.can_start;
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
                self.button("onboarding-start", start_label, cx, move |view, _, _| {
                    if can_start {
                        view.complete_onboarding();
                    } else {
                        view.open_settings();
                    }
                })
                .primary()
                .icon(IconName::ArrowRight)
                .disabled(!connected || self.connecting || (!can_start && checking)),
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
                                            view.connect();
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

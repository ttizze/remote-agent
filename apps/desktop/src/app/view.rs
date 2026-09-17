mod composer;
mod conversation;
mod media;
mod settings;
mod sidebar;
mod workbench;
use super::*;
use agent_core::state::operations as op;
use base64::Engine;
use gpui_kit::component::{
    resizable::{h_resizable, resizable_panel},
    sidebar::{Sidebar, SidebarItem, SidebarMenu, SidebarMenuItem},
    tab::{Tab as UiTab, TabBar},
};
use std::path::PathBuf;

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or_default()
}
fn array(value: &Value) -> &[Value] {
    value.as_array().map(Vec::as_slice).unwrap_or_default()
}
fn field<'a>(map: &'a serde_json::Map<String, Value>, key: &str) -> &'a Value {
    map.get(key).unwrap_or(&Value::Null)
}
fn file_name(path: &str) -> String {
    Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(path)
        .to_owned()
}

const CHAT_WIDTH: f32 = 780.;

fn fitted_image(source: ImageSource, height: f32) -> Img {
    img(source)
        .w_full()
        .min_w_0()
        .h(px(height))
        .min_h(px(height))
        .max_h(px(height))
        .object_fit(ObjectFit::Contain)
        .debug_selector(|| "chat-image".into())
}

fn user_message_bubble() -> Div {
    v_flex()
        .debug_selector(|| "user-message-bubble".into())
        .gap_3()
        .min_w_0()
        .max_w(px(560.))
        .p_4()
        .rounded(px(18.))
        .bg(rgb(0x303030))
}

fn review_counts(additions: Option<u64>, deletions: Option<u64>) -> AnyElement {
    let counts = h_flex().gap_1().text_xs();
    match (additions, deletions) {
        (Some(added), Some(deleted)) => counts
            .child(div().text_color(rgb(0x37cf77)).child(format!("+{added}")))
            .child(div().text_color(rgb(0xff6259)).child(format!("−{deleted}")))
            .into_any_element(),
        _ => counts
            .text_color(rgb(0x999999))
            .child("バイナリ")
            .into_any_element(),
    }
}

fn model_effort_slider(
    state: &Entity<slider::SliderState>,
    effort_count: usize,
    cx: &App,
) -> impl IntoElement {
    let disabled = effort_count < 2;
    let steps = effort_count.saturating_sub(1).max(1) as f32;
    let position = state.read(cx).percentage().end;
    let track = base::SliderIndicator::new(state)
        .relative()
        .w_full()
        .h(px(24.))
        .child(
            div()
                .absolute()
                .left(px(-14.))
                .right(relative(1. - position))
                .h_full()
                .rounded_full()
                .bg(rgb(0x3982f7)),
        )
        .children((0..effort_count).map(|i| {
            div()
                .absolute()
                .left(relative(i as f32 / steps))
                .ml(px(-2.))
                .top(px(10.))
                .size(px(4.))
                .rounded_full()
                .bg(rgb(0x9c9c9c))
        }))
        .child(
            base::SliderThumb::new(state)
                .disabled(disabled)
                .absolute()
                .left(relative(position))
                .ml(px(-14.))
                .top(px(-2.))
                .size(px(28.))
                .rounded_full()
                .bg(rgb(0xffffff)),
        );
    base::Slider::new(state)
        .disabled(disabled)
        .w_full()
        .py_1()
        .child(
            base::SliderTrack::new(state)
                .disabled(disabled)
                .w_full()
                .h(px(24.))
                .px(px(14.))
                .rounded_full()
                .bg(rgb(0x454545))
                .child(track),
        )
}

fn conversation_file_path(source: &str, cwd: &str) -> Result<PathBuf, String> {
    let source = source
        .rsplit_once(':')
        .filter(|(_, line)| !line.is_empty() && line.bytes().all(|c| c.is_ascii_digit()))
        .map_or(source, |(path, _)| path);
    let base = url::Url::from_directory_path(cwd).map_err(|_| "作業フォルダが不正です")?;
    let mut url = base.join(source).map_err(|e| e.to_string())?;
    if url.scheme() != "file" {
        return Err("未対応のリンクです".into());
    }
    url.set_fragment(None);
    url.set_query(None);
    url.to_file_path()
        .map_err(|_| "ファイルパスが不正です".into())
}

#[cfg(test)]
mod model_slider_tests {
    use super::model_effort_slider;
    use gpui_kit as gpui;
    use gpui_kit::{
        AppContext, Context, Entity, IntoElement, Modifiers, MouseButton, ParentElement, Render,
        Styled, TestAppContext, Window, component::slider, div, point, px,
    };

    struct SliderView {
        state: Entity<slider::SliderState>,
        effort_count: usize,
    }

    impl Render for SliderView {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .w(px(280.))
                .p_4()
                .child(model_effort_slider(&self.state, self.effort_count, cx))
        }
    }

    #[gpui::test]
    fn effort_thumb_drags_both_ways_and_single_option_is_inert(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        for effort_count in [4, 1] {
            let state = cx.new(|_| {
                slider::SliderState::new()
                    .max(3.)
                    .step(1.)
                    .default_value(1.)
            });
            let owner = state.clone();
            let (_, cx) = cx.add_window_view(move |_, _| SliderView {
                state: owner,
                effort_count,
            });
            cx.update(|window, cx| window.draw(cx).clear(cx));
            let bounds = cx.update(|_, cx| state.read(cx).bounds());
            let at = |fraction| {
                point(
                    bounds.left() + bounds.size.width * fraction,
                    bounds.center().y,
                )
            };
            let mut from = 1. / 3.;
            for to in [1., 0.] {
                cx.simulate_mouse_move(at(from), None, Modifiers::default());
                cx.simulate_mouse_down(at(from), MouseButton::Left, Modifiers::default());
                for step in 1..=8 {
                    cx.simulate_mouse_move(
                        at(from + (to - from) * step as f32 / 8.),
                        MouseButton::Left,
                        Modifiers::default(),
                    );
                }
                cx.simulate_mouse_up(at(to), MouseButton::Left, Modifiers::default());
                cx.update(|_, cx| {
                    assert_eq!(
                        state.read(cx).value(),
                        slider::SliderValue::Single(if effort_count > 1 { to * 3. } else { 1. })
                    )
                });
                from = to;
            }
        }
    }
}

#[cfg(test)]
mod link_tests {
    use super::conversation_file_path;

    #[test]
    fn file_links_resolve_on_the_selected_host_with_spaces_and_line_numbers() {
        for source in [
            "/tmp/project/a%20b.png",
            "file:///tmp/project/a%20b.png",
            "a%20b.png",
            "a%20b.png:12",
            "a%20b.png#L12",
        ] {
            assert_eq!(
                conversation_file_path(source, "/tmp/project").unwrap(),
                std::path::Path::new("/tmp/project/a b.png")
            );
        }
        assert!(conversation_file_path("https://example.com", "/tmp/project").is_err());
        assert!(conversation_file_path("javascript:alert(1)", "/tmp/project").is_err());
        assert!(conversation_file_path("file://other-host/private.png", "/tmp/project").is_err());
    }
}

impl Render for Desktop {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let active = self.image_gallery.is_none() && self.panel_open && self.tab == Tab::Chat;
        let composer_visible = self.image_gallery.is_none()
            && self.tab == Tab::Chat
            && (!self.panel_open
                || (!self.side_chat_mode && window.viewport_size().width >= px(1080.)));
        if !composer_visible {
            self.cancel_recording();
        }
        if let Some(chat) = &self.side_chat
            && (!active || self.panel != Panel::SideChat)
        {
            chat.update(cx, |chat, _| chat.cancel_recording());
        }
        if let Some(view) = self.browser.clone() {
            view.update(cx, |v, cx| {
                v.set_visible(active && self.panel == Panel::Browser, cx)
            });
        }
        if self.image_gallery.is_some() {
            let gallery = self.image_gallery_view(window, cx);
            return h_flex()
                .size_full()
                .bg(rgb(0x181818))
                .text_color(rgb(0xececec))
                .text_size(px(14.))
                .child(gallery);
        }
        if self.tab == Tab::Settings {
            return h_flex()
                .size_full()
                .items_stretch()
                .bg(rgb(0x191919))
                .text_color(rgb(0xececec))
                .text_size(px(14.))
                .font_weight(FontWeight::NORMAL)
                .child(self.settings_sidebar(cx))
                .child(self.settings(cx));
        }
        let wide = window.viewport_size().width >= px(1080.);
        let title = self
            .thread()
            .and_then(|thread| thread.name.as_deref())
            .or_else(|| {
                self.snapshot
                    .threads
                    .as_ref()
                    .and_then(|page| {
                        page.data
                            .iter()
                            .find(|thread| thread.id.as_deref() == Some(self.selected()))
                    })
                    .and_then(|thread| thread.name.as_deref())
            })
            .unwrap_or("新しいチャット")
            .to_owned();
        let mut header = h_flex().h(px(48.)).flex_shrink_0().px_4().gap_2();
        if !self.sidebar {
            header = header.pl(px(88.)).child(self.icon_button(
                "expand-sidebar",
                IconName::PanelLeftOpen,
                "サイドバーを開く",
                cx,
                |s, _, _| s.sidebar = true,
            ));
        }
        header = header.child(
            div()
                .flex_1()
                .min_w_0()
                .text_sm()
                .text_ellipsis()
                .child(title),
        );
        header = header.child(
            self.icon_button(
                "panel-toggle",
                if self.panel_open {
                    IconName::PanelRightClose
                } else {
                    IconName::PanelRightOpen
                },
                "右パネルを切り替え",
                cx,
                |s, _, _| s.panel_open = !s.panel_open,
            )
            .selected(self.panel_open),
        );
        let content = if self.side_chat_mode {
            if self.panel_open {
                self.workbench(cx)
            } else {
                self.chat(cx)
            }
        } else if self.panel_open {
            let right = self.workbench(cx);
            if wide {
                h_resizable("chat-workbench-split")
                    .child(
                        resizable_panel()
                            .size_range(px(360.)..px(2400.))
                            .child(self.chat(cx)),
                    )
                    .child(
                        resizable_panel()
                            .size(px(540.))
                            .size_range(px(320.)..px(1100.))
                            .child(right),
                    )
                    .into_any_element()
            } else {
                right
            }
        } else {
            h_flex()
                .size_full()
                .items_stretch()
                .child(self.chat(cx))
                .when(window.viewport_size().width >= px(1280.), |v| {
                    v.child(
                        div()
                            .w(px(300.))
                            .flex_shrink_0()
                            .child(self.workspace_card(cx)),
                    )
                })
                .into_any_element()
        };
        let main = v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .when(!self.side_chat_mode, |v| v.child(header))
            .when(!self.error.is_empty(), |body| {
                body.child(
                    h_flex()
                        .p_3()
                        .gap_2()
                        .bg(rgb(0x352523))
                        .child(Icon::new(IconName::TriangleAlert).text_color(rgb(0xff8e86)))
                        .child(
                            div()
                                .flex_1()
                                .text_sm()
                                .text_color(rgb(0xff8e86))
                                .child(error_message(&self.error)),
                        )
                        .child(self.icon_button(
                            "dismiss-error",
                            IconName::Close,
                            "エラーを閉じる",
                            cx,
                            |s, _, _| s.error.clear(),
                        )),
                )
            })
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .items_stretch()
                    .child(content),
            );
        h_flex()
            .size_full()
            .items_stretch()
            .bg(rgb(0x181818))
            .text_color(rgb(0xececec))
            .text_size(px(14.))
            .font_weight(FontWeight::NORMAL)
            .when(self.sidebar && !self.side_chat_mode, |body| {
                body.child(self.sidebar(window, cx))
            })
            .child(main)
    }
}

impl Desktop {
    fn button(
        &self,
        id: impl Into<SharedString>,
        label: impl Into<SharedString>,
        cx: &Context<Self>,
        action: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) -> Button {
        let label = label.into();
        Button::new(id.into())
            .accessibility_label(label.clone())
            .child(div().flex_1().min_w_0().text_ellipsis().child(label))
            .small()
            .ghost()
            .on_click(cx.listener(move |s, _, w, cx| {
                action(s, w, cx);
                cx.notify();
            }))
    }
    pub(super) fn icon_button(
        &self,
        id: &'static str,
        icon: IconName,
        label: &'static str,
        cx: &Context<Self>,
        action: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) -> Button {
        Button::new(id)
            .icon(icon)
            .small()
            .ghost()
            .tooltip(label)
            .accessibility_label(label)
            .on_click(cx.listener(move |s, _, w, cx| {
                action(s, w, cx);
                cx.notify();
            }))
    }
}

fn error_message(error: &str) -> String {
    let Some(raw) = error.strip_prefix("remote RPC error: ") else {
        return error.into();
    };
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return "接続先で操作に失敗しました。もう一度お試しください。".into();
    };
    if value["code"] == "dictation_failed" {
        let reason = value["message"]
            .as_str()
            .filter(|message| !message.trim().is_empty())
            .map(agent_core::diagnostics::sanitize)
            .unwrap_or_else(|| "もう一度録音してください。".into());
        return format!("音声を文字起こしできませんでした。{reason}");
    }
    value["message"]
        .as_str()
        .unwrap_or("接続先で操作に失敗しました。もう一度お試しください。")
        .into()
}

#[cfg(test)]
mod error_tests {
    use super::error_message;
    #[test]
    fn dictation_errors_explain_recovery_without_rendering_rpc_payloads() {
        let message = error_message(
            r#"remote RPC error: {"code":"dictation_failed","message":"provider failed","data":{"audio":"private"}}"#,
        );
        assert!(message.contains("音声を文字起こしできませんでした。provider failed"));
        assert!(!message.contains("private") && !message.contains("{"));
        assert!(error_message(r#"remote RPC error: {"code":"dictation_failed","message":"Codexにログインしてください。"}"#).contains("Codexにログインしてください。"));
        assert_eq!(
            error_message(r#"remote RPC error: {"code":-1,"message":"permission denied"}"#),
            "permission denied"
        );
        assert_eq!(
            error_message("マイクへのアクセスを許可してください"),
            "マイクへのアクセスを許可してください"
        );
    }
}

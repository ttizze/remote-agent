//! The thread's error banner floating over the top of the timeline; the
//! usage-limit recovery sits in the composer's banner stack.
use super::{Desktop, color, icon, tint};
use agent_core::{
    state::Intent,
    view::{
        thread::ThreadView,
        timeline::banners::{BannerVariant, ThreadErrorBanner},
    },
};
use gpui_kit::{
    component::{
        Sizable,
        button::{Button, ButtonVariants},
        h_flex,
        tooltip::Tooltip,
        v_flex,
    },
    *,
};

/// A floating alert: rounded, bordered and tinted by its variant.
fn alert(warning: bool) -> Div {
    let (accent, surface, foreground) = if warning {
        ("warning", "warningSurface", "warningForeground")
    } else {
        ("error", "errorSurface", "errorForeground")
    };
    h_flex()
        .items_start()
        .gap_2()
        .rounded(px(14.))
        .border_1()
        .border_color(tint(accent, 0.32))
        .bg(color(surface))
        .px(px(14.))
        .py_3()
        .text_sm()
        .text_color(color(foreground))
        .shadow_md()
}

impl Desktop {
    pub(super) fn render_timeline_banners(
        &mut self,
        thread: &ThreadView,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        v_flex()
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .items_center()
            .gap_2()
            .px_4()
            .children(
                thread
                    .error_banner
                    .as_ref()
                    .map(|banner| self.render_error_banner(banner, cx)),
            )
            .into_any_element()
    }

    fn render_error_banner(
        &mut self,
        banner: &ThreadErrorBanner,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let warning = banner.variant == BannerVariant::Warning;
        let key = banner.dismiss_key.clone();
        let text = SharedString::from(banner.text.clone());
        div()
            .pt_3()
            .max_w(px(768.))
            .child(
                alert(warning)
                    .child(
                        icon("circle-alert")
                            .size(px(16.))
                            .mt(px(2.))
                            .text_color(color(if warning { "warning" } else { "error" })),
                    )
                    .child(
                        div()
                            .id("thread-error-text")
                            .flex_1()
                            .min_w_0()
                            .line_clamp(3)
                            .text_color(tint(
                                if warning {
                                    "warningForeground"
                                } else {
                                    "errorForeground"
                                },
                                0.8,
                            ))
                            .tooltip({
                                let text = text.clone();
                                move |window, cx| Tooltip::new(text.clone()).build(window, cx)
                            })
                            .child(text),
                    )
                    .child(
                        Button::new("thread-error-dismiss")
                            .icon(icon("x"))
                            .ghost()
                            .xsmall()
                            .accessibility_label(banner.dismiss_label.clone())
                            .on_click(cx.listener(move |view, _, _, _| {
                                view.perform(Intent::DismissThreadError {
                                    dismiss_key: key.clone(),
                                })
                            })),
                    ),
            )
            .into_any_element()
    }
}

//! The Appearance page: color scheme, interface and typography.
use super::{Choice, Row, page_container, reset_button, section, select, select_sized};
use crate::app::{
    Desktop,
    ui::{
        Appearance, AppearanceMode, ChatWidth, DiffColors, appearance, color, set_appearance, tint,
    },
};
use gpui_kit::{
    component::{
        Sizable, h_flex,
        input::{Input, InputEvent, InputState},
        slider::{Slider, SliderEvent, SliderState},
        switch::Switch,
        v_flex,
    },
    prelude::FluentBuilder,
    *,
};

/// The size choices of a font, inclusive.
const INTERFACE_SIZES: (u32, u32) = (12, 20);
const MONOSPACE_SIZES: (u32, u32) = (10, 18);

pub(super) struct AppearanceState {
    glass: Entity<SliderState>,
    interface_font: Entity<InputState>,
    monospace_font: Entity<InputState>,
    _subscriptions: Vec<Subscription>,
}

impl AppearanceState {
    pub(super) fn new(window: &mut Window, cx: &mut Context<Desktop>) -> Self {
        let current = appearance();
        let glass = cx.new(|_| {
            SliderState::new()
                .min(40.)
                .max(100.)
                .step(5.)
                .default_value(current.glass_opacity as f32)
        });
        let interface_font = cx.new(|cx| InputState::new(window, cx).placeholder("System UI"));
        let monospace_font = cx.new(|cx| InputState::new(window, cx).placeholder("Menlo"));
        let subscriptions = vec![
            cx.subscribe(&glass, |view, _, event: &SliderEvent, cx| {
                let (SliderEvent::Change(value) | SliderEvent::Release(value)) = event;
                let percent = value.start().round() as u32;
                view.change_appearance(|appearance| appearance.glass_opacity = percent, cx);
            }),
            cx.subscribe_in(
                &interface_font,
                window,
                |view, input, event: &InputEvent, window, cx| {
                    view.commit_font(input, event, false, window, cx)
                },
            ),
            cx.subscribe_in(
                &monospace_font,
                window,
                |view, input, event: &InputEvent, window, cx| {
                    view.commit_font(input, event, true, window, cx)
                },
            ),
        ];
        Self {
            glass,
            interface_font,
            monospace_font,
            _subscriptions: subscriptions,
        }
    }
}

/// A mini window in one appearance's colors; `split` shows light on the
/// left and dark on the right.
fn wireframe(split: Option<bool>) -> AnyElement {
    let pane = |dark: bool| {
        let palette = agent_core::presentation::theme::theme(dark);
        let tone = |role: &str| -> Hsla {
            let value = palette
                .colors
                .get(role)
                .map_or("ffffff", |value| value.trim_start_matches('#'));
            let hex = u32::from_str_radix(&value[..6.min(value.len())], 16).unwrap_or(0xffffff);
            rgb(hex).into()
        };
        h_flex()
            .flex_1()
            .h_full()
            .bg(tone("canvas"))
            .child(
                v_flex()
                    .w(px(36.))
                    .h_full()
                    .gap_1()
                    .p_1p5()
                    .bg(tone("sidebar"))
                    .border_r_1()
                    .border_color(tone("border"))
                    .children((0..4).map(move |_| {
                        div()
                            .h(px(4.))
                            .w_full()
                            .rounded_full()
                            .bg(tone("textMuted").opacity(0.4))
                    })),
            )
            .child(
                v_flex()
                    .flex_1()
                    .h_full()
                    .gap_1()
                    .p_2()
                    .child(
                        div()
                            .h(px(5.))
                            .w(px(48.))
                            .rounded_full()
                            .bg(tone("textMuted").opacity(0.5)),
                    )
                    .child(
                        div()
                            .h(px(5.))
                            .w(px(64.))
                            .rounded_full()
                            .bg(tone("textMuted").opacity(0.3)),
                    )
                    .child(div().flex_1())
                    .child(
                        div()
                            .h(px(18.))
                            .w_full()
                            .rounded(px(6.))
                            .border_1()
                            .border_color(tone("border"))
                            .bg(tone("surface")),
                    ),
            )
    };
    let frame = h_flex()
        .h(px(140.))
        .w_full()
        .overflow_hidden()
        .rounded(px(8.))
        .border_1()
        .border_color(tint("border", 0.7));
    match split {
        None => frame.child(pane(false)).child(pane(true)),
        Some(dark) => frame.child(pane(dark)),
    }
    .into_any_element()
}

impl Desktop {
    /// Changes this device's appearance and redraws with it.
    fn change_appearance(&mut self, change: impl FnOnce(&mut Appearance), cx: &mut Context<Self>) {
        let mut next = appearance();
        change(&mut next);
        if next == appearance() {
            return;
        }
        if next.word_wrap != appearance().word_wrap {
            self.set_diff_wrap(next.word_wrap, cx);
        }
        set_appearance(next, cx);
        self.refresh_views(cx);
        cx.refresh_windows();
    }

    /// A font family typed into its field takes effect once it is installed.
    fn commit_font(
        &mut self,
        input: &Entity<InputState>,
        event: &InputEvent,
        monospace: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !matches!(event, InputEvent::Blur | InputEvent::PressEnter { .. }) {
            return;
        }
        let family = input.read(cx).value().trim().to_owned();
        let installed = family.is_empty() || cx.text_system().all_font_names().contains(&family);
        if !installed {
            let current = if monospace {
                appearance().monospace_font
            } else {
                appearance().interface_font
            };
            input.update(cx, |input, cx| input.set_value(current, window, cx));
            return;
        }
        self.change_appearance(
            |appearance| {
                if monospace {
                    appearance.monospace_font = family;
                } else {
                    appearance.interface_font = family;
                }
            },
            cx,
        );
    }

    pub(super) fn render_appearance(
        &mut self,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let current = appearance();
        let defaults = Appearance::default();
        let tiles = [
            (
                AppearanceMode::System,
                "System",
                "Follow the system appearance",
                None,
            ),
            (
                AppearanceMode::Light,
                "Light",
                "Use light mode",
                Some(false),
            ),
            (AppearanceMode::Dark, "Dark", "Use dark mode", Some(true)),
        ]
        .into_iter()
        .map(|(mode, label, accessible, split)| {
            let active = current.mode == mode;
            v_flex()
                .id(SharedString::from(format!("appearance-{label}")))
                .flex_1()
                .gap_1p5()
                .p_2()
                .rounded(px(12.))
                .border_1()
                .cursor_pointer()
                .map(|tile| {
                    if active {
                        tile.border_color(color("focus"))
                            .bg(tint("accentSurface", 0.3))
                    } else {
                        tile.border_color(tint("border", 0.7))
                            .bg(tint("surface", 0.6))
                            .hover(|tile| tile.bg(tint("accentSurface", 0.1)))
                    }
                })
                .tooltip(move |window, cx| {
                    gpui_kit::component::tooltip::Tooltip::new(accessible).build(window, cx)
                })
                .on_click(cx.listener(move |view, _, _, cx| {
                    view.change_appearance(|appearance| appearance.mode = mode, cx)
                }))
                .child(wireframe(split))
                .child(
                    div()
                        .text_center()
                        .text_xs()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(if active {
                            color("text")
                        } else {
                            color("textMuted")
                        })
                        .child(label),
                )
        });
        let colors = v_flex()
            .gap(px(10.))
            .child(
                div()
                    .px_4()
                    .text_sm()
                    .text_color(tint("text", 0.7))
                    .child("Color scheme"),
            )
            .child(h_flex().gap_3().children(tiles))
            .into_any_element();
        let glass_reset = (current.glass_opacity != defaults.glass_opacity).then(|| {
            reset_button(
                "reset-glass",
                "glass opacity",
                "Reset to default",
                |view, window, cx| {
                    view.change_appearance(|appearance| appearance.glass_opacity = 80, cx);
                    view.settings
                        .appearance
                        .glass
                        .update(cx, |slider, cx| slider.set_value(80., window, cx));
                },
                cx,
            )
        });
        let glass = Row::new("Glass opacity")
            .description("Higher values make menus, dialogs, and the composer more solid.")
            .reset(glass_reset)
            .control(
                h_flex()
                    .w(px(208.))
                    .gap_3()
                    .child(value_readout(format!("{}%", current.glass_opacity), 48.))
                    .child(
                        div()
                            .flex_1()
                            .child(Slider::new(&self.settings.appearance.glass)),
                    ),
            )
            .render();
        let diff_label = |colors: DiffColors| match colors {
            DiffColors::RedGreen => "Red & green",
            DiffColors::BlueOrange => "Blue & orange",
        };
        let diff = Row::new("Diff colors")
            .description("Choose colors for additions and deletions, including change counts.")
            .reset((current.diff_colors != defaults.diff_colors).then(|| {
                reset_button(
                    "reset-diff-colors",
                    "diff colors",
                    "Reset to default",
                    |view, _, cx| {
                        view.change_appearance(
                            |appearance| appearance.diff_colors = DiffColors::RedGreen,
                            cx,
                        )
                    },
                    cx,
                )
            }))
            .control(select(
                "diff-colors",
                diff_label(current.diff_colors),
                vec![
                    Choice {
                        id: "red-green".into(),
                        label: "Red & green (default)".into(),
                        description: None,
                        icon: None,
                        selected: current.diff_colors == DiffColors::RedGreen,
                    },
                    Choice {
                        id: "blue-orange".into(),
                        label: "Blue & orange".into(),
                        description: None,
                        icon: None,
                        selected: current.diff_colors == DiffColors::BlueOrange,
                    },
                ],
                |view, choice, _, cx| {
                    let colors = if choice == "blue-orange" {
                        DiffColors::BlueOrange
                    } else {
                        DiffColors::RedGreen
                    };
                    view.change_appearance(|appearance| appearance.diff_colors = colors, cx)
                },
                cx,
            ))
            .render();
        let width_label = |width: ChatWidth| match width {
            ChatWidth::Comfortable => "Comfortable",
            ChatWidth::Wide => "Wide",
            ChatWidth::Full => "Full",
        };
        let chat_width = Row::new("Chat width")
            .description("Set how wide messages and the composer can grow on large screens.")
            .reset((current.chat_width != defaults.chat_width).then(|| {
                reset_button(
                    "reset-chat-width",
                    "chat width",
                    "Reset to default",
                    |view, _, cx| {
                        view.change_appearance(
                            |appearance| appearance.chat_width = ChatWidth::Comfortable,
                            cx,
                        )
                    },
                    cx,
                )
            }))
            .control(select(
                "chat-width",
                width_label(current.chat_width),
                [ChatWidth::Comfortable, ChatWidth::Wide, ChatWidth::Full]
                    .into_iter()
                    .map(|width| Choice {
                        id: width_label(width).into(),
                        label: match width {
                            ChatWidth::Comfortable => "Comfortable (default)".into(),
                            other => width_label(other).into(),
                        },
                        description: None,
                        icon: None,
                        selected: current.chat_width == width,
                    })
                    .collect(),
                |view, choice, _, cx| {
                    let width = match choice.as_str() {
                        "Wide" => ChatWidth::Wide,
                        "Full" => ChatWidth::Full,
                        _ => ChatWidth::Comfortable,
                    };
                    view.change_appearance(|appearance| appearance.chat_width = width, cx)
                },
                cx,
            ))
            .render();
        let interface = section(
            Some("Interface".into()),
            None,
            None,
            vec![glass, diff, chat_width],
        )
        .into_any_element();
        let typography = section(
            Some("Typography".into()),
            None,
            None,
            vec![
                self.font_row(false, &current, cx),
                self.font_row(true, &current, cx),
                Row::new("Word wrap")
                    .description(
                        "Wrap long lines in code blocks, tables, diffs, and file previews by default.",
                    )
                    .reset((!current.word_wrap).then(|| {
                        reset_button(
                            "reset-word-wrap",
                            "word wrap",
                            "Reset to default",
                            |view, _, cx| {
                                view.change_appearance(|appearance| appearance.word_wrap = true, cx)
                            },
                            cx,
                        )
                    }))
                    .control(
                        Switch::new("word-wrap")
                            .checked(current.word_wrap)
                            .accessibility_label("Word wrap")
                            .on_click(cx.listener(|view, on: &bool, _, cx| {
                                let on = *on;
                                view.change_appearance(|appearance| appearance.word_wrap = on, cx)
                            })),
                    )
                    .render(),
            ],
        )
        .into_any_element();
        page_container(896., vec![colors, interface, typography])
    }

    /// The interface or monospace font: its family field and size.
    fn font_row(
        &self,
        monospace: bool,
        current: &Appearance,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (title, description, family, size, range, default_size) = if monospace {
            (
                "Monospace font",
                Some("Code blocks, diffs, file previews, and the terminal."),
                &self.settings.appearance.monospace_font,
                current.monospace_size,
                MONOSPACE_SIZES,
                13,
            )
        } else {
            (
                "Interface font",
                None,
                &self.settings.appearance.interface_font,
                current.interface_size,
                INTERFACE_SIZES,
                16,
            )
        };
        let changed = size != default_size
            || if monospace {
                !current.monospace_font.is_empty()
            } else {
                !current.interface_font.is_empty()
            };
        let reset = changed.then(|| {
            reset_button(
                SharedString::from(format!("reset-font-{monospace}")),
                &title.to_lowercase(),
                "Reset to default",
                move |view, window, cx| {
                    view.change_appearance(
                        |appearance| {
                            if monospace {
                                appearance.monospace_font.clear();
                                appearance.monospace_size = 13;
                            } else {
                                appearance.interface_font.clear();
                                appearance.interface_size = 16;
                            }
                        },
                        cx,
                    );
                    let field = if monospace {
                        view.settings.appearance.monospace_font.clone()
                    } else {
                        view.settings.appearance.interface_font.clone()
                    };
                    field.update(cx, |input, cx| input.set_value("", window, cx));
                },
                cx,
            )
        });
        let sizes = (range.0..=range.1)
            .map(|value| Choice {
                id: value.to_string(),
                label: format!("{value} px"),
                description: None,
                icon: None,
                selected: value == size,
            })
            .collect();
        Row::new(title)
            .when_some(description, |row, description| row.description(description))
            .reset(reset)
            .control(
                h_flex()
                    .gap_2()
                    .child(
                        Input::new(family)
                            .small()
                            .w(px(176.))
                            .aria_label(format!("{title} family")),
                    )
                    .child(select_sized(
                        SharedString::from(format!("font-size-{monospace}")),
                        format!("{size} px"),
                        sizes,
                        88.,
                        move |view, choice, _, cx| {
                            let Ok(size) = choice.parse::<u32>() else {
                                return;
                            };
                            view.change_appearance(
                                |appearance| {
                                    if monospace {
                                        appearance.monospace_size = size;
                                    } else {
                                        appearance.interface_size = size;
                                    }
                                },
                                cx,
                            )
                        },
                        cx,
                    )),
            )
            .render()
    }
}

/// The value beside a slider, such as `80%`.
fn value_readout(text: String, min_width: f32) -> impl IntoElement {
    div()
        .min_w(px(min_width))
        .px_1()
        .py(px(2.))
        .rounded(px(6.))
        .bg(color("muted"))
        .text_center()
        .text_xs()
        .font_family("Menlo")
        .child(text)
}

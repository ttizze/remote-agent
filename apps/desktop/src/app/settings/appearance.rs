//! The Appearance page: the color scheme and theme library, the interface,
//! motion and typography.
use super::{Choice, Row, page_container, reset_button, section, select, select_sized};
use crate::app::{
    Desktop,
    ui::{appearance, color, hex_color, icon, set_appearance, tint},
};
use agent_core::view::appearance::{
    Appearance, AppearanceMode, CODE_FONT_SIZES, ChatWidth, DiffColors, INTERFACE_FONT_SIZES,
    MAX_CONTRAST, MAX_GLASS_OPACITY, MAX_PANEL_ANIMATION_MS, MIN_CONTRAST, MIN_GLASS_OPACITY,
    MIN_PANEL_ANIMATION_MS, PROMPT_FONT_SIZES, TERMINAL_FONT_SIZES, ThemeCard, ThemePreviewColors,
    preview_render_spec,
};
use gpui_kit::{
    component::{
        Sizable, h_flex,
        input::{Input, InputEvent, InputState},
        slider::{Slider, SliderEvent, SliderState},
        switch::Switch,
        tooltip::Tooltip,
        v_flex,
    },
    prelude::FluentBuilder,
    *,
};
use std::time::Duration;

/// A typography surface with its own family and size.
#[derive(Clone, Copy, PartialEq, Eq)]
enum FontSurface {
    Interface,
    Prompt,
    Code,
    Terminal,
}

impl FontSurface {
    const ALL: [FontSurface; 4] = [
        FontSurface::Interface,
        FontSurface::Prompt,
        FontSurface::Code,
        FontSurface::Terminal,
    ];

    fn index(self) -> usize {
        self as usize
    }

    /// Code and terminal text sets in columns, so only monospace fonts fit.
    fn monospace(self) -> bool {
        matches!(self, FontSurface::Code | FontSurface::Terminal)
    }

    fn sizes(self) -> (u32, u32) {
        match self {
            FontSurface::Interface => INTERFACE_FONT_SIZES,
            FontSurface::Prompt => PROMPT_FONT_SIZES,
            FontSurface::Code => CODE_FONT_SIZES,
            FontSurface::Terminal => TERMINAL_FONT_SIZES,
        }
    }

    fn family(self, appearance: &Appearance) -> &str {
        match self {
            FontSurface::Interface => &appearance.interface_font,
            FontSurface::Prompt => &appearance.prompt_font,
            FontSurface::Code => &appearance.code_font,
            FontSurface::Terminal => &appearance.terminal_font,
        }
    }

    fn family_mut(self, appearance: &mut Appearance) -> &mut String {
        match self {
            FontSurface::Interface => &mut appearance.interface_font,
            FontSurface::Prompt => &mut appearance.prompt_font,
            FontSurface::Code => &mut appearance.code_font,
            FontSurface::Terminal => &mut appearance.terminal_font,
        }
    }

    fn size(self, appearance: &Appearance) -> u32 {
        match self {
            FontSurface::Interface => appearance.interface_size,
            FontSurface::Prompt => appearance.prompt_size,
            FontSurface::Code => appearance.code_size,
            FontSurface::Terminal => appearance.terminal_size,
        }
    }

    fn size_mut(self, appearance: &mut Appearance) -> &mut u32 {
        match self {
            FontSurface::Interface => &mut appearance.interface_size,
            FontSurface::Prompt => &mut appearance.prompt_size,
            FontSurface::Code => &mut appearance.code_size,
            FontSurface::Terminal => &mut appearance.terminal_size,
        }
    }

    /// What an empty family field renders as.
    fn placeholder(self, appearance: &Appearance) -> String {
        match self {
            FontSurface::Interface => "System UI".into(),
            FontSurface::Prompt => match appearance.interface_font.as_str() {
                "" => "System UI".into(),
                family => family.into(),
            },
            FontSurface::Code | FontSurface::Terminal => "Menlo".into(),
        }
    }
}

pub(super) struct AppearanceState {
    contrast: Entity<SliderState>,
    glass: Entity<SliderState>,
    motion: Entity<SliderState>,
    fonts: Vec<Entity<InputState>>,
    /// The motion preview's panels are open; clicking replays the motion.
    motion_preview_open: bool,
    motion_preview_runs: usize,
    _subscriptions: Vec<Subscription>,
}

impl AppearanceState {
    pub(super) fn new(window: &mut Window, cx: &mut Context<Desktop>) -> Self {
        let current = appearance();
        let slider = |cx: &mut Context<Desktop>, (min, max): (u32, u32), step: f32, value: u32| {
            cx.new(|_| {
                SliderState::new()
                    .min(min as f32)
                    .max(max as f32)
                    .step(step)
                    .default_value(value as f32)
            })
        };
        let contrast = slider(cx, (MIN_CONTRAST, MAX_CONTRAST), 5., current.contrast);
        let glass = slider(
            cx,
            (MIN_GLASS_OPACITY, MAX_GLASS_OPACITY),
            5.,
            current.glass_opacity,
        );
        let motion = slider(
            cx,
            (MIN_PANEL_ANIMATION_MS, MAX_PANEL_ANIMATION_MS),
            25.,
            current.panel_animation_ms,
        );
        let fonts: Vec<Entity<InputState>> = FontSurface::ALL
            .iter()
            .map(|surface| {
                let placeholder = surface.placeholder(&current);
                let family = surface.family(&current).to_owned();
                cx.new(|cx| {
                    let mut input = InputState::new(window, cx).placeholder(placeholder);
                    input.set_value(family, window, cx);
                    input
                })
            })
            .collect();
        let mut subscriptions = vec![
            cx.subscribe(&contrast, |view, _, event: &SliderEvent, cx| {
                let (SliderEvent::Change(value) | SliderEvent::Release(value)) = event;
                let percent = value.start().round() as u32;
                view.change_appearance(|appearance| appearance.contrast = percent, cx);
            }),
            cx.subscribe(&glass, |view, _, event: &SliderEvent, cx| {
                let (SliderEvent::Change(value) | SliderEvent::Release(value)) = event;
                let percent = value.start().round() as u32;
                view.change_appearance(|appearance| appearance.glass_opacity = percent, cx);
            }),
            cx.subscribe(&motion, |view, _, event: &SliderEvent, cx| {
                let (SliderEvent::Change(value) | SliderEvent::Release(value)) = event;
                let ms = value.start().round() as u32;
                view.change_appearance(|appearance| appearance.panel_animation_ms = ms, cx);
            }),
        ];
        for (surface, input) in FontSurface::ALL.into_iter().zip(&fonts) {
            subscriptions.push(cx.subscribe_in(
                input,
                window,
                move |view, input, event: &InputEvent, window, cx| {
                    view.commit_font(surface, input, event, window, cx)
                },
            ));
        }
        Self {
            contrast,
            glass,
            motion,
            fonts,
            motion_preview_open: true,
            motion_preview_runs: 0,
            _subscriptions: subscriptions,
        }
    }
}

/// Whether `family` sets every glyph in the same width.
fn is_monospace(family: &str, window: &Window) -> bool {
    let width = |text: &'static str| {
        let run = TextRun {
            len: text.len(),
            font: font(SharedString::from(family.to_owned())),
            color: Hsla::default(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        window
            .text_system()
            .shape_line(text.into(), px(16.), &[run], None)
            .width
    };
    (width("iiiiiiiiii") - width("MMMMMMMMMM")).abs() < px(0.5)
}

/// The miniature app a mode tile shows: sidebar, a short conversation and
/// the composer, with the side panel floating over it.
fn wireframe_pane(colors: &ThemePreviewColors) -> Div {
    let line = hsla(0., 0., 0.5, 0.25);
    let block = |left: f32, top: f32, width: f32, height: f32| {
        div()
            .absolute()
            .left(relative(left))
            .top(relative(top))
            .w(relative(width))
            .h(relative(height))
    };
    div()
        .absolute()
        .inset_0()
        .bg(hex_color(&colors.canvas))
        .child(
            block(0., 0., 0.22, 1.)
                .bg(hex_color(&colors.sidebar))
                .border_r_1()
                .border_color(line),
        )
        .child(
            block(0.03, 0.08, 0.16, 0.08)
                .rounded(px(4.))
                .bg(hex_color(&colors.surface))
                .border_1()
                .border_color(line),
        )
        .child(
            block(0.03, 0.22, 0.16, 0.07)
                .rounded(px(4.))
                .bg(hex_color(&colors.accent_surface)),
        )
        .child(
            block(0.03, 0.32, 0.16, 0.07)
                .rounded(px(4.))
                .bg(hex_color(&colors.message_surface).opacity(0.7)),
        )
        .child(
            block(0.03, 0.42, 0.16, 0.07)
                .rounded(px(4.))
                .bg(hex_color(&colors.message_surface).opacity(0.5)),
        )
        .child(
            block(0.48, 0.11, 0.24, 0.09)
                .rounded(px(6.))
                .bg(hex_color(&colors.message_surface)),
        )
        .child(block(0.27, 0.28, 0.34, 0.05).rounded(px(2.)).bg(line))
        .child(block(0.27, 0.38, 0.26, 0.05).rounded(px(2.)).bg(line))
        .child(
            block(0.26, 0.77, 0.68, 0.15)
                .rounded(px(4.))
                .px_1()
                .flex()
                .items_center()
                .justify_between()
                .bg(hex_color(&colors.surface))
                .border_1()
                .border_color(line)
                .child(div().h(px(3.)).w(relative(0.34)).rounded_full().bg(line))
                .child(
                    div()
                        .size(px(9.))
                        .rounded_full()
                        .bg(hex_color(&colors.message_action)),
                ),
        )
        .child(
            block(0.75, 0.08, 0.2, 0.46)
                .rounded(px(6.))
                .bg(hex_color(&colors.surface))
                .border_1()
                .border_color(line)
                .shadow_sm()
                .flex()
                .flex_col()
                .justify_around()
                .px(relative(0.11))
                .children(
                    [
                        hsla(0.44, 0.65, 0.53, 0.55),
                        hex_color(&colors.message_action).opacity(0.55),
                        hsla(0.12, 0.96, 0.56, 0.55),
                    ]
                    .into_iter()
                    .map(move |dot| {
                        h_flex()
                            .gap_1()
                            .child(div().size(px(4.)).rounded_full().bg(dot))
                            .child(div().h(px(3.)).w(relative(0.52)).rounded_sm().bg(line))
                    }),
                ),
        )
}

/// A mode tile's miniature: one appearance, or light left and dark right.
fn wireframe(current: &Appearance, mode: AppearanceMode) -> AnyElement {
    let frame = h_flex()
        .relative()
        .h(px(140.))
        .w_full()
        .overflow_hidden()
        .rounded(px(8.))
        .border_1()
        .border_color(tint("border", 0.6));
    let pane = |dark: bool| {
        div()
            .relative()
            .flex_1()
            .h_full()
            .overflow_hidden()
            .child(wireframe_pane(&current.wireframe_colors(dark)))
    };
    match mode {
        AppearanceMode::System => frame
            .child(pane(false))
            .child(div().w(px(2.)).h_full().bg(tint("border", 0.6)))
            .child(pane(true)),
        AppearanceMode::Light => frame.child(pane(false)),
        AppearanceMode::Dark => frame.child(pane(true)),
    }
    .into_any_element()
}

/// A theme's preview ball: its canvas with an accent glow and an action tint
/// from the opposite corner.
fn preview_ball(colors: &ThemePreviewColors, dark: bool, size: f32) -> Div {
    let spec = preview_render_spec(dark);
    let glow = |center: (f64, f64), color: Hsla, reach: f64, opacity: f64| {
        let diameter = (reach * 2. * f64::from(size)) as f32;
        // Rings fade outward to stand in for a radial gradient.
        (0..6).map(move |ring| {
            let fraction = 1. - ring as f32 / 6.;
            let ring_size = diameter * fraction;
            div()
                .absolute()
                .left(px(center.0 as f32 * size - ring_size / 2.))
                .top(px(center.1 as f32 * size - ring_size / 2.))
                .size(px(ring_size))
                .rounded_full()
                .bg(color.opacity(opacity as f32 / 6.))
        })
    };
    div()
        .relative()
        .size(px(size))
        .flex_none()
        .overflow_hidden()
        .rounded_full()
        .border_2()
        .border_color(color("canvas"))
        .bg(hex_color(&colors.ball_base(dark)))
        .children(glow(
            spec.accent_center,
            hex_color(&colors.accent),
            spec.accent_end_offset,
            spec.accent_middle_opacity,
        ))
        .children(glow(
            spec.action_center,
            hex_color(&colors.message_action),
            spec.action_end_offset,
            spec.action_start_opacity,
        ))
        .shadow(vec![BoxShadow {
            color: if dark {
                hsla(0., 0., 1., 0.14)
            } else {
                hsla(0., 0., 0., 0.1)
            },
            offset: point(px(0.), px(0.)),
            blur_radius: px(0.),
            spread_radius: px(1.),
            inset: true,
        }])
}

impl Desktop {
    /// Changes this device's appearance and redraws with it.
    fn change_appearance(&mut self, change: impl FnOnce(&mut Appearance), cx: &mut Context<Self>) {
        let mut next = appearance();
        change(&mut next);
        let next = next.normalized();
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

    fn change_word_wrap(&mut self, wrap: bool, window: &mut Window, cx: &mut Context<Self>) {
        let changed = appearance().word_wrap != wrap;
        self.change_appearance(|appearance| appearance.word_wrap = wrap, cx);
        if changed {
            self.panels.files.set_word_wrap(wrap, window, cx);
        }
    }

    /// A family typed into its field takes effect once it is installed (and
    /// monospace where columns need it); an unknown name snaps back.
    fn commit_font(
        &mut self,
        surface: FontSurface,
        input: &Entity<InputState>,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !matches!(event, InputEvent::Blur | InputEvent::PressEnter { .. }) {
            return;
        }
        let family = input.read(cx).value().trim().to_owned();
        let accepted = family.is_empty()
            || (cx.text_system().all_font_names().contains(&family)
                && (!surface.monospace() || is_monospace(&family, window)));
        if !accepted {
            let current = surface.family(&appearance()).to_owned();
            input.update(cx, |input, cx| input.set_value(current, window, cx));
            return;
        }
        self.change_appearance(|appearance| *surface.family_mut(appearance) = family, cx);
        self.sync_font_placeholders(window, cx);
    }

    /// The prompt field shows the interface font it falls back to.
    fn sync_font_placeholders(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let current = appearance();
        let prompt = FontSurface::Prompt.placeholder(&current);
        self.settings.appearance.fonts[FontSurface::Prompt.index()]
            .update(cx, |input, cx| input.set_placeholder(prompt, window, cx));
    }

    pub(super) fn render_appearance(
        &mut self,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let current = appearance();
        page_container(
            896.,
            vec![
                self.color_scheme(&current, cx),
                self.theme_library(&current, cx),
                self.interface_section(&current, cx),
                self.motion_section(&current, cx),
                self.typography_section(&current, cx),
            ],
        )
    }

    /// "Color scheme": System, Light and Dark tiles.
    fn color_scheme(&self, current: &Appearance, cx: &mut Context<Self>) -> AnyElement {
        let tiles = [
            (
                AppearanceMode::System,
                "System",
                "Follow the system appearance",
            ),
            (AppearanceMode::Light, "Light", "Use light mode"),
            (AppearanceMode::Dark, "Dark", "Use dark mode"),
        ]
        .into_iter()
        .map(|(mode, label, accessible)| {
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
                .tooltip(move |window, cx| Tooltip::new(accessible).build(window, cx))
                .on_click(cx.listener(move |view, _, _, cx| {
                    view.change_appearance(|appearance| appearance.mode = mode, cx)
                }))
                .child(wireframe(current, mode))
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
        v_flex()
            .gap(px(10.))
            .child(heading("Color scheme"))
            .child(h_flex().gap_3().children(tiles))
            .into_any_element()
    }

    /// "Themes": the stock palette and the built-in themes as cards.
    fn theme_library(&self, current: &Appearance, cx: &mut Context<Self>) -> AnyElement {
        let cards: Vec<AnyElement> = current
            .theme_cards()
            .into_iter()
            .enumerate()
            .map(|(index, card)| self.theme_card(index, card, cx))
            .collect();
        let mut rows: Vec<AnyElement> = vec![];
        let mut cards = cards.into_iter().peekable();
        while cards.peek().is_some() {
            let row: Vec<AnyElement> = cards.by_ref().take(3).collect();
            let fill = 3 - row.len();
            rows.push(
                h_flex()
                    .gap_2()
                    .children(row)
                    .children((0..fill).map(|_| div().flex_1()))
                    .into_any_element(),
            );
        }
        v_flex()
            .gap(px(10.))
            .child(div().pt_2().child(heading("Themes")))
            .child(v_flex().gap_2().children(rows))
            .into_any_element()
    }

    fn theme_card(&self, index: usize, card: ThemeCard, cx: &mut Context<Self>) -> AnyElement {
        let id = card.id.clone();
        let label: SharedString = card.label.clone().into();
        let balls = card.previews.iter().map(|preview| {
            let dark = preview.dark;
            let picked = if dark {
                card.dark_owner
            } else {
                card.light_owner
            };
            let id = card.id.clone();
            let tooltip = if dark {
                "Use for dark mode only"
            } else {
                "Use for light mode only"
            };
            div()
                .id(SharedString::from(format!("theme-{index}-{dark}")))
                .relative()
                .size(px(68.))
                .p_1()
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .cursor_pointer()
                .tooltip(move |window, cx| Tooltip::new(tooltip).build(window, cx))
                .on_click(cx.listener(move |view, _, _, cx| {
                    cx.stop_propagation();
                    let id = id.clone();
                    view.change_appearance(
                        |appearance| appearance.assign_theme(dark, id.as_deref()),
                        cx,
                    )
                }))
                .child(preview_ball(&preview.colors, dark, 56.))
                .when(picked, |ball| {
                    ball.child(
                        div()
                            .absolute()
                            .inset_0()
                            .rounded_full()
                            .border_2()
                            .border_color(color("focus")),
                    )
                    .child(
                        div()
                            .absolute()
                            .right(px(2.))
                            .bottom(px(2.))
                            .size_5()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_full()
                            .border_1()
                            .border_color(tint("border", 0.7))
                            .bg(color("canvas"))
                            .shadow_sm()
                            .child(
                                icon(if dark { "moon" } else { "sun" })
                                    .size(px(12.))
                                    .text_color(color("text")),
                            ),
                    )
                })
        });
        let use_label = label.clone();
        v_flex()
            .id(("theme-card", index))
            .flex_1()
            .min_w_0()
            .overflow_hidden()
            .rounded(px(12.))
            .border_1()
            .border_color(tint("border", 0.7))
            .bg(tint("surface", 0.6))
            .cursor_pointer()
            .hover(|card| card.bg(tint("accentSurface", 0.1)))
            .tooltip(|window, cx| Tooltip::new("Use for both light and dark").build(window, cx))
            .on_click(cx.listener(move |view, _, _, cx| {
                let id = id.clone();
                view.change_appearance(|appearance| appearance.use_theme(id.as_deref()), cx)
            }))
            .child(
                h_flex()
                    .min_h(px(64.))
                    .px_3()
                    .pt_3()
                    .gap(px(10.))
                    .justify_center()
                    .children(balls),
            )
            .child(
                div()
                    .px_3()
                    .pb_3()
                    .pt_2()
                    .truncate()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(color("text"))
                    .child(use_label),
            )
            .into_any_element()
    }

    /// "Interface": contrast, glass, diff colors, composer context and chat
    /// width.
    fn interface_section(&self, current: &Appearance, cx: &mut Context<Self>) -> AnyElement {
        let defaults = Appearance::default();
        let contrast = Row::new("Contrast")
            .description("Adjust the contrast of colors and borders across the interface.")
            .reset((current.contrast != defaults.contrast).then(|| {
                reset_button(
                    "reset-contrast",
                    "contrast",
                    "Reset to default",
                    move |view, window, cx| {
                        view.change_appearance(
                            |appearance| appearance.contrast = defaults.contrast,
                            cx,
                        );
                        let value = defaults.contrast as f32;
                        view.settings
                            .appearance
                            .contrast
                            .update(cx, |slider, cx| slider.set_value(value, window, cx));
                    },
                    cx,
                )
            }))
            .control(slider_control(
                format!("{}%", current.contrast),
                48.,
                &self.settings.appearance.contrast,
            ))
            .render();
        let glass = Row::new("Glass opacity")
            .description("Higher values make menus, dialogs, and the composer more solid.")
            .reset((current.glass_opacity != defaults.glass_opacity).then(|| {
                reset_button(
                    "reset-glass",
                    "glass opacity",
                    "Reset to default",
                    move |view, window, cx| {
                        view.change_appearance(
                            |appearance| appearance.glass_opacity = defaults.glass_opacity,
                            cx,
                        );
                        let value = defaults.glass_opacity as f32;
                        view.settings
                            .appearance
                            .glass
                            .update(cx, |slider, cx| slider.set_value(value, window, cx));
                    },
                    cx,
                )
            }))
            .control(slider_control(
                format!("{}%", current.glass_opacity),
                48.,
                &self.settings.appearance.glass,
            ))
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
        let composer_context = Row::new("Composer context")
            .description(
                "Keep branch and worktree controls below the composer after a thread starts.",
            )
            .reset(
                (current.composer_context != defaults.composer_context).then(|| {
                    reset_button(
                        "reset-composer-context",
                        "composer context",
                        "Reset to default",
                        |view, _, cx| {
                            view.change_appearance(
                                |appearance| appearance.composer_context = false,
                                cx,
                            )
                        },
                        cx,
                    )
                }),
            )
            .control(
                Switch::new("composer-context")
                    .checked(current.composer_context)
                    .accessibility_label("Keep composer context visible in active threads")
                    .on_click(cx.listener(|view, on: &bool, _, cx| {
                        let on = *on;
                        view.change_appearance(|appearance| appearance.composer_context = on, cx)
                    })),
            )
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
        section(
            Some("Interface".into()),
            None,
            None,
            vec![contrast, glass, diff, composer_context, chat_width],
        )
        .into_any_element()
    }

    /// "Motion": how fast panels open and close, with a preview to replay.
    fn motion_section(&self, current: &Appearance, cx: &mut Context<Self>) -> AnyElement {
        let defaults = Appearance::default();
        let state = &self.settings.appearance;
        let open = state.motion_preview_open;
        let duration = Duration::from_millis(u64::from(current.panel_animation_ms));
        let progress = move |delta: f32| if open { delta } else { 1. - delta };
        let side = div()
            .h_full()
            .flex_none()
            .rounded(px(6.))
            .bg(color("sidebar"))
            .w(px(if open { 16. } else { 0. }));
        let footer = div()
            .flex_none()
            .bg(tint("text", 0.05))
            .h(px(if open { 8. } else { 0. }))
            .when(open, |footer| {
                footer.border_t_1().border_color(tint("border", 0.7))
            });
        let animated = |element: Div, name: &'static str, apply: fn(Div, f32) -> Div| {
            if duration.is_zero() {
                element.into_any_element()
            } else {
                element
                    .with_animation(
                        (name, state.motion_preview_runs),
                        Animation::new(duration),
                        move |element, delta| apply(element, progress(delta)),
                    )
                    .into_any_element()
            }
        };
        let preview = h_flex()
            .id("motion-preview")
            .w(px(112.))
            .h_10()
            .p_1()
            .overflow_hidden()
            .rounded(px(8.))
            .border_1()
            .border_color(color("border"))
            .bg(color("canvas"))
            .cursor_pointer()
            .tooltip(|window, cx| Tooltip::new("Replay panel animation preview").build(window, cx))
            .on_click(cx.listener(|view, _, _, cx| {
                let state = &mut view.settings.appearance;
                state.motion_preview_open = !state.motion_preview_open;
                state.motion_preview_runs += 1;
                cx.notify();
            }))
            .child(animated(side, "motion-side", |side, open| {
                side.w(px(16. * open))
            }))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .px_1()
                    .child(
                        v_flex()
                            .flex_1()
                            .gap_1()
                            .pt_1()
                            .child(
                                div()
                                    .h(px(2.))
                                    .w_full()
                                    .rounded_full()
                                    .bg(tint("textMuted", 0.25)),
                            )
                            .child(
                                div()
                                    .h(px(2.))
                                    .w(relative(0.8))
                                    .rounded_full()
                                    .bg(tint("textMuted", 0.2)),
                            )
                            .child(
                                div()
                                    .h(px(2.))
                                    .w(relative(0.6))
                                    .rounded_full()
                                    .bg(tint("textMuted", 0.15)),
                            ),
                    )
                    .child(animated(footer, "motion-footer", |footer, open| {
                        footer.h(px(8. * open))
                    })),
            )
            .child(animated(
                div()
                    .h_full()
                    .flex_none()
                    .rounded(px(6.))
                    .bg(color("muted"))
                    .w(px(if open { 24. } else { 0. })),
                "motion-panel",
                |panel, open| panel.w(px(24. * open)),
            ));
        let row = Row::new("Panel animations")
            .description("Set how fast panels open and close.")
            .reset(
                (current.panel_animation_ms != defaults.panel_animation_ms).then(|| {
                    reset_button(
                        "reset-panel-animations",
                        "panel animations",
                        "Reset to default",
                        move |view, window, cx| {
                            view.change_appearance(
                                |appearance| {
                                    appearance.panel_animation_ms = defaults.panel_animation_ms
                                },
                                cx,
                            );
                            let value = defaults.panel_animation_ms as f32;
                            view.settings
                                .appearance
                                .motion
                                .update(cx, |slider, cx| slider.set_value(value, window, cx));
                        },
                        cx,
                    )
                }),
            )
            .control(h_flex().gap_4().child(preview).child(slider_control(
                format!("{} ms", current.panel_animation_ms),
                64.,
                &state.motion,
            )))
            .render();
        section(Some("Motion".into()), None, None, vec![row]).into_any_element()
    }

    /// "Typography": one sans and one monospace font, or every surface's own
    /// font with Advanced on, then word wrap.
    fn typography_section(&self, current: &Appearance, cx: &mut Context<Self>) -> AnyElement {
        let advanced = current.advanced_typography;
        let switch = h_flex()
            .gap_2()
            .text_xs()
            .font_weight(FontWeight::MEDIUM)
            .text_color(color("textMuted"))
            .child("Advanced")
            .child(
                Switch::new("advanced-typography")
                    .checked(advanced)
                    .accessibility_label("Show advanced typography settings")
                    .on_click(cx.listener(|view, on: &bool, _, cx| {
                        let on = *on;
                        view.change_appearance(|appearance| appearance.advanced_typography = on, cx)
                    })),
            )
            .into_any_element();
        let mut rows = if advanced {
            vec![
                self.font_row(
                    FontSurface::Interface,
                    "Interface font",
                    "Everything outside code blocks and the terminal.",
                    current,
                    cx,
                ),
                self.font_row(
                    FontSurface::Prompt,
                    "Prompt font",
                    "Only the box you write prompts in. Mono works well here.",
                    current,
                    cx,
                ),
                self.font_row(
                    FontSurface::Code,
                    "Code font",
                    "Code blocks, diffs, and file previews.",
                    current,
                    cx,
                ),
                self.font_row(
                    FontSurface::Terminal,
                    "Terminal font",
                    "Terminal output, independent from code blocks and diffs.",
                    current,
                    cx,
                ),
            ]
        } else {
            vec![
                self.font_row(
                    FontSurface::Interface,
                    "Interface font",
                    "Everything outside code blocks and the terminal.",
                    current,
                    cx,
                ),
                self.font_row(
                    FontSurface::Code,
                    "Monospace font",
                    "Code blocks, diffs, file previews, and the terminal.",
                    current,
                    cx,
                ),
            ]
        };
        rows.push(
            Row::new("Word wrap")
                .description(
                    "Wrap long lines in code blocks, tables, diffs, and file previews by default.",
                )
                .reset((!current.word_wrap).then(|| {
                    reset_button(
                        "reset-word-wrap",
                        "word wrapping",
                        "Reset to default",
                        |view, window, cx| view.change_word_wrap(true, window, cx),
                        cx,
                    )
                }))
                .control(
                    Switch::new("word-wrap")
                        .checked(current.word_wrap)
                        .accessibility_label(
                            "Wrap code, tables, diffs, and file previews by default",
                        )
                        .on_click(cx.listener(|view, on: &bool, window, cx| {
                            let on = *on;
                            view.change_word_wrap(on, window, cx)
                        })),
                )
                .render(),
        );
        section(Some("Typography".into()), None, Some(switch), rows).into_any_element()
    }

    /// One surface's family field and size, with a sample of it below.
    fn font_row(
        &self,
        surface: FontSurface,
        title: &'static str,
        description: &'static str,
        current: &Appearance,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let defaults = Appearance::default();
        let size = surface.size(current);
        let changed =
            size != surface.size(&defaults) || surface.family(current) != surface.family(&defaults);
        let reset = changed.then(|| {
            reset_button(
                SharedString::from(format!("reset-font-{}", surface.index())),
                &title.to_lowercase(),
                "Reset to default",
                move |view, window, cx| {
                    let defaults = Appearance::default();
                    view.change_appearance(
                        |appearance| {
                            *surface.family_mut(appearance) = surface.family(&defaults).to_owned();
                            *surface.size_mut(appearance) = surface.size(&defaults);
                        },
                        cx,
                    );
                    view.settings.appearance.fonts[surface.index()]
                        .update(cx, |input, cx| input.set_value("", window, cx));
                    view.sync_font_placeholders(window, cx);
                },
                cx,
            )
        });
        let (min, max) = surface.sizes();
        let sizes = (min..=max)
            .map(|value| Choice {
                id: value.to_string(),
                label: format!("{value} px"),
                description: None,
                icon: None,
                selected: value == size,
            })
            .collect();
        Row::new(title)
            .description(description)
            .reset(reset)
            .control(
                h_flex()
                    .gap_2()
                    .child(
                        Input::new(&self.settings.appearance.fonts[surface.index()])
                            .small()
                            .w(px(176.))
                            .aria_label(format!("{title} family")),
                    )
                    .child(select_sized(
                        SharedString::from(format!("font-size-{}", surface.index())),
                        format!("{size} px"),
                        sizes,
                        88.,
                        move |view, choice, _, cx| {
                            let Ok(size) = choice.parse::<u32>() else {
                                return;
                            };
                            view.change_appearance(
                                |appearance| *surface.size_mut(appearance) = size,
                                cx,
                            )
                        },
                        cx,
                    )),
            )
            .below(font_preview(surface, current))
            .render()
    }
}

/// A sample of the text a font row reaches.
fn font_preview(surface: FontSurface, current: &Appearance) -> AnyElement {
    let family = |name: &str, fallback: &str| -> SharedString {
        if name.is_empty() {
            fallback.to_owned().into()
        } else {
            name.to_owned().into()
        }
    };
    let card = || {
        div()
            .mt_1()
            .mb_2()
            .px_3()
            .py_2()
            .rounded(px(8.))
            .border_1()
            .border_color(color("border"))
    };
    let prompt = || {
        card().bg(color("canvas")).child(
            div()
                .text_size(px(current.prompt_size as f32))
                .line_height(relative(1.625))
                .when(!current.prompt_font().is_empty(), |text| {
                    text.font_family(current.prompt_font().to_owned())
                })
                .child(
                    "Use $frontend-design to fix the flaky test in surface.test.ts and align the header with SettingsPanels.tsx before shipping.",
                ),
        )
    };
    let code = || {
        let line = |sign: &'static str, text: &'static str, tone: Option<&'static str>| {
            div()
                .px_2()
                .whitespace_nowrap()
                .when_some(tone, |line, tone| line.bg(tint(tone, 0.12)))
                .child(format!("{sign} {text}"))
        };
        card()
            .bg(color("codeBackground"))
            .text_color(color("codeForeground"))
            .font_family(family(&current.code_font, "Menlo"))
            .text_size(px(current.code_size as f32))
            .child(line(" ", "export function formatUser(user: User) {", None))
            .child(line(
                "-",
                "  return user.name.toUpperCase();",
                Some("error"),
            ))
            .child(line(
                "+",
                "  return `${user.name} <${user.email}>`; // 0O 1lI",
                Some("successForeground"),
            ))
            .child(line(" ", "}", None))
    };
    let terminal = || {
        card()
            .bg(color("terminalBackground"))
            .text_color(color("terminalForeground"))
            .font_family(family(current.terminal_font(), "Menlo"))
            .text_size(px(current.terminal_font_size() as f32))
            .child(
                h_flex()
                    .gap_1()
                    .child(div().text_color(hsla(0.39, 0.7, 0.45, 1.)).child("→"))
                    .child(div().text_color(hsla(0.5, 0.7, 0.45, 1.)).child("project"))
                    .child(
                        div()
                            .text_color(hsla(0.6, 0.7, 0.55, 1.))
                            .child("git:(main)"),
                    )
                    .child("vpr dev"),
            )
            .child(div().child("  VITE v7.1.1  ready in 1.24s"))
            .child(div().child("  ✓ 85 passed   △ 2 warnings   ✗ 0 failed"))
    };
    match (surface, current.advanced_typography) {
        (FontSurface::Interface, false) | (FontSurface::Prompt, _) => prompt().into_any_element(),
        (FontSurface::Interface, true) => div().into_any_element(),
        (FontSurface::Code, false) => v_flex().child(code()).child(terminal()).into_any_element(),
        (FontSurface::Code, true) => code().into_any_element(),
        (FontSurface::Terminal, _) => terminal().into_any_element(),
    }
}

/// A section heading outside a card, such as "Color scheme".
fn heading(text: &'static str) -> impl IntoElement {
    div()
        .px_4()
        .text_sm()
        .text_color(tint("text", 0.7))
        .child(text)
}

/// A slider with its value beside it.
fn slider_control(value: String, readout_width: f32, slider: &Entity<SliderState>) -> Div {
    h_flex()
        .w(px(208.))
        .gap_3()
        .child(value_readout(value, readout_width))
        .child(div().flex_1().child(Slider::new(slider)))
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

//! The app's controls, drawn with NSH's views in the existing desktop client's look.
//! Text input, scrolling, menus, alerts and web views stay the platform's.
use agent_core::presentation::conversation::ActionTone;
use nsh_core::{
    element::{ImageSrc, Rgba, desktop::MenuItem, rgb, rgba},
    prelude::*,
};
use serde_json::Value;

pub(crate) mod colors {
    use super::{Rgba, rgb, rgba};
    use agent_core::presentation::appearance::{DEFAULT_CONTRAST, contrast_border, contrast_text};
    use std::sync::atomic::{AtomicU32, Ordering};

    pub(crate) const BACKGROUND: Rgba = rgb(0x080808);
    pub(crate) const SIDEBAR: Rgba = rgb(0x000000);
    pub(crate) const SURFACE: Rgba = rgb(0x0f0f0f);
    pub(crate) const RAISED: Rgba = rgb(0x141414);
    pub(crate) const SELECTED: Rgba = rgb(0x282828);
    /// The device's interface contrast, read when views are made: the root
    /// is made again for each, as for a language.
    static CONTRAST: AtomicU32 = AtomicU32::new(DEFAULT_CONTRAST);

    pub(crate) fn set_contrast(contrast: u32) {
        CONTRAST.store(contrast, Ordering::Relaxed);
    }

    pub(crate) fn contrast() -> u32 {
        CONTRAST.load(Ordering::Relaxed)
    }

    fn channels(hex: u32) -> [u8; 3] {
        [(hex >> 16) as u8, (hex >> 8) as u8, hex as u8]
    }

    fn hex([r, g, b]: [u8; 3]) -> u32 {
        u32::from(r) << 16 | u32::from(g) << 8 | u32::from(b)
    }

    /// `base` at the device's contrast, as T3 Code's `--contrast-*` text.
    pub(crate) fn contrasted(base: u32) -> u32 {
        hex(contrast_text(channels(base), [0, 0, 0], contrast()))
    }

    pub(crate) fn text(base: u32) -> Rgba {
        rgb(contrasted(base))
    }

    pub(crate) fn line(base: u32) -> Rgba {
        rgb(hex(contrast_border(
            channels(base),
            [0, 0, 0],
            [0xf5, 0xf5, 0xf5],
            contrast(),
        )))
    }

    pub(crate) fn border() -> Rgba {
        line(0x303030)
    }

    pub(crate) fn foreground() -> Rgba {
        text(0xe6e6e6)
    }

    pub(crate) fn muted() -> Rgba {
        text(0xa6a6a6)
    }
    /// T3 Code's `--primary`.
    pub(crate) const ACCENT: Rgba = rgb(0x346bf1);
    // T3 Code's dark theme, for the conversation's header and the banners
    // over its timeline and composer.
    /// `--foreground` (neutral-100).
    pub(crate) fn t3_foreground() -> Rgba {
        text(0xf5f5f5)
    }

    /// `--muted-foreground` (neutral-400).
    pub(crate) fn t3_muted() -> Rgba {
        text(0xa1a1a1)
    }

    /// `--accent`: a pressed or hovered ghost control, white at 4%.
    #[cfg(desktop)]
    pub(crate) const T3_ACCENT: Rgba = rgba(0xffffff0a);
    /// `--input`: an outlined control's edge, white at 8%.
    pub(crate) const T3_INPUT: Rgba = rgba(0xffffff14);
    /// `--warning` (amber-500).
    pub(crate) const WARNING: Rgba = rgb(0xfe9a00);
    /// A ghost control under the pointer: the selection fill at half opacity.
    pub(crate) const GHOST_HOVER: Rgba = rgba(0x28282880);
    /// A sidebar row under the pointer.
    pub(crate) const ROW_HOVER: Rgba = rgba(0x282828cc);
}

pub(crate) const RADIUS: f32 = 6.0;
pub(crate) const HEADER_HEIGHT: f32 = 52.0;

/// T3 Code's distance from the window's end to its top-right controls (12
/// points and a pixel), past the window controls Windows draws over the bar.
#[cfg(desktop)]
pub(crate) fn titlebar_end_inset() -> Points {
    px(13.0 + nsh_core::element::desktop::title_bar().right.0)
}

/// An icon: its markup, drawn in the current text color. Every platform draws
/// the same Lucide set, as the existing desktop client did, so icons side by
/// side share one size and stroke.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct Icon {
    markup: &'static str,
}

/// A Lucide icon, its markup taken from the catalog at compile time: nothing
/// looks icons up at run time, so the linker can drop the rest of the catalog.
macro_rules! icon {
    ($lucide:ident) => {
        Icon {
            markup: lucide_static_svg::Icon::$lucide.svg_str(),
        }
    };
}

impl Icon {
    pub(crate) const FAST: Self = Self {
        markup: include_str!("../assets/bolt.svg"),
    };
    pub(crate) const FAST_ON: Self = Self {
        markup: include_str!("../assets/bolt-fill.svg"),
    };
    pub(crate) const ARROW_LEFT: Self = icon!(ArrowLeft);
    pub(crate) const ARROW_RIGHT: Self = icon!(ArrowRight);
    pub(crate) const ARROW_UP: Self = icon!(ArrowUp);
    pub(crate) const ARROW_DOWN: Self = icon!(ArrowDown);
    pub(crate) const GIT_FORK: Self = icon!(GitFork);
    #[cfg(desktop)]
    pub(crate) const GIT_BRANCH: Self = icon!(GitBranch);
    pub(crate) const WRAP: Self = icon!(TextWrap);
    #[cfg(desktop)]
    pub(crate) const MAXIMIZE: Self = icon!(Maximize2);
    pub(crate) const MINIMIZE: Self = icon!(Minimize2);
    pub(crate) const WARNING: Self = icon!(TriangleExclamationPoint);
    pub(crate) const CHECK: Self = icon!(Check);
    pub(crate) const CHEVRON_DOWN: Self = icon!(ChevronDown);
    pub(crate) const CHEVRON_RIGHT: Self = icon!(ChevronRight);
    pub(crate) const CHEVRON_UP: Self = icon!(ChevronUp);
    pub(crate) const CLOSE: Self = icon!(X);
    pub(crate) const CIRCLE_STOP: Self = icon!(CircleStop);
    pub(crate) const COPY: Self = icon!(Copy);
    #[cfg(desktop)]
    pub(crate) const DETAILS: Self = icon!(SquareMenu);
    pub(crate) const DIFF: Self = icon!(Diff);
    #[cfg(desktop)]
    pub(crate) const DOWNLOAD: Self = icon!(Download);
    pub(crate) const ELLIPSIS: Self = icon!(Ellipsis);
    pub(crate) const WRENCH: Self = icon!(Wrench);
    pub(crate) const TERMINAL: Self = icon!(SquareTerminal);
    pub(crate) const BRAIN: Self = icon!(Brain);
    pub(crate) const REPLACE: Self = icon!(Replace);
    #[cfg(desktop)]
    pub(crate) const FOLDER_OPEN: Self = icon!(FolderOpen);
    pub(crate) const FILE_TEXT: Self = icon!(FileText);
    pub(crate) const EXTERNAL_LINK: Self = icon!(ExternalLink);
    pub(crate) const FOLDER: Self = icon!(Folder);
    pub(crate) const GLOBE: Self = icon!(Globe);
    pub(crate) const NETWORK: Self = icon!(Network);
    pub(crate) const KEYBOARD: Self = icon!(Keyboard);
    pub(crate) const FOLDER_PLUS: Self = icon!(FolderPlus);
    pub(crate) const LOADER: Self = icon!(Loader);
    #[cfg(dictation)]
    pub(crate) const MICROPHONE: Self = icon!(Mic);
    #[cfg(desktop)]
    pub(crate) const MONITOR: Self = icon!(Monitor);
    pub(crate) const NEW_CHAT: Self = icon!(SquarePen);
    pub(crate) const PENCIL: Self = icon!(Pencil);
    pub(crate) const TRASH: Self = icon!(Trash);
    #[cfg(desktop)]
    pub(crate) const PANEL_LEFT_CLOSE: Self = icon!(PanelLeftClose);
    #[cfg(desktop)]
    pub(crate) const PANEL_LEFT_OPEN: Self = icon!(PanelLeftOpen);
    #[cfg(desktop)]
    pub(crate) const PANEL_RIGHT: Self = icon!(PanelRight);
    #[cfg(desktop)]
    pub(crate) const PANEL_BOTTOM: Self = icon!(PanelBottom);
    pub(crate) const PLUS: Self = icon!(Plus);
    pub(crate) const PUZZLE: Self = icon!(Puzzle);
    pub(crate) const SHIELD: Self = icon!(Shield);
    pub(crate) const SPARKLES: Self = icon!(Sparkles);
    pub(crate) const QUESTION: Self = icon!(MessageCircleQuestionMark);
    #[cfg(desktop)]
    pub(crate) const QUOTE: Self = icon!(Quote);
    pub(crate) const REFRESH: Self = icon!(RotateCw);
    #[cfg(desktop)]
    pub(crate) const SEARCH: Self = icon!(Search);
    pub(crate) const SHARE: Self = icon!(Share);
    pub(crate) const EXPAND_ALL: Self = icon!(ChevronsUpDown);
    pub(crate) const COLLAPSE_ALL: Self = icon!(ChevronsDownUp);
    #[cfg(desktop)]
    pub(crate) const SETTINGS: Self = icon!(Settings);
    #[cfg(desktop)]
    pub(crate) const SLIDERS: Self = icon!(Settings2);
    #[cfg(desktop)]
    pub(crate) const FOLDER_TREE: Self = icon!(FolderTree);
    #[cfg(desktop)]
    pub(crate) const SERVER: Self = icon!(Server);
    pub(crate) const BOT: Self = icon!(Bot);
    pub(crate) const STEER: Self = icon!(Redo2);
    #[cfg(desktop)]
    pub(crate) const ZOOM_IN: Self = icon!(ZoomIn);
    #[cfg(desktop)]
    pub(crate) const ZOOM_OUT: Self = icon!(ZoomOut);
    /// T3 Code's follow-up actions and the queue above its composer.
    pub(crate) const QUEUE: Self = icon!(ListPlus);
    pub(crate) const STEER_NOW: Self = icon!(CornerUpRight);
    pub(crate) const QUEUED: Self = icon!(ListOrdered);
    pub(crate) const GRIP: Self = icon!(GripVertical);
    pub(crate) const SAVING: Self = icon!(Clock3);
    pub(crate) const RESUME: Self = icon!(Play);
    /// A filled square, which Lucide's outlined `square` is not.
    pub(crate) const STOP: Self = Self {
        markup: include_str!("../assets/stop.svg"),
    };
}

impl Icon {
    const NOTIFY_BOT: Self = icon!(Bot);
    const NOTIFY_TERMINAL: Self = icon!(Terminal);
    const NOTIFY_EYE: Self = icon!(Eye);
    const NOTIFY_ZAP: Self = icon!(Zap);
    /// Lucide 1 names circle-alert circle-exclamation-point.
    pub(crate) const ALERT: Self = icon!(CircleExclamationPoint);
    /// T3 Code's sidebar statuses: working, waiting on background work, an
    /// unseen completion and a pending approval.
    pub(crate) const STATUS_WORKING: Self = icon!(CircleDashed);
    pub(crate) const STATUS_WAITING: Self = icon!(CircleDotDashed);
    pub(crate) const STATUS_DONE: Self = icon!(CircleCheck);
    pub(crate) const STATUS_APPROVAL: Self = icon!(ShieldQuestionMark);
    const HAMMER: Self = icon!(Hammer);

    /// A notification row's icon, as T3 Code's work log draws it.
    pub(crate) fn notification(
        icon: agent_core::presentation::background_work::NotificationIcon,
    ) -> Self {
        use agent_core::presentation::background_work::NotificationIcon::*;
        match icon {
            Bot => Self::NOTIFY_BOT,
            Terminal => Self::NOTIFY_TERMINAL,
            Eye => Self::NOTIFY_EYE,
            Zap => Self::NOTIFY_ZAP,
            CircleAlert => Self::ALERT,
        }
    }

    /// A work item's tool, as T3 Code's work log shows it.
    pub(crate) fn work(kind: &str) -> Self {
        match kind {
            "command" => Self::NOTIFY_TERMINAL,
            "fileChange" => Self::NEW_CHAT,
            "mixed" => Self::HAMMER,
            "reasoning" => Self::BRAIN,
            "webSearch" => Self::GLOBE,
            _ => Self::WRENCH,
        }
    }
}

/// An icon in the current text color.
pub(crate) fn icon(icon: Icon, size: f32) -> impl IntoElement {
    svg(ImageSrc::svg_markup(icon.markup))
        .w(px(size))
        .h(px(size))
        .flex_shrink_0()
}

/// A card of rows divided by rules, as T3 Code's settings groups.
pub(crate) fn card(rows: Vec<View>) -> View {
    rows.into_iter().enumerate().fold(
        view()
            .rounded(px(12.0))
            .border_1()
            .border_color(colors::border())
            .bg(colors::SURFACE),
        |group, (index, row)| {
            group.child(if index == 0 {
                row
            } else {
                row.border_t_1().border_t_color(colors::border())
            })
        },
    )
}

/// A text field in a bordered box, as T3 Code's inputs.
pub(crate) fn field() -> nsh_core::element::Input {
    nsh_core::element::input()
        .auto_correct(false)
        .auto_capitalize(AutoCapitalize::None)
        .text_color(move || nsh_base::theme().foreground)
        .placeholder_color(move || color(nsh_base::theme().muted_foreground))
        .w_full()
        .h(px(32.0))
        .px_3()
        .rounded(px(RADIUS))
        .border_1()
        .border_color(colors::border())
}

/// A labeled action in T3 Code's button tones: the primary fill, an outline,
/// or the destructive tint. NSH draws it on every platform.
pub(crate) fn action_button(
    title: impl IntoReactive<SharedString>,
    tone: ActionTone,
    press: impl Fn() + 'static,
) -> nsh_base::ButtonElement {
    let (fill, foreground, hover, outline) = match tone {
        ActionTone::Primary => (rgb(0xf5f5f5), rgb(0x0f0f0f), rgb(0xd4d4d4), rgb(0xf5f5f5)),
        ActionTone::Secondary => (
            rgba(0x00000000),
            colors::foreground(),
            colors::GHOST_HOVER,
            colors::border(),
        ),
        ActionTone::Danger => (rgb(0x301214), rgb(0xff6467), rgb(0x3d171a), rgb(0x301214)),
    };
    let title = title.into_reactive();
    nsh_base::button()
        .accessibility_label(nsh_core::element::Prop::from(title.clone().map(Some)))
        .flex_shrink_0()
        .flex_row()
        .items_center()
        .justify_center()
        .h(px(28.0))
        .px_3()
        .rounded(px(RADIUS))
        .border_1()
        .border_color(outline)
        .bg(fill)
        .text_color(foreground)
        .hover(move |s| s.bg(hover))
        .disabled_style(|s| s.opacity(0.5))
        .child(
            text(title)
                .text_size(px(13.0))
                .font_weight(FontWeight::MEDIUM)
                .number_of_lines(1),
        )
        .on_press(press)
}

/// A setting: its title and description, and its control at the end.
pub(crate) fn setting(
    title: impl IntoReactive<SharedString>,
    description: impl Fn() -> String + 'static,
    control: impl IntoElement,
) -> View {
    view()
        .flex_row()
        .items_center()
        .gap_4()
        .px_5()
        .py_4()
        .child(
            view()
                .flex_1()
                .min_w_0()
                .gap_1()
                .child(
                    text(title)
                        .text_size(px(14.0))
                        .font_weight(FontWeight::MEDIUM),
                )
                .child(
                    text(description)
                        .text_size(px(13.0))
                        .text_color(colors::muted())
                        .selectable(true),
                ),
        )
        .child(control)
}

/// The activity indicator: a turning loader, as the desktop client's.
pub(crate) fn spinner() -> impl IntoElement {
    view()
        .w(px(14.0))
        .h(px(14.0))
        .animate_spin()
        .child(icon(Icon::LOADER, 14.0))
}

/// A text control: nsh-base's button in the desktop client's ghost look, or
/// AppKit's own on the Mac.
#[cfg(not(target_os = "macos"))]
pub(crate) type Control = nsh_base::ButtonElement;

/// nsh-base's button (focus, keys, accessibility) with no look of its own,
/// for a control drawn in T3 Code's.
pub(crate) fn bare_button(
    label: impl IntoReactive<SharedString>,
    press: impl Fn() + 'static,
) -> nsh_base::ButtonElement {
    nsh_base::button()
        .accessibility_label(nsh_core::element::Prop::from(
            label.into_reactive().map(Some),
        ))
        .flex_shrink_0()
        .flex_row()
        .items_center()
        .justify_center()
        .disabled_style(|s| s.opacity(0.64))
        .on_press(press)
}

/// T3 Code's ghost toggle (`Toggle variant="ghost" size="sm"`): 28 points,
/// rounded by 10, a 16-point icon at 80%, filled while pressed or hovered.
#[cfg(desktop)]
pub(crate) fn toggle(
    glyph: Icon,
    label: impl IntoReactive<SharedString>,
    pressed: impl Fn() -> bool + Copy + 'static,
    press: impl Fn() + 'static,
) -> nsh_base::ButtonElement {
    let label = label.into_reactive();
    bare_button(label.clone(), press)
        .w(px(28.0))
        .h(px(28.0))
        .rounded(px(10.0))
        .text_color(colors::t3_foreground().opacity(0.8))
        .hover(|s| s.bg(colors::T3_ACCENT))
        .selected(pressed)
        .selected_style(|s| s.bg(colors::T3_ACCENT))
        .child(view().tooltip(tooltip(label)).child(icon(glyph, 16.0)))
}

/// nsh-base's button (focus, keys, accessibility) in the desktop client's
/// ghost look, its content filling it.
pub(crate) fn ghost(
    label: impl IntoReactive<SharedString>,
    content: View,
) -> nsh_base::ButtonElement {
    nsh_base::button()
        .accessibility_label(nsh_core::element::Prop::from(
            label.into_reactive().map(Some),
        ))
        .flex_shrink_0()
        .flex_row()
        .items_center()
        .justify_center()
        .gap_2()
        .rounded(px(RADIUS))
        .text_color(colors::foreground())
        .hover(|s| s.bg(colors::GHOST_HOVER))
        .active(|s| s.bg(colors::SELECTED))
        .disabled_style(|s| s.opacity(0.5))
        .child(content.flex_row().items_center().min_w_0())
}

/// A small ghost icon button (24 pt, a 14 pt icon) with its label as
/// tooltip, as the desktop client's `small` buttons. NSH draws it on every
/// platform: AppKit's symbols differ in size and weight side by side, and its
/// buttons measure themselves through SwiftUI, which scrolling rows cannot
/// afford.
pub(crate) fn icon_button(
    icon: Icon,
    label: impl IntoReactive<SharedString>,
    press: impl Fn() + 'static,
) -> nsh_base::ButtonElement {
    icon_button_sized(icon, label, 24.0, 14.0, press)
}

/// An extra-small ghost icon button (20 pt, a 12 pt icon).
pub(crate) fn small_icon_button(
    icon: Icon,
    label: impl IntoReactive<SharedString>,
    press: impl Fn() + 'static,
) -> nsh_base::ButtonElement {
    icon_button_sized(icon, label, 20.0, 12.0, press)
}

pub(crate) fn icon_button_sized(
    glyph: Icon,
    label: impl IntoReactive<SharedString>,
    size: f32,
    icon_size: f32,
    press: impl Fn() + 'static,
) -> nsh_base::ButtonElement {
    let label = label.into_reactive();
    ghost(
        label.clone(),
        view().tooltip(tooltip(label)).child(icon(glyph, icon_size)),
    )
    .w(px(size))
    .h(px(size))
    .on_press(press)
}

/// A hint over a control, as the desktop client's tooltips.
pub(crate) fn tooltip(label: impl IntoReactive<SharedString>) -> AnyElement {
    view()
        .px_2()
        .py_1()
        .rounded(px(RADIUS))
        .bg(colors::SURFACE)
        .border_1()
        .border_color(colors::border())
        .child(
            text(label)
                .text_size(px(12.0))
                .text_color(colors::foreground()),
        )
        .into_element()
}

/// A small ghost text button.
#[cfg(not(target_os = "macos"))]
pub(crate) fn text_button(
    title: impl IntoReactive<SharedString>,
    press: impl Fn() + 'static,
) -> Control {
    row_text_button(title, press)
}

/// A text button whose label for assistive technologies says what its short
/// title leaves to its row (the account a row's "サインアウト" signs out of).
#[cfg(not(target_os = "macos"))]
pub(crate) fn labelled_text_button(
    title: impl IntoReactive<SharedString>,
    label: impl IntoReactive<SharedString>,
    press: impl Fn() + 'static,
) -> Control {
    ghost_text(title, label, press)
}

/// A small ghost text button drawn by NSH on every platform, for controls
/// repeated on each row of a list.
pub(crate) fn row_text_button(
    title: impl IntoReactive<SharedString>,
    press: impl Fn() + 'static,
) -> nsh_base::ButtonElement {
    let title = title.into_reactive();
    ghost_text(title.clone(), title, press)
}

/// A ghost text button that assistive technologies name by `label`.
fn ghost_text(
    title: impl IntoReactive<SharedString>,
    label: impl IntoReactive<SharedString>,
    press: impl Fn() + 'static,
) -> nsh_base::ButtonElement {
    ghost(
        label,
        view().child(text(title).text_size(px(14.0)).number_of_lines(1)),
    )
    .h(px(24.0))
    .px_2()
    .on_press(press)
}

/// A small ghost button with an icon and a label drawn by NSH, as a queued
/// message's Steer above T3 Code's composer.
pub(crate) fn row_labeled_button(
    glyph: Icon,
    title: impl IntoReactive<SharedString>,
    press: impl Fn() + 'static,
) -> nsh_base::ButtonElement {
    let title = title.into_reactive();
    ghost(
        title.clone(),
        view()
            .gap_1()
            .child(icon(glyph, 14.0))
            .child(text(title).text_size(px(12.0)).number_of_lines(1)),
    )
    .h(px(24.0))
    .px_2()
    .on_press(press)
}

/// A ghost button with an icon and a title (32 pt), as the composer's folder;
/// screen readers and its tooltip name it by `label`.
pub(crate) fn labeled_button(
    glyph: Icon,
    title: impl Fn() -> String + Copy + 'static,
    label: impl Fn() -> String + 'static,
    press: impl Fn() + 'static,
) -> nsh_base::ButtonElement {
    ghost(
        move || SharedString::from(label()),
        view()
            .gap_2()
            .min_w_0()
            .child(icon(glyph, 16.0))
            .child(text(title).text_size(px(14.0)).number_of_lines(1).min_w_0()),
    )
    .h(px(32.0))
    .px_2()
    .min_w_0()
    .max_w(px(260.0))
    .on_press(press)
}

pub(crate) trait Enabled {
    fn enabled(self, enabled: impl Fn() -> bool + Copy + 'static) -> Self;
}

impl Enabled for nsh_base::ButtonElement {
    fn enabled(self, enabled: impl Fn() -> bool + Copy + 'static) -> Self {
        self.disabled(move || !enabled())
    }
}

/// On the Mac a text control is AppKit's own: a borderless `NSButton`,
/// placed and sized by NSH.
#[cfg(target_os = "macos")]
pub(crate) type Control = nsh_core::element::NativeView;

#[cfg(target_os = "macos")]
pub(crate) fn text_button(
    title: impl IntoReactive<SharedString>,
    press: impl Fn() + 'static,
) -> Control {
    let title = title.into_reactive();
    labelled_text_button(title.clone(), title, press)
}

#[cfg(target_os = "macos")]
pub(crate) fn labelled_text_button(
    title: impl IntoReactive<SharedString>,
    label: impl IntoReactive<SharedString>,
    press: impl Fn() + 'static,
) -> Control {
    let label = label.into_reactive();
    native("bex.button")
        .prop(
            "title",
            title
                .into_reactive()
                .map(|title| serde_json::json!(title.to_string())),
        )
        .prop(
            "label",
            label
                .clone()
                .map(|label| serde_json::json!(label.to_string())),
        )
        .prop(
            "tooltip",
            label.map(|label| serde_json::json!(label.to_string())),
        )
        .prop("appearance", serde_json::json!("plain"))
        .on_native_event(move |event: &NativeEvent| {
            if &*event.name == "press" {
                press();
            }
        })
}

/// On Apple platforms the app's controls are the platform's native components.
#[cfg(any(target_os = "macos", target_os = "ios"))]
impl Enabled for nsh_core::element::NativeView {
    fn enabled(self, enabled: impl Fn() -> bool + Copy + 'static) -> Self {
        self.prop("enabled", move || serde_json::json!(enabled()))
    }
}

#[cfg(not(any(target_os = "macos", target_os = "ios")))]
impl Enabled for nsh_base::SwitchElement {
    fn enabled(self, enabled: impl Fn() -> bool + Copy + 'static) -> Self {
        self.disabled(nsh_core::element::Prop::bind(move || !enabled()))
    }
}

/// A control shown as on: AppKit's selected state, or selected for screen
/// readers on the selection fill.
#[cfg(desktop)]
pub(crate) trait Selected {
    fn selected_when(self, selected: impl Fn() -> bool + Copy + 'static) -> Self;
}

#[cfg(target_os = "macos")]
impl Selected for Control {
    fn selected_when(self, selected: impl Fn() -> bool + Copy + 'static) -> Self {
        self.prop("selected", move || serde_json::json!(selected()))
    }
}

#[cfg(desktop)]
impl Selected for nsh_base::ButtonElement {
    fn selected_when(self, selected: impl Fn() -> bool + Copy + 'static) -> Self {
        self.selected(selected)
            .selected_style(|s| s.bg(colors::SELECTED))
    }
}

/// A labeled on/off setting, its label for assistive technologies: the
/// platform's switch on Apple platforms, NSH's in T3 Code's look elsewhere.
#[cfg(any(target_os = "macos", target_os = "ios"))]
pub(crate) type Switch = nsh_core::element::NativeView;
#[cfg(not(any(target_os = "macos", target_os = "ios")))]
pub(crate) type Switch = nsh_base::SwitchElement;

pub(crate) fn switch(
    label: impl Fn() -> String + 'static,
    value: impl Fn() -> bool + 'static,
    change: impl Fn(bool) + 'static,
) -> Switch {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    return native("bex.switch")
        .prop("label", move || serde_json::json!(label()))
        .prop("value", move || serde_json::json!(value()))
        .on_native_event(move |event: &NativeEvent| {
            if &*event.name == "change"
                && let Some(value) = event.data.as_bool()
            {
                change(value);
            }
        });
    #[cfg(not(any(target_os = "macos", target_os = "ios")))]
    {
        use nsh_core::element::{Prop, white};
        nsh_base::switch()
            .checked(Prop::bind(value))
            .accessibility_label(Prop::bind(move || Some(SharedString::from(label()))))
            .on_change(move |next: &bool| change(*next))
            .flex_shrink_0()
            .disabled_style(|s| s.opacity(0.5))
            .track_style(|s| {
                s.w(px(36.0))
                    .h(px(20.0))
                    .p(px(2.0))
                    .rounded_full()
                    .bg(colors::border())
                    .checked(|s| s.bg(colors::ACCENT))
            })
            .thumb_style(|s| {
                s.size(px(16.0))
                    .rounded_full()
                    .bg(white())
                    .checked(|s| s.translate_x(px(16.0)))
            })
    }
}

/// A search field showing `value` and reporting each change: the platform's
/// on Apple platforms, an NSH field elsewhere. Without a placeholder the
/// platform's own shows.
#[cfg(any(target_os = "macos", target_os = "ios"))]
pub(crate) type SearchField = nsh_core::element::NativeView;
#[cfg(not(any(target_os = "macos", target_os = "ios")))]
pub(crate) type SearchField = nsh_core::element::Input;

pub(crate) fn search_field(
    value: impl Fn() -> String + 'static,
    change: impl Fn(&str) + 'static,
    placeholder: Option<&'static str>,
) -> SearchField {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    {
        let field = native("bex.search")
            .prop("value", move || serde_json::json!(value()))
            .on_native_event(move |event: &NativeEvent| {
                if &*event.name == "change"
                    && let Some(text) = event.data.as_str()
                {
                    change(text);
                }
            });
        match placeholder {
            Some(placeholder) => field.prop("placeholder", serde_json::json!(placeholder)),
            None => field,
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "ios")))]
    {
        let placeholder = placeholder.unwrap_or(crate::l10n::tr("検索"));
        field()
            .label(placeholder)
            .placeholder(placeholder)
            .value(value)
            .on_change(move |text: &str| change(text))
            .text_size(px(13.0))
            .h(px(28.0))
    }
}

/// The search field of a desktop list: a borderless field after a search
/// icon, both centered on the row, as the desktop client's.
#[cfg(desktop)]
pub(crate) fn search(query: Signal<String>, placeholder: &'static str) -> View {
    view()
        .flex_row()
        .items_center()
        .gap_2()
        .h(px(24.0))
        .px_2()
        .text_color(colors::muted())
        .child(icon(Icon::SEARCH, 14.0))
        .child(
            nsh_core::element::input()
                .auto_correct(false)
                .auto_capitalize(AutoCapitalize::None)
                .label(placeholder)
                .placeholder(placeholder)
                .value(move || query.get())
                .on_change(move |text: &str| query.set(text.to_owned()))
                .text_size(px(14.0))
                .text_color(colors::foreground())
                .placeholder_color(colors::muted())
                .flex_1()
                .min_w_0(),
        )
}

/// A share of a known extent (a usage window): AppKit's or UIKit's indicator
/// on Apple platforms, an NSH bar elsewhere.
pub(crate) fn progress(value: impl Fn() -> f64 + 'static) -> AnyElement {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    return native("bex.progress")
        .prop("value", move || serde_json::json!(value()))
        .w_full()
        .h(px(8.0))
        .into_element();
    #[cfg(not(any(target_os = "macos", target_os = "ios")))]
    nsh_base::progress()
        .value(nsh_core::element::Prop::bind(move || value() as f32))
        .w_full()
        .track_style(|s| s.w_full().h(px(8.0)).rounded_full().bg(colors::border()))
        .indicator_style(|s| s.h_full().rounded_full().bg(colors::foreground()))
        .into_element()
}

/// A menu button titled with its current choice: the platform's on Apple
/// platforms, a ghost button opening the platform's popup elsewhere.
pub(crate) fn choice_menu(
    symbol: &'static str,
    label: &'static str,
    title: impl Fn() -> String + Copy + 'static,
    items: impl Fn() -> Value + 'static,
    select: impl Fn(&str) + Clone + 'static,
) -> impl Enabled + IntoElement {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    return crate::native::menu(symbol, label, items, move |id| select(id))
        .prop("title", move || serde_json::json!(title()));
    #[cfg(not(any(target_os = "macos", target_os = "ios")))]
    let _ = symbol;
    #[cfg(not(any(target_os = "macos", target_os = "ios")))]
    ghost(
        label,
        view()
            .gap_1()
            .child(text(title).text_size(px(14.0)).number_of_lines(1))
            .child(icon(Icon::CHEVRON_DOWN, 14.0)),
    )
    .h(px(28.0))
    .px_2()
    .on_press(move || popup(&items(), select.clone()))
}

/// Shows `items` (`{id, title, enabled, selected}`, `{separator}`, `{title, children}`)
/// as the platform's popup menu and reports the chosen id.
pub(crate) fn popup(items: &Value, select: impl Fn(&str) + Clone + 'static) {
    let items = menu_items(items, &select);
    drop(nsh_core::element::desktop::popup_menu(items));
}

fn menu_items(items: &Value, select: &(impl Fn(&str) + Clone + 'static)) -> Vec<MenuItem> {
    items
        .as_array()
        .map(|items| {
            items
                .iter()
                .map(|entry| {
                    let title = entry["title"].as_str().unwrap_or_default();
                    if entry["separator"].as_bool().unwrap_or(false) {
                        MenuItem::separator()
                    } else if let Some(children) = entry.get("children") {
                        MenuItem::submenu(title, menu_items(children, select))
                    } else {
                        let id = entry["id"].as_str().unwrap_or_default().to_owned();
                        let select = select.clone();
                        MenuItem::new(title)
                            .disabled(!entry["enabled"].as_bool().unwrap_or(true))
                            .checked(entry["selected"].as_bool().unwrap_or(false))
                            .on_select(move || select(&id))
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

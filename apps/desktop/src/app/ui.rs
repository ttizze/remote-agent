//! Palette colors, icons and the small controls every screen shares.
use agent_domain::Driver;
use gpui_kit::{
    component::{
        Icon, Sizable, Theme, ThemeMode,
        button::{Button, ButtonVariants},
    },
    *,
};
use std::cell::RefCell;

thread_local! {
    static PALETTE: RefCell<agent_core::presentation::theme::Theme> =
        RefCell::new(agent_core::presentation::theme::theme(true));
    static DARK: RefCell<bool> = const { RefCell::new(true) };
}

pub(crate) fn is_dark() -> bool {
    DARK.with(|dark| *dark.borrow())
}

/// A palette role (`canvas`, `textMuted`, `sidebarRowHover`, ...). Unknown
/// roles read as the text color.
pub(crate) fn color(role: &str) -> Hsla {
    PALETTE.with(|palette| {
        let palette = palette.borrow();
        let value = palette
            .colors
            .get(role)
            .or_else(|| palette.colors.get("text"))
            .map_or("ffffff", |value| value.trim_start_matches('#'));
        rgb(u32::from_str_radix(value, 16).unwrap_or(0xffffff)).into()
    })
}

/// A palette role at `alpha` opacity, as `bg-warning/8` writes it.
pub(crate) fn tint(role: &str, alpha: f32) -> Hsla {
    color(role).opacity(alpha)
}

/// The layout constants of the stock theme.
pub(crate) struct Metrics {
    pub header_height: f32,
    pub chat_max_width: f32,
    pub sidebar_width: f32,
    pub panel_width: f32,
    pub prompt_size: f32,
    pub code_size: f32,
}
pub(crate) fn metrics() -> Metrics {
    PALETTE.with(|palette| {
        let palette = palette.borrow();
        Metrics {
            header_height: palette.header_height,
            chat_max_width: palette.chat_max_width,
            sidebar_width: palette.sidebar_width,
            panel_width: palette.panel_width,
            prompt_size: palette.prompt_size,
            code_size: palette.code_size,
        }
    })
}

/// Follows the system appearance, as the stock theme does.
pub(crate) fn apply_appearance(appearance: WindowAppearance, cx: &mut App) {
    let dark = matches!(
        appearance,
        WindowAppearance::Dark | WindowAppearance::VibrantDark
    );
    DARK.with(|value| *value.borrow_mut() = dark);
    PALETTE.with(|palette| {
        *palette.borrow_mut() = agent_core::presentation::theme::theme(dark);
    });
    Theme::change(
        if dark {
            ThemeMode::Dark
        } else {
            ThemeMode::Light
        },
        None,
        cx,
    );
    let metrics = metrics();
    let theme = Theme::global_mut(cx);
    // The rem stays 16 px so Tailwind sizes (text-sm = 14 px) read as written.
    theme.font_size = px(16.);
    theme.mono_font_size = px(metrics.code_size);
    theme.radius = px(8.);
    theme.radius_lg = px(10.);
    for (target, role) in [
        (&mut theme.colors.background, "canvas"),
        (&mut theme.colors.foreground, "text"),
        (&mut theme.colors.border, "border"),
        (&mut theme.colors.input, "input"),
        (&mut theme.colors.muted, "muted"),
        (&mut theme.colors.muted_foreground, "textMuted"),
        (&mut theme.colors.popover, "surfaceOverlay"),
        (&mut theme.colors.popover_foreground, "text"),
        (&mut theme.colors.primary, "messageAction"),
        (&mut theme.colors.primary_hover, "messageActionHover"),
        (&mut theme.colors.primary_active, "messageActionHover"),
        (
            &mut theme.colors.primary_foreground,
            "messageActionForeground",
        ),
        (&mut theme.colors.ring, "focus"),
        (&mut theme.colors.accent, "accentSurface"),
        (
            &mut theme.colors.accent_foreground,
            "accentSurfaceForeground",
        ),
        (&mut theme.colors.secondary, "secondary"),
        (
            &mut theme.colors.secondary_foreground,
            "secondaryForeground",
        ),
        (&mut theme.colors.secondary_hover, "accentSurface"),
        (&mut theme.colors.sidebar, "sidebar"),
        (&mut theme.colors.sidebar_foreground, "sidebarForeground"),
        (&mut theme.colors.sidebar_border, "sidebarBorder"),
        (&mut theme.colors.sidebar_accent, "sidebarRowHover"),
        (
            &mut theme.colors.sidebar_accent_foreground,
            "sidebarForeground",
        ),
        (&mut theme.colors.link, "updateForeground"),
        (&mut theme.colors.button, "surface"),
        (&mut theme.colors.button_foreground, "text"),
        (&mut theme.colors.button_hover, "toolbarControlHover"),
        (&mut theme.colors.button_primary, "messageAction"),
        (&mut theme.colors.button_primary_hover, "messageActionHover"),
        (
            &mut theme.colors.button_primary_active,
            "messageActionHover",
        ),
        (
            &mut theme.colors.button_primary_foreground,
            "messageActionForeground",
        ),
        (&mut theme.colors.danger, "error"),
        (&mut theme.colors.danger_foreground, "accentForeground"),
        (&mut theme.colors.warning, "warning"),
        (&mut theme.colors.warning_foreground, "warningForeground"),
        (&mut theme.colors.success_foreground, "successForeground"),
        (&mut theme.colors.tab_bar, "toolbar"),
        (&mut theme.colors.title_bar, "chrome"),
        (&mut theme.colors.title_bar_border, "toolbarBorder"),
        (&mut theme.colors.list_hover, "accentSurface"),
        (&mut theme.colors.list_active, "accentSurface"),
        (&mut theme.colors.selection, "terminalSelection"),
    ] {
        *target = color(role);
    }
}

/// A lucide icon by its name, such as `git-fork`.
pub(crate) fn icon(name: &str) -> Icon {
    Icon::default().path(SharedString::from(format!("lucide/{name}.svg")))
}

/// The provider's mark.
pub(crate) fn driver_icon(driver: Driver) -> Icon {
    Icon::default().path(match driver {
        Driver::Codex => "brand/openai.svg",
        Driver::Claude => "brand/claude.svg",
    })
}

/// The label of the secondary modifier: ⌘ on macOS, Ctrl elsewhere.
pub(crate) fn mod_key() -> &'static str {
    if cfg!(target_os = "macos") {
        "⌘"
    } else {
        "Ctrl+"
    }
}

/// A shortcut label such as `⌘J`.
pub(crate) fn shortcut(key: &str) -> String {
    format!("{}{key}", mod_key())
}

/// A 28 px ghost icon button whose tooltip names it.
pub(crate) fn icon_button(id: impl Into<ElementId>, icon_name: &str, label: &str) -> Button {
    Button::new(id)
        .icon(icon(icon_name))
        .ghost()
        .small()
        .size(px(28.))
        .tooltip(label.to_owned())
        .accessibility_label(label.to_owned())
}

/// `text-2xs` (11/16).
pub(crate) fn text_2xs<E: Styled>(element: E) -> E {
    element.text_size(px(11.)).line_height(px(16.))
}

/// The current time in milliseconds since the epoch.
pub(crate) fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

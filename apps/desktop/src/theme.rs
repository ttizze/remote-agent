use agent_core::presentation::theme::{ThemePlatform, native_palette};
use gpui_kit::{
    App, SharedString,
    component::{Theme, ThemeConfig, ThemeConfigColors, ThemeMode},
};
use std::rc::Rc;

fn config(dark: bool) -> ThemeConfig {
    let palette = native_palette(ThemePlatform::Desktop, dark);
    let color = |rgb: u32| Some(SharedString::from(format!("#{rgb:06x}")));
    let mut colors = ThemeConfigColors::default();
    colors.background = color(palette.background);
    colors.foreground = color(palette.foreground);
    colors.border = color(palette.border);
    colors.primary = color(palette.primary);
    colors.primary_foreground = color(0xffffff);
    colors.secondary = color(palette.surface);
    colors.secondary_foreground = color(palette.foreground);
    colors.muted = color(palette.user_bubble);
    colors.muted_foreground = color(palette.muted);
    colors.accent = color(palette.user_bubble);
    colors.accent_foreground = color(palette.foreground);
    colors.popover = color(palette.surface);
    colors.popover_foreground = color(palette.foreground);
    colors.sidebar = color(palette.sidebar);
    colors.sidebar_foreground = color(palette.foreground);
    colors.sidebar_border = color(palette.border);
    colors.danger = color(palette.error);
    ThemeConfig {
        name: "Bex".into(),
        mode: if dark {
            ThemeMode::Dark
        } else {
            ThemeMode::Light
        },
        font_size: Some(14.),
        radius: Some(10),
        colors,
        ..Default::default()
    }
}

pub(crate) fn install(cx: &mut App) {
    let theme = Theme::global_mut(cx);
    theme.light_theme = Rc::new(config(false));
    theme.dark_theme = Rc::new(config(true));
    Theme::sync_system_appearance(None, cx);
}

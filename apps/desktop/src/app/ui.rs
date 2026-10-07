//! Palette colors, icons and the small controls every screen shares.
use agent_core::view::appearance::{Appearance, ChatWidth, DiffColors};
use agent_domain::Driver;
use gpui_kit::{
    component::{
        Icon, Sizable, Theme, ThemeMode,
        button::{Button, ButtonVariants},
    },
    *,
};
use std::{
    cell::RefCell,
    path::PathBuf,
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

thread_local! {
    static PALETTE: RefCell<agent_core::presentation::theme::Theme> =
        RefCell::new(agent_core::presentation::theme::theme(true));
    static DARK: RefCell<bool> = const { RefCell::new(true) };
    static SYSTEM_DARK: RefCell<bool> = const { RefCell::new(true) };
    static APPEARANCE: RefCell<Appearance> = RefCell::new(Appearance::default());
}

/// The newest appearance save; an older save still waiting skips its write.
static SAVE_GENERATION: AtomicU64 = AtomicU64::new(0);
static SAVE_LOCK: Mutex<()> = Mutex::new(());

fn appearance_path() -> Option<PathBuf> {
    crate::platform::state_dir()
        .ok()
        .map(|directory| directory.join("appearance.json"))
}

/// Reads this device's saved appearance; call before the first window paints.
pub(crate) fn load_appearance() {
    let saved = appearance_path()
        .and_then(|path| std::fs::read(path).ok())
        .map(|bytes| Appearance::decode(&bytes))
        .unwrap_or_default();
    APPEARANCE.with(|appearance| *appearance.borrow_mut() = saved);
}

pub(crate) fn appearance() -> Appearance {
    APPEARANCE.with(|appearance| appearance.borrow().clone())
}

/// Stores, saves and redraws with the preferences.
pub(crate) fn set_appearance(appearance: Appearance, cx: &mut App) {
    let appearance = appearance.normalized();
    let bytes = appearance.encode();
    APPEARANCE.with(|current| *current.borrow_mut() = appearance);
    let dark = SYSTEM_DARK.with(|dark| *dark.borrow());
    apply_theme(dark, cx);
    let generation = SAVE_GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    if let Some(path) = appearance_path() {
        cx.background_spawn(async move {
            let _guard = SAVE_LOCK.lock().unwrap_or_else(|error| error.into_inner());
            if SAVE_GENERATION.load(Ordering::SeqCst) != generation {
                return;
            }
            if let Err(error) = host_daemon::platform::save_private_bytes(&path, &bytes) {
                tracing::warn!(target: "desktop", error = %error, "Could not save the appearance");
            }
        })
        .detach();
    }
}

/// An `#rrggbb[aa]` palette value.
pub(crate) fn hex_color(value: &str) -> Hsla {
    let value = value.trim_start_matches('#');
    let hex = u32::from_str_radix(value, 16).unwrap_or(0xffffff);
    if value.len() == 8 {
        rgba(hex).into()
    } else {
        rgb(hex).into()
    }
}

/// The terminal's font family, size and line height.
pub(crate) fn terminal_font() -> (SharedString, f32, f32) {
    APPEARANCE.with(|appearance| {
        let appearance = appearance.borrow();
        let family = match appearance.terminal_font() {
            "" => "Menlo".to_owned(),
            family => family.to_owned(),
        };
        let size = appearance.terminal_font_size() as f32;
        (family.into(), size, (size * 4. / 3.).round())
    })
}

/// The prompt's font family, when it is not the interface font.
pub(crate) fn prompt_font() -> Option<SharedString> {
    APPEARANCE.with(|appearance| {
        let appearance = appearance.borrow();
        let family = appearance.prompt_font();
        (!family.is_empty()).then(|| SharedString::from(family.to_owned()))
    })
}

/// The diff colors of additions and of deletions.
pub(crate) fn diff_colors() -> (Hsla, Hsla) {
    match APPEARANCE.with(|appearance| appearance.borrow().diff_colors) {
        DiffColors::RedGreen => (color("successForeground"), color("errorForeground")),
        DiffColors::BlueOrange if is_dark() => (rgb(0x60a5fa).into(), rgb(0xfb923c).into()),
        DiffColors::BlueOrange => (rgb(0x2563eb).into(), rgb(0xea580c).into()),
    }
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
            .map_or("ffffff", String::as_str);
        hex_color(value)
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
            chat_max_width: match APPEARANCE.with(|appearance| appearance.borrow().chat_width) {
                ChatWidth::Comfortable => palette.chat_max_width,
                ChatWidth::Wide => 1152.,
                ChatWidth::Full => f32::MAX,
            },
            sidebar_width: palette.sidebar_width,
            panel_width: palette.panel_width,
            prompt_size: APPEARANCE.with(|appearance| appearance.borrow().prompt_size) as f32,
            code_size: APPEARANCE.with(|appearance| appearance.borrow().code_size) as f32,
        }
    })
}

/// Follows the system appearance unless the user chose light or dark.
pub(crate) fn apply_appearance(appearance: WindowAppearance, cx: &mut App) {
    let dark = matches!(
        appearance,
        WindowAppearance::Dark | WindowAppearance::VibrantDark
    );
    SYSTEM_DARK.with(|value| *value.borrow_mut() = dark);
    apply_theme(dark, cx);
}

fn apply_theme(system_dark: bool, cx: &mut App) {
    let preferences = appearance();
    let dark = preferences.resolved_dark(system_dark);
    DARK.with(|value| *value.borrow_mut() = dark);
    PALETTE.with(|palette| {
        *palette.borrow_mut() = preferences.palette(system_dark);
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
    let theme = Theme::global_mut(cx);
    // The interface font size is the rem, so every Tailwind size follows it.
    theme.font_size = px(preferences.interface_size as f32);
    theme.mono_font_size = px(preferences.code_size as f32);
    if !preferences.interface_font.is_empty() {
        theme.font_family = preferences.interface_font.clone().into();
    }
    if !preferences.code_font.is_empty() {
        theme.mono_font_family = preferences.code_font.clone().into();
    }
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

//! This device's appearance: the color scheme and themes, contrast, glass,
//! diff colors, composer context, chat width, panel motion and typography;
//! the palette they paint and the theme cards of the Appearance page.
use crate::presentation::{
    color::{Color, to_hex},
    theme::{Theme, theme},
    themes::{BUILT_IN_THEMES, built_in_theme},
};
use crate::view::terminals::text_size::{
    DEFAULT_TERMINAL_FONT_SIZE, MAX_TERMINAL_FONT_SIZE, MIN_TERMINAL_FONT_SIZE,
    normalize_terminal_font_size,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub const MIN_CONTRAST: u32 = 50;
pub const MAX_CONTRAST: u32 = 200;
pub const MIN_GLASS_OPACITY: u32 = 40;
pub const MAX_GLASS_OPACITY: u32 = 100;
pub const MIN_PANEL_ANIMATION_MS: u32 = 0;
pub const MAX_PANEL_ANIMATION_MS: u32 = 400;
pub const INTERFACE_FONT_SIZES: (u32, u32) = (12, 20);
pub const PROMPT_FONT_SIZES: (u32, u32) = (12, 20);
pub const CODE_FONT_SIZES: (u32, u32) = (10, 18);
pub const TERMINAL_FONT_SIZES: (u32, u32) = (8, 20);
/// The longest font family a field keeps.
pub const MAX_FONT_FAMILY_CHARS: usize = 200;
/// The stock palette's card in the theme library.
pub const STANDARD_THEME_LABEL: &str = "Bex";
const MATERIAL_YOU_THEME_ID: &str = "material-you";

/// The color scheme choices exposed by the mobile Appearance screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
#[serde(rename_all = "camelCase")]
pub enum MobileColorScheme {
    #[default]
    System,
    Light,
    Dark,
}

/// Appearance values that are meaningful on a phone or tablet. The native
/// clients persist this record locally and ask core to normalize and resolve
/// it, so their controls do not grow separate ranges or typography rules.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[serde(rename_all = "camelCase")]
pub struct MobileAppearance {
    pub color_scheme: MobileColorScheme,
    /// The theme used for both appearances; `None` is the stock palette.
    pub theme: Option<String>,
    pub light_theme: Option<String>,
    pub dark_theme: Option<String>,
    /// Body text size in points, inclusive 11 through 22.
    pub base_font_size: u32,
    /// `None` follows the base scale's code size.
    pub code_font_size: Option<u32>,
    /// `None` follows the terminal's platform default.
    pub terminal_font_size: Option<f64>,
    pub code_word_wrap: bool,
}

/// A theme name a mobile client can present for selection.
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct MobileThemeChoice {
    /// `None` selects the stock palette.
    pub id: Option<String>,
    pub label: String,
}

impl Default for MobileAppearance {
    fn default() -> Self {
        Self {
            color_scheme: MobileColorScheme::System,
            theme: None,
            light_theme: None,
            dark_theme: None,
            base_font_size: 16,
            code_font_size: None,
            terminal_font_size: None,
            code_word_wrap: false,
        }
    }
}

/// The resolved mobile sizes in points. Line heights use the same scale as
/// the mobile type ramp, including custom base text sizes.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct MobileTypography {
    pub base_font_size: f64,
    pub micro_font_size: f64,
    pub micro_line_height: f64,
    pub caption_font_size: f64,
    pub caption_line_height: f64,
    pub label_font_size: f64,
    pub label_line_height: f64,
    pub footnote_font_size: f64,
    pub footnote_line_height: f64,
    pub body_font_size: f64,
    pub body_line_height: f64,
    pub headline_font_size: f64,
    pub headline_line_height: f64,
    pub title_font_size: f64,
    pub title_line_height: f64,
    pub large_title_font_size: f64,
    pub large_title_line_height: f64,
    pub display_font_size: f64,
    pub display_line_height: f64,
    pub markdown_body_font_size: f64,
    pub markdown_body_line_height: f64,
    pub markdown_h1_font_size: f64,
    pub markdown_h2_font_size: f64,
    pub markdown_h3_font_size: f64,
    pub markdown_h4_font_size: f64,
    pub markdown_code_font_size: f64,
    pub markdown_code_line_height: f64,
    pub code_font_size: f64,
    pub code_line_number_font_size: f64,
    pub code_line_height: f64,
    pub terminal_font_size: f64,
}

pub const MOBILE_BASE_FONT_SIZES: (u32, u32) = (11, 22);
pub const MOBILE_DEFAULT_CODE_FONT_SIZE: f64 = 12.0;
pub const MOBILE_CODE_FONT_SIZES: (u32, u32) = (8, 18);

fn known_mobile_theme(id: Option<String>) -> Option<String> {
    id.filter(|id| id == MATERIAL_YOU_THEME_ID || built_in_theme(id).is_some())
}

/// Lists the mobile theme choices shared by all clients. Material You is
/// included only when the platform can provide it.
pub fn mobile_theme_choices(include_material_you: bool) -> Vec<MobileThemeChoice> {
    let mut choices =
        Vec::with_capacity(1 + BUILT_IN_THEMES.len() + usize::from(include_material_you));
    choices.push(MobileThemeChoice {
        id: None,
        label: STANDARD_THEME_LABEL.into(),
    });
    choices.extend(BUILT_IN_THEMES.iter().map(|theme| MobileThemeChoice {
        id: Some(theme.id.into()),
        label: theme.label.into(),
    }));
    if include_material_you {
        choices.push(MobileThemeChoice {
            id: Some(MATERIAL_YOU_THEME_ID.into()),
            label: "Material You".into(),
        });
    }
    choices
}

/// Pulls saved mobile values into the ranges the native controls expose.
pub fn normalize_mobile_appearance(mut appearance: MobileAppearance) -> MobileAppearance {
    appearance.theme = known_mobile_theme(appearance.theme);
    appearance.light_theme = known_mobile_theme(appearance.light_theme);
    appearance.dark_theme = known_mobile_theme(appearance.dark_theme);
    appearance.base_font_size = appearance
        .base_font_size
        .clamp(MOBILE_BASE_FONT_SIZES.0, MOBILE_BASE_FONT_SIZES.1);
    appearance.code_font_size = appearance
        .code_font_size
        .map(|size| size.clamp(MOBILE_CODE_FONT_SIZES.0, MOBILE_CODE_FONT_SIZES.1));
    appearance.terminal_font_size = appearance
        .terminal_font_size
        .map(|size| normalize_terminal_font_size(Some(size)));
    appearance
}

/// Selects one mobile appearance's theme while retaining the other side of a
/// shared theme. Selecting the stock palette for one side moves a shared
/// built-in theme to the other side so the `None` value remains an actual
/// independent stock choice.
pub fn mobile_assign_theme(
    mut appearance: MobileAppearance,
    dark: bool,
    theme_id: Option<String>,
) -> MobileAppearance {
    let theme_id = known_mobile_theme(theme_id);
    if theme_id.is_none() && appearance.theme.is_some() {
        let other = if dark {
            appearance
                .light_theme
                .clone()
                .or_else(|| appearance.theme.clone())
        } else {
            appearance
                .dark_theme
                .clone()
                .or_else(|| appearance.theme.clone())
        };
        appearance.theme = None;
        if dark {
            appearance.light_theme = other;
            appearance.dark_theme = None;
        } else {
            appearance.dark_theme = other;
            appearance.light_theme = None;
        }
    } else if dark {
        appearance.dark_theme = theme_id;
    } else {
        appearance.light_theme = theme_id;
    }
    normalize_mobile_appearance(appearance)
}

/// Resolves the shared mobile type ramp after the current appearance values.
pub fn mobile_typography(appearance: MobileAppearance) -> MobileTypography {
    let appearance = normalize_mobile_appearance(appearance);
    let scale = f64::from(appearance.base_font_size) / 16.0;
    let derived_terminal = ((DEFAULT_TERMINAL_FONT_SIZE * scale) * 2.0).round() / 2.0;
    let role = |font_size: f64, line_height: f64| {
        (
            (font_size * scale).round().max(8.0),
            (line_height * scale).round().max(10.0),
        )
    };
    let (micro_font_size, micro_line_height) = role(11.0, 14.0);
    let (caption_font_size, caption_line_height) = role(12.0, 16.0);
    let (label_font_size, label_line_height) = role(13.0, 17.0);
    let (footnote_font_size, footnote_line_height) = role(14.0, 19.0);
    let (body_font_size, body_line_height) = role(16.0, 23.0);
    let (headline_font_size, headline_line_height) = role(18.0, 23.0);
    let (title_font_size, title_line_height) = role(21.0, 28.0);
    let (large_title_font_size, large_title_line_height) = role(26.0, 32.0);
    let (display_font_size, display_line_height) = role(30.0, 36.0);
    let markdown_body_font_size = f64::from(appearance.base_font_size);
    let markdown_body_line_height = (23.0 * scale).round().max(18.0);
    let markdown_h1_font_size = (21.0 * scale).round().max(16.0);
    let markdown_h2_font_size = (19.0 * scale).round().max(14.0);
    let markdown_h3_font_size = (17.0 * scale).round().max(13.0);
    let markdown_h4_font_size = (15.0 * scale).round().max(12.0);
    let markdown_code_font_size = (13.0 * scale).round().max(10.0);
    let markdown_code_line_height = markdown_code_font_size + 6.0;
    let code_font_size = f64::from(appearance.code_font_size.unwrap_or_else(|| {
        (MOBILE_DEFAULT_CODE_FONT_SIZE * scale).round().clamp(
            MOBILE_CODE_FONT_SIZES.0 as f64,
            MOBILE_CODE_FONT_SIZES.1 as f64,
        ) as u32
    }));
    let code_line_number_font_size = (11.0 * code_font_size / MOBILE_DEFAULT_CODE_FONT_SIZE)
        .round()
        .max(8.0);
    MobileTypography {
        base_font_size: f64::from(appearance.base_font_size),
        micro_font_size,
        micro_line_height,
        caption_font_size,
        caption_line_height,
        label_font_size,
        label_line_height,
        footnote_font_size,
        footnote_line_height,
        body_font_size,
        body_line_height,
        headline_font_size,
        headline_line_height,
        title_font_size,
        title_line_height,
        large_title_font_size,
        large_title_line_height,
        display_font_size,
        display_line_height,
        markdown_body_font_size,
        markdown_body_line_height,
        markdown_h1_font_size,
        markdown_h2_font_size,
        markdown_h3_font_size,
        markdown_h4_font_size,
        markdown_code_font_size,
        markdown_code_line_height,
        code_font_size,
        code_line_number_font_size,
        code_line_height: (22.0 * code_font_size / MOBILE_DEFAULT_CODE_FONT_SIZE)
            .round()
            .max(14.0),
        terminal_font_size: appearance
            .terminal_font_size
            .unwrap_or(derived_terminal)
            .clamp(MIN_TERMINAL_FONT_SIZE, MAX_TERMINAL_FONT_SIZE),
    }
}

/// Returns the resolved hex palette for a stock or built-in mobile theme.
/// Android's Material You choice is supplied by the platform and therefore
/// intentionally falls back to the stock roles here.
pub fn mobile_theme_colors(theme_id: Option<&str>, dark: bool) -> HashMap<String, String> {
    let mut appearance = Appearance::default();
    if let Some(theme_id) = theme_id.filter(|id| *id != MATERIAL_YOU_THEME_ID) {
        appearance.use_theme(Some(theme_id));
    }
    appearance.palette(dark).colors
}

/// Light, dark, or following the system.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AppearanceMode {
    #[default]
    System,
    Light,
    Dark,
}

/// How wide messages and the composer grow on large screens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ChatWidth {
    #[default]
    Comfortable,
    Wide,
    Full,
}

/// The colors of additions and deletions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DiffColors {
    #[default]
    RedGreen,
    BlueOrange,
}

/// The appearance preferences this device keeps across launches and Hosts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Appearance {
    pub mode: AppearanceMode,
    /// The theme both appearances wear; `None` is the stock palette.
    pub theme: Option<String>,
    /// A theme worn only in the light appearance, over `theme`.
    pub light_theme: Option<String>,
    /// A theme worn only in the dark appearance, over `theme`.
    pub dark_theme: Option<String>,
    /// Percent, `MIN_CONTRAST` to `MAX_CONTRAST`.
    pub contrast: u32,
    /// Percent, `MIN_GLASS_OPACITY` to `MAX_GLASS_OPACITY`.
    pub glass_opacity: u32,
    pub diff_colors: DiffColors,
    /// Keeps the branch and worktree controls under the composer once a
    /// thread has started.
    pub composer_context: bool,
    pub chat_width: ChatWidth,
    /// How long panels take to open and close.
    pub panel_animation_ms: u32,
    /// Shows the prompt and terminal font rows; off, the terminal follows the
    /// code font.
    pub advanced_typography: bool,
    /// Empty for the platform default.
    pub interface_font: String,
    pub interface_size: u32,
    /// Empty to follow the interface font.
    pub prompt_font: String,
    pub prompt_size: u32,
    /// Empty for the platform monospace font.
    pub code_font: String,
    pub code_size: u32,
    /// Empty for the platform monospace font.
    pub terminal_font: String,
    pub terminal_size: u32,
    /// Wraps long lines in code blocks, tables, diffs and file previews.
    pub word_wrap: bool,
}

impl Default for Appearance {
    fn default() -> Self {
        Self {
            mode: AppearanceMode::System,
            theme: None,
            light_theme: None,
            dark_theme: None,
            contrast: 100,
            glass_opacity: 80,
            diff_colors: DiffColors::RedGreen,
            composer_context: false,
            chat_width: ChatWidth::Comfortable,
            panel_animation_ms: 0,
            advanced_typography: false,
            interface_font: String::new(),
            interface_size: 16,
            prompt_font: String::new(),
            prompt_size: 14,
            code_font: String::new(),
            code_size: 13,
            terminal_font: String::new(),
            terminal_size: 12,
            word_wrap: true,
        }
    }
}

fn clamp(value: u32, (min, max): (u32, u32)) -> u32 {
    value.clamp(min, max)
}

fn font(value: String) -> String {
    let value = value.trim();
    if value.chars().count() > MAX_FONT_FAMILY_CHARS {
        return String::new();
    }
    value.to_owned()
}

/// A theme id the library can paint, else `None`.
fn known(id: Option<String>) -> Option<String> {
    id.filter(|id| built_in_theme(id).is_some())
}

impl Appearance {
    /// Reads saved preferences; anything unreadable is the default, and
    /// values outside their bounds are pulled back in.
    pub fn decode(bytes: &[u8]) -> Self {
        serde_json::from_slice::<Self>(bytes)
            .map(Self::normalized)
            .unwrap_or_default()
    }

    pub fn encode(&self) -> Vec<u8> {
        serde_json::to_vec_pretty(self).unwrap_or_default()
    }

    /// Bounds every value and drops themes the library cannot paint.
    pub fn normalized(self) -> Self {
        Self {
            theme: known(self.theme),
            light_theme: known(self.light_theme),
            dark_theme: known(self.dark_theme),
            contrast: clamp(self.contrast, (MIN_CONTRAST, MAX_CONTRAST)),
            glass_opacity: clamp(self.glass_opacity, (MIN_GLASS_OPACITY, MAX_GLASS_OPACITY)),
            panel_animation_ms: clamp(
                self.panel_animation_ms,
                (MIN_PANEL_ANIMATION_MS, MAX_PANEL_ANIMATION_MS),
            ),
            interface_font: font(self.interface_font),
            interface_size: clamp(self.interface_size, INTERFACE_FONT_SIZES),
            prompt_font: font(self.prompt_font),
            prompt_size: clamp(self.prompt_size, PROMPT_FONT_SIZES),
            code_font: font(self.code_font),
            code_size: clamp(self.code_size, CODE_FONT_SIZES),
            terminal_font: font(self.terminal_font),
            terminal_size: clamp(self.terminal_size, TERMINAL_FONT_SIZES),
            ..self
        }
    }

    /// Wears `theme` in both appearances, replacing any per-appearance mix.
    pub fn use_theme(&mut self, theme: Option<&str>) {
        self.theme = known(theme.map(str::to_owned));
        self.light_theme = None;
        self.dark_theme = None;
    }

    fn half_mut(&mut self, dark: bool) -> &mut Option<String> {
        if dark {
            &mut self.dark_theme
        } else {
            &mut self.light_theme
        }
    }

    /// The theme painting one appearance: its own theme, else the shared one.
    pub fn theme_owner(&self, dark: bool) -> Option<&str> {
        if dark {
            &self.dark_theme
        } else {
            &self.light_theme
        }
        .as_deref()
        .or(self.theme.as_deref())
    }

    /// Wears `card` (`None` for the stock palette) in one appearance only.
    /// The stock palette cannot be a per-appearance theme over a themed
    /// base, so the base moves to the other appearance instead.
    pub fn assign_theme(&mut self, dark: bool, card: Option<&str>) {
        let card = known(card.map(str::to_owned));
        if card.is_none() && self.theme.is_some() {
            let other = self.theme_owner(!dark).map(str::to_owned);
            self.use_theme(None);
            *self.half_mut(!dark) = other;
            return;
        }
        *self.half_mut(dark) = card;
    }

    /// Whether the dark palette shows, given the system's appearance. Every
    /// theme has both appearances, so the mode alone decides.
    pub fn resolved_dark(&self, system_dark: bool) -> bool {
        match self.mode {
            AppearanceMode::System => system_dark,
            AppearanceMode::Light => false,
            AppearanceMode::Dark => true,
        }
    }

    /// The palette the app paints: the stock tokens, the theme's roles over
    /// them, then the contrast preference.
    pub fn palette(&self, system_dark: bool) -> Theme {
        let dark = self.resolved_dark(system_dark);
        let mut palette = theme(dark);
        if let Some(theme) = self.theme_owner(dark).and_then(built_in_theme) {
            for (role, value) in theme.colors(dark) {
                palette.colors.insert((*role).to_owned(), to_hex(value));
            }
        }
        apply_contrast(&mut palette, self.contrast, dark);
        palette
    }

    /// The font the terminal uses: its own in advanced typography, else the
    /// code font.
    pub fn terminal_font(&self) -> &str {
        if self.advanced_typography {
            &self.terminal_font
        } else {
            &self.code_font
        }
    }

    pub fn terminal_font_size(&self) -> u32 {
        if self.advanced_typography {
            self.terminal_size
        } else {
            self.code_size
        }
    }

    /// The font the prompt is written in: its own, else the interface font.
    pub fn prompt_font(&self) -> &str {
        if self.prompt_font.is_empty() {
            &self.interface_font
        } else {
            &self.prompt_font
        }
    }

    /// The theme library: the stock palette, then the built-in themes, with
    /// the appearances each one paints now.
    pub fn theme_cards(&self) -> Vec<ThemeCard> {
        let card = |id: Option<&str>, label: &str| ThemeCard {
            id: id.map(str::to_owned),
            label: label.to_owned(),
            previews: [false, true]
                .into_iter()
                .map(|dark| ThemePreview {
                    dark,
                    colors: preview_colors(id, dark),
                })
                .collect(),
            light_owner: self.theme_owner(false) == id,
            dark_owner: self.theme_owner(true) == id,
        };
        std::iter::once(card(None, STANDARD_THEME_LABEL))
            .chain(
                BUILT_IN_THEMES
                    .iter()
                    .map(|theme| card(Some(theme.id), theme.label)),
            )
            .collect()
    }

    /// The colors of the mode tiles' miniature app in one appearance.
    pub fn wireframe_colors(&self, dark: bool) -> ThemePreviewColors {
        preview_colors(self.theme_owner(dark), dark)
    }
}

/// The roles a theme card previews.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThemePreviewColors {
    pub sidebar: String,
    pub canvas: String,
    pub surface: String,
    pub accent_surface: String,
    pub accent: String,
    pub message_surface: String,
    pub message_action: String,
}

impl ThemePreviewColors {
    /// The preview ball's base: the canvas pulled toward white (light) or
    /// near-black (dark).
    pub fn ball_base(&self, dark: bool) -> String {
        let spec = preview_render_spec(dark);
        match (Color::parse(&self.canvas), Color::parse(spec.base_target)) {
            (Some(canvas), Some(target)) => canvas.mix_oklab(target, spec.base_weight).hex(),
            _ => self.canvas.clone(),
        }
    }
}

/// One appearance of a theme card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThemePreview {
    pub dark: bool,
    pub colors: ThemePreviewColors,
}

/// A card of the theme library.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThemeCard {
    /// `None` for the stock palette.
    pub id: Option<String>,
    pub label: String,
    pub previews: Vec<ThemePreview>,
    /// This card paints the light appearance now.
    pub light_owner: bool,
    /// This card paints the dark appearance now.
    pub dark_owner: bool,
}

fn preview_colors(id: Option<&str>, dark: bool) -> ThemePreviewColors {
    let Some(theme) = id.and_then(built_in_theme) else {
        // The stock card's artwork, not the stock palette.
        let colors = if dark {
            [
                "#0f0f10", "#0a0a0a", "#121212", "#27272a", "#1c1c1f", "#27272a", "#8b9cff",
            ]
        } else {
            [
                "#fafafa", "#fcfcfc", "#ffffff", "#f4f4f5", "#f4f4f5", "#e4e4e7", "#4f46e5",
            ]
        }
        .map(str::to_owned);
        let [
            sidebar,
            canvas,
            surface,
            accent_surface,
            accent,
            message_surface,
            message_action,
        ] = colors;
        return ThemePreviewColors {
            sidebar,
            canvas,
            surface,
            accent_surface,
            accent,
            message_surface,
            message_action,
        };
    };
    let role = |name: &str| theme.color(dark, name).map(to_hex).unwrap_or_default();
    ThemePreviewColors {
        sidebar: role("sidebar"),
        canvas: role("canvas"),
        surface: role("surface"),
        accent_surface: role("accentSurface"),
        accent: role("accent"),
        message_surface: role("messageSurface"),
        message_action: role("messageAction"),
    }
}

/// Where a preview ball's accent glow and action tint sit, as fractions of
/// its size.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PreviewRenderSpec {
    pub base_target: &'static str,
    pub base_weight: f64,
    pub accent_center: (f64, f64),
    pub accent_middle_offset: f64,
    pub accent_middle_opacity: f64,
    pub accent_end_offset: f64,
    pub action_center: (f64, f64),
    pub action_start_opacity: f64,
    pub action_end_offset: f64,
}

pub fn preview_render_spec(dark: bool) -> PreviewRenderSpec {
    if dark {
        PreviewRenderSpec {
            base_target: "#09090b",
            base_weight: 0.8,
            accent_center: (0.28, 0.78),
            accent_middle_offset: 0.28,
            accent_middle_opacity: 0.62,
            accent_end_offset: 0.58,
            action_center: (0.82, 0.18),
            action_start_opacity: 0.45,
            action_end_offset: 0.55,
        }
    } else {
        PreviewRenderSpec {
            base_target: "#ffffff",
            base_weight: 0.8,
            accent_center: (0.72, 0.22),
            accent_middle_offset: 0.28,
            accent_middle_opacity: 0.72,
            accent_end_offset: 0.58,
            action_center: (0.18, 0.82),
            action_start_opacity: 0.45,
            action_end_offset: 0.55,
        }
    }
}

/// How the contrast preference reshapes colors, in percent: text keeps
/// `base` of itself over its surface, then moves `boost` toward black (light)
/// or white (dark); borders keep `base` opacity, then move `border_boost`
/// toward their text.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ContrastMix {
    pub base: f64,
    pub boost: f64,
    pub border_boost: f64,
}

pub fn contrast_mix(contrast: u32) -> ContrastMix {
    let contrast = f64::from(contrast);
    ContrastMix {
        base: contrast.min(100.),
        boost: (contrast - 100.).max(0.),
        border_boost: (contrast - 100.).max(0.) / 4.,
    }
}

/// Text roles and the surface each is read over.
const CONTRAST_TEXT: &[(&str, &str)] = &[
    ("text", "canvas"),
    ("textMuted", "canvas"),
    ("mutedForeground", "canvas"),
    ("placeholder", "canvas"),
    ("secondaryLabel", "canvas"),
    ("iconMuted", "canvas"),
    ("toolbarForeground", "toolbar"),
    ("toolbarControlForeground", "toolbarControl"),
    ("accentSurfaceForeground", "accentSurface"),
    ("secondaryForeground", "secondary"),
    ("messageForeground", "messageSurface"),
    ("sidebarForeground", "sidebar"),
    ("sidebarMutedForeground", "sidebar"),
];

/// Border roles and the text each moves toward.
const CONTRAST_BORDERS: &[(&str, &str)] = &[
    ("border", "text"),
    ("input", "text"),
    ("toolbarBorder", "toolbarForeground"),
    ("sidebarBorder", "sidebarForeground"),
];

fn apply_contrast(palette: &mut Theme, contrast: u32, dark: bool) {
    let mix = contrast_mix(contrast);
    if mix.base >= 100. && mix.boost <= 0. {
        return;
    }
    let read = |palette: &Theme, role: &str| palette.colors.get(role).and_then(|v| Color::parse(v));
    let target =
        Color::parse(if dark { "#ffffff" } else { "#000000" }).unwrap_or(Color::TRANSPARENT);
    let original = palette.clone();
    for (role, surface) in CONTRAST_TEXT {
        if let (Some(color), Some(surface)) = (read(&original, role), read(&original, surface)) {
            let value = color
                .mix_oklab(surface, mix.base / 100.)
                .mix_oklab(target, 1. - mix.boost / 100.);
            palette.colors.insert((*role).to_owned(), value.hex());
        }
    }
    for (role, toward) in CONTRAST_BORDERS {
        if let (Some(color), Some(toward)) = (read(&original, role), read(&original, toward)) {
            let value = color
                .mix_srgb(Color::TRANSPARENT, mix.base / 100.)
                .mix_srgb(toward, 1. - mix.border_boost / 100.);
            palette.colors.insert((*role).to_owned(), value.hex());
        }
    }
}

#[cfg(test)]
mod tests;

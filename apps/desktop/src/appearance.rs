use gpui_kit::{
    App,
    component::{Theme, ThemeColor, ThemeMode},
    px, rgb,
};

pub(crate) const BACKGROUND: u32 = 0x080808;
pub(crate) const SIDEBAR: u32 = 0x000000;
pub(crate) const SURFACE: u32 = 0x0f0f0f;
pub(crate) const RAISED: u32 = 0x141414;
pub(crate) const HOVER: u32 = 0x202020;
pub(crate) const SELECTED: u32 = 0x282828;
pub(crate) const BORDER: u32 = 0x303030;
pub(crate) const FOREGROUND: u32 = 0xe6e6e6;
pub(crate) const MUTED: u32 = 0xa6a6a6;
pub(crate) const ACCENT: u32 = 0x346bf1;

pub(crate) fn init(cx: &mut App) {
    gpui_kit::init(cx);
    Theme::change(ThemeMode::Dark, None, cx);
    let theme = Theme::global_mut(cx);
    theme.colors = ThemeColor {
        background: rgb(BACKGROUND).into(),
        foreground: rgb(FOREGROUND).into(),
        border: rgb(BORDER).into(),
        input: rgb(0x666666).into(),
        caret: rgb(FOREGROUND).into(),
        accent: rgb(SELECTED).into(),
        accent_foreground: rgb(FOREGROUND).into(),
        muted: rgb(RAISED).into(),
        muted_foreground: rgb(MUTED).into(),
        secondary: rgb(RAISED).into(),
        secondary_hover: rgb(HOVER).into(),
        secondary_active: rgb(SELECTED).into(),
        secondary_foreground: rgb(FOREGROUND).into(),
        primary: rgb(ACCENT).into(),
        primary_hover: rgb(0x477af5).into(),
        primary_active: rgb(0x285bd5).into(),
        primary_foreground: rgb(FOREGROUND).into(),
        button_primary: rgb(ACCENT).into(),
        button_primary_hover: rgb(0x477af5).into(),
        button_primary_active: rgb(0x285bd5).into(),
        button_primary_foreground: rgb(FOREGROUND).into(),
        button_secondary: rgb(RAISED).into(),
        button_secondary_hover: rgb(HOVER).into(),
        button_secondary_active: rgb(SELECTED).into(),
        button_secondary_foreground: rgb(FOREGROUND).into(),
        ring: rgb(ACCENT).into(),
        selection: rgb(ACCENT).opacity(0.25).into(),
        button: rgb(RAISED).into(),
        button_hover: rgb(HOVER).into(),
        button_active: rgb(SELECTED).into(),
        button_foreground: rgb(FOREGROUND).into(),
        sidebar: rgb(SIDEBAR).into(),
        sidebar_foreground: rgb(MUTED).into(),
        sidebar_accent: rgb(SELECTED).into(),
        sidebar_accent_foreground: rgb(FOREGROUND).into(),
        sidebar_border: rgb(BORDER).into(),
        sidebar_primary: rgb(SELECTED).into(),
        sidebar_primary_foreground: rgb(FOREGROUND).into(),
        popover: rgb(SURFACE).into(),
        popover_foreground: rgb(FOREGROUND).into(),
        list: rgb(SURFACE).into(),
        list_hover: rgb(HOVER).into(),
        list_active: rgb(SELECTED).into(),
        list_active_border: rgb(BORDER).into(),
        list_even: rgb(RAISED).into(),
        list_head: rgb(SURFACE).into(),
        tab: rgb(BACKGROUND).into(),
        tab_bar: rgb(BACKGROUND).into(),
        tab_bar_segmented: rgb(SURFACE).into(),
        tab_active: rgb(SELECTED).into(),
        tab_active_foreground: rgb(FOREGROUND).into(),
        tab_foreground: rgb(MUTED).into(),
        table: rgb(BACKGROUND).into(),
        table_even: rgb(SURFACE).into(),
        table_head: rgb(RAISED).into(),
        table_hover: rgb(HOVER).into(),
        table_active: rgb(SELECTED).into(),
        table_active_border: rgb(BORDER).into(),
        table_head_foreground: rgb(MUTED).into(),
        table_foot: rgb(SURFACE).into(),
        table_foot_foreground: rgb(MUTED).into(),
        table_row_border: rgb(BORDER).into(),
        title_bar: rgb(BACKGROUND).into(),
        title_bar_border: rgb(BORDER).into(),
        window_border: rgb(BORDER).into(),
        accordion: rgb(SURFACE).into(),
        group_box: rgb(SURFACE).into(),
        group_box_foreground: rgb(MUTED).into(),
        skeleton: rgb(RAISED).into(),
        scrollbar_thumb: rgb(0x333333).into(),
        scrollbar_thumb_hover: rgb(0x454545).into(),
        ..theme.colors
    };
    // Components and Base both keep resolved copies of the palette.
    theme.tokens = theme.colors.into();
    theme.radius = px(8.);
    Theme::sync_base(cx);
}

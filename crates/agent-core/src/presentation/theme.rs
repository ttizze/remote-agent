//! Native color roles from T3 Code's default desktop and mobile palettes.
//! Reference: 4ee6bfd50ef4a089440d5c3662db2298da9cc50e.
//! Copyright (c) 2026 T3 Tools Inc. MIT; see third-party/T3-Code-LICENSE.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum ThemePlatform {
    Desktop,
    Ios,
    Android,
}

/// Opaque sRGB roles; each UI converts these values to its native color type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct NativePalette {
    pub background: u32,
    pub foreground: u32,
    pub sidebar: u32,
    pub surface: u32,
    pub muted: u32,
    pub primary: u32,
    pub border: u32,
    pub user_bubble: u32,
    pub selected: u32,
    pub composer: u32,
    pub error: u32,
}

#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn native_palette(platform: ThemePlatform, dark: bool) -> NativePalette {
    let mobile = platform != ThemePlatform::Desktop;
    if dark {
        NativePalette {
            background: 0x0a0a0a,
            foreground: 0xf5f5f5,
            sidebar: if mobile { 0x000000 } else { 0x111111 },
            surface: 0x111111,
            muted: 0x838383,
            primary: 0x346bf1,
            border: 0x191919,
            user_bubble: if mobile { 0x161616 } else { 0x141414 },
            selected: if mobile { 0x1a1b1b } else { 0x141414 },
            composer: if mobile { 0x181919 } else { 0x111111 },
            error: 0xff6467,
        }
    } else {
        NativePalette {
            background: 0xfcfcfc,
            foreground: 0x27272a,
            sidebar: if platform == ThemePlatform::Ios {
                0xf4f4f5
            } else {
                0xfafafa
            },
            surface: 0xffffff,
            muted: if mobile { 0x6f6f79 } else { 0x71717b },
            primary: 0x1b4ed8,
            border: 0xe4e4e7,
            user_bubble: if mobile { 0xefeff1 } else { 0xf4f4f5 },
            selected: 0xffffff,
            composer: if mobile { 0xf4f4f5 } else { 0xffffff },
            error: 0xc10007,
        }
    }
}

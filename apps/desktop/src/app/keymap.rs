//! Key presses as agent-core's keymap reads them.
use agent_core::view::keybindings::KeyPress;
use gpui_kit::Keystroke;

/// Whether `mod` is ⌘ here.
pub(crate) const MAC: bool = cfg!(target_os = "macos");

/// A keystroke as a key press: arrows as `arrowup`, ⌘ as meta.
pub(crate) fn key_press(keystroke: &Keystroke) -> KeyPress {
    let modifiers = keystroke.modifiers;
    let key = match keystroke.key.as_str() {
        "up" => "arrowup",
        "down" => "arrowdown",
        "left" => "arrowleft",
        "right" => "arrowright",
        other => other,
    };
    KeyPress {
        key: key.to_lowercase(),
        meta: modifiers.platform,
        ctrl: modifiers.control,
        shift: modifiers.shift,
        alt: modifiers.alt,
    }
}

/// A modifier key alone, which never makes a shortcut.
pub(crate) fn is_modifier_only(keystroke: &Keystroke) -> bool {
    matches!(
        keystroke.key.as_str(),
        "shift" | "control" | "alt" | "platform" | "function" | "capslock"
    )
}

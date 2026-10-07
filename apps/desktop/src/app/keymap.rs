//! Keyboard shortcuts: each command's default bindings, the user's own,
//! and which command a keystroke runs where focus is.
use gpui_kit::Keystroke;

/// The default bindings: key, command and the context they apply in.
const DEFAULTS: &[(&str, &str, Option<&str>)] = &[
    ("mod+b", "sidebar.toggle", None),
    ("mod+j", "terminal.toggle", None),
    ("mod+alt+b", "rightPanel.toggle", None),
    ("mod+d", "terminal.split", Some("terminalFocus")),
    (
        "mod+shift+d",
        "terminal.splitVertical",
        Some("terminalFocus"),
    ),
    ("mod+n", "terminal.new", Some("terminalFocus")),
    ("mod+w", "terminal.close", Some("terminalFocus")),
    ("mod+w", "rightPanel.close", Some("!terminalFocus")),
    ("mod+d", "diff.toggle", Some("!terminalFocus")),
    ("mod+s", "composer.stash", Some("!terminalFocus")),
    (
        "mod+shift+enter",
        "thread.steerQueuedMessage",
        Some("!terminalFocus"),
    ),
    (
        "alt+arrowup",
        "thread.editQueuedMessage",
        Some("composerFocus"),
    ),
    ("mod+n", "chat.new", Some("!terminalFocus")),
    ("mod+shift+o", "chat.new", Some("!terminalFocus")),
    ("mod+shift+m", "modelPicker.toggle", Some("!terminalFocus")),
    ("mod+shift+[", "thread.previous", Some("!terminalFocus")),
    ("mod+shift+]", "thread.next", Some("!terminalFocus")),
    ("mod+shift+s", "thread.settle", Some("!terminalFocus")),
    ("mod+shift+p", "thread.pin", Some("!terminalFocus")),
    (
        "mod+z",
        "thread.undo",
        Some("!terminalFocus && !editableFocus"),
    ),
    ("mod+1", "thread.jump.1", Some("isDesktop")),
    ("mod+2", "thread.jump.2", Some("isDesktop")),
    ("mod+3", "thread.jump.3", Some("isDesktop")),
    ("mod+4", "thread.jump.4", Some("isDesktop")),
    ("mod+5", "thread.jump.5", Some("isDesktop")),
    ("mod+6", "thread.jump.6", Some("isDesktop")),
    ("mod+7", "thread.jump.7", Some("isDesktop")),
    ("mod+8", "thread.jump.8", Some("isDesktop")),
    ("mod+9", "thread.jump.9", Some("isDesktop")),
];

/// The most bindings the user can have.
pub(crate) const MAX_BINDINGS: usize = 256;

/// Where focus is when a key is pressed.
#[derive(Clone, Copy, Default)]
pub(crate) struct KeyContext {
    pub(crate) terminal_focus: bool,
    pub(crate) terminal_open: bool,
    pub(crate) composer_focus: bool,
    /// A text field has focus, so it handles its own undo.
    pub(crate) editable_focus: bool,
}
impl KeyContext {
    fn value(&self, name: &str) -> bool {
        match name {
            "terminalFocus" => self.terminal_focus,
            "terminalOpen" => self.terminal_open,
            "composerFocus" => self.composer_focus,
            "editableFocus" => self.editable_focus,
            "isDesktop" | "true" => true,
            _ => false,
        }
    }
}

/// Whether a when-clause (`a && !b || c`) holds; an empty one always does.
pub(crate) fn when_matches(when: &str, context: &KeyContext) -> bool {
    let when = when.trim();
    if when.is_empty() {
        return true;
    }
    when.split("||").any(|any| {
        any.split("&&").all(|term| {
            let term = term.trim();
            match term.strip_prefix('!') {
                Some(name) => !context.value(name.trim()),
                None => context.value(term),
            }
        })
    })
}

/// The canonical form of a keystroke: `mod+shift+k`, with ⌘ written as
/// `mod` on macOS and Ctrl as `mod` elsewhere.
pub(crate) fn keystroke_key(keystroke: &Keystroke) -> String {
    let modifiers = keystroke.modifiers;
    let mut parts: Vec<&str> = vec![];
    if modifiers.secondary() {
        parts.push("mod");
    }
    if cfg!(target_os = "macos") && modifiers.control {
        parts.push("ctrl");
    }
    if modifiers.alt {
        parts.push("alt");
    }
    if modifiers.shift {
        parts.push("shift");
    }
    let key = match keystroke.key.as_str() {
        "up" => "arrowup",
        "down" => "arrowdown",
        "left" => "arrowleft",
        "right" => "arrowright",
        other => other,
    };
    let mut joined = parts.join("+");
    if !joined.is_empty() {
        joined.push('+');
    }
    joined.push_str(&key.to_lowercase());
    joined
}

/// A modifier key alone, which never makes a shortcut.
pub(crate) fn is_modifier_only(keystroke: &Keystroke) -> bool {
    matches!(
        keystroke.key.as_str(),
        "shift" | "control" | "alt" | "platform" | "function" | "capslock"
    )
}

/// The keys of a binding as the key caps show them: `⌘`, `⇧`, `⌥`, `⌃` on
/// macOS, else `Ctrl`, `Shift`, `Alt`.
pub(crate) fn key_caps(key: &str) -> Vec<String> {
    let mac = cfg!(target_os = "macos");
    key.split('+')
        .filter(|part| !part.is_empty())
        .map(|part| match part {
            "mod" if mac => "⌘".into(),
            "mod" | "ctrl" if !mac => "Ctrl".into(),
            "ctrl" => "⌃".into(),
            "shift" if mac => "⇧".into(),
            "shift" => "Shift".into(),
            "alt" if mac => "⌥".into(),
            "alt" => "Alt".into(),
            "arrowup" => "↑".into(),
            "arrowdown" => "↓".into(),
            "arrowleft" => "←".into(),
            "arrowright" => "→".into(),
            "enter" => "↵".into(),
            other if other.chars().count() == 1 => other.to_uppercase(),
            other => {
                let mut chars = other.chars();
                chars
                    .next()
                    .map(|first| first.to_uppercase().chain(chars).collect())
                    .unwrap_or_default()
            }
        })
        .collect()
}

/// A command id as its title: `terminal.splitVertical` is
/// "Terminal: Split Vertical".
pub(crate) fn command_label(command: &str) -> String {
    match command {
        "thread.steerQueuedMessage" => return "Queue: Send First Queued Message as Steer".into(),
        "thread.editQueuedMessage" => return "Queue: Edit Last Queued Message".into(),
        _ => {}
    }
    command
        .split('.')
        .map(|segment| {
            let mut words: Vec<String> = vec![];
            let mut word = String::new();
            for character in segment.chars() {
                if character.is_uppercase() && !word.is_empty() {
                    words.push(std::mem::take(&mut word));
                }
                word.push(character);
            }
            words.push(word);
            words
                .iter()
                .map(|word| {
                    let mut chars = word.chars();
                    chars
                        .next()
                        .map(|first| first.to_uppercase().chain(chars).collect::<String>())
                        .unwrap_or_default()
                })
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect::<Vec<_>>()
        .join(": ")
}

/// Where a binding comes from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Source {
    Default,
    Custom,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct Binding {
    pub(crate) key: String,
    pub(crate) command: String,
    pub(crate) when: Option<String>,
    pub(crate) source: Source,
}

/// The user's bindings over the defaults: a command the user bound keeps
/// only the user's bindings.
#[derive(Clone, Default)]
pub(crate) struct Keymap {
    custom: Vec<Binding>,
}
impl Keymap {
    /// Every command the defaults bind, in order.
    pub(crate) fn commands() -> Vec<&'static str> {
        let mut commands: Vec<&str> = DEFAULTS.iter().map(|(_, command, _)| *command).collect();
        commands.sort_unstable();
        commands.dedup();
        commands
    }

    pub(crate) fn has_default(command: &str) -> bool {
        DEFAULTS.iter().any(|(_, default, _)| *default == command)
    }

    /// The bindings in effect, by command and then key.
    pub(crate) fn bindings(&self) -> Vec<Binding> {
        let mut bindings: Vec<Binding> = DEFAULTS
            .iter()
            .filter(|(_, command, _)| !self.custom.iter().any(|custom| custom.command == *command))
            .map(|(key, command, when)| Binding {
                key: (*key).into(),
                command: (*command).into(),
                when: when.map(Into::into),
                source: Source::Default,
            })
            .chain(self.custom.iter().cloned())
            .collect();
        bindings.sort_by(|a, b| (&a.command, &a.key).cmp(&(&b.command, &b.key)));
        bindings
    }

    /// The command `key` runs where focus is.
    pub(crate) fn resolve(&self, key: &str, context: &KeyContext) -> Option<String> {
        self.bindings()
            .into_iter()
            .find(|binding| {
                binding.key == key && when_matches(binding.when.as_deref().unwrap_or(""), context)
            })
            .map(|binding| binding.command)
    }

    /// The shortcut that runs `command`, as menus and hints write it: `⇧⌘Z`
    /// on macOS, `Ctrl+Shift+Z` elsewhere.
    pub(crate) fn shortcut_label(&self, command: &str) -> Option<String> {
        let binding = self
            .bindings()
            .into_iter()
            .find(|binding| binding.command == command)?;
        let parts: Vec<&str> = binding.key.split('+').collect();
        let (key, modifiers) = parts.split_last()?;
        let has = |name: &str| modifiers.contains(&name);
        let mac = cfg!(target_os = "macos");
        let key = key_caps(key).concat();
        if mac {
            return Some(format!(
                "{}{}{}{}{key}",
                if has("ctrl") { "⌃" } else { "" },
                if has("alt") { "⌥" } else { "" },
                if has("shift") { "⇧" } else { "" },
                if has("mod") { "⌘" } else { "" },
            ));
        }
        let mut words: Vec<String> = vec![];
        if has("mod") || has("ctrl") {
            words.push("Ctrl".into());
        }
        if has("alt") {
            words.push("Alt".into());
        }
        if has("shift") {
            words.push("Shift".into());
        }
        words.push(key);
        Some(words.join("+"))
    }

    /// The commands besides `command` that `key` also runs.
    pub(crate) fn conflicts(&self, key: &str, command: &str) -> Vec<String> {
        self.bindings()
            .into_iter()
            .filter(|binding| binding.key == key && binding.command != command)
            .map(|binding| binding.command)
            .collect()
    }

    /// Binds `key` to `command`, replacing `replace` when it is given.
    pub(crate) fn upsert(
        &mut self,
        binding: Binding,
        replace: Option<&Binding>,
    ) -> Result<(), String> {
        if binding.key.is_empty() {
            return Err("Press a shortcut first.".into());
        }
        if let Some(replace) = replace {
            self.remove(replace);
        }
        if self.bindings().len() >= MAX_BINDINGS {
            return Err(format!("You can have at most {MAX_BINDINGS} keybindings."));
        }
        self.custom
            .retain(|custom| !(custom.command == binding.command && custom.key == binding.key));
        self.custom.push(Binding {
            source: Source::Custom,
            ..binding
        });
        Ok(())
    }

    /// Removes one of the user's bindings.
    pub(crate) fn remove(&mut self, binding: &Binding) {
        self.custom.retain(|custom| custom != binding);
    }

    /// Drops the user's bindings of `command`, restoring its defaults.
    pub(crate) fn reset(&mut self, command: &str) {
        self.custom.retain(|custom| custom.command != command);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    #[test]
    fn when_clauses_combine_negation_and_both_operators() {
        let terminal = KeyContext {
            terminal_focus: true,
            ..KeyContext::default()
        };
        assert!(when_matches("", &terminal));
        assert!(when_matches("terminalFocus", &terminal));
        assert!(!when_matches("!terminalFocus", &terminal));
        assert!(when_matches("composerFocus || terminalFocus", &terminal));
        assert!(!when_matches("composerFocus && terminalFocus", &terminal));
        assert!(when_matches("isDesktop && !composerFocus", &terminal));
    }

    #[test]
    fn a_key_runs_the_command_bound_for_where_focus_is() {
        let keymap = Keymap::default();
        let terminal = KeyContext {
            terminal_focus: true,
            ..KeyContext::default()
        };
        assert_eq!(
            keymap.resolve("mod+d", &terminal).as_deref(),
            Some("terminal.split")
        );
        assert_eq!(
            keymap.resolve("mod+d", &KeyContext::default()).as_deref(),
            Some("diff.toggle")
        );
        assert_eq!(keymap.resolve("mod+k", &KeyContext::default()), None);
    }

    #[test]
    fn a_custom_binding_replaces_every_default_of_its_command() {
        let mut keymap = Keymap::default();
        keymap
            .upsert(
                Binding {
                    key: "mod+shift+t".into(),
                    command: "chat.new".into(),
                    when: None,
                    source: Source::Custom,
                },
                None,
            )
            .unwrap();
        let chat: Vec<String> = keymap
            .bindings()
            .into_iter()
            .filter(|binding| binding.command == "chat.new")
            .map(|binding| binding.key)
            .collect();
        assert_eq!(chat, ["mod+shift+t"]);
        assert_eq!(keymap.resolve("mod+n", &KeyContext::default()), None);
        keymap.reset("chat.new");
        assert_eq!(
            keymap.resolve("mod+n", &KeyContext::default()).as_deref(),
            Some("chat.new")
        );
    }

    #[test]
    fn command_ids_read_as_titles() {
        assert_eq!(
            command_label("terminal.splitVertical"),
            "Terminal: Split Vertical"
        );
        assert_eq!(command_label("sidebar.toggle"), "Sidebar: Toggle");
        assert_eq!(command_label("thread.jump.3"), "Thread: Jump: 3");
        assert_eq!(
            command_label("thread.editQueuedMessage"),
            "Queue: Edit Last Queued Message"
        );
    }

    // web keybindings.test.ts "thread undo shortcut"
    #[test]
    fn undo_runs_only_with_nothing_editable_focused() {
        let keymap = Keymap::default();
        assert_eq!(
            keymap.resolve("mod+z", &KeyContext::default()).as_deref(),
            Some("thread.undo")
        );
        for context in [
            KeyContext {
                editable_focus: true,
                ..KeyContext::default()
            },
            KeyContext {
                terminal_focus: true,
                ..KeyContext::default()
            },
        ] {
            assert_eq!(keymap.resolve("mod+z", &context), None);
        }
        let label = keymap.shortcut_label("thread.undo").unwrap();
        if cfg!(target_os = "macos") {
            assert_eq!(label, "⌘Z");
        } else {
            assert_eq!(label, "Ctrl+Z");
        }
    }

    #[test]
    fn key_caps_follow_the_platform() {
        let caps = key_caps("mod+shift+arrowup");
        if cfg!(target_os = "macos") {
            assert_eq!(caps, ["⌘", "⇧", "↑"]);
        } else {
            assert_eq!(caps, ["Ctrl", "Shift", "↑"]);
        }
    }
}

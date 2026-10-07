//! Keyboard shortcuts: the command a key press runs where focus is, the
//! label of a command's shortcut, and the rows of the Keybindings page. The
//! rules come from the Host's keybindings stream, else the defaults.
use crate::state::Snapshot;
use crate::view::collation::locale_compare;
use agent_protocol::keybindings::{
    CompiledRule, KEYBINDING_COMMANDS, KeybindingRule, MODEL_PICKER_JUMP_COMMANDS, Shortcut,
    THREAD_JUMP_COMMANDS, When, compile_rule, default_keybindings, parse_when,
};

/// A key press: the key as typed (`k`, `arrowup`, `enter`, `[`) and the
/// modifiers held. On macOS `meta` is ⌘.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyPress {
    pub key: String,
    pub meta: bool,
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
}

/// Where focus is and what is open, as `when` clauses name them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct KeyContext {
    pub terminal_focus: bool,
    pub terminal_open: bool,
    pub composer_focus: bool,
    pub model_picker_open: bool,
    /// A text field has focus, so it handles its own undo.
    pub editable_focus: bool,
}

impl KeyContext {
    fn value(&self, name: &str) -> bool {
        match name {
            "terminalFocus" => self.terminal_focus,
            "terminalOpen" => self.terminal_open,
            "composerFocus" => self.composer_focus,
            "modelPickerOpen" => self.model_picker_open,
            "editableFocus" => self.editable_focus,
            "isDesktop" => true,
            _ => false,
        }
    }

    fn matches(&self, when: Option<&When>) -> bool {
        when.is_none_or(|when| when.evaluate(&|name| self.value(name)))
    }
}

/// Which modifier `mod` is.
fn modifiers(shortcut: &Shortcut, mac: bool) -> (bool, bool) {
    (
        shortcut.meta || (shortcut.mod_key && mac),
        shortcut.ctrl || (shortcut.mod_key && !mac),
    )
}

fn matches_modifiers(press: &KeyPress, shortcut: &Shortcut, mac: bool) -> bool {
    let (meta, ctrl) = modifiers(shortcut, mac);
    press.meta == meta
        && press.ctrl == ctrl
        && press.shift == shortcut.shift
        && press.alt == shortcut.alt
}

/// The keys a shortcut can be compared by: the same press with any `mod`.
fn conflict_key(shortcut: &Shortcut, mac: bool) -> (String, bool, bool, bool, bool) {
    let (meta, ctrl) = modifiers(shortcut, mac);
    (
        shortcut.key.clone(),
        meta,
        ctrl,
        shortcut.shift,
        shortcut.alt,
    )
}

/// The rules in effect, ready to match a key press.
#[derive(Debug, Clone)]
pub struct Keymap {
    rules: Vec<CompiledRule>,
    mac: bool,
}

impl Keymap {
    pub fn new(rules: &[KeybindingRule], mac: bool) -> Self {
        Self {
            rules: rules
                .iter()
                .filter_map(|rule| compile_rule(rule).ok())
                .collect(),
            mac,
        }
    }

    /// The command the press runs; a later rule wins over an earlier one.
    pub fn resolve(&self, press: &KeyPress, context: &KeyContext) -> Option<String> {
        let key = press.key.to_lowercase();
        self.rules
            .iter()
            .rev()
            .find(|rule| {
                context.matches(rule.when.as_ref())
                    && matches_modifiers(press, &rule.shortcut, self.mac)
                    && rule.shortcut.key == key
            })
            .map(|rule| rule.command.clone())
    }

    /// The shortcut that runs `command` here, unless a later rule claims it.
    pub fn shortcut_for(&self, command: &str, context: &KeyContext) -> Option<&Shortcut> {
        let mut claimed = vec![];
        for rule in self.rules.iter().rev() {
            if !context.matches(rule.when.as_ref()) {
                continue;
            }
            let key = conflict_key(&rule.shortcut, self.mac);
            if claimed.contains(&key) {
                continue;
            }
            claimed.push(key);
            if rule.command == command {
                return Some(&rule.shortcut);
            }
        }
        None
    }

    /// `⌘⇧M` on macOS, `Ctrl+Shift+M` elsewhere.
    pub fn shortcut_label(&self, command: &str, context: &KeyContext) -> Option<String> {
        self.shortcut_for(command, context)
            .map(|shortcut| shortcut_label(shortcut, self.mac))
    }

    /// The model picker's jump hint for its `index`th selectable model.
    pub fn model_jump_label(&self, index: usize, context: &KeyContext) -> Option<String> {
        let command = MODEL_PICKER_JUMP_COMMANDS.get(index)?;
        let context = KeyContext {
            terminal_focus: false,
            model_picker_open: true,
            ..*context
        };
        self.shortcut_label(command, &context)
    }
}

impl Snapshot {
    /// The Host's keybindings once its stream reports them, else the defaults.
    pub fn keymap(&self, mac: bool) -> Keymap {
        match &self.keybindings {
            Some(config) => Keymap::new(&config.rules, mac),
            None => Keymap::new(&default_keybindings(), mac),
        }
    }

    /// The rules the Keybindings page lists.
    pub fn keybinding_rules(&self) -> Vec<KeybindingRule> {
        self.keybindings
            .as_ref()
            .map_or_else(default_keybindings, |config| config.rules.clone())
    }
}

fn key_label(key: &str) -> String {
    match key {
        " " => "Space".into(),
        "escape" => "Esc".into(),
        "arrowup" => "Up".into(),
        "arrowdown" => "Down".into(),
        "arrowleft" => "Left".into(),
        "arrowright" => "Right".into(),
        key if key.chars().count() == 1 => key.to_uppercase(),
        key => {
            let mut chars = key.chars();
            chars
                .next()
                .map(|first| first.to_uppercase().chain(chars).collect())
                .unwrap_or_default()
        }
    }
}

/// A shortcut as a label: `⌃⌥⇧⌘K` on macOS, `Ctrl+Alt+Shift+K` elsewhere.
pub fn shortcut_label(shortcut: &Shortcut, mac: bool) -> String {
    let (meta, ctrl) = modifiers(shortcut, mac);
    let key = key_label(&shortcut.key);
    if mac {
        let mut label = String::new();
        for (on, glyph) in [
            (ctrl, "\u{2303}"),
            (shortcut.alt, "\u{2325}"),
            (shortcut.shift, "\u{21e7}"),
            (meta, "\u{2318}"),
        ] {
            if on {
                label.push_str(glyph);
            }
        }
        label.push_str(&key);
        return label;
    }
    let mut parts: Vec<String> = vec![];
    for (on, name) in [
        (ctrl, "Ctrl"),
        (shortcut.alt, "Alt"),
        (shortcut.shift, "Shift"),
        (meta, "Meta"),
    ] {
        if on {
            parts.push(name.into());
        }
    }
    parts.push(key);
    parts.join("+")
}

/// The keys of a binding as key caps: `⌘`, `⇧`, `K` on macOS.
pub fn key_caps(key: &str, mac: bool) -> Vec<String> {
    let Some(shortcut) = agent_protocol::keybindings::parse_shortcut(key) else {
        return vec![key.to_owned()];
    };
    let (meta, ctrl) = modifiers(&shortcut, mac);
    let mut caps: Vec<String> = vec![];
    let mut push = |on: bool, mac_glyph: &str, name: &str| {
        if on {
            caps.push(if mac { mac_glyph } else { name }.to_owned());
        }
    };
    push(ctrl, "\u{2303}", "Ctrl");
    push(shortcut.alt, "\u{2325}", "Alt");
    push(shortcut.shift, "\u{21e7}", "Shift");
    push(meta, "\u{2318}", "Meta");
    caps.push(match shortcut.key.as_str() {
        "arrowup" => "\u{2191}".into(),
        "arrowdown" => "\u{2193}".into(),
        "arrowleft" => "\u{2190}".into(),
        "arrowright" => "\u{2192}".into(),
        "enter" if mac => "\u{21b5}".into(),
        other => key_label(other),
    });
    caps
}

/// A pressed key as it is written in a rule, or `None` for a modifier
/// alone: `mod` is ⌘ on macOS and Ctrl elsewhere.
pub fn keybinding_from_press(press: &KeyPress, mac: bool) -> Option<String> {
    let key = press.key.to_lowercase();
    let token = match key.as_str() {
        "meta" | "control" | "ctrl" | "shift" | "alt" | "option" => return None,
        " " | "space" => "space".to_owned(),
        "escape" => "esc".to_owned(),
        "arrowup" | "arrowdown" | "arrowleft" | "arrowright" | "enter" | "tab" | "backspace"
        | "delete" | "home" | "end" | "pageup" | "pagedown" => key.clone(),
        key if key.chars().count() == 1 => key.to_owned(),
        key if key.len() <= 3
            && key.starts_with('f')
            && key.len() > 1
            && key[1..].chars().all(|c| c.is_ascii_digit()) =>
        {
            key.to_owned()
        }
        _ => return None,
    };
    let mut parts: Vec<&str> = vec![];
    if mac {
        if press.meta {
            parts.push("mod");
        }
        if press.ctrl {
            parts.push("ctrl");
        }
    } else {
        if press.ctrl {
            parts.push("mod");
        }
        if press.meta {
            parts.push("meta");
        }
    }
    if press.alt {
        parts.push("alt");
    }
    if press.shift {
        parts.push("shift");
    }
    parts.push(&token);
    Some(parts.join("+"))
}

/// Where a binding comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeybindingSource {
    Default,
    Custom,
    /// A project script's command.
    Project,
}

impl KeybindingSource {
    pub fn label(self) -> &'static str {
        match self {
            KeybindingSource::Default => "Default",
            KeybindingSource::Custom => "Custom",
            KeybindingSource::Project => "Project",
        }
    }
}

/// One binding on the Keybindings page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeybindingRow {
    pub id: String,
    pub command: String,
    pub title: String,
    pub key: String,
    /// The `when` expression, empty for always.
    pub when: String,
    pub source: KeybindingSource,
    /// The default shortcut of the command, for "Reset to default".
    pub default_key: Option<String>,
    pub default_when: String,
    /// The commands another binding of this shortcut runs in the same
    /// context, as titles.
    pub conflicts: Vec<String>,
    /// The rule the Host stores, to replace or remove it.
    pub rule: KeybindingRule,
}

impl KeybindingRow {
    /// A changed default can go back to its command's default.
    pub fn can_reset(&self) -> bool {
        self.source == KeybindingSource::Custom && self.default_key.is_some()
    }

    /// Defaults cannot be removed, only replaced.
    pub fn can_remove(&self) -> bool {
        self.source != KeybindingSource::Default
    }
}

/// A rule's shortcut written canonically (`esc`, `space`), and its
/// condition with minimal parentheses.
fn canonical(rule: &CompiledRule) -> (String, String) {
    let shortcut = &rule.shortcut;
    let mut parts: Vec<&str> = vec![];
    for (on, name) in [
        (shortcut.mod_key, "mod"),
        (shortcut.meta, "meta"),
        (shortcut.ctrl, "ctrl"),
        (shortcut.alt, "alt"),
        (shortcut.shift, "shift"),
    ] {
        if on {
            parts.push(name);
        }
    }
    let key = match shortcut.key.as_str() {
        " " => "space",
        "escape" => "esc",
        key => key,
    };
    parts.push(key);
    (
        parts.join("+"),
        rule.when.as_ref().map(When::expression).unwrap_or_default(),
    )
}

fn compiled_defaults() -> Vec<(CompiledRule, String, String)> {
    default_keybindings()
        .iter()
        .filter_map(|rule| compile_rule(rule).ok())
        .map(|rule| {
            let (key, when) = canonical(&rule);
            (rule, key, when)
        })
        .collect()
}

fn conflicts_with_when(left: &str, right: &str) -> bool {
    left.is_empty() || right.is_empty() || left == right
}

/// The titles of other bindings that share `key` where both can apply.
pub fn keybinding_conflict_labels(
    rows: &[KeybindingRow],
    row_id: &str,
    key: &str,
    when: &str,
) -> Vec<String> {
    if key.trim().is_empty() {
        return vec![];
    }
    let mut labels: Vec<String> = rows
        .iter()
        .filter(|row| row.id != row_id && row.key == key && conflicts_with_when(&row.when, when))
        .map(|row| command_label(&row.command))
        .collect();
    labels.sort();
    labels.dedup();
    labels
}

/// Every binding, sorted by command then shortcut, matching `query` against
/// the command, its title, the shortcut, the condition and the source.
pub fn keybinding_rows(rules: &[KeybindingRule], query: &str) -> Vec<KeybindingRow> {
    let defaults = compiled_defaults();
    let mut rows: Vec<KeybindingRow> = rules
        .iter()
        .enumerate()
        .filter_map(|(index, rule)| {
            let compiled = compile_rule(rule).ok()?;
            let (key, when) = canonical(&compiled);
            let exact = defaults
                .iter()
                .find(|(default, default_key, default_when)| {
                    default.command == compiled.command
                        && *default_key == key
                        && *default_when == when
                });
            let default = exact
                .or_else(|| {
                    defaults.iter().find(|(default, _, default_when)| {
                        default.command == compiled.command && *default_when == when
                    })
                })
                .or_else(|| {
                    defaults
                        .iter()
                        .find(|(default, _, _)| default.command == compiled.command)
                });
            let source = if compiled.command.starts_with("script.") {
                KeybindingSource::Project
            } else if exact.is_some() {
                KeybindingSource::Default
            } else {
                KeybindingSource::Custom
            };
            Some(KeybindingRow {
                id: format!("{}\u{0}{key}\u{0}{when}\u{0}{index}", compiled.command),
                title: command_label(&compiled.command),
                command: compiled.command.clone(),
                key,
                when,
                source,
                default_key: default.map(|(_, key, _)| key.clone()),
                default_when: default.map(|(_, _, when)| when.clone()).unwrap_or_default(),
                conflicts: vec![],
                rule: rule.clone(),
            })
        })
        .collect();
    let conflicts: Vec<Vec<String>> = rows
        .iter()
        .map(|row| keybinding_conflict_labels(&rows, &row.id, &row.key, &row.when))
        .collect();
    for (row, conflicts) in rows.iter_mut().zip(conflicts) {
        row.conflicts = conflicts;
    }
    rows.sort_by(|left, right| {
        locale_compare(&left.command, &right.command)
            .then_with(|| locale_compare(&left.key, &right.key))
    });
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return rows;
    }
    rows.into_iter()
        .filter(|row| {
            [
                row.command.as_str(),
                &row.title,
                &row.key,
                &row.when,
                row.source.label(),
            ]
            .iter()
            .any(|field| field.to_lowercase().contains(&query))
        })
        .collect()
}

/// The commands a new binding can run: every command the app has and any a
/// rule names, by title.
pub fn keybinding_command_options(rules: &[KeybindingRule]) -> Vec<String> {
    let mut commands: Vec<String> = KEYBINDING_COMMANDS
        .iter()
        .map(|c| (*c).to_owned())
        .collect();
    for rule in rules {
        if !commands.contains(&rule.command) {
            commands.push(rule.command.clone());
        }
    }
    commands.sort_by(|left, right| locale_compare(&command_label(left), &command_label(right)));
    commands
}

fn title_case(segment: &str) -> String {
    let mut spaced = String::new();
    let mut previous: Option<char> = None;
    for c in segment.chars() {
        if c.is_ascii_uppercase()
            && previous.is_some_and(|p| p.is_ascii_lowercase() || p.is_ascii_digit())
        {
            spaced.push(' ');
        }
        spaced.push(c);
        previous = Some(c);
    }
    spaced
        .split(|c: char| c == '-' || c == '_' || c.is_whitespace())
        .filter(|word| !word.is_empty())
        .map(|word| {
            let mut chars = word.chars();
            chars
                .next()
                .map(|first| first.to_uppercase().chain(chars).collect::<String>())
                .unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// A command as its title: `terminal.splitVertical` is
/// "Terminal: Split Vertical".
pub fn command_label(command: &str) -> String {
    match command {
        "thread.steerQueuedMessage" => return "Queue: Send First Queued Message as Steer".into(),
        "thread.editQueuedMessage" => return "Queue: Edit Last Queued Message".into(),
        _ => {}
    }
    if let Some(id) = command
        .strip_prefix("script.")
        .and_then(|rest| rest.strip_suffix(".run"))
    {
        return format!("Run Script: {}", title_case(id));
    }
    command
        .split('.')
        .map(title_case)
        .collect::<Vec<_>>()
        .join(": ")
}

/// Why a typed `when` expression cannot be saved.
pub fn when_expression_error(expression: &str) -> Option<String> {
    let trimmed = expression.trim();
    (!trimmed.is_empty() && parse_when(trimmed).is_none())
        .then(|| "Use variables with !, &&, ||, and parentheses.".to_owned())
}

/// The thread jump a command is, from 0.
pub fn thread_jump_index(command: &str) -> Option<usize> {
    THREAD_JUMP_COMMANDS
        .iter()
        .position(|jump| *jump == command)
}

/// The model picker jump a command is, from 0.
pub fn model_jump_index(command: &str) -> Option<usize> {
    MODEL_PICKER_JUMP_COMMANDS
        .iter()
        .position(|jump| *jump == command)
}

/// The rule an intent names: its shortcut, command and condition.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct KeybindingTarget {
    pub key: String,
    pub command: String,
    pub when: Option<String>,
}

impl From<KeybindingTarget> for KeybindingRule {
    fn from(target: KeybindingTarget) -> Self {
        Self {
            key: target.key,
            command: target.command,
            when: target.when.filter(|when| !when.trim().is_empty()),
        }
    }
}

impl From<&KeybindingRule> for KeybindingTarget {
    fn from(rule: &KeybindingRule) -> Self {
        Self {
            key: rule.key.clone(),
            command: rule.command.clone(),
            when: rule.when.clone(),
        }
    }
}

#[cfg(test)]
mod tests;

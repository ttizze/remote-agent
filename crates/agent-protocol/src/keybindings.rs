//! Keybindings: the rules the Host keeps in `keybindings.json`, the default
//! rules, and parsing of shortcuts (`mod+shift+k`) and `when` expressions
//! (`terminalFocus && !composerFocus`).
use serde::{Deserialize, Serialize};

pub const MAX_KEYBINDINGS: usize = 256;
pub const MAX_KEY_CHARS: usize = 64;
pub const MAX_WHEN_CHARS: usize = 256;
pub const MAX_WHEN_DEPTH: usize = 64;
const MAX_SCRIPT_ID_CHARS: usize = 24;

/// One rule: a shortcut, the command it runs and where it applies.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct KeybindingRule {
    pub key: String,
    pub command: String,
    pub when: Option<String>,
}

impl KeybindingRule {
    pub fn new(key: &str, command: &str, when: Option<&str>) -> Self {
        Self {
            key: key.into(),
            command: command.into(),
            when: when.map(Into::into),
        }
    }
}

/// Why part of `keybindings.json` was not used.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum KeybindingIssue {
    /// The file is not a JSON array; the default rules apply.
    MalformedConfig { message: String },
    /// One entry was skipped.
    InvalidEntry { index: u32, message: String },
}

/// The rules in effect: the file's rules over the defaults of every command
/// the file does not bind, in order (a later rule wins).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct KeybindingsConfig {
    pub rules: Vec<KeybindingRule>,
    pub issues: Vec<KeybindingIssue>,
    /// Where the Host keeps the file.
    pub path: String,
}

/// `host/keybindings/upsert`: adds `rule`, replacing `replace` when given.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpsertKeybinding {
    pub rule: KeybindingRule,
    pub replace: Option<KeybindingRule>,
}

/// The thread jumps, `mod+1` to `mod+9`.
pub const THREAD_JUMP_COMMANDS: [&str; 9] = [
    "thread.jump.1",
    "thread.jump.2",
    "thread.jump.3",
    "thread.jump.4",
    "thread.jump.5",
    "thread.jump.6",
    "thread.jump.7",
    "thread.jump.8",
    "thread.jump.9",
];

/// The model picker's jumps to its first nine selectable models.
pub const MODEL_PICKER_JUMP_COMMANDS: [&str; 9] = [
    "modelPicker.jump.1",
    "modelPicker.jump.2",
    "modelPicker.jump.3",
    "modelPicker.jump.4",
    "modelPicker.jump.5",
    "modelPicker.jump.6",
    "modelPicker.jump.7",
    "modelPicker.jump.8",
    "modelPicker.jump.9",
];

/// Every command a rule may name besides `script.<id>.run`.
pub const KEYBINDING_COMMANDS: &[&str] = &[
    "sidebar.toggle",
    "terminal.toggle",
    "terminal.split",
    "terminal.splitVertical",
    "terminal.new",
    "terminal.close",
    "rightPanel.toggle",
    "rightPanel.close",
    "diff.toggle",
    "composer.stash",
    "chat.new",
    "modelPicker.toggle",
    "modelPicker.previousProvider",
    "modelPicker.nextProvider",
    "modelPicker.jump.1",
    "modelPicker.jump.2",
    "modelPicker.jump.3",
    "modelPicker.jump.4",
    "modelPicker.jump.5",
    "modelPicker.jump.6",
    "modelPicker.jump.7",
    "modelPicker.jump.8",
    "modelPicker.jump.9",
    "thread.steerQueuedMessage",
    "thread.editQueuedMessage",
    "thread.previous",
    "thread.next",
    "thread.settle",
    "thread.pin",
    "thread.undo",
    "thread.jump.1",
    "thread.jump.2",
    "thread.jump.3",
    "thread.jump.4",
    "thread.jump.5",
    "thread.jump.6",
    "thread.jump.7",
    "thread.jump.8",
    "thread.jump.9",
];

/// The rules a new `keybindings.json` starts with, in order.
pub fn default_keybindings() -> Vec<KeybindingRule> {
    let mut rules: Vec<KeybindingRule> = [
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
        (
            "mod+shift+arrowup",
            "modelPicker.previousProvider",
            Some("modelPickerOpen"),
        ),
        (
            "mod+shift+arrowdown",
            "modelPicker.nextProvider",
            Some("modelPickerOpen"),
        ),
        ("mod+shift+[", "thread.previous", None),
        ("mod+shift+]", "thread.next", None),
        ("mod+shift+s", "thread.settle", Some("!terminalFocus")),
        ("mod+shift+p", "thread.pin", Some("!terminalFocus")),
        (
            "mod+z",
            "thread.undo",
            Some("!terminalFocus && !editableFocus"),
        ),
    ]
    .into_iter()
    .map(|(key, command, when)| KeybindingRule::new(key, command, when))
    .collect();
    for (index, command) in THREAD_JUMP_COMMANDS.iter().enumerate() {
        rules.push(KeybindingRule::new(
            &format!("mod+{}", index + 1),
            command,
            Some("isDesktop"),
        ));
    }
    for (index, command) in MODEL_PICKER_JUMP_COMMANDS.iter().enumerate() {
        rules.push(KeybindingRule::new(
            &format!("mod+{}", index + 1),
            command,
            Some("modelPickerOpen && isDesktop"),
        ));
    }
    rules
}

/// A parsed shortcut. `mod` is ⌘ on macOS and Ctrl elsewhere.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Shortcut {
    pub key: String,
    pub meta: bool,
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub mod_key: bool,
}

impl Shortcut {
    /// `mod+shift+k`: modifiers in a fixed order, then the key.
    pub fn encode(&self) -> Option<String> {
        if self.key.is_empty() || (self.key != "+" && self.key.contains('+')) {
            return None;
        }
        let mut parts: Vec<&str> = vec![];
        for (on, name) in [
            (self.mod_key, "mod"),
            (self.meta, "meta"),
            (self.ctrl, "ctrl"),
            (self.alt, "alt"),
            (self.shift, "shift"),
        ] {
            if on {
                parts.push(name);
            }
        }
        let key = if self.key == " " { "space" } else { &self.key };
        parts.push(key);
        Some(parts.join("+"))
    }
}

/// `mod+j`, `ctrl+shift+arrowup`, `mod++`; `None` when it names no key or
/// two keys.
pub fn parse_shortcut(value: &str) -> Option<Shortcut> {
    let lowered = value.to_lowercase();
    let mut tokens: Vec<&str> = lowered.split('+').map(str::trim).collect();
    let mut trailing = 0;
    while tokens.last() == Some(&"") {
        trailing += 1;
        tokens.pop();
    }
    if trailing > 0 {
        tokens.push("+");
    }
    if tokens.is_empty() || tokens.iter().any(|token| token.is_empty()) {
        return None;
    }
    let mut shortcut = Shortcut {
        key: String::new(),
        meta: false,
        ctrl: false,
        shift: false,
        alt: false,
        mod_key: false,
    };
    let mut key = None;
    for token in tokens {
        match token {
            "cmd" | "meta" => shortcut.meta = true,
            "ctrl" | "control" => shortcut.ctrl = true,
            "shift" => shortcut.shift = true,
            "alt" | "option" => shortcut.alt = true,
            "mod" => shortcut.mod_key = true,
            other => {
                if key.is_some() {
                    return None;
                }
                key = Some(match other {
                    "space" => " ",
                    "esc" => "escape",
                    other => other,
                });
            }
        }
    }
    shortcut.key = key?.to_owned();
    Some(shortcut)
}

/// A parsed `when` expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum When {
    Identifier(String),
    Not(Box<When>),
    And(Box<When>, Box<When>),
    Or(Box<When>, Box<When>),
}

impl When {
    /// Whether it holds, reading identifiers with `value`; `true` and `false`
    /// are literals.
    pub fn evaluate(&self, value: &impl Fn(&str) -> bool) -> bool {
        match self {
            When::Identifier(name) => match name.as_str() {
                "true" => true,
                "false" => false,
                name => value(name),
            },
            When::Not(node) => !node.evaluate(value),
            When::And(left, right) => left.evaluate(value) && right.evaluate(value),
            When::Or(left, right) => left.evaluate(value) || right.evaluate(value),
        }
    }

    /// The expression with parentheses only where they are needed.
    pub fn expression(&self) -> String {
        let wrap = |node: &When| match node {
            When::Identifier(_) | When::Not(_) => node.expression(),
            _ => format!("({})", node.expression()),
        };
        match self {
            When::Identifier(name) => name.clone(),
            When::Not(node) => format!("!{}", wrap(node)),
            When::And(left, right) => format!("{} && {}", wrap(left), wrap(right)),
            When::Or(left, right) => format!("{} || {}", wrap(left), wrap(right)),
        }
    }

    /// Every identifier it reads.
    pub fn identifiers(&self, into: &mut Vec<String>) {
        match self {
            When::Identifier(name) => into.push(name.clone()),
            When::Not(node) => node.identifiers(into),
            When::And(left, right) | When::Or(left, right) => {
                left.identifiers(into);
                right.identifiers(into);
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Identifier(String),
    Not,
    And,
    Or,
    Open,
    Close,
}

fn tokenize(expression: &str) -> Option<Vec<Token>> {
    let mut tokens = vec![];
    let mut chars = expression.char_indices().peekable();
    while let Some((at, current)) = chars.next() {
        match current {
            c if c.is_whitespace() => {}
            '&' if expression[at..].starts_with("&&") => {
                chars.next();
                tokens.push(Token::And);
            }
            '|' if expression[at..].starts_with("||") => {
                chars.next();
                tokens.push(Token::Or);
            }
            '!' => tokens.push(Token::Not),
            '(' => tokens.push(Token::Open),
            ')' => tokens.push(Token::Close),
            c if c.is_ascii_alphabetic() || c == '_' => {
                let mut name = c.to_string();
                while let Some((_, next)) = chars.peek() {
                    if next.is_ascii_alphanumeric() || matches!(next, '_' | '.' | '-') {
                        name.push(*next);
                        chars.next();
                    } else {
                        break;
                    }
                }
                tokens.push(Token::Identifier(name));
            }
            _ => return None,
        }
    }
    Some(tokens)
}

struct WhenParser {
    tokens: Vec<Token>,
    index: usize,
}

impl WhenParser {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.index)
    }

    fn primary(&mut self, depth: usize) -> Option<When> {
        if depth > MAX_WHEN_DEPTH {
            return None;
        }
        match self.peek()?.clone() {
            Token::Identifier(name) => {
                self.index += 1;
                Some(When::Identifier(name))
            }
            Token::Open => {
                self.index += 1;
                let node = self.or(depth + 1)?;
                if self.peek() != Some(&Token::Close) {
                    return None;
                }
                self.index += 1;
                Some(node)
            }
            _ => None,
        }
    }

    fn unary(&mut self, depth: usize) -> Option<When> {
        let mut nots = 0;
        while self.peek() == Some(&Token::Not) {
            self.index += 1;
            nots += 1;
            if nots > MAX_WHEN_DEPTH {
                return None;
            }
        }
        let mut node = self.primary(depth)?;
        for _ in 0..nots {
            node = When::Not(Box::new(node));
        }
        Some(node)
    }

    fn and(&mut self, depth: usize) -> Option<When> {
        let mut left = self.unary(depth)?;
        while self.peek() == Some(&Token::And) {
            self.index += 1;
            left = When::And(Box::new(left), Box::new(self.unary(depth)?));
        }
        Some(left)
    }

    fn or(&mut self, depth: usize) -> Option<When> {
        let mut left = self.and(depth)?;
        while self.peek() == Some(&Token::Or) {
            self.index += 1;
            left = When::Or(Box::new(left), Box::new(self.and(depth)?));
        }
        Some(left)
    }
}

/// `a && !(b || c)`; `None` when it does not parse.
pub fn parse_when(expression: &str) -> Option<When> {
    let tokens = tokenize(expression)?;
    if tokens.is_empty() {
        return None;
    }
    let mut parser = WhenParser { tokens, index: 0 };
    let node = parser.or(0)?;
    (parser.index == parser.tokens.len()).then_some(node)
}

/// Whether a rule may name `command`.
pub fn is_keybinding_command(command: &str) -> bool {
    if KEYBINDING_COMMANDS.contains(&command) {
        return true;
    }
    command
        .strip_prefix("script.")
        .and_then(|rest| rest.strip_suffix(".run"))
        .is_some_and(|id| {
            !id.is_empty()
                && id.chars().count() <= MAX_SCRIPT_ID_CHARS
                && id.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
                && id
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        })
}

/// A rule ready to match: its parsed shortcut and condition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledRule {
    pub command: String,
    pub shortcut: Shortcut,
    pub when: Option<When>,
}

/// Why a rule cannot be used, or the rule ready to match.
pub fn compile_rule(rule: &KeybindingRule) -> Result<CompiledRule, String> {
    let key = rule.key.trim();
    if key.is_empty() || key.chars().count() > MAX_KEY_CHARS {
        return Err(format!(
            "Invalid keybinding rule: the key must be 1 to {MAX_KEY_CHARS} characters"
        ));
    }
    if !is_keybinding_command(&rule.command) {
        return Err(format!(
            "Invalid keybinding rule: unknown command \"{}\"",
            rule.command
        ));
    }
    let shortcut = parse_shortcut(key)
        .ok_or_else(|| format!("Invalid keybinding rule: cannot parse the key \"{key}\""))?;
    let when = match rule.when.as_deref().map(str::trim) {
        None => None,
        Some(when) if when.is_empty() || when.chars().count() > MAX_WHEN_CHARS => {
            return Err(format!(
                "Invalid keybinding rule: the when clause must be 1 to {MAX_WHEN_CHARS} characters"
            ));
        }
        Some(when) => Some(parse_when(when).ok_or_else(|| {
            format!("Invalid keybinding rule: cannot parse the when clause \"{when}\"")
        })?),
    };
    Ok(CompiledRule {
        command: rule.command.clone(),
        shortcut,
        when,
    })
}

/// The usable rules, the newest `MAX_KEYBINDINGS` of them.
pub fn compile_rules(rules: &[KeybindingRule]) -> Vec<KeybindingRule> {
    let usable: Vec<KeybindingRule> = rules
        .iter()
        .filter(|rule| compile_rule(rule).is_ok())
        .cloned()
        .collect();
    let skip = usable.len().saturating_sub(MAX_KEYBINDINGS);
    usable.into_iter().skip(skip).collect()
}

/// The file's rules after the defaults of every command they leave unbound;
/// no file rules means the defaults alone.
pub fn merge_with_defaults(custom: &[KeybindingRule]) -> Vec<KeybindingRule> {
    let custom = compile_rules(custom);
    let defaults = default_keybindings();
    if custom.is_empty() {
        return defaults;
    }
    let mut merged: Vec<KeybindingRule> = defaults
        .into_iter()
        .filter(|rule| !custom.iter().any(|custom| custom.command == rule.command))
        .collect();
    merged.extend(custom);
    let skip = merged.len().saturating_sub(MAX_KEYBINDINGS);
    merged.into_iter().skip(skip).collect()
}

/// The shortcut and condition two rules share, comparing parsed values.
pub fn same_shortcut_context(left: &KeybindingRule, right: &KeybindingRule) -> bool {
    let context = |rule: &KeybindingRule| {
        let key = parse_shortcut(&rule.key)?.encode()?;
        Some((key, rule.when.clone().unwrap_or_default()))
    };
    match (context(left), context(right)) {
        (Some(left), Some(right)) => left == right,
        _ => false,
    }
}

#[cfg(test)]
mod tests;

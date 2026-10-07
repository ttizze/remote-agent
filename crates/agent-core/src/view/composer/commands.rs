//! The composer's command menu: the `@` path, `#` pull request, `$` skill and
//! `/` command triggers at the caret, the items each trigger offers, and what
//! choosing one does to the draft.
//!
//! Offsets are UTF-16 code units, the unit of the native text editors.
use crate::js_text::{is_js_space, js_trim, js_trim_start, utf16_len, utf16_units};
use crate::presentation::markdown::links::{MarkdownFileIcon, markdown_file_icon};
use crate::view::search_ranking::{
    QueryMatch, Ranked, insert_ranked, normalize_search_query, score_query_match,
};
use agent_domain::{Driver, InteractionMode, Message, Role, State, ThreadShell, Timestamp};
use serde_json::{Value, json};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum ComposerTriggerKind {
    Path,
    PullRequest,
    SlashCommand,
    SlashModel,
    Skill,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ComposerTrigger {
    pub kind: ComposerTriggerKind,
    pub query: String,
    pub range_start: u32,
    pub range_end: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct TextReplacement {
    pub text: String,
    pub cursor: u32,
}

fn slice(units: &[u16], start: usize, end: usize) -> String {
    if start >= end {
        return String::new();
    }
    String::from_utf16_lossy(&units[start..end])
}

fn token_boundary(unit: u16) -> bool {
    matches!(unit, 0x20 | 0x0a | 0x09 | 0x0d)
}

fn line_terminator(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}')
}

/// The Unicode `Sc` (currency symbol) category.
fn currency_symbol(c: char) -> bool {
    matches!(
        c as u32,
        0x24 | 0xa2..=0xa5
            | 0x58f
            | 0x60b
            | 0x7fe..=0x7ff
            | 0x9f2..=0x9f3
            | 0x9fb
            | 0xaf1
            | 0xbf9
            | 0xe3f
            | 0x17db
            | 0x20a0..=0x20c0
            | 0xa838
            | 0xfdfc
            | 0xfe69
            | 0xff04
            | 0xffe0..=0xffe1
            | 0xffe5..=0xffe6
            | 0x11fdd..=0x11fe0
            | 0x1e2ff
            | 0x1ecb0
    )
}

/// `#` alone or followed by a word that starts with a letter or digit.
fn pull_request_query(token: &str) -> Option<String> {
    let rest = token.strip_prefix('#')?;
    let mut chars = rest.chars();
    match chars.next() {
        None => Some(String::new()),
        Some(first) if first.is_alphanumeric() => chars
            .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
            .then(|| rest.to_owned()),
        Some(_) => None,
    }
}

/// The slash-command trigger of a line prefix that starts with `/`.
fn slash_trigger(prefix: &str) -> Option<(ComposerTriggerKind, String)> {
    let command = prefix.strip_prefix('/')?;
    if !command.chars().any(is_js_space) {
        return Some(if command.to_lowercase() == "model" {
            (ComposerTriggerKind::SlashModel, String::new())
        } else {
            (ComposerTriggerKind::SlashCommand, command.to_owned())
        });
    }
    let arguments = command.strip_prefix("model")?;
    let query = js_trim_start(arguments);
    (query.len() < arguments.len() && !query.chars().any(line_terminator))
        .then(|| (ComposerTriggerKind::SlashModel, js_trim(query).to_owned()))
}

/// The `@path`, `#pull-request`, `$skill` or `/command` trigger at the caret.
pub fn detect_composer_trigger(text: &str, cursor: u32) -> Option<ComposerTrigger> {
    let units = utf16_units(text);
    let cursor = (cursor as usize).min(units.len());
    let search_from = cursor.saturating_sub(1);
    let line_start = units[..units.len().min(search_from + 1)]
        .iter()
        .rposition(|unit| *unit == 0x0a)
        .map_or(0, |index| index + 1);
    let line_prefix = slice(&units, line_start, cursor);
    let range = |kind, query| {
        Some(ComposerTrigger {
            kind,
            query,
            range_start: line_start as u32,
            range_end: cursor as u32,
        })
    };
    if line_prefix.starts_with('/')
        && let Some((kind, query)) = slash_trigger(&line_prefix)
    {
        return range(kind, query);
    }

    let token_start = units[..cursor]
        .iter()
        .rposition(|unit| token_boundary(*unit))
        .map_or(0, |index| index + 1);
    let token = slice(&units, token_start, cursor);
    let trigger = |kind, query| {
        Some(ComposerTrigger {
            kind,
            query,
            range_start: token_start as u32,
            range_end: cursor as u32,
        })
    };
    if let Some(query) = pull_request_query(&token) {
        return trigger(ComposerTriggerKind::PullRequest, query);
    }
    if let Some(prefix) = token.chars().next().filter(|c| currency_symbol(*c)) {
        return trigger(
            ComposerTriggerKind::Skill,
            token[prefix.len_utf8()..].to_owned(),
        );
    }
    token
        .strip_prefix('@')
        .and_then(|query| trigger(ComposerTriggerKind::Path, query.to_owned()))
}

pub fn replace_text_range(text: &str, start: u32, end: u32, replacement: &str) -> TextReplacement {
    let units = utf16_units(text);
    let start = (start as usize).min(units.len());
    let end = (end as usize).min(units.len()).max(start);
    TextReplacement {
        text: format!(
            "{}{replacement}{}",
            slice(&units, 0, start),
            slice(&units, end, units.len())
        ),
        cursor: start as u32 + utf16_len(replacement) as u32,
    }
}

/// A Markdown link to a workspace file, labelled with its basename.
pub fn serialize_composer_file_link(path: &str) -> String {
    let basename = path.rsplit(['/', '\\']).next().unwrap_or(path);
    let label = basename
        .replace('\\', "\\\\")
        .replace('[', "\\[")
        .replace(']', "\\]");
    let mut destination = String::new();
    for c in path.chars() {
        match c {
            '(' => destination.push_str("%28"),
            ')' => destination.push_str("%29"),
            '#' => destination.push_str("%23"),
            '?' => destination.push_str("%3F"),
            '\\' => destination.push_str("%5C"),
            c if c.is_ascii_alphanumeric() || ";,/:@&=+$-_.!~*'".contains(c) => destination.push(c),
            c => {
                let mut bytes = [0; 4];
                for byte in c.encode_utf8(&mut bytes).bytes() {
                    destination.push_str(&format!("%{byte:02X}"));
                }
            }
        }
    }
    format!("[{label}]({destination})")
}

/// A prompt that is only `/plan` or `/default` switches the interaction mode.
pub fn parse_standalone_slash_command(text: &str) -> Option<InteractionMode> {
    let command = js_trim(text).strip_prefix('/')?;
    let name = command.trim_end_matches(is_js_space).to_lowercase();
    match name.as_str() {
        "plan" => Some(InteractionMode::Plan),
        "default" => Some(InteractionMode::Default),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum BuiltInSlashCommand {
    Model,
    Plan,
    Default,
}
impl BuiltInSlashCommand {
    fn name(self) -> &'static str {
        match self {
            Self::Model => "model",
            Self::Plan => "plan",
            Self::Default => "default",
        }
    }
}

/// What a menu item stands for.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum ComposerCommandTarget {
    BuiltIn { command: BuiltInSlashCommand },
    ProviderCommand { name: String },
    Skill { name: String },
    Path { path: String, directory: bool },
    Thread { thread_id: String, title: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ComposerCommandItem {
    pub id: String,
    pub label: String,
    pub description: String,
    pub target: ComposerCommandTarget,
    /// A skill's source, for its badge and icon.
    pub skill_source: Option<SkillSourceKind>,
    /// A file's icon; directories show the folder symbol.
    pub file_icon: Option<MarkdownFileIcon>,
}

/// Where a provider skill comes from, for its badge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum SkillSourceKind {
    App,
    Repo,
    Project,
    Personal,
    System,
    #[default]
    Other,
}
impl SkillSourceKind {
    /// The badge text: "App", "Repo", "Project", "Personal", "System" or "Provider".
    pub fn label(self) -> &'static str {
        match self {
            Self::App => "App",
            Self::Repo => "Repo",
            Self::Project => "Project",
            Self::Personal => "Personal",
            Self::System => "System",
            Self::Other => "Provider",
        }
    }
}

/// A plugin path names an app skill, else the provider's scope decides.
pub fn skill_source_kind(path: &str, scope: Option<&str>) -> SkillSourceKind {
    let path = path.replace('\\', "/");
    if path.contains("/.codex/plugins/") || path.contains("/.agents/plugins/") {
        return SkillSourceKind::App;
    }
    match scope.map(|scope| scope.trim().to_lowercase()).as_deref() {
        Some("repo" | "repository") => SkillSourceKind::Repo,
        Some("project" | "workspace" | "local") => SkillSourceKind::Project,
        Some("user" | "personal") => SkillSourceKind::Personal,
        Some("system") => SkillSourceKind::System,
        _ => SkillSourceKind::Other,
    }
}

/// A skill the selected provider offers for the project.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ComposerSkill {
    pub source: SkillSourceKind,
    pub name: String,
    pub display_name: Option<String>,
    pub short_description: Option<String>,
    pub description: Option<String>,
    pub enabled: bool,
    /// `Some(false)` when only the agent may start it.
    pub user_invocable: Option<bool>,
}

/// A slash command the selected provider offers for the project.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ProviderSlashCommand {
    pub name: String,
    pub description: Option<String>,
}

/// One result of the Host's workspace entry search.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ComposerPathEntry {
    pub path: String,
    pub directory: bool,
}

/// Everything the menu shows for one trigger; Host lists are passed as they are.
#[derive(Debug, Clone, Copy)]
pub struct ComposerCommandMenuInput<'a> {
    pub trigger: &'a ComposerTrigger,
    pub driver: Option<Driver>,
    pub has_thread: bool,
    pub has_compactable_conversation: bool,
    /// The composer can switch the interaction mode and the provider shows the toggle.
    pub allow_interaction_mode: bool,
    pub skills: &'a [ComposerSkill],
    pub slash_commands: &'a [ProviderSlashCommand],
    pub path_entries: &'a [ComposerPathEntry],
    pub threads: &'a [ThreadShell],
    pub current_thread: Option<&'a str>,
}

/// The `/` menu's built-in and provider commands.
pub fn slash_command_items(
    query: &str,
    at_message_start: bool,
    input: &ComposerCommandMenuInput<'_>,
) -> Vec<ComposerCommandItem> {
    let query = query.to_lowercase();
    let mut items: Vec<ComposerCommandItem> = [
        (BuiltInSlashCommand::Model, "Switch model"),
        (BuiltInSlashCommand::Plan, "Switch to plan mode"),
        (BuiltInSlashCommand::Default, "Switch to default mode"),
    ]
    .into_iter()
    .filter(|(command, _)| {
        command.name().contains(&query)
            && (*command == BuiltInSlashCommand::Model || input.allow_interaction_mode)
    })
    .map(|(command, description)| ComposerCommandItem {
        id: format!("cmd:{}", command.name()),
        label: format!("/{}", command.name()),
        description: description.into(),
        skill_source: None,
        file_icon: None,
        target: ComposerCommandTarget::BuiltIn { command },
    })
    .collect();
    // Providers expand commands only at the start of a message; built-ins
    // change local state anywhere.
    if !at_message_start {
        return items;
    }
    let skill_names: Vec<String> = slash_menu_skills(input.skills)
        .iter()
        .map(|skill| skill.name.trim().to_lowercase())
        .collect();
    for command in input.slash_commands {
        let name = command.name.trim().to_lowercase();
        if skill_names.contains(&name)
            || !command.name.to_lowercase().contains(&query)
            || (command.name == "compact" && !input.has_compactable_conversation)
            || (!input.has_thread
                && input.driver == Some(Driver::Codex)
                && command.name == "feedback")
        {
            continue;
        }
        items.push(ComposerCommandItem {
            id: format!("pcmd:{}", command.name),
            label: format!("/{}", command.name),
            description: command.description.clone().unwrap_or_default(),
            skill_source: None,
            file_icon: None,
            target: ComposerCommandTarget::ProviderCommand {
                name: command.name.clone(),
            },
        });
    }
    items
}

fn skill_invocable(skill: &ComposerSkill) -> bool {
    skill.enabled && skill.user_invocable != Some(false)
}

/// Enabled, user-invocable skills, first of each name.
fn slash_menu_skills(skills: &[ComposerSkill]) -> Vec<&ComposerSkill> {
    let mut seen = Vec::new();
    skills
        .iter()
        .filter(|skill| skill_invocable(skill))
        .filter(|skill| {
            let name = skill.name.trim().to_lowercase();
            let first = !seen.contains(&name);
            seen.push(name);
            first
        })
        .collect()
}

/// A slash-menu skill matches `skill`, `skill:<query>` or any of its text.
pub fn matches_slash_skill_query(skill: &ComposerSkill, query: &str) -> bool {
    if !skill.enabled {
        return false;
    }
    let query = query.to_lowercase();
    let query = if query == "skill" {
        ""
    } else {
        query.strip_prefix("skill:").unwrap_or(&query)
    };
    query.is_empty()
        || [
            Some(&skill.name),
            skill.display_name.as_ref(),
            skill.short_description.as_ref(),
            skill.description.as_ref(),
        ]
        .into_iter()
        .flatten()
        .any(|value| value.to_lowercase().contains(query))
}

fn skill_description(skill: &ComposerSkill) -> String {
    skill
        .short_description
        .clone()
        .or_else(|| skill.description.clone())
        .unwrap_or_default()
}

fn skill_item(skill: &ComposerSkill, label: String) -> ComposerCommandItem {
    ComposerCommandItem {
        id: format!("skill:{}", skill.name),
        label,
        description: skill_description(skill),
        skill_source: Some(skill.source),
        file_icon: None,
        target: ComposerCommandTarget::Skill {
            name: skill.name.clone(),
        },
    }
}

const SKILL_RESULT_LIMIT: usize = 20;

fn ranked_skill_items(skills: &[ComposerSkill], query: &str) -> Vec<ComposerCommandItem> {
    let skills = slash_menu_skills(skills);
    let label = |skill: &ComposerSkill| {
        skill
            .display_name
            .clone()
            .unwrap_or_else(|| skill.name.clone())
    };
    let query = normalize_search_query(query, currency_symbol);
    if query.is_empty() {
        return skills
            .into_iter()
            .take(SKILL_RESULT_LIMIT)
            .map(|skill| skill_item(skill, label(skill)))
            .collect();
    }
    let mut ranked = Vec::new();
    for skill in skills {
        let display = label(skill).to_lowercase();
        let lower = |value: &Option<String>| value.as_deref().unwrap_or_default().to_lowercase();
        let scores = [
            score_query_match(&QueryMatch {
                prefix_base: Some(2),
                boundary_base: Some(4),
                includes_base: Some(6),
                fuzzy_base: Some(100),
                boundary_markers: &["-", "_", "/"],
                ..QueryMatch::exact(&skill.name.to_lowercase(), &query, 0)
            }),
            score_query_match(&QueryMatch {
                prefix_base: Some(3),
                boundary_base: Some(5),
                includes_base: Some(7),
                fuzzy_base: Some(110),
                ..QueryMatch::exact(&display, &query, 1)
            }),
            score_query_match(&QueryMatch {
                prefix_base: Some(22),
                boundary_base: Some(24),
                includes_base: Some(26),
                ..QueryMatch::exact(&lower(&skill.short_description), &query, 20)
            }),
            score_query_match(&QueryMatch {
                prefix_base: Some(32),
                boundary_base: Some(34),
                includes_base: Some(36),
                ..QueryMatch::exact(&lower(&skill.description), &query, 30)
            }),
        ];
        if let Some(score) = scores.into_iter().flatten().min() {
            insert_ranked(
                &mut ranked,
                Ranked {
                    item: skill,
                    score,
                    tie_breaker: format!("{display}\u{0}{}", skill.name),
                },
                SKILL_RESULT_LIMIT,
            );
        }
    }
    ranked
        .into_iter()
        .map(|entry| skill_item(entry.item, label(entry.item)))
        .collect()
}

const THREAD_RESULT_LIMIT: usize = 5;

/// Threads the `@` picker offers before files. A bare `@` stays a file picker.
pub fn thread_items(
    threads: &[ThreadShell],
    current_thread: Option<&str>,
    query: &str,
) -> Vec<ComposerCommandItem> {
    let query = js_trim(query).to_lowercase();
    if query.is_empty() {
        return vec![];
    }
    let mut matches: Vec<&ThreadShell> = threads
        .iter()
        .filter(|shell| {
            Some(shell.id.as_str()) != current_thread
                && shell.archived_at.is_none()
                && shell.title.to_lowercase().contains(&query)
        })
        .collect();
    matches.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
    matches
        .into_iter()
        .take(THREAD_RESULT_LIMIT)
        .map(|shell| ComposerCommandItem {
            id: format!("thread:{}", shell.id),
            label: shell.title.clone(),
            description: "Thread".into(),
            skill_source: None,
            file_icon: None,
            target: ComposerCommandTarget::Thread {
                thread_id: shell.id.to_string(),
                title: shell.title.clone(),
            },
        })
        .collect()
}

fn path_item(entry: &ComposerPathEntry) -> ComposerCommandItem {
    let parts: Vec<&str> = entry.path.split('/').collect();
    ComposerCommandItem {
        id: format!("path:{}", entry.path),
        label: parts.last().copied().unwrap_or_default().to_owned(),
        description: if parts.len() > 1 {
            parts[..parts.len() - 1].join("/")
        } else {
            String::new()
        },
        skill_source: None,
        file_icon: (!entry.directory).then(|| markdown_file_icon(&entry.path)),
        target: ComposerCommandTarget::Path {
            path: entry.path.clone(),
            directory: entry.directory,
        },
    }
}

/// The menu items for the active trigger.
pub fn composer_command_items(input: &ComposerCommandMenuInput<'_>) -> Vec<ComposerCommandItem> {
    let trigger = input.trigger;
    match trigger.kind {
        ComposerTriggerKind::SlashCommand => {
            let query = trigger.query.to_lowercase();
            let mut items = slash_command_items(&query, trigger.range_start == 0, input);
            items.extend(
                slash_menu_skills(input.skills)
                    .into_iter()
                    .filter(|skill| matches_slash_skill_query(skill, &query))
                    .map(|skill| skill_item(skill, format!("skill:{}", skill.name))),
            );
            items
        }
        ComposerTriggerKind::Skill => ranked_skill_items(input.skills, &trigger.query),
        ComposerTriggerKind::Path => {
            let mut items = thread_items(input.threads, input.current_thread, &trigger.query);
            items.extend(input.path_entries.iter().map(path_item));
            items
        }
        ComposerTriggerKind::PullRequest | ComposerTriggerKind::SlashModel => vec![],
    }
}

/// The context record a thread item attaches; one record per thread.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ThreadContextAttachment {
    pub context_id: String,
    pub thread_id: String,
    pub label: String,
}
impl ThreadContextAttachment {
    pub fn new(thread_id: &str, title: &str) -> Self {
        Self {
            context_id: super::chips::kind_scoped_context_id("thread", thread_id),
            thread_id: thread_id.into(),
            label: agent_domain::sanitize_context_label(title, "thread"),
        }
    }
    /// The message context record; `environment_id` names the Host.
    pub fn record(&self, environment_id: &str) -> Value {
        json!({
            "version": 1,
            "kind": "thread",
            "contextId": self.context_id,
            "label": self.label,
            "environmentId": environment_id,
            "threadId": self.thread_id,
            "title": self.label,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ComposerCommandSelection {
    pub text: String,
    pub cursor: u32,
    pub interaction_mode: Option<InteractionMode>,
    /// A thread record to add to the draft's context, when it has none for that thread yet.
    pub attach_thread: Option<ThreadContextAttachment>,
}

pub const TOO_MANY_CONTEXT_ITEMS: &str =
    "Too many context items. Remove some context from the draft and try again.";

/// What choosing `item` does to the draft. `context_ids` are the draft's
/// current context records.
pub fn resolve_composer_command_selection(
    draft: &str,
    trigger: &ComposerTrigger,
    item: &ComposerCommandItem,
    allow_interaction_mode: bool,
    context_ids: &[String],
) -> Result<ComposerCommandSelection, String> {
    let replace = |replacement: &str, interaction_mode, attach_thread| {
        let TextReplacement { text, cursor } =
            replace_text_range(draft, trigger.range_start, trigger.range_end, replacement);
        ComposerCommandSelection {
            text,
            cursor,
            interaction_mode,
            attach_thread,
        }
    };
    Ok(match &item.target {
        ComposerCommandTarget::BuiltIn {
            command: command @ (BuiltInSlashCommand::Plan | BuiltInSlashCommand::Default),
        } if allow_interaction_mode => replace(
            "",
            Some(if *command == BuiltInSlashCommand::Plan {
                InteractionMode::Plan
            } else {
                InteractionMode::Default
            }),
            None,
        ),
        ComposerCommandTarget::BuiltIn { command } => {
            replace(&format!("/{} ", command.name()), None, None)
        }
        ComposerCommandTarget::ProviderCommand { name } => {
            replace(&format!("/{name} "), None, None)
        }
        ComposerCommandTarget::Skill { name } => replace(&format!("${name} "), None, None),
        ComposerCommandTarget::Path { path, .. } => replace(
            &format!("{} ", serialize_composer_file_link(path)),
            None,
            None,
        ),
        ComposerCommandTarget::Thread { thread_id, title } => {
            let attachment = ThreadContextAttachment::new(thread_id, title);
            let attached = context_ids.contains(&attachment.context_id);
            if !attached && context_ids.len() >= agent_domain::COMPOSER_CONTEXT_MAX_RECORDS {
                return Err(TOO_MANY_CONTEXT_ITEMS.into());
            }
            let reference = super::chips::format_context_reference(
                "thread",
                &attachment.context_id,
                &attachment.label,
            );
            replace(
                &format!("{reference} "),
                None,
                (!attached).then_some(attachment),
            )
        }
    })
}

fn is_compact_command(message: &Message) -> bool {
    message.attachments.is_empty() && js_trim(&message.text).to_lowercase() == "/compact"
}

/// `/compact` is offered once the thread has a user message other than a bare
/// `/compact`, or older history holds one.
pub fn has_compactable_conversation(
    state: &State,
    has_more_history: bool,
    latest_user_message_at: Option<&Timestamp>,
) -> bool {
    state
        .inherited_messages
        .iter()
        .chain(&state.messages)
        .any(|message| message.role == Role::User && !is_compact_command(message))
        || (has_more_history && latest_user_message_at.is_some())
}

#[cfg(test)]
mod tests;

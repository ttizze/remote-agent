//! `keybindings.json`: the Host's keybinding rules. A missing file starts
//! with the default rules, defaults added later are backfilled at startup,
//! hand edits are picked up while the Host runs, and clients follow every
//! change.
use agent_protocol::keybindings::{
    KeybindingIssue, KeybindingRule, KeybindingsConfig, MAX_KEYBINDINGS, UpsertKeybinding,
    compile_rule, default_keybindings, merge_with_defaults, same_shortcut_context,
};
use anyhow::{Context as _, Result, anyhow};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
    time::{Duration, SystemTime},
};
use tokio::sync::broadcast;

/// How often a hand edit of the file is looked for.
const WATCH_INTERVAL: Duration = Duration::from_millis(250);

/// One rule as the file writes it: `when` only when it has one.
#[derive(Serialize, Deserialize)]
struct FileRule {
    key: String,
    command: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    when: Option<String>,
}

impl From<&KeybindingRule> for FileRule {
    fn from(rule: &KeybindingRule) -> Self {
        Self {
            key: rule.key.clone(),
            command: rule.command.clone(),
            when: rule.when.clone(),
        }
    }
}

/// Drops `//` and `/* */` comments and trailing commas outside strings, so
/// a hand-edited file may keep them.
fn strip_lenient(text: &str) -> String {
    // Comments go first, so a comment before a closing bracket still leaves
    // the comma trailing.
    strip(&strip(text, false), true)
}

fn strip(text: &str, trailing_commas: bool) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut in_string = false;
    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            match c {
                '\\' => out.extend(chars.next()),
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => {
                in_string = true;
                out.push(c);
            }
            '/' if chars.peek() == Some(&'/') => {
                for next in chars.by_ref() {
                    if next == '\n' {
                        out.push('\n');
                        break;
                    }
                }
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                let mut previous = ' ';
                for next in chars.by_ref() {
                    if previous == '*' && next == '/' {
                        break;
                    }
                    previous = next;
                }
            }
            ',' if trailing_commas => {
                let next = chars.clone().find(|next| !next.is_whitespace());
                if !matches!(next, Some('}' | ']')) {
                    out.push(c);
                }
            }
            _ => out.push(c),
        }
    }
    out
}

/// The file's entries; anything but a JSON array is malformed.
fn parse_entries(text: &str) -> Result<Vec<serde_json::Value>, String> {
    serde_json::from_str::<Vec<serde_json::Value>>(&strip_lenient(text))
        .map_err(|error| format!("expected JSON array ({error})"))
}

/// One entry as a rule the Host can use, or why it cannot.
fn decode_entry(entry: serde_json::Value) -> Result<KeybindingRule, String> {
    let rule: FileRule = serde_json::from_value(entry).map_err(|error| error.to_string())?;
    let rule = KeybindingRule {
        key: rule.key.trim().to_owned(),
        command: rule.command,
        when: rule.when.map(|when| when.trim().to_owned()),
    };
    compile_rule(&rule)?;
    Ok(rule)
}

fn read_text(path: &Path) -> Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).context("failed to read keybindings config"),
    }
}

/// The file's usable rules and what was skipped. A malformed file has no
/// rules and one issue.
fn runtime_rules(path: &Path) -> Result<(Vec<KeybindingRule>, Vec<KeybindingIssue>)> {
    let Some(text) = read_text(path)? else {
        return Ok((vec![], vec![]));
    };
    let entries = match parse_entries(&text) {
        Ok(entries) => entries,
        Err(message) => {
            tracing::warn!(target: "keybindings", path = %path.display(), %message, "Keybindings config is malformed");
            return Ok((vec![], vec![KeybindingIssue::MalformedConfig { message }]));
        }
    };
    let mut rules = vec![];
    let mut issues = vec![];
    for (index, entry) in entries.into_iter().enumerate() {
        match decode_entry(entry) {
            Ok(rule) => rules.push(rule),
            Err(message) => {
                tracing::warn!(target: "keybindings", path = %path.display(), index, %message, "Ignoring an invalid keybinding entry");
                issues.push(KeybindingIssue::InvalidEntry {
                    index: u32::try_from(index).unwrap_or(u32::MAX),
                    message,
                });
            }
        }
    }
    Ok((rules, issues))
}

/// The rules a change starts from; a malformed file is never overwritten.
fn writable_rules(path: &Path) -> Result<Vec<KeybindingRule>> {
    let Some(text) = read_text(path)? else {
        return Ok(vec![]);
    };
    let entries = parse_entries(&text).map_err(|_| anyhow!("expected JSON array"))?;
    Ok(entries
        .into_iter()
        .filter_map(|entry| decode_entry(entry).ok())
        .collect())
}

fn write_rules(path: &Path, rules: &[KeybindingRule]) -> Result<()> {
    let rules: Vec<FileRule> = rules.iter().map(FileRule::from).collect();
    let mut text = serde_json::to_string_pretty(&rules)?;
    text.push('\n');
    crate::platform::save_private_bytes(path, text.as_bytes())
        .context("failed to write keybindings config")
}

fn config(
    path: &Path,
    rules: &[KeybindingRule],
    issues: Vec<KeybindingIssue>,
) -> KeybindingsConfig {
    KeybindingsConfig {
        rules: merge_with_defaults(rules),
        issues,
        path: path.display().to_string(),
    }
}

/// The rules in effect and what was skipped.
fn load(path: &Path) -> Result<KeybindingsConfig> {
    let (rules, issues) = runtime_rules(path)?;
    Ok(config(path, &rules, issues))
}

/// Writes the defaults when there is no file, else appends the defaults of
/// commands the file does not bind yet, unless their shortcut is taken or
/// the file has problems. Rules the user wrote are never evicted.
fn sync_defaults(path: &Path) -> Result<()> {
    if read_text(path)?.is_none() {
        return write_rules(path, &default_keybindings());
    }
    let (existing, issues) = runtime_rules(path)?;
    if !issues.is_empty() {
        tracing::warn!(target: "keybindings", path = %path.display(), "Skipping the default keybinding backfill because the config has issues");
        return Ok(());
    }
    let mut missing = vec![];
    for default in default_keybindings() {
        if existing.iter().any(|rule| rule.command == default.command) {
            continue;
        }
        if let Some(conflict) = existing
            .iter()
            .find(|rule| same_shortcut_context(rule, &default))
        {
            tracing::warn!(
                target: "keybindings",
                path = %path.display(),
                default_command = %default.command,
                conflicting_command = %conflict.command,
                key = %default.key,
                when = ?default.when,
                "skipping default keybinding due to shortcut conflict: shortcut context already used by existing rule"
            );
            continue;
        }
        missing.push(default);
    }
    let slots = MAX_KEYBINDINGS.saturating_sub(existing.len());
    if missing.len() > slots {
        tracing::warn!(target: "keybindings", path = %path.display(), skipped = missing.len() - slots, "Skipping default keybinding backfill at max entries");
        missing.truncate(slots);
    }
    if missing.is_empty() {
        return Ok(());
    }
    write_rules(path, &[existing, missing].concat())
}

fn upsert(path: &Path, input: &UpsertKeybinding) -> Result<KeybindingsConfig> {
    compile_rule(&input.rule).map_err(|error| anyhow!(error))?;
    let mut rules: Vec<KeybindingRule> = writable_rules(path)?
        .into_iter()
        .filter(|rule| *rule != input.rule && Some(rule) != input.replace.as_ref())
        .collect();
    rules.push(input.rule.clone());
    let skip = rules.len().saturating_sub(MAX_KEYBINDINGS);
    let rules: Vec<KeybindingRule> = rules.into_iter().skip(skip).collect();
    write_rules(path, &rules)?;
    Ok(config(path, &rules, vec![]))
}

fn remove(path: &Path, target: &KeybindingRule) -> Result<KeybindingsConfig> {
    let rules: Vec<KeybindingRule> = writable_rules(path)?
        .into_iter()
        .filter(|rule| rule != target)
        .collect();
    write_rules(path, &rules)?;
    Ok(config(path, &rules, vec![]))
}

/// The file's change stamp, to notice hand edits.
fn stamp(path: &Path) -> Option<(SystemTime, u64)> {
    let metadata = std::fs::metadata(path).ok()?;
    Some((metadata.modified().ok()?, metadata.len()))
}

pub(crate) struct Keybindings {
    path: PathBuf,
    /// Held by the blocking work itself, so changes never interleave.
    lock: Arc<tokio::sync::Mutex<()>>,
    changes: broadcast::Sender<KeybindingsConfig>,
    watcher: OnceLock<tokio_util::task::AbortOnDropHandle<()>>,
}

impl Keybindings {
    pub(crate) fn new(path: PathBuf) -> Self {
        Self {
            path,
            lock: Default::default(),
            changes: broadcast::channel(16).0,
            watcher: OnceLock::new(),
        }
    }

    async fn locked<T: Send + 'static>(
        &self,
        work: impl FnOnce(&Path) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let guard = self.lock.clone().lock_owned().await;
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || {
            let _guard = guard;
            work(&path)
        })
        .await?
    }

    /// Backfills the defaults and starts following hand edits.
    pub(crate) async fn start(self: &Arc<Self>) -> Result<()> {
        self.locked(sync_defaults).await?;
        let keybindings = Arc::downgrade(self);
        let path = self.path.clone();
        let task = tokio::spawn(async move {
            let mut last = stamp(&path);
            loop {
                tokio::time::sleep(WATCH_INTERVAL).await;
                let current = stamp(&path);
                if current == last {
                    continue;
                }
                last = current;
                let Some(keybindings) = keybindings.upgrade() else {
                    return;
                };
                match keybindings.config().await {
                    Ok(config) => {
                        let _ = keybindings.changes.send(config);
                    }
                    Err(error) => {
                        tracing::warn!(target: "keybindings", error = %format!("{error:#}"), "Could not reload keybindings")
                    }
                }
            }
        });
        let _ = self
            .watcher
            .set(tokio_util::task::AbortOnDropHandle::new(task));
        Ok(())
    }

    pub(crate) async fn config(&self) -> Result<KeybindingsConfig> {
        self.locked(load).await
    }

    /// The rules now, and every change after.
    pub(crate) fn subscribe(&self) -> broadcast::Receiver<KeybindingsConfig> {
        self.changes.subscribe()
    }

    pub(crate) async fn upsert(&self, input: UpsertKeybinding) -> Result<KeybindingsConfig> {
        let config = self.locked(move |path| upsert(path, &input)).await?;
        let _ = self.changes.send(config.clone());
        Ok(config)
    }

    pub(crate) async fn remove(&self, rule: KeybindingRule) -> Result<KeybindingsConfig> {
        let config = self.locked(move |path| remove(path, &rule)).await?;
        let _ = self.changes.send(config.clone());
        Ok(config)
    }
}

#[cfg(test)]
mod tests;

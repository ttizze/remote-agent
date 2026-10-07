//! Worktree branch names generated from a launch's first message. Callers run
//! the model; nothing here does I/O. Lengths count UTF-16 code units, as the
//! clients' strings do.
use crate::sync::js_space as is_js_space;
use crate::title::{js_trim, limit_section};
use agent_domain::{Attachment, BranchNaming, BranchNamingMode};
use serde_json::{Value, json};

/// The temporary branch a launch checks out until its generated name is known:
/// `agent/session-` and the 12 hex digits the Host derives from the thread.
const TEMPORARY_BRANCH_PREFIX: &str = "agent/session-";

/// Whether `name` is a temporary branch a launch names after its first message.
pub fn is_temporary_worktree_branch(name: &str) -> bool {
    js_trim(name)
        .to_lowercase()
        .strip_prefix(TEMPORARY_BRANCH_PREFIX)
        .is_some_and(|key| key.len() == 12 && key.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// The model prompt naming a branch after `message`; the model answers with
/// [`branch_name_output_schema`].
pub fn branch_name_prompt(
    naming: &BranchNaming,
    message: &str,
    attachments: &[Attachment],
) -> String {
    let mut rules = vec![
        "Branch should describe the requested work from the user message.",
        "Return a valid Git branch name without spaces.",
    ];
    match naming.mode {
        BranchNamingMode::Custom => rules.push(
            "Return the complete branch name, following the user's naming instructions. No prefix or suffix will be added.",
        ),
        mode => {
            rules.push(
                "Keep it short and specific (2-6 words), in lowercase with hyphen-separated words.",
            );
            rules.push(if mode == BranchNamingMode::Semantic {
                "Include a semantic prefix and a slash in the branch name, for example feat/add-search, fix/login-error, refactor/auth, docs/setup, or chore/update-deps. Choose the prefix that best describes the work."
            } else {
                "Return only the descriptive branch fragment, without a prefix or namespace. The application adds the configured prefix."
            });
        }
    }
    rules.push("If images are attached, use them as primary context for visual/UI issues.");
    let mut sections = vec![
        "You generate concise git branch names.".to_owned(),
        "Return a JSON object with key: branch.".to_owned(),
        "Rules:".to_owned(),
    ];
    sections.extend(rules.iter().map(|rule| format!("- {rule}")));
    sections.extend([
        "".into(),
        "User message:".into(),
        limit_section(message, 8_000),
    ]);
    let instructions = js_trim(&naming.instructions);
    if naming.mode == BranchNamingMode::Custom && !instructions.is_empty() {
        sections.extend([
            "".into(),
            "Additional instructions:".into(),
            limit_section(instructions, 20_000),
        ]);
    }
    if !attachments.is_empty() {
        let lines = attachments
            .iter()
            .map(|attachment| {
                format!(
                    "- {} ({}, {} bytes)",
                    attachment.name, attachment.mime_type, attachment.size
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        sections.extend([
            "".into(),
            "Attachment metadata:".into(),
            limit_section(&lines, 4_000),
        ]);
    }
    sections.join("\n")
}

/// Strict structured-output schema for the branch name response.
pub fn branch_name_output_schema() -> Value {
    json!({
        "type": "object",
        "properties": { "branch": { "type": "string" } },
        "required": ["branch"],
        "additionalProperties": false,
    })
}

/// The branch the model's `{"branch": …}` answer names, formatted by `naming`;
/// `None` when the answer has no such field.
pub fn generated_branch_name(raw: &str, naming: &BranchNaming) -> Option<String> {
    let value: Value = serde_json::from_str(raw).ok()?;
    let branch = value.as_object()?.get("branch")?.as_str()?;
    Some(format_generated_branch_name(branch, naming))
}

/// A generated name as a branch: custom names stay complete (Git validates
/// them), others become a sanitized fragment, which the static mode prefixes.
pub fn format_generated_branch_name(raw: &str, naming: &BranchNaming) -> String {
    if naming.mode == BranchNamingMode::Custom {
        return js_trim(raw).to_owned();
    }
    let branch = sanitize_branch_fragment(raw);
    if naming.mode != BranchNamingMode::Static {
        return branch;
    }
    let prefix = naming
        .prefix
        .split('/')
        .map(|part| {
            let part = replace_runs(part, |c| {
                c.is_ascii_alphanumeric() || matches!(c, '_' | '-')
            });
            collapse(&part, '-').trim_matches('-').to_owned()
        })
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("/");
    if prefix.is_empty() {
        branch
    } else {
        format!("{prefix}/{branch}")
    }
}

/// A lowercase branch fragment of `[a-z0-9/_-]`, at most 64 characters, not
/// starting or ending with a separator; `update` when nothing remains.
pub fn sanitize_branch_fragment(raw: &str) -> String {
    let lowered = js_trim(raw).to_lowercase();
    let unquoted: String = lowered
        .chars()
        .filter(|c| !matches!(c, '\'' | '"' | '`'))
        .collect();
    let normalized =
        unquoted.trim_matches(|c: char| matches!(c, '.' | '/' | '_' | '-') || is_js_space(c));
    let fragment = replace_runs(normalized, |c| {
        c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '/' | '_' | '-')
    });
    let fragment = collapse(&collapse(&fragment, '/'), '-');
    let separator = |c: char| matches!(c, '.' | '/' | '_' | '-');
    let fragment = fragment.trim_matches(separator);
    // Only ASCII remains, so 64 characters are 64 bytes.
    let fragment = fragment[..fragment.len().min(64)].trim_end_matches(separator);
    if fragment.is_empty() {
        "update".into()
    } else {
        fragment.into()
    }
}

/// `text` with every run of characters outside `keep` replaced by one `-`.
fn replace_runs(text: &str, keep: impl Fn(char) -> bool) -> String {
    let mut result = String::with_capacity(text.len());
    let mut replacing = false;
    for c in text.chars() {
        if keep(c) {
            result.push(c);
            replacing = false;
        } else if !replacing {
            result.push('-');
            replacing = true;
        }
    }
    result
}

/// `text` with every run of `c` reduced to one.
fn collapse(text: &str, c: char) -> String {
    let mut result = String::with_capacity(text.len());
    for next in text.chars() {
        if !(next == c && result.ends_with(c)) {
            result.push(next);
        }
    }
    result
}

#[cfg(test)]
mod tests;

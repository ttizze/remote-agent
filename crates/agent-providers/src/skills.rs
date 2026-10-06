//! Composer skill mentions: a currency sigil before a skill name, bounded by
//! whitespace, that is not an amount such as `€20`, `€5k` or `€1e6`.

fn currency_sigil(c: char) -> bool {
    matches!(c,
        '$' | '\u{A2}'..='\u{A5}' | '\u{58F}' | '\u{60B}' | '\u{7FE}'..='\u{7FF}'
        | '\u{9F2}'..='\u{9F3}' | '\u{9FB}' | '\u{AF1}' | '\u{BF9}' | '\u{E3F}'
        | '\u{17DB}' | '\u{20A0}'..='\u{20C0}' | '\u{A838}' | '\u{FDFC}' | '\u{FE69}'
        | '\u{FF04}' | '\u{FFE0}'..='\u{FFE1}' | '\u{FFE5}'..='\u{FFE6}'
        | '\u{11FDD}'..='\u{11FE0}' | '\u{1E2FF}' | '\u{1ECB0}')
}
fn amount(token: &str) -> bool {
    let digits = token
        .char_indices()
        .find(|(_, c)| !(c.is_ascii_digit() || *c == '_'))
        .map_or(token.len(), |(index, _)| index);
    if digits == 0 || !token.starts_with(|c: char| c.is_ascii_digit()) {
        return false;
    }
    let rest = &token[digits..];
    rest.is_empty()
        || rest.len() == 1 && "kKmMbBtT".contains(rest)
        || rest.len() > 1
            && rest.starts_with(['e', 'E'])
            && rest[1..].chars().all(|c| c.is_ascii_digit())
}
/// `(sigil_start, name_start, end)` byte offsets of every mention.
pub fn skill_mentions(text: &str) -> Vec<(usize, usize, usize)> {
    let mut mentions = vec![];
    let mut previous: Option<char> = None;
    for (index, c) in text.char_indices() {
        if currency_sigil(c) && previous.is_none_or(char::is_whitespace) {
            let start = index + c.len_utf8();
            let end = text[start..]
                .find(char::is_whitespace)
                .map_or(text.len(), |offset| start + offset);
            let token = &text[start..end];
            if token.starts_with(|c: char| c.is_ascii_alphanumeric())
                && token
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, ':' | '_' | '-'))
                && token.chars().any(|c| c.is_ascii_alphabetic())
                && !amount(token)
            {
                mentions.push((index, start, end));
            }
        }
        previous = Some(c);
    }
    mentions
}
/// Codex parses `$name`; any other sigil becomes `$`.
pub fn codex_skill_mention_text(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut copied = 0;
    for (sigil, name, _) in skill_mentions(text) {
        result.push_str(&text[copied..sigil]);
        result.push('$');
        copied = name;
    }
    result.push_str(&text[copied..]);
    result
}
/// Claude runs the last known skill as a slash command block; earlier known
/// mentions in the leading text become slash references.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudeSkillDispatch {
    pub leading_text: Option<String>,
    pub command_text: String,
}
pub fn claude_skill_dispatch(text: &str, skills: &[String]) -> Option<ClaudeSkillDispatch> {
    let known = skill_mentions(text)
        .into_iter()
        .filter(|(_, name, end)| skills.iter().any(|skill| skill == &text[*name..*end]))
        .collect::<Vec<_>>();
    let (last, earlier) = known.split_last()?;
    let mut leading = text[..last.0].to_owned();
    for (sigil, name, end) in earlier.iter().rev() {
        leading.replace_range(*sigil..*end, &format!("/{}", &text[*name..*end]));
    }
    let leading = leading.trim_end().to_owned();
    Some(ClaudeSkillDispatch {
        leading_text: (!leading.is_empty()).then_some(leading),
        command_text: format!("/{}{}", &text[last.1..last.2], &text[last.2..])
            .trim_end()
            .to_owned(),
    })
}

/// What a Claude skill's `SKILL.md` says about invoking it (T3 ClaudeSkills).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ClaudeSkillFrontmatter {
    pub user_invocation_only: bool,
    pub user_invocable: bool,
}
/// The YAML 1.1 boolean spellings Claude Code accepts.
fn frontmatter_boolean(value: &str) -> Option<bool> {
    match value.trim().to_lowercase().as_str() {
        "true" | "yes" | "on" | "y" | "1" => Some(true),
        "false" | "no" | "off" | "n" | "0" => Some(false),
        _ => None,
    }
}
/// A plain, quoted or flow scalar without its trailing comment; `None` when
/// the CLI's YAML parser would reject it.
fn frontmatter_scalar(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if let Some(quote) = raw.chars().next().filter(|c| matches!(c, '"' | '\'')) {
        let close = raw[1..].find(quote)? + 1;
        let rest = raw[close + 1..].trim_start();
        return (rest.is_empty() || rest.starts_with('#')).then(|| raw[1..close].to_owned());
    }
    if let Some(open) = raw.chars().next().filter(|c| matches!(c, '[' | '{')) {
        let close = if open == '[' { ']' } else { '}' };
        let mut depth = 0i32;
        for (index, c) in raw.char_indices() {
            if c == open {
                depth += 1;
            } else if c == close {
                depth -= 1;
                if depth == 0 {
                    return Some(raw[..=index].to_owned());
                }
            }
        }
        return None;
    }
    let value = match raw.find(" #").or_else(|| raw.find("\t#")) {
        Some(comment) => &raw[..comment],
        None => raw,
    };
    Some(value.trim().to_owned())
}
/// The skill's frontmatter; `None` when it is malformed, as Claude Code then
/// does not load the skill. A file without frontmatter is an ordinary skill.
pub fn claude_skill_frontmatter(contents: &str) -> Option<ClaudeSkillFrontmatter> {
    let mut parsed = ClaudeSkillFrontmatter {
        user_invocation_only: false,
        user_invocable: true,
    };
    let Some(rest) = contents
        .strip_prefix("---\n")
        .or_else(|| contents.strip_prefix("---\r\n"))
    else {
        return Some(parsed);
    };
    let mut lines = vec![];
    let mut closed = false;
    for line in rest.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line == "---" {
            closed = true;
            break;
        }
        lines.push(line);
    }
    if !closed {
        return Some(parsed);
    }
    for line in lines {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || line.starts_with([' ', '\t']) {
            continue;
        }
        if trimmed.starts_with("- ") {
            continue;
        }
        let (key, value) = line.split_once(':')?;
        if key.is_empty()
            || !key
                .chars()
                .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
        {
            return None;
        }
        let value = frontmatter_scalar(value)?;
        match key {
            "disable-model-invocation" => {
                parsed.user_invocation_only |= frontmatter_boolean(&value) == Some(true);
            }
            "user-invocable" => {
                parsed.user_invocable = frontmatter_boolean(&value) != Some(false);
            }
            _ => {}
        }
    }
    Some(parsed)
}
/// A `skillOverrides` entry: `(enabled, user invocation only)`.
pub type ClaudeSkillOverride = (bool, bool);
/// The `skillOverrides` of one Claude settings file. Settings files are
/// hand-edited, so comments and trailing commas are accepted; one invalid
/// value drops the whole map, as Claude Code does.
pub fn claude_skill_overrides(contents: &str) -> Vec<(String, ClaudeSkillOverride)> {
    let Ok(settings) = serde_json::from_str::<serde_json::Value>(&lenient_json(contents)) else {
        return vec![];
    };
    let Some(overrides) = settings["skillOverrides"].as_object() else {
        return vec![];
    };
    let mut parsed = overrides
        .iter()
        .map(|(name, value)| {
            let parsed = match value.as_str()? {
                "off" => (false, false),
                "user-invocable-only" => (true, true),
                "on" | "name-only" => (true, false),
                _ => return None,
            };
            Some((name.clone(), parsed))
        })
        .collect::<Option<Vec<_>>>()
        .unwrap_or_default();
    parsed.sort_by(|a, b| a.0.cmp(&b.0));
    parsed
}
/// JSON with `//` and `/* */` comments and trailing commas removed.
fn lenient_json(text: &str) -> String {
    let mut uncommented = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut string = false;
    while let Some(c) = chars.next() {
        if string {
            uncommented.push(c);
            if c == '\\' {
                uncommented.extend(chars.next());
            } else if c == '"' {
                string = false;
            }
            continue;
        }
        match (c, chars.peek()) {
            ('/', Some('/')) => {
                for c in chars.by_ref() {
                    if c == '\n' {
                        uncommented.push('\n');
                        break;
                    }
                }
            }
            ('/', Some('*')) => {
                chars.next();
                let mut previous = ' ';
                for c in chars.by_ref() {
                    if previous == '*' && c == '/' {
                        break;
                    }
                    previous = c;
                }
            }
            _ => {
                string = c == '"';
                uncommented.push(c);
            }
        }
    }
    let mut output = String::with_capacity(uncommented.len());
    let mut string = false;
    let mut escaped = false;
    for (index, c) in uncommented.char_indices() {
        if string {
            string = escaped || c != '"';
            escaped = !escaped && c == '\\';
        } else if c == '"' {
            string = true;
        } else if c == ','
            && uncommented[index + 1..]
                .trim_start()
                .starts_with(['}', ']'])
        {
            continue;
        }
        output.push(c);
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn codex_mentions_use_the_native_sigil_and_leave_amounts() {
        for (input, output) in [
            ("€review do it", "$review do it"),
            ("£ship", "$ship"),
            ("please ¥review this diff", "please $review this diff"),
            ("first line\n₹ship it", "first line\n$ship it"),
            ("𑿝review then €2spec", "$review then $2spec"),
            ("$review", "$review"),
            ("costs €20", "costs €20"),
            ("€5k", "€5k"),
            ("budget €100M or €1e6", "budget €100M or €1e6"),
            ("5€review", "5€review"),
        ] {
            assert_eq!(codex_skill_mention_text(input), output, "{input}");
        }
    }
    #[test]
    fn claude_dispatch_runs_the_last_known_skill_as_a_command() {
        let skills = ["2spec", "implement", "review", "re-release-version"]
            .map(str::to_owned)
            .to_vec();
        let plan = |text: &str| claude_skill_dispatch(text, &skills);
        assert_eq!(plan("fix the build"), None);
        assert_eq!(plan("echo $HOME then $unknown"), None);
        assert_eq!(
            plan("ok, now $implement all the tickets"),
            Some(ClaudeSkillDispatch {
                leading_text: Some("ok, now".into()),
                command_text: "/implement all the tickets".into()
            })
        );
        assert_eq!(
            plan("$review\nfocus on auth"),
            Some(ClaudeSkillDispatch {
                leading_text: None,
                command_text: "/review\nfocus on auth".into()
            })
        );
        assert_eq!(
            plan("use $2spec for this"),
            Some(ClaudeSkillDispatch {
                leading_text: Some("use".into()),
                command_text: "/2spec for this".into()
            })
        );
        for sigil in ["$", "€", "£", "¥", "₹", "₩", "₿", "𑿝"] {
            assert_eq!(
                plan(&format!(
                    "{sigil}review the diff, then {sigil}implement the fixes"
                )),
                Some(ClaudeSkillDispatch {
                    leading_text: Some("/review the diff, then".into()),
                    command_text: "/implement the fixes".into()
                })
            );
            assert_eq!(plan(&format!("5{sigil}review {sigil}unknown")), None);
        }
        assert_eq!(plan("cost is 5$implement"), None);
        let amounts = ["20", "20k", "100M", "1e6"].map(str::to_owned).to_vec();
        assert_eq!(
            claude_skill_dispatch("pay $20 $20k $100M $1e6 tomorrow", &amounts),
            None
        );
    }
    // T3 ClaudeSkills.test.ts frontmatter cases.
    #[test]
    fn skill_frontmatter_reads_invocation_and_rejects_what_the_cli_rejects() {
        let skill = |lines: &[&str]| claude_skill_frontmatter(&lines.join("\n"));
        assert_eq!(
            claude_skill_frontmatter("# Just a heading\n"),
            Some(ClaudeSkillFrontmatter {
                user_invocation_only: false,
                user_invocable: true
            })
        );
        for (description, comment) in [
            (
                "Browser automation + AI test authoring via kane-cli: run browser objectives, ...",
                "",
            ),
            (
                "Read C:\\skills\\guide#tag: continue with \"quoted\".",
                " # trailing: comment",
            ),
        ] {
            assert_eq!(
                skill(&[
                    "---",
                    "name: frontmatter-alias",
                    &format!("description: {description}{comment}"),
                    "allowed-tools: [Read, Write]",
                    "disable-model-invocation: yes",
                    "user-invocable: no",
                    "---",
                ]),
                Some(ClaudeSkillFrontmatter {
                    user_invocation_only: true,
                    user_invocable: false
                })
            );
        }
        for field in [
            "name: [unclosed",
            "allowed-tools: [Read, Write",
            "name: \"unclosed: text",
        ] {
            assert_eq!(
                skill(&["---", "description: Run: browser objectives.", field, "---"]),
                None,
                "{field}"
            );
        }
        for (value, invocable) in [("no", false), ("off", false), ("0", false), ("yes", true)] {
            assert_eq!(
                skill(&["---", &format!("user-invocable: {value}"), "---"])
                    .unwrap()
                    .user_invocable,
                invocable
            );
        }
    }

    // T3 ClaudeSkills.test.ts skillOverrides cases.
    #[test]
    fn skill_overrides_follow_the_cli() {
        assert_eq!(
            claude_skill_overrides(
                r#"{ "skillOverrides": { "off-by-user": "off", "kept": "on" } }"#
            ),
            [
                ("kept".to_owned(), (true, false)),
                ("off-by-user".to_owned(), (false, false))
            ]
        );
        assert_eq!(
            claude_skill_overrides(
                r#"{ "skillOverrides": { "ask-matt": "user-invocable-only" } }"#
            ),
            [("ask-matt".to_owned(), (true, true))]
        );
        assert!(claude_skill_overrides("{ not json").is_empty());
        assert!(
            claude_skill_overrides(
                r#"{ "skillOverrides": { "unknown-mode": "some-future-mode", "boolean-false": false, "sibling-off": "off" } }"#
            )
            .is_empty()
        );
        assert_eq!(
            claude_skill_overrides(
                "{\n  // hand edited\n  \"skillOverrides\": { \"x\": \"off\", /* note */ },\n}"
            ),
            [("x".to_owned(), (false, false))]
        );
    }
}

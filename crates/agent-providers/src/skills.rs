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

/// What a Claude skill's `SKILL.md` says about invoking it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ClaudeSkillFrontmatter {
    pub user_invocation_only: bool,
    pub user_invocable: bool,
    pub description: Option<String>,
}
/// Claude Code accepts the YAML 1.1 boolean spellings, which the YAML 1.2
/// core schema leaves as strings and numbers.
fn frontmatter_boolean(value: Option<&serde_yaml::Value>) -> Option<bool> {
    match value? {
        serde_yaml::Value::Bool(value) => Some(*value),
        serde_yaml::Value::Number(number) => {
            let number = number.as_f64()?;
            if number == 1.0 {
                Some(true)
            } else if number == 0.0 {
                Some(false)
            } else {
                None
            }
        }
        serde_yaml::Value::String(value) => match value.trim().to_lowercase().as_str() {
            "true" | "yes" | "on" | "y" => Some(true),
            "false" | "no" | "off" | "n" => Some(false),
            _ => None,
        },
        serde_yaml::Value::Tagged(tagged) => frontmatter_boolean(Some(&tagged.value)),
        _ => None,
    }
}
/// The text between the opening `---` line and the next `---` line, matched by
/// `^---\r?\n([\s\S]*?)\r?\n---(?:\r?\n|$)`.
fn frontmatter_block(contents: &str) -> Option<&str> {
    let rest = contents
        .strip_prefix("---\n")
        .or_else(|| contents.strip_prefix("---\r\n"))?;
    rest.char_indices().find_map(|(index, _)| {
        let tail = &rest[index..];
        let tail = tail
            .strip_prefix("\r\n")
            .or_else(|| tail.strip_prefix('\n'))?;
        let after = tail.strip_prefix("---")?;
        (after.is_empty() || after.starts_with('\n') || after.starts_with("\r\n"))
            .then(|| &rest[..index])
    })
}
/// Claude Code accepts plain scalars containing `: `; this quotes only those
/// and leaves comments and YAML structure to the full-document parser. As in
/// a multiline `^([\w-]+:[ \t]*)([^\r\n]*)` replacement, a line starts after
/// any of `\n`, `\r`, U+2028 and U+2029, while its value runs to `\r` or `\n`.
fn quote_plain_colon_scalars(frontmatter: &str) -> String {
    let mut repaired = String::with_capacity(frontmatter.len());
    let mut copied = 0;
    let mut line_start = Some(0);
    while let Some(start) = line_start {
        let mut scanned = start;
        let line = &frontmatter[start..];
        let text = &line[..line.find(['\r', '\n']).unwrap_or(line.len())];
        if let Some(quoted) = quote_plain_colon_scalar(text) {
            repaired.push_str(&frontmatter[copied..start]);
            repaired.push_str(&quoted);
            copied = start + text.len();
            if text.ends_with(['\u{2028}', '\u{2029}']) {
                line_start = Some(copied);
                continue;
            }
            scanned = copied;
        }
        line_start = frontmatter[scanned..]
            .char_indices()
            .find(|(_, c)| matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}'))
            .map(|(index, c)| scanned + index + c.len_utf8());
    }
    repaired.push_str(&frontmatter[copied..]);
    repaired
}
fn quote_plain_colon_scalar(line: &str) -> Option<String> {
    let key = line
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
        .filter(|&end| end > 0 && line[end..].starts_with(':'))?;
    let value_start = key
        + 1
        + line[key + 1..]
            .find(|c: char| c != ' ' && c != '\t')
            .unwrap_or(line.len() - key - 1);
    let value = &line[value_start..];
    let scalar = value
        .char_indices()
        .find(|&(index, c)| {
            matches!(c, ' ' | '\t')
                && value[index..]
                    .trim_start_matches([' ', '\t'])
                    .starts_with('#')
        })
        .map_or(value, |(index, _)| &value[..index]);
    let indicator = scalar.starts_with([
        '"', '\'', '[', ']', '{', '}', '|', '>', '&', '*', '!', '#', '%', '@', '`',
    ]) || scalar.starts_with(['-', '?', ':'])
        && scalar[1..]
            .chars()
            .next()
            .is_none_or(|c| c == ' ' || c == '\t');
    let colon = scalar
        .char_indices()
        .any(|(index, c)| c == ':' && scalar[index + 1..].starts_with([' ', '\t']));
    if !colon || indicator {
        return None;
    }
    Some(format!(
        "{}{}{}",
        &line[..value_start],
        serde_json::Value::String(scalar.to_owned()),
        &value[scalar.len()..]
    ))
}
/// The skill's frontmatter; `None` when it is malformed, as Claude Code then
/// does not load the skill. A file without frontmatter is an ordinary skill.
pub fn claude_skill_frontmatter(contents: &str) -> Option<ClaudeSkillFrontmatter> {
    let mut parsed = ClaudeSkillFrontmatter {
        user_invocation_only: false,
        user_invocable: true,
        description: None,
    };
    let Some(frontmatter) = frontmatter_block(contents) else {
        return Some(parsed);
    };
    let document = serde_yaml::from_str::<serde_yaml::Value>(frontmatter)
        .or_else(|_| serde_yaml::from_str(&quote_plain_colon_scalars(frontmatter)))
        .ok()?;
    let document = match document {
        serde_yaml::Value::Tagged(tagged) => tagged.value,
        document => document,
    };
    match &document {
        serde_yaml::Value::Mapping(_) => {
            parsed.user_invocation_only =
                frontmatter_boolean(document.get("disable-model-invocation")) == Some(true);
            parsed.user_invocable =
                frontmatter_boolean(document.get("user-invocable")) != Some(false);
            parsed.description = document
                .get("description")
                .and_then(serde_yaml::Value::as_str)
                .map(str::trim)
                .filter(|description| !description.is_empty())
                .map(str::to_owned);
            Some(parsed)
        }
        serde_yaml::Value::Sequence(_) => Some(parsed),
        _ => None,
    }
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
    #[test]
    fn skill_frontmatter_reads_invocation_and_rejects_what_the_cli_rejects() {
        let skill = |lines: &[&str]| claude_skill_frontmatter(&lines.join("\n"));
        assert_eq!(
            claude_skill_frontmatter("# Just a heading\n"),
            Some(ClaudeSkillFrontmatter {
                user_invocation_only: false,
                user_invocable: true,
                description: None,
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
                    user_invocable: false,
                    description: Some(description.into()),
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

    // The whole frontmatter parses as YAML, so quoted keys, tags and block
    // scalars count, and what the parser rejects drops the skill.
    #[test]
    fn skill_frontmatter_is_read_as_a_yaml_document() {
        let skill = |lines: &[&str]| claude_skill_frontmatter(&lines.join("\n"));
        let not_invocable = Some(ClaudeSkillFrontmatter {
            user_invocation_only: true,
            user_invocable: false,
            description: None,
        });
        for (invocation, invocable) in [
            (
                "\"disable-model-invocation\": true",
                "'user-invocable': false",
            ),
            (
                "disable-model-invocation: !!bool true",
                "user-invocable: !!bool false",
            ),
            (
                "disable-model-invocation: \"yes\"",
                "user-invocable: !!str off",
            ),
            ("{disable-model-invocation: on, user-invocable: n}", ""),
        ] {
            assert_eq!(
                skill(&["---", invocation, invocable, "---", "body"]),
                not_invocable,
                "{invocation} {invocable}"
            );
        }
        assert_eq!(
            skill(&[
                "---",
                "description: |",
                "  Runs: the deploy.",
                "  user-invocable: false",
                "user-invocable: false # hidden",
                "---",
            ]),
            Some(ClaudeSkillFrontmatter {
                user_invocation_only: false,
                user_invocable: false,
                description: Some("Runs: the deploy.\nuser-invocable: false".into()),
            })
        );
        assert_eq!(
            skill(&["---", "- listed", "---"]),
            Some(ClaudeSkillFrontmatter {
                user_invocation_only: false,
                user_invocable: true,
                description: None,
            })
        );
        for malformed in [
            &["---", "name: first", "name: second", "---"][..],
            &["---", "", "---"],
            &["---", "# only a comment", "---"],
            &["---", "just text", "---"],
        ] {
            assert_eq!(skill(malformed), None, "{malformed:?}");
        }
    }

    #[test]
    fn skill_frontmatter_with_multibyte_text_is_read() {
        let skill = |lines: &[&str]| claude_skill_frontmatter(&lines.join("\n"));
        assert_eq!(
            skill(&[
                "---",
                "description: 日本語の説明",
                "user-invocable: no",
                "---"
            ]),
            Some(ClaudeSkillFrontmatter {
                user_invocation_only: false,
                user_invocable: false,
                description: Some("日本語の説明".into()),
            })
        );
        assert_eq!(
            skill(&[
                "---",
                "description: 手順: デプロイを実行する",
                "disable-model-invocation: true",
                "---",
                "本文",
            ]),
            Some(ClaudeSkillFrontmatter {
                user_invocation_only: true,
                user_invocable: true,
                description: Some("手順: デプロイを実行する".into()),
            })
        );
    }

    // A multiline `^` also starts a line after `\r`, U+2028 and U+2029, and a
    // value runs on to the next `\r` or `\n`.
    #[test]
    fn colon_scalars_are_quoted_on_every_line_start() {
        assert_eq!(
            quote_plain_colon_scalars("a: x\rb: c: d\ne: f: g # note"),
            "a: x\rb: \"c: d\"\ne: \"f: g\" # note"
        );
        assert_eq!(
            quote_plain_colon_scalars("a: b: c\u{2028}d: e: f\u{2029}g: h"),
            "a: \"b: c\u{2028}d: e: f\u{2029}g: h\""
        );
        assert_eq!(
            quote_plain_colon_scalars("a: b: c d\u{2028}\re: f: g"),
            "a: \"b: c d\u{2028}\"\re: \"f: g\""
        );
        assert_eq!(
            quote_plain_colon_scalars("x\u{2028}k: v: w"),
            "x\u{2028}k: \"v: w\""
        );
    }

    fn text_from(
        pieces: &'static [&'static str],
    ) -> impl proptest::strategy::Strategy<Value = String> {
        use proptest::prelude::*;
        prop::collection::vec(prop::sample::select(pieces), 0..48).prop_map(|parts| parts.concat())
    }

    /// The frontmatter pattern, `^` and `$` anchored to the whole text.
    fn reference_frontmatter_block(contents: &str) -> Option<&str> {
        regex::Regex::new(r"^---\r?\n([\s\S]*?)\r?\n---(?:\r?\n|$)")
            .unwrap()
            .captures(contents)
            .map(|captures| captures.get(1).unwrap().as_str())
    }

    /// The repair replacement over lines split at `\r` and `\n`.
    fn reference_quote_plain_colon_scalars(frontmatter: &str) -> String {
        let line = regex::Regex::new(r"(?mR)^([A-Za-z0-9_-]+:[ \t]*)([^\r\n]*)").unwrap();
        let comment = regex::Regex::new(r"[ \t]+#").unwrap();
        let colon = regex::Regex::new(r":[ \t]").unwrap();
        let indicator = regex::Regex::new(r#"^(?:["'\[\]{}|>&*!#%@`]|[-?:](?:[ \t]|$))"#).unwrap();
        line.replace_all(frontmatter, |captures: &regex::Captures| {
            let (prefix, value) = (&captures[1], &captures[2]);
            let scalar = comment.split(value).next().unwrap_or("");
            if !colon.is_match(scalar) || indicator.is_match(scalar) {
                return captures[0].to_owned();
            }
            format!(
                "{prefix}{}{}",
                serde_json::Value::String(scalar.to_owned()),
                &value[scalar.len()..]
            )
        })
        .into_owned()
    }

    proptest::proptest! {
        #[test]
        fn frontmatter_block_matches_the_pattern(
            contents in text_from(&["---", "-", "\n", "\r", "\r\n", "a", " ", "日本語", "é", "𑿝"]),
        ) {
            proptest::prop_assert_eq!(frontmatter_block(&contents), reference_frontmatter_block(&contents));
        }

        #[test]
        fn any_frontmatter_block_matches_the_pattern(body in proptest::prelude::any::<String>()) {
            for contents in [format!("---\n{body}"), format!("---\r\n{body}\n---")] {
                proptest::prop_assert_eq!(frontmatter_block(&contents), reference_frontmatter_block(&contents));
            }
        }

        #[test]
        fn colon_scalars_are_quoted_as_the_replacement_does(
            frontmatter in text_from(&[
                "key", "a-b_1", ":", ": ", " ", "\t", "#", " #", "\n", "\r", "\r\n",
                "\"", "'", "[", "|", "-", "?", "x", "日本語", "\\",
            ]),
        ) {
            proptest::prop_assert_eq!(
                quote_plain_colon_scalars(&frontmatter),
                reference_quote_plain_colon_scalars(&frontmatter)
            );
        }

        #[test]
        fn skill_files_never_panic(contents in proptest::prelude::any::<String>()) {
            for contents in [contents.clone(), format!("---\n{contents}\n---\n"), format!("---\ndescription: {contents}\n---")] {
                let _ = claude_skill_frontmatter(&contents);
            }
        }
    }

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

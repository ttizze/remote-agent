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
}

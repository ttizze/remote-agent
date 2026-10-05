//! T3 prompt-effort rules shared by the composer and native execution.
//! Adapted from T3 Tools Inc.'s MIT implementation; see third-party/T3-Code-LICENSE.

pub const ULTRATHINK_PREFIX: &str = "Ultrathink:\n";

/// Match JavaScript's ASCII word boundaries, including next to non-ASCII text.
pub fn contains_ultrathink(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.windows(10).enumerate().any(|(index, word)| {
        word.eq_ignore_ascii_case(b"ultrathink")
            && (index == 0 || !is_word(bytes[index - 1]))
            && (index + 10 == bytes.len() || !is_word(bytes[index + 10]))
    })
}

fn is_word(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// Strip only the prefix at the beginning, retaining body mentions and whitespace.
pub fn strip_ultrathink_prefix(text: &str) -> &str {
    if text
        .get(..11)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("Ultrathink:"))
    {
        text[11..].trim_start()
    } else {
        text
    }
}

pub fn apply_prompt_effort(text: &str, effort: Option<&str>) -> String {
    let text = text.trim();
    let slash_command = text.strip_prefix('/').is_some_and(|rest| {
        let first = rest.split_whitespace().next().unwrap_or_default();
        rest.chars()
            .next()
            .is_some_and(|character| !character.is_whitespace() && character != '/')
            && !first.is_empty()
            && !first.contains('/')
    });
    if text.is_empty()
        || effort != Some("ultrathink")
        || slash_command
        || text.starts_with("Ultrathink:")
    {
        text.into()
    } else {
        format!("{ULTRATHINK_PREFIX}{text}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_words_and_prefix_have_distinct_boundaries() {
        for text in [
            "ULTRATHINK",
            "please ultrathink here",
            "日本語ultrathink日本語",
            "(ultrathink)",
        ] {
            assert!(contains_ultrathink(text), "{text}");
        }
        for text in [
            "",
            "ultrathinking",
            "preultrathink",
            "_ultrathink",
            "ultrathink2",
        ] {
            assert!(!contains_ultrathink(text), "{text}");
        }
        assert_eq!(strip_ultrathink_prefix("uLtRaThInK:\n \t本文 "), "本文 ");
        for text in [
            " ultrathink:\nbody",
            "本文 ultrathink",
            "日本語ultrathink",
            "ultrathink without colon",
        ] {
            assert_eq!(strip_ultrathink_prefix(text), text);
        }
    }

    #[test]
    fn effort_prefix_preserves_native_commands_and_files_remain_prose() {
        for command in ["/compact", "/plugin:skill now", "/deploy.prod now"] {
            assert_eq!(apply_prompt_effort(command, Some("ultrathink")), command);
        }
        for text in ["/home/user/app.rs", "/ deploy", "//comment", "investigate"] {
            assert_eq!(
                apply_prompt_effort(text, Some("ultrathink")),
                format!("Ultrathink:\n{text}")
            );
            assert_eq!(apply_prompt_effort(text, Some("high")), text);
        }
        assert_eq!(apply_prompt_effort("   ", Some("ultrathink")), "");
        assert_eq!(apply_prompt_effort("  investigate  ", None), "investigate");
    }

    proptest::proptest! {
        #[test]
        fn prefix_is_idempotent_and_preserves_command_tokens(name in "[a-z][a-z.:_-]{0,20}", body in "[a-zA-Z0-9 ]{0,40}") {
            let command = format!("/{name} {body}");
            proptest::prop_assert_eq!(apply_prompt_effort(&command, Some("ultrathink")), command.trim());
            let prefixed = apply_prompt_effort(&body, Some("ultrathink"));
            proptest::prop_assert_eq!(apply_prompt_effort(&prefixed, Some("ultrathink")), prefixed);
        }
    }
}

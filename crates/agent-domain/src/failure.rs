//! Provider failure text with credentials and unsafe control characters
//! removed, trimmed, and bounded in UTF-16 units.
use regex::{Captures, Regex};
use std::sync::LazyLock;

pub const MAX_PROVIDER_FAILURE_MESSAGE_LENGTH: usize = 4_096;
pub const MAX_PROVIDER_FAILURE_CODE_LENGTH: usize = 128;
pub const DEFAULT_PROVIDER_FAILURE_MESSAGE: &str = "Provider turn failed.";

static URL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)(?-u:\b)https?://[^\s<>"']+"#).unwrap());
static AUTH_SCHEME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)(?-u:\b)(Bearer|Basic)\s+[^\s,;]+").unwrap());
static QUOTED_SECRET: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)(["'](?:access[_-]?token|api[_-]?key|authorization|credential|password|secret|token)["']\s*:\s*["'])[^"']*(["'])"#).unwrap()
});
static ASSIGNED_SECRET: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)((?-u:\b)(?:access[_-]?token|api[_-]?key|authorization|credential|password|secret|token)(?-u:\b)\s*[:=]\s*)(?:"[^"]*"|'[^']*'|[^\s,;]+)"#).unwrap()
});
static API_KEY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?-u:\b)sk-[A-Za-z0-9_-]{16,}(?-u:\b)").unwrap());

fn redact_url(found: &str) -> String {
    let candidate = found.trim_end_matches([')', ',', '.', ';', '!', '?']);
    let trailing = &found[candidate.len()..];
    match url::Url::parse(candidate) {
        Ok(mut url) => {
            let _ = url.set_username("");
            let _ = url.set_password(None);
            url.set_query(None);
            url.set_fragment(None);
            format!("{url}{trailing}")
        }
        Err(_) => "[REDACTED_URL]".into(),
    }
}

fn redact(value: &str) -> String {
    let controls = value
        .chars()
        .map(|c| match c as u32 {
            0x00..=0x08 | 0x0b..=0x0c | 0x0e..=0x1f | 0x7f => ' ',
            _ => c,
        })
        .collect::<String>();
    let urls = URL.replace_all(&controls, |caps: &Captures| redact_url(&caps[0]));
    let schemes = AUTH_SCHEME.replace_all(&urls, "$1 [REDACTED]");
    let quoted = QUOTED_SECRET.replace_all(&schemes, "${1}[REDACTED]${2}");
    let assigned = ASSIGNED_SECRET.replace_all(&quoted, "${1}[REDACTED]");
    API_KEY
        .replace_all(&assigned, "[REDACTED]")
        .trim()
        .to_owned()
}

/// UTF-16 bound with an ellipsis that never splits a surrogate pair.
pub fn bounded_failure_text(text: &str, max: usize) -> String {
    let units = text.encode_utf16().collect::<Vec<_>>();
    if units.len() <= max {
        return text.to_owned();
    }
    let mut end = max - 1;
    if (0xDC00..=0xDFFF).contains(&units[end]) {
        end -= 1;
    }
    String::from_utf16_lossy(&units[..end]) + "…"
}

pub fn provider_failure_message(message: &str) -> String {
    let message = bounded_failure_text(&redact(message), MAX_PROVIDER_FAILURE_MESSAGE_LENGTH);
    if message.is_empty() {
        DEFAULT_PROVIDER_FAILURE_MESSAGE.into()
    } else {
        message
    }
}

pub fn provider_failure_code(code: &str) -> Option<String> {
    Some(bounded_failure_text(
        &redact(code),
        MAX_PROVIDER_FAILURE_CODE_LENGTH,
    ))
    .filter(|code| !code.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_credentials_and_url_secrets_from_provider_failures() {
        let message = provider_failure_message(
            r#"request failed: Authorization: Bearer bearer-secret https://user:pass@example.test/path?access_token=url-secret#fragment {"token":"json-secret"} api_key=key-secret sk-abcdefghijklmnop"#,
        );
        assert_eq!(
            provider_failure_code("provider_rejected").as_deref(),
            Some("provider_rejected")
        );
        assert!(message.contains("[REDACTED]"));
        assert!(message.contains("https://example.test/path"));
        for secret in [
            "bearer-secret",
            "user:pass",
            "url-secret",
            "json-secret",
            "key-secret",
            "sk-abcdefghijklmnop",
        ] {
            assert!(!message.contains(secret), "{secret} in {message}");
        }
    }

    #[test]
    fn replaces_unsafe_control_characters_without_stripping_whitespace() {
        assert_eq!(
            provider_failure_message("before\u{0}\u{7}\t\nafter\u{7f}"),
            "before  \t\nafter"
        );
    }

    #[test]
    fn bounds_provider_controlled_failure_strings() {
        let message =
            provider_failure_message(&"m".repeat(MAX_PROVIDER_FAILURE_MESSAGE_LENGTH + 500));
        let code =
            provider_failure_code(&"c".repeat(MAX_PROVIDER_FAILURE_CODE_LENGTH + 50)).unwrap();
        assert_eq!(
            message.encode_utf16().count(),
            MAX_PROVIDER_FAILURE_MESSAGE_LENGTH
        );
        assert_eq!(
            code.encode_utf16().count(),
            MAX_PROVIDER_FAILURE_CODE_LENGTH
        );
        assert!(message.ends_with('…') && code.ends_with('…'));
    }

    #[test]
    fn does_not_split_a_surrogate_pair_at_the_truncation_boundary() {
        let message = provider_failure_message(&format!(
            "{}🚀tail",
            "a".repeat(MAX_PROVIDER_FAILURE_MESSAGE_LENGTH - 2)
        ));
        assert_eq!(
            message.encode_utf16().count(),
            MAX_PROVIDER_FAILURE_MESSAGE_LENGTH - 1
        );
        assert!(message.ends_with(&format!("{}…", "a")));
    }

    #[test]
    fn empty_failures_use_the_default_message_and_drop_the_code() {
        assert_eq!(
            provider_failure_message(" \u{0} "),
            DEFAULT_PROVIDER_FAILURE_MESSAGE
        );
        assert_eq!(provider_failure_code("  "), None);
        assert_eq!(
            provider_failure_message("see https://[bad"),
            "see [REDACTED_URL]"
        );
    }
}

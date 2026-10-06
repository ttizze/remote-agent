//! Assistant-quote links in user messages, reduced to their quoted text for titling.
use super::{is_js_space, js_trim, utf16_len};
use url::Url;

const LABEL: &str = "[Assistant quote](";
pub(super) const HREF_PREFIX: &str = "citation://v1/";
pub(super) const MAX_TEXT: usize = 8_000;
pub(super) const MAX_COMMENT: usize = 8_000;
pub(super) const CONTEXT: usize = 32;
const MAX_ID: usize = 512;
// Percent encoding needs up to nine units per UTF-16 code unit; 16k covers selectors.
const MAX_HREF: usize = 9 * (MAX_TEXT + MAX_COMMENT) + 16_000;
const MAX_SAFE_INTEGER: u64 = (1 << 53) - 1;
const REQUIRED_KEYS: [&str; 5] = ["text", "start", "end", "prefix", "suffix"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Citation {
    pub environment_id: String,
    pub thread_id: String,
    pub message_id: String,
    pub text: String,
    pub comment: Option<String>,
    pub start: u64,
    pub end: u64,
    pub prefix: String,
    pub suffix: String,
}

/// Replaces each valid quote link with its text and optional user comment, unescaped.
pub(super) fn citations_to_plain_text(prompt: &str) -> String {
    let mut out = String::with_capacity(prompt.len());
    let mut rest = prompt;
    while let Some(at) = rest.find(LABEL) {
        let after = &rest[at + LABEL.len()..];
        let Some(href_len) = link_href_len(after) else {
            out.push_str(&rest[..=at]);
            rest = &rest[at + 1..];
            continue;
        };
        let source_end = at + LABEL.len() + href_len + 1;
        match parse_citation_href(&after[..href_len]) {
            Some(citation) => {
                out.push_str(&rest[..at]);
                out.push_str(&citation.text);
                if let Some(comment) = &citation.comment {
                    out.push_str("\nComment: ");
                    out.push_str(comment);
                }
            }
            None => out.push_str(&rest[..source_end]),
        }
        rest = &rest[source_end..];
    }
    out.push_str(rest);
    out
}

/// Length of the href in `href)`: the prefix plus 1..=limit units without whitespace or `)`.
fn link_href_len(after_label: &str) -> Option<usize> {
    let body = after_label.strip_prefix(HREF_PREFIX)?;
    let end = body.find(|c: char| c == ')' || is_js_space(c))?;
    let units = utf16_len(&body[..end]);
    (body[end..].starts_with(')') && (1..=MAX_HREF - HREF_PREFIX.len()).contains(&units))
        .then_some(HREF_PREFIX.len() + end)
}

pub(super) fn parse_citation_href(href: &str) -> Option<Citation> {
    if !href.starts_with(HREF_PREFIX) || utf16_len(href) > MAX_HREF {
        return None;
    }
    let url = Url::parse(href).ok()?;
    let parts: Vec<&str> = url.path().get(1..)?.split('/').collect();
    if url.scheme() != "citation"
        || url.host_str() != Some("v1")
        || parts.len() != 3
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
        || url.fragment().is_some_and(|fragment| !fragment.is_empty())
    {
        return None;
    }
    let pairs: Vec<(String, String)> = url.query_pairs().into_owned().collect();
    let count = |key: &str| pairs.iter().filter(|(name, _)| name == key).count();
    let get = |key: &str| {
        let (_, value) = pairs.iter().find(|(name, _)| name == key)?;
        Some(value.clone())
    };
    let comment = get("comment");
    if pairs.len() != REQUIRED_KEYS.len() + usize::from(comment.is_some())
        || REQUIRED_KEYS.iter().any(|key| count(key) != 1)
    {
        return None;
    }
    let position = |key: &str| {
        let value = get(key)?;
        let digits = (1..=16).contains(&value.len()) && value.bytes().all(|b| b.is_ascii_digit());
        digits
            .then(|| value.parse::<u64>().ok())
            .flatten()
            .filter(|&n| n <= MAX_SAFE_INTEGER)
    };
    let (start, end) = (position("start")?, position("end")?);
    let id = |part: &str| {
        let decoded = decode_uri_component(part)?;
        let trimmed = js_trim(&decoded);
        (!trimmed.is_empty() && utf16_len(trimmed) <= MAX_ID).then(|| trimmed.to_owned())
    };
    let text = get("text")?;
    let prefix = get("prefix")?;
    let suffix = get("suffix")?;
    let valid = end > start
        && !js_trim(&text).is_empty()
        && utf16_len(&text) <= MAX_TEXT
        && comment
            .as_deref()
            .is_none_or(|comment| utf16_len(comment) <= MAX_COMMENT)
        && utf16_len(&prefix) <= CONTEXT
        && utf16_len(&suffix) <= CONTEXT;
    if !valid {
        return None;
    }
    Some(Citation {
        environment_id: id(parts[0])?,
        thread_id: id(parts[1])?,
        message_id: id(parts[2])?,
        text,
        comment,
        start,
        end,
        prefix,
        suffix,
    })
}

/// JavaScript `decodeURIComponent`: malformed escapes and invalid UTF-8 fail.
fn decode_uri_component(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = |at: usize| (*bytes.get(at)? as char).to_digit(16);
            out.push((hex(index + 1)? * 16 + hex(index + 2)?) as u8);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(out).ok()
}

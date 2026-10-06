//! File citations Codex emits as `:codex-file-citation{path=… line_range_start=…}`,
//! resolved to a file link and its portable Markdown form.
use crate::js_text::{is_js_space, js_trim};
use std::collections::BTreeMap;

/// Directive attributes by name; a later duplicate replaces an earlier one.
pub type DirectiveAttributes = BTreeMap<String, String>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileCitationLink {
    pub path: String,
    pub href: String,
    pub label: String,
    pub line_range_start: Option<u64>,
}

const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

/// JavaScript `Number(value)` for a non-blank string, or `None` when it is NaN.
fn js_number(value: &str) -> Option<f64> {
    let text = js_trim(value);
    let radix = |prefix: [&str; 2], radix: u32| {
        let digits = prefix.iter().find_map(|prefix| text.strip_prefix(prefix))?;
        let valid = !digits.is_empty() && digits.chars().all(|c| c.is_digit(radix));
        Some(valid.then(|| {
            digits.chars().fold(0.0, |total, c| {
                total * f64::from(radix) + f64::from(c.to_digit(radix).unwrap_or(0))
            })
        }))
    };
    for (prefix, base) in [(["0x", "0X"], 16), (["0o", "0O"], 8), (["0b", "0B"], 2)] {
        if let Some(number) = radix(prefix, base) {
            return number;
        }
    }
    let unsigned = text.strip_prefix(['+', '-']).unwrap_or(text);
    if unsigned == "Infinity" {
        return Some(f64::INFINITY);
    }
    let (mantissa, exponent) = unsigned
        .split_once(['e', 'E'])
        .map_or((unsigned, None), |(mantissa, exponent)| {
            (mantissa, Some(exponent))
        });
    let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let digits = |part: &str| part.bytes().all(|b| b.is_ascii_digit());
    let exponent_valid = exponent.is_none_or(|exponent| {
        let exponent = exponent.strip_prefix(['+', '-']).unwrap_or(exponent);
        !exponent.is_empty() && digits(exponent)
    });
    let valid = (!whole.is_empty() || !fraction.is_empty())
        && digits(whole)
        && digits(fraction)
        && exponent_valid;
    valid.then(|| text.parse().ok()).flatten()
}

fn positive_integer(value: Option<&str>) -> Option<u64> {
    let value = value.filter(|value| !value.chars().all(is_js_space))?;
    let parsed = js_number(value)?;
    (parsed.fract() == 0.0 && parsed.abs() <= MAX_SAFE_INTEGER && parsed > 0.0)
        .then_some(parsed as u64)
}

fn citation_label(path: &str) -> String {
    let normalized = path.replace('\\', "/");
    let normalized = normalized.trim_end_matches('/');
    let last = &normalized[normalized.rfind('/').map_or(0, |index| index + 1)..];
    if !last.is_empty() {
        last.to_owned()
    } else if !normalized.is_empty() {
        normalized.to_owned()
    } else {
        "File".into()
    }
}

fn markdown_destination_path(path: &str) -> String {
    path.replace('%', "%25")
        .replace('#', "%23")
        .replace('?', "%3F")
}

pub fn file_citation_link(attributes: &DirectiveAttributes) -> Option<FileCitationLink> {
    let path = js_trim(attributes.get("path")?);
    if path.is_empty() {
        return None;
    }
    let line_range_start = positive_integer(attributes.get("line_range_start").map(String::as_str));
    let destination_path = markdown_destination_path(path);
    Some(FileCitationLink {
        path: path.to_owned(),
        href: match line_range_start {
            Some(line) => format!("{destination_path}#L{line}"),
            None => destination_path,
        },
        label: citation_label(path),
        line_range_start,
    })
}

fn markdown_label(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for c in value.chars() {
        if matches!(c, '\\' | '[' | ']' | '*' | '_' | '`' | '<' | '&') {
            escaped.push('\\');
        }
        escaped.push(c);
    }
    escaped
}

fn markdown_destination(value: &str) -> String {
    value
        .replace('\\', "%5C")
        .replace('<', "%3C")
        .replace('>', "%3E")
        .replace('\r', "%0D")
        .replace('\n', "%0A")
}

pub fn file_citation_markdown(citation: &FileCitationLink) -> String {
    format!(
        "[{}](<{}>)",
        markdown_label(&citation.label),
        markdown_destination(&citation.href)
    )
}

#[cfg(test)]
#[path = "citations_tests.rs"]
mod tests;

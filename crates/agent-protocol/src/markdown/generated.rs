//! Codex's generated-image footer, from T3 1a12c4af generatedImageMarkdown.ts.
use super::parse;
use crate::paths;
use markdown::mdast::Node;
use regex::Regex;
use std::{borrow::Cow, collections::HashSet, sync::LazyLock};

static FENCE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^ {0,3}(\x60{3,}|~{3,})").unwrap());
static CLOSE_FENCE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^ {0,3}(\x60{3,}|~{3,})[\t ]*$").unwrap());
static HTML: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^ {0,3}<(script|pre|style|textarea)(?:[\s>]|$)").unwrap());
static CLOSE_HTML: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)</(?:script|pre|style|textarea)>").unwrap());

fn unclosed_block_end(text: &str) -> Option<String> {
    let root = parse(text);
    let last = root.children()?.last()?;
    let position = last.position()?;
    let source = text.get(position.start.offset..position.end.offset)?;
    match last {
        Node::Html(_) => {
            if let Some(tag) = HTML.captures(source).and_then(|capture| capture.get(1)) {
                return (!CLOSE_HTML.is_match(source)).then(|| format!("</{}>", tag.as_str()));
            }
            (source.trim_start_matches(' ').starts_with("<!--") && !source.contains("-->"))
                .then(|| "-->".into())
        }
        Node::Code(_) => {
            let fence = FENCE.captures(source)?.get(1)?.as_str();
            let lines: Vec<_> = source.split(['\r', '\n']).collect();
            let closing = CLOSE_FENCE
                .captures(lines.last().copied().unwrap_or_default())
                .and_then(|capture| capture.get(1));
            let closed = lines.len() > 1
                && closing.is_some_and(|end| {
                    end.as_str().as_bytes()[0] == fence.as_bytes()[0] && end.len() >= fence.len()
                });
            (!closed).then(|| fence.into())
        }
        _ => None,
    }
}

fn embed(path: &str) -> String {
    let destination = if paths::windows_drive(path) {
        path.replace('\\', "/")
    } else if path.starts_with(r"\\") {
        format!("file:{}", path.replace('\\', "/"))
    } else {
        path.to_owned()
    };
    let mut escaped = String::with_capacity(destination.len());
    for ch in destination.chars() {
        match ch {
            '%' | '?' | '#' | '&' | '\\' | '\r' | '\n' => {
                use std::fmt::Write;
                write!(escaped, "%{:02X}", ch as u32).expect("writing to a string");
            }
            '<' | '>' => {
                escaped.push('\\');
                escaped.push(ch);
            }
            _ => escaped.push(ch),
        }
    }
    format!("![](<{escaped}>)")
}

/// Existing rendered embeds and repeated saved paths do not create another preview.
pub fn append_generated_image_embeds<'a>(text: &'a str, saved_paths: &[&str]) -> Cow<'a, str> {
    let mut shown: HashSet<_> = super::images(text)
        .iter()
        .filter_map(|source| paths::markdown_destination_host_path(source))
        .map(|path| paths::comparison_key(&path))
        .collect();
    let missing: Vec<_> = saved_paths
        .iter()
        .copied()
        .filter(|path| shown.insert(paths::comparison_key(path)))
        .collect();
    if missing.is_empty() {
        return Cow::Borrowed(text);
    }
    let mut result = text.to_owned();
    if let Some(end) = unclosed_block_end(text) {
        if !text.ends_with(['\r', '\n']) {
            result.push('\n');
        }
        result.push_str(&end);
    }
    for path in missing {
        if !result.is_empty() {
            result.push_str("\n\n");
        }
        result.push_str(&embed(path));
    }
    Cow::Owned(result)
}

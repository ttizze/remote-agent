//! Pinned assistantTextSelection's rendered-text annotation policy. Protocol
//! citation validation/locate keeps its stricter raw-text contract; presentation
//! normalizes JS whitespace and may use a unique quote after its context drifts.
use agent_protocol::context::TextRange;

pub const BLOCK_TAGS: &[&str] = &[
    "address",
    "article",
    "aside",
    "blockquote",
    "dd",
    "div",
    "dl",
    "dt",
    "figcaption",
    "figure",
    "footer",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "header",
    "hr",
    "li",
    "main",
    "nav",
    "ol",
    "p",
    "pre",
    "section",
    "table",
    "td",
    "th",
    "tr",
    "ul",
];
pub const EXCLUDED_TAGS: &[&str] = &[
    "button", "input", "textarea", "select", "script", "style", "template", "noscript", "svg",
];

/// Return native UTF-16 offsets, including collapsed whitespace's original
/// extent. Saved offsets cannot disambiguate repeated identical contexts: all
/// occurrences must be checked, so they are not needed by this full scan.
pub fn resolve(text: &str, quote: &str, prefix: &str, suffix: &str) -> Option<TextRange> {
    let (body, boundaries) = normalized(text)?;
    let quote = normalized(quote)?.0;
    if crate::text::trim(&quote).is_empty() {
        return None;
    }
    let prefix = normalized(prefix)?.0;
    let suffix = normalized(suffix)?.0;
    let mut cursor = 0;
    let mut count = 0;
    let mut only = None;
    let mut contextual = None;
    while let Some(relative) = body[cursor..].find(&quote) {
        let start = cursor + relative;
        let end = start + quote.len();
        count += 1;
        only = Some((start, end));
        if body[..start].ends_with(&prefix) && body[end..].starts_with(&suffix) {
            if contextual.is_some() {
                return None;
            }
            contextual = Some((start, end));
        }
        // Include overlapping matches without cutting a UTF-8 character.
        cursor = start + quote.chars().next()?.len_utf8();
    }
    let (start, end) = contextual.or_else(|| only.filter(|_| count == 1))?;
    let from = body[..start].encode_utf16().count();
    let to = from + body[start..end].encode_utf16().count();
    Some(TextRange {
        start: *boundaries.get(from)?,
        end: *boundaries.get(to)?,
    })
}

fn normalized(text: &str) -> Option<(String, Vec<u32>)> {
    let mut chars = text.chars().peekable();
    let mut output = String::new();
    let mut boundaries = vec![0];
    let mut offset = 0_u32;
    while let Some(character) = chars.next() {
        let start = offset;
        offset = offset.checked_add(character.len_utf16() as u32)?;
        if crate::text::is_space(character) {
            while chars
                .peek()
                .is_some_and(|character| crate::text::is_space(*character))
            {
                offset = offset.checked_add(chars.next()?.len_utf16() as u32)?;
            }
            output.push(' ');
            boundaries.push(offset);
        } else {
            output.push(character);
            boundaries.extend((start + 1)..=offset);
        }
    }
    Some((output, boundaries))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_context_ties_and_unique_fallback_do_not_depend_on_saved_offsets() {
        assert_eq!(
            resolve("Prefix 👋 chosen suffix", "chosen", "Prefix 👋 ", " suffix"),
            Some(TextRange { start: 10, end: 16 })
        );
        assert_eq!(
            resolve(
                "Different chosen ending",
                "chosen",
                "old prefix",
                "old suffix"
            ),
            Some(TextRange { start: 10, end: 16 })
        );
        assert_eq!(
            resolve("one chosen end one chosen end", "chosen", "one ", " end"),
            None
        );
        assert_eq!(
            resolve("chosen elsewhere; one chosen end", "chosen", "one ", " end"),
            Some(TextRange { start: 22, end: 28 })
        );
        assert_eq!(
            resolve("aaaa", "aa", "a", "a"),
            Some(TextRange { start: 1, end: 3 }),
            "overlapping candidates participate"
        );
        assert_eq!(resolve("different words", "chosen", "", ""), None);
        assert_eq!(resolve("spaces", "\u{feff}\t", "", ""), None);
    }

    #[test]
    fn collapsed_js_whitespace_maps_back_to_native_utf16_boundaries() {
        // FEFF(1), Alpha(5), two spaces(2), emoji(2), newline(1).
        let text = "\u{feff}Alpha  👋\nchosen\tlast";
        assert_eq!(
            resolve(text, "chosen", "Alpha 👋 ", " last"),
            Some(TextRange { start: 11, end: 17 })
        );
        assert_eq!(
            resolve(text, "👋 chosen", "", ""),
            Some(TextRange { start: 8, end: 17 })
        );
        assert_eq!(
            resolve("a\u{0085}b", "\u{0085}", "", ""),
            Some(TextRange { start: 1, end: 2 }),
            "NEL is not JS whitespace"
        );
    }
}

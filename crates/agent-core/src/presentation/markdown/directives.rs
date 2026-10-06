//! The two Markdown directives Codex emits: the inline `:codex-file-citation{…}` and the
//! block `::artifact-template{…}`. The `markdown` crate has no directive syntax, so this
//! tokenizes them as micromark's directive extension does and uses a CommonMark parse
//! for what surrounds them: directives inside code, HTML, autolinks or link
//! destinations do not exist, and a citation inside link text stays literal.
use super::artifact_templates::{
    ArtifactTemplate, artifact_template, artifact_template_presentation_label,
};
use super::citations::{
    DirectiveAttributes, FileCitationLink, file_citation_link, file_citation_markdown,
};
use crate::js_text::{is_js_space, utf16_offset};
use ::markdown::{ParseOptions, mdast::Node};
use regex::Regex;
use std::sync::LazyLock;

const FILE_CITATION_NAME: &str = "codex-file-citation";
const ARTIFACT_TEMPLATE_NAME: &str = "artifact-template";

/// Native renderers show artifact cards between runs of ordinary Markdown.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ArtifactTemplateMarkdownSegment {
    Markdown {
        markdown: String,
        /// UTF-16 offset in the source message.
        source_offset: u64,
    },
    ArtifactTemplate {
        source_offset: u64,
        template: ArtifactTemplate,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum DirectiveContent {
    FileCitation(FileCitationLink),
    ArtifactTemplate(ArtifactTemplate),
}

/// A rendered directive and its byte range in the source.
#[derive(Clone, Debug, PartialEq, Eq)]
struct DirectiveMatch {
    start: usize,
    end: usize,
    content: DirectiveContent,
}

static PUNCTUATION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[\p{P}\p{S}]$").expect("valid pattern"));

/// micromark reads UTF-16 code units, so astral characters are never punctuation.
fn unicode_punctuation(c: char) -> bool {
    c.len_utf16() == 1 && PUNCTUATION.is_match(c.encode_utf8(&mut [0; 4]))
}

fn line_ending(c: char) -> bool {
    matches!(c, '\n' | '\r')
}

fn space_or_tab(c: char) -> bool {
    matches!(c, ' ' | '\t')
}

fn line_ending_or_space(c: char) -> bool {
    line_ending(c) || space_or_tab(c)
}

fn name_ends(c: Option<char>) -> bool {
    c.is_none_or(|c| {
        line_ending(c) || is_js_space(c) || (unicode_punctuation(c) && c != '-' && c != '_')
    })
}

struct Cursor<'a> {
    source: &'a str,
    at: usize,
}

impl Cursor<'_> {
    fn peek(&self) -> Option<char> {
        self.source[self.at..].chars().next()
    }

    fn bump(&mut self) {
        if let Some(c) = self.peek() {
            self.at += c.len_utf8();
        }
    }

    fn skip_while(&mut self, test: impl Fn(char) -> bool) {
        while self.peek().is_some_and(&test) {
            self.bump();
        }
    }

    fn skip_whitespace(&mut self, allow_eol: bool) {
        self.skip_while(|c| space_or_tab(c) || (allow_eol && line_ending(c)));
    }

    /// `[label]` with balanced brackets; returns false and leaves the cursor unspecified on failure.
    fn label(&mut self, allow_eol: bool) -> bool {
        self.bump();
        let mut balance = 0u32;
        let mut escapes = 0u32;
        loop {
            let Some(c) = self.peek() else { return false };
            if escapes > 999 {
                return false;
            }
            if c == '[' {
                balance += 1;
                if balance > 32 {
                    return false;
                }
            }
            if c == ']' {
                if balance == 0 {
                    self.bump();
                    return true;
                }
                balance -= 1;
            }
            if line_ending(c) && !allow_eol {
                return false;
            }
            self.bump();
            if c == '\\' && matches!(self.peek(), Some('[' | '\\' | ']')) {
                self.bump();
                escapes += 1;
            }
        }
    }

    /// `{name="value" #id .class}`; values keep backslashes and decode character references.
    fn attributes(&mut self, allow_eol: bool) -> Option<DirectiveAttributes> {
        self.bump();
        let mut list: Vec<(String, String)> = Vec::new();
        let source = self.source;
        loop {
            let c = self.peek()?;
            if c == '#' || c == '.' {
                self.bump();
                let first = self.peek()?;
                if matches!(first, '"' | '#' | '\'' | '.' | '<' | '=' | '>' | '`' | '}')
                    || line_ending_or_space(first)
                {
                    return None;
                }
                let start = self.at;
                loop {
                    let c = self.peek()?;
                    if matches!(c, '"' | '\'' | '<' | '=' | '>' | '`') {
                        return None;
                    }
                    if matches!(c, '#' | '.' | '}') || line_ending_or_space(c) {
                        break;
                    }
                    self.bump();
                }
                let name = if c == '#' { "id" } else { "class" };
                list.push((
                    name.into(),
                    decode_attribute_references(&source[start..self.at]),
                ));
                continue;
            }
            if space_or_tab(c) || (allow_eol && line_ending(c)) {
                self.skip_whitespace(allow_eol);
                continue;
            }
            if line_ending(c) || is_js_space(c) || (unicode_punctuation(c) && c != '-' && c != '_')
            {
                return (c == '}').then(|| {
                    self.bump();
                    clean_attributes(list)
                });
            }
            let start = self.at;
            self.bump();
            self.skip_while(|c| {
                !(line_ending(c)
                    || is_js_space(c)
                    || (unicode_punctuation(c) && !matches!(c, '-' | '.' | ':' | '_')))
            });
            list.push((source[start..self.at].into(), String::new()));
            self.skip_whitespace(allow_eol);
            if self.peek() != Some('=') {
                continue;
            }
            self.bump();
            let value = loop {
                let c = self.peek()?;
                if matches!(c, '<' | '=' | '>' | '`' | '}') || (!allow_eol && line_ending(c)) {
                    return None;
                }
                if c == '"' || c == '\'' {
                    self.bump();
                    let start = self.at;
                    loop {
                        let next = self.peek()?;
                        if next == c {
                            break;
                        }
                        if line_ending(next) && !allow_eol {
                            return None;
                        }
                        self.bump();
                    }
                    let value = &source[start..self.at];
                    self.bump();
                    if !self
                        .peek()
                        .is_some_and(|after| after == '}' || line_ending_or_space(after))
                    {
                        return None;
                    }
                    break value;
                }
                if line_ending_or_space(c) {
                    self.skip_whitespace(allow_eol);
                    continue;
                }
                let start = self.at;
                loop {
                    let c = self.peek()?;
                    if matches!(c, '"' | '\'' | '<' | '=' | '>' | '`') {
                        return None;
                    }
                    if c == '}' || line_ending_or_space(c) {
                        break;
                    }
                    self.bump();
                }
                break &source[start..self.at];
            };
            if !value.is_empty() {
                list.last_mut().expect("attribute name").1 = decode_attribute_references(value);
            }
        }
    }
}

fn clean_attributes(list: Vec<(String, String)>) -> DirectiveAttributes {
    let mut cleaned = DirectiveAttributes::new();
    for (name, value) in list {
        match cleaned.get_mut(&name) {
            Some(class) if name == "class" && !class.is_empty() => {
                class.push(' ');
                class.push_str(&value);
            }
            _ => {
                cleaned.insert(name, value);
            }
        }
    }
    cleaned
}

/// `:codex-file-citation[label]{attributes}` at `start`; the label and attributes are
/// optional, so a malformed tail ends the directive after its name.
fn text_directive(source: &str, start: usize) -> Option<(usize, DirectiveAttributes)> {
    let name_end = start + 1 + FILE_CITATION_NAME.len();
    if !source[start + 1..].starts_with(FILE_CITATION_NAME) {
        return None;
    }
    let mut cursor = Cursor {
        source,
        at: name_end,
    };
    if !name_ends(cursor.peek()) || cursor.peek() == Some(':') {
        return None;
    }
    if cursor.peek() == Some('[') {
        let mut label = Cursor {
            source,
            at: cursor.at,
        };
        if label.label(true) {
            cursor.at = label.at;
        }
    }
    let mut attributes = DirectiveAttributes::new();
    if cursor.peek() == Some('{') {
        let mut attempt = Cursor {
            source,
            at: cursor.at,
        };
        if let Some(parsed) = attempt.attributes(true) {
            cursor.at = attempt.at;
            attributes = parsed;
        }
    }
    Some((cursor.at, attributes))
}

/// `::artifact-template[label]{attributes}` filling the rest of its line.
fn leaf_directive(source: &str, start: usize) -> Option<(usize, DirectiveAttributes)> {
    let rest = source[start..].strip_prefix("::")?;
    if !rest.starts_with(ARTIFACT_TEMPLATE_NAME) {
        return None;
    }
    let mut cursor = Cursor {
        source,
        at: start + 2 + ARTIFACT_TEMPLATE_NAME.len(),
    };
    if !name_ends(cursor.peek()) {
        return None;
    }
    if cursor.peek() == Some('[') && !cursor.label(false) {
        return None;
    }
    let mut attributes = DirectiveAttributes::new();
    if cursor.peek() == Some('{') {
        attributes = cursor.attributes(false)?;
    }
    cursor.skip_whitespace(false);
    cursor
        .peek()
        .is_none_or(line_ending)
        .then_some((cursor.at, attributes))
}

fn parse(source: &str) -> Node {
    // CommonMark has no syntax errors; only the disabled MDX extensions can fail.
    ::markdown::to_mdast(source, &ParseOptions::default()).expect("CommonMark is infallible")
}

fn span(node: &Node) -> Option<(usize, usize)> {
    node.position()
        .map(|position| (position.start.offset, position.end.offset))
}

fn column(source: &str, offset: usize) -> usize {
    let line_start = source[..offset].rfind('\n').map_or(0, |index| index + 1);
    source[line_start..offset].chars().fold(0, |column, c| {
        if c == '\t' {
            (column / 4 + 1) * 4
        } else {
            column + 1
        }
    })
}

/// Leaf directives start a paragraph line (and may interrupt the paragraph), so they are
/// found in the lines of paragraphs and setext headings; an ATX heading's line starts
/// with `#` and never matches.
fn leaf_directives(
    source: &str,
    node: &Node,
    found: &mut Vec<(usize, usize, DirectiveAttributes)>,
) {
    if matches!(node, Node::Paragraph(_) | Node::Heading(_))
        && let Some((start, end)) = span(node)
    {
        // Container markers and indentation; a paragraph's position includes its indentation.
        let content_start = |line_start: usize| {
            source[line_start..]
                .find(|c| !matches!(c, ' ' | '\t' | '>'))
                .map_or(source.len(), |skip| line_start + skip)
        };
        let mut content = content_start(start);
        let first_column = column(source, content);
        while content < end {
            // A continuation line indented by four or more columns stays paragraph text.
            if column(source, content) < first_column + 4
                && let Some((directive_end, attributes)) = leaf_directive(source, content)
            {
                found.push((content, directive_end, attributes));
            }
            let Some(line_end) = source[content..end].find('\n') else {
                break;
            };
            content = content_start(content + line_end + 1);
        }
        return;
    }
    for child in node.children().into_iter().flatten() {
        leaf_directives(source, child, found);
    }
}

enum TextContext {
    /// Inline text of the paragraph or heading ending at `block_end`, inside links whose
    /// labels close at `link_label_ends`.
    Text {
        link_label_ends: Vec<usize>,
        block_end: usize,
    },
    Other,
}

fn text_context(source: &str, root: &Node, offset: usize) -> TextContext {
    let mut node = root;
    let mut link_label_ends = Vec::new();
    let mut block_end = None;
    loop {
        match node {
            Node::Text(_) => {
                return block_end.map_or(TextContext::Other, |block_end| TextContext::Text {
                    link_label_ends,
                    block_end,
                });
            }
            Node::Paragraph(_) | Node::Heading(_) => block_end = span(node).map(|(_, end)| end),
            Node::Link(_)
                if span(node).is_some_and(|(start, _)| source[start..].starts_with('<')) =>
            {
                return TextContext::Other;
            }
            Node::Link(_) | Node::LinkReference(_) => link_label_ends.extend(
                node.children()
                    .and_then(|children| children.last())
                    .and_then(span)
                    .map(|(_, end)| end),
            ),
            _ => {}
        }
        let child =
            node.children().into_iter().flatten().find(|child| {
                span(child).is_some_and(|(start, end)| start <= offset && offset < end)
            });
        match child {
            Some(child) => node = child,
            None => return TextContext::Other,
        }
    }
}

fn backslashes_before(source: &str, offset: usize) -> usize {
    source.as_bytes()[..offset]
        .iter()
        .rev()
        .take_while(|&&b| b == b'\\')
        .count()
}

fn neutralize(work: &mut [u8], start: usize, end: usize, filler: u8) {
    for byte in &mut work[start..end] {
        if !matches!(*byte, b'\n' | b'\r') {
            *byte = filler;
        }
    }
}

fn directive_matches(source: &str) -> Vec<DirectiveMatch> {
    let mut leaves = Vec::new();
    leaf_directives(source, &parse(source), &mut leaves);
    let mut matches = Vec::new();
    // Inline syntax is decided on a copy where consumed directives are inert text.
    let mut work = source.as_bytes().to_vec();
    for (start, end, attributes) in &leaves {
        neutralize(&mut work, *start, *end, b' ');
        if let Some(template) = artifact_template(attributes) {
            matches.push(DirectiveMatch {
                start: *start,
                end: *end,
                content: DirectiveContent::ArtifactTemplate(template),
            });
        }
    }
    let mut root = None;
    let mut consumed_until = 0;
    for (start, _) in source.match_indices(':') {
        let inside_leaf = leaves
            .iter()
            .any(|(leaf_start, leaf_end, _)| (*leaf_start..*leaf_end).contains(&start));
        let previous_is_marker = start > 0
            && source.as_bytes()[start - 1] == b':'
            && backslashes_before(source, start - 1).is_multiple_of(2);
        if start < consumed_until
            || inside_leaf
            || previous_is_marker
            || !backslashes_before(source, start).is_multiple_of(2)
            || text_directive(source, start).is_none()
        {
            continue;
        }
        let work_text = std::str::from_utf8(&work).expect("filler keeps UTF-8");
        let tree = root.get_or_insert_with(|| parse(work_text));
        let TextContext::Text {
            link_label_ends,
            block_end,
        } = text_context(work_text, tree, start)
        else {
            continue;
        };
        let Some((end, attributes)) = text_directive(&source[..block_end], start) else {
            continue;
        };
        // A link whose closing bracket the directive consumes does not exist.
        let in_link = link_label_ends.iter().any(|&label_end| label_end >= end);
        if !in_link && let Some(citation) = file_citation_link(&attributes) {
            matches.push(DirectiveMatch {
                start,
                end,
                content: DirectiveContent::FileCitation(citation),
            });
        }
        neutralize(&mut work, start, end, b'a');
        root = None;
        consumed_until = end;
    }
    matches.sort_by_key(|found| found.start);
    matches
}

fn render_directive_matches(
    markdown: &str,
    replacement: impl Fn(&DirectiveContent) -> Option<String>,
) -> String {
    let mut rendered = markdown.to_owned();
    for found in directive_matches(markdown).iter().rev() {
        if let Some(text) = replacement(&found.content) {
            rendered.replace_range(found.start..found.end, &text);
        }
    }
    rendered
}

/// Native Markdown renderers use this adapter because they cannot render directives.
pub fn render_file_citations_as_markdown(markdown: &str) -> String {
    if !markdown.contains(&format!(":{FILE_CITATION_NAME}")) {
        return markdown.to_owned();
    }
    render_directive_matches(markdown, |content| match content {
        DirectiveContent::FileCitation(citation) => Some(file_citation_markdown(citation)),
        DirectiveContent::ArtifactTemplate(_) => None,
    })
}

/// Matches the Markdown emitted when users copy rendered directive UI.
pub fn render_directives_for_copy(markdown: &str) -> String {
    if !markdown.contains(&format!(":{FILE_CITATION_NAME}"))
        && !markdown.contains(&format!("::{ARTIFACT_TEMPLATE_NAME}"))
    {
        return markdown.to_owned();
    }
    render_directive_matches(markdown, |content| {
        Some(match content {
            DirectiveContent::FileCitation(citation) => file_citation_markdown(citation),
            DirectiveContent::ArtifactTemplate(template) => format!(
                "{} ({})",
                template.display_name,
                artifact_template_presentation_label(template.artifact_kind)
            ),
        })
    })
}

/// Native renderers split cards out because they cannot host a view inside Markdown text.
pub fn split_artifact_template_markdown(markdown: &str) -> Vec<ArtifactTemplateMarkdownSegment> {
    let whole = || {
        vec![ArtifactTemplateMarkdownSegment::Markdown {
            markdown: markdown.to_owned(),
            source_offset: 0,
        }]
    };
    if !markdown.contains(&format!("::{ARTIFACT_TEMPLATE_NAME}")) {
        return whole();
    }
    let templates: Vec<_> = directive_matches(markdown)
        .into_iter()
        .filter_map(|found| match found.content {
            DirectiveContent::ArtifactTemplate(template) => {
                Some((found.start, found.end, template))
            }
            DirectiveContent::FileCitation(_) => None,
        })
        .collect();
    if templates.is_empty() {
        return whole();
    }
    let mut segments = Vec::new();
    let mut cursor = 0;
    for (start, end, template) in templates {
        if start > cursor {
            segments.push(ArtifactTemplateMarkdownSegment::Markdown {
                markdown: markdown[cursor..start].to_owned(),
                source_offset: utf16_offset(markdown, cursor),
            });
        }
        segments.push(ArtifactTemplateMarkdownSegment::ArtifactTemplate {
            source_offset: utf16_offset(markdown, start),
            template,
        });
        cursor = end;
    }
    if cursor < markdown.len() {
        segments.push(ArtifactTemplateMarkdownSegment::Markdown {
            markdown: markdown[cursor..].to_owned(),
            source_offset: utf16_offset(markdown, cursor),
        });
    }
    segments
}

// Named character references that HTML still decodes without a trailing semicolon.
const SEMICOLONLESS_REFERENCES: [&str; 106] = [
    "AElig", "AMP", "Aacute", "Acirc", "Agrave", "Aring", "Atilde", "Auml", "COPY", "Ccedil",
    "ETH", "Eacute", "Ecirc", "Egrave", "Euml", "GT", "Iacute", "Icirc", "Igrave", "Iuml", "LT",
    "Ntilde", "Oacute", "Ocirc", "Ograve", "Oslash", "Otilde", "Ouml", "QUOT", "REG", "THORN",
    "Uacute", "Ucirc", "Ugrave", "Uuml", "Yacute", "aacute", "acirc", "acute", "aelig", "agrave",
    "amp", "aring", "atilde", "auml", "brvbar", "ccedil", "cedil", "cent", "copy", "curren", "deg",
    "divide", "eacute", "ecirc", "egrave", "eth", "euml", "frac12", "frac14", "frac34", "gt",
    "iacute", "icirc", "iexcl", "igrave", "iquest", "iuml", "laquo", "lt", "macr", "micro",
    "middot", "nbsp", "not", "ntilde", "oacute", "ocirc", "ograve", "ordf", "ordm", "oslash",
    "otilde", "ouml", "para", "plusmn", "pound", "quot", "raquo", "reg", "sect", "shy", "sup1",
    "sup2", "sup3", "szlig", "thorn", "times", "uacute", "ucirc", "ugrave", "uml", "uuml",
    "yacute", "yen", "yuml",
];

/// HTML's replacements for numeric references to C1 controls and NUL.
fn replaced_numeric_reference(code: u64) -> Option<char> {
    Some(match code {
        0 => '\u{fffd}',
        128 => '€',
        130 => '‚',
        131 => 'ƒ',
        132 => '„',
        133 => '…',
        134 => '†',
        135 => '‡',
        136 => 'ˆ',
        137 => '‰',
        138 => 'Š',
        139 => '‹',
        140 => 'Œ',
        142 => 'Ž',
        145 => '‘',
        146 => '’',
        147 => '“',
        148 => '”',
        149 => '•',
        150 => '–',
        151 => '—',
        152 => '˜',
        153 => '™',
        154 => 'š',
        155 => '›',
        156 => 'œ',
        158 => 'ž',
        159 => 'Ÿ',
        _ => return None,
    })
}

/// The reference after the `&` at `start - 1`, decoded as an HTML attribute value does.
fn character_reference(value: &str, start: usize) -> Option<(String, usize)> {
    let bytes = value.as_bytes();
    if matches!(
        bytes.get(start),
        None | Some(b'\t' | b'\n' | 0x0c | b' ' | b'&' | b'<')
    ) {
        return None;
    }
    let run = |from: usize, test: fn(&u8) -> bool| {
        from + bytes[from..].iter().take_while(|b| test(b)).count()
    };
    if bytes[start] == b'#' {
        let hex = matches!(bytes.get(start + 1), Some(b'x' | b'X'));
        let digits_start = start + 1 + usize::from(hex);
        let digits_end = run(
            digits_start,
            if hex {
                u8::is_ascii_hexdigit
            } else {
                u8::is_ascii_digit
            },
        );
        if digits_end == digits_start {
            return None;
        }
        let radix = if hex { 16 } else { 10 };
        let code = value[digits_start..digits_end]
            .chars()
            .fold(0u64, |code, c| {
                code.saturating_mul(radix)
                    .saturating_add(u64::from(c.to_digit(radix as u32).unwrap_or(0)))
            });
        let decoded = if (0xd800..=0xdfff).contains(&code) || code > 0x10_ffff {
            '\u{fffd}'
        } else {
            replaced_numeric_reference(code)
                .or_else(|| char::from_u32(code as u32))
                .unwrap_or('\u{fffd}')
        };
        let end = digits_end + usize::from(bytes.get(digits_end) == Some(&b';'));
        return Some((decoded.to_string(), end));
    }
    let name_end = run(start, u8::is_ascii_alphanumeric);
    if name_end == start {
        return None;
    }
    if bytes.get(name_end) == Some(&b';')
        && let Some(decoded) = ::markdown::decode_named(&value[start..name_end], true)
    {
        return Some((decoded, name_end + 1));
    }
    let prefix_end = (start + 1..=name_end)
        .rev()
        .find(|&end| SEMICOLONLESS_REFERENCES.contains(&&value[start..end]))?;
    // In attributes, `&amp=` and `&ampx` stay literal.
    if bytes
        .get(prefix_end)
        .is_some_and(|&b| b == b'=' || b.is_ascii_alphanumeric())
    {
        return None;
    }
    Some((
        ::markdown::decode_named(&value[start..prefix_end], true)?,
        prefix_end,
    ))
}

fn decode_attribute_references(value: &str) -> String {
    if !value.contains('&') {
        return value.to_owned();
    }
    let mut decoded = String::with_capacity(value.len());
    let mut copied = 0;
    let mut index = 0;
    while let Some(offset) = value[index..].find('&') {
        let ampersand = index + offset;
        match character_reference(value, ampersand + 1) {
            Some((text, end)) => {
                decoded.push_str(&value[copied..ampersand]);
                decoded.push_str(&text);
                copied = end;
                index = end;
            }
            None => index = ampersand + 1,
        }
    }
    decoded.push_str(&value[copied..]);
    decoded
}

#[cfg(test)]
#[path = "directives_tests.rs"]
mod tests;

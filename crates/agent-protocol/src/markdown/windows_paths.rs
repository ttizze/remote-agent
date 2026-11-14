//! T3's remarkKeepWindowsPaths preserves authored Windows destinations unless
//! they contain character references. Apply that rule only to parsed links,
//! images and definitions; prose, code and escaped image syntax stay untouched.
use markdown::mdast::Node;

fn label_end(source: &str) -> Option<usize> {
    let mut depth = 0usize;
    let mut escaped = false;
    for (offset, ch) in source.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match ch {
            '\\' => escaped = true,
            '[' => depth += 1,
            ']' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(offset + 1);
                }
            }
            _ => {}
        }
    }
    None
}

fn destination(source: &str, definition: bool) -> Option<&str> {
    let tail = &source[label_end(source)?..];
    let tail = tail
        .strip_prefix(if definition { ":" } else { "(" })?
        .trim_start();
    let angle = tail.starts_with('<');
    let tail = if angle { &tail[1..] } else { tail };
    let mut depth = 0usize;
    let mut escaped = false;
    for (offset, ch) in tail.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match ch {
            '\\' => escaped = true,
            '>' if angle => return Some(&tail[..offset]),
            '(' if !angle => depth += 1,
            ')' if !angle => {
                if depth == 0 {
                    return Some(&tail[..offset]);
                }
                depth -= 1;
            }
            _ if !angle && ch.is_whitespace() => return Some(&tail[..offset]),
            _ => {}
        }
    }
    (!angle && definition).then_some(tail)
}

pub(super) fn preserve_destinations(node: &mut Node, source: &str) {
    let definition = matches!(node, Node::Definition(_));
    if matches!(node, Node::Image(_) | Node::Link(_) | Node::Definition(_))
        && let Some(position) = node.position()
        && let Some(authored) = source.get(position.start.offset..position.end.offset)
        && let Some(path) = destination(authored, definition)
        && crate::paths::is_windows_absolute_host_path(path)
        && !path.contains('&')
    {
        match node {
            Node::Image(image) => image.url = path.into(),
            Node::Link(link) => link.url = path.into(),
            Node::Definition(definition) => definition.url = path.into(),
            _ => {}
        }
    }
    for child in node.children_mut().into_iter().flatten() {
        preserve_destinations(child, source);
    }
}

//! Pinned TimelineMinimap geometry and display-only exchange previews.
/// A strip on T3 Code's timeline minimap.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MinimapMarker {
    /// The timeline row of the user message it jumps to.
    pub row: usize,
    /// Whether its message is among the rows in view, which lights the strip.
    pub in_view: bool,
    pub current: bool,
    /// Where it sits from the rail's top.
    pub top: f32,
}

/// The rail's spacing between strips, as T3 Code's.
const MINIMAP_STRIP_SPACING: f32 = 8.;
const MINIMAP_MIN_ITEMS: usize = 2;

/// How tall T3 Code's rail is for `count` strips: 8 points apart, at most
/// `max_height` (the timeline's height less 14.75rem, at least 4rem).
pub fn minimap_rail_height(count: usize, max_height: f32) -> f32 {
    (count.saturating_sub(1) as f32 * MINIMAP_STRIP_SPACING)
        .max(1.)
        .min(max_height.max(1.))
}

/// As T3 Code's minimap: a strip for each user message once there are two,
/// spread evenly over the rail, each lit while its message is in `viewable`
/// (the first and last rows in view).
pub fn minimap_markers(
    user_rows: &[usize],
    viewable: Option<(usize, usize)>,
    max_height: f32,
) -> Vec<MinimapMarker> {
    if user_rows.len() < MINIMAP_MIN_ITEMS {
        return Vec::new();
    }
    let height = minimap_rail_height(user_rows.len(), max_height);
    let last = (user_rows.len() - 1) as f32;
    let current = viewable.and_then(|(first, last)| {
        user_rows
            .iter()
            .position(|row| (first..=last).contains(row))
            .or_else(|| user_rows.iter().rposition(|row| *row <= first))
    });
    user_rows
        .iter()
        .enumerate()
        .map(|(index, &row)| MinimapMarker {
            row,
            in_view: viewable.is_some_and(|(first, last)| (first..=last).contains(&row)),
            current: current == Some(index),
            top: index as f32 / last * height,
        })
        .collect()
}

/// The side gutter T3 Code's minimap needs to stay shown.
const MINIMAP_PERSISTENT_GUTTER: f32 = 48.;

/// As T3 Code's: the minimap stays shown while the chat canvas (`canvas_width`:
/// the conversation's whole width with its side panel, which T3 Code floats
/// over its canvas) leaves 48 points beside a centred lane `lane_width` wide.
/// Over narrower gutters it shows while the pointer is on it.
pub fn minimap_persistent(canvas_width: f32, lane_width: f32) -> bool {
    (canvas_width - lane_width.min(canvas_width)) / 2. >= MINIMAP_PERSISTENT_GUTTER
}

/// Where the strips start from the minimap's leading edge.
pub const MINIMAP_STRIP_INSET: f32 = 12.;

/// How wide the strips' pointer area is: T3 Code's 40 points at most, and,
/// so that it never covers message text, no further than the lane, which
/// starts `gutter` points from the minimap's edge.
pub fn minimap_hit_width(gutter: f32) -> f32 {
    (gutter.floor() - MINIMAP_STRIP_INSET).clamp(0., 40.)
}

/// A strip's length: T3 Code's 24 point strip, scaled toward the hovered one.
pub fn minimap_marker_width(distance_from_hover: Option<usize>) -> f32 {
    24. * match distance_from_hover {
        Some(0) => 1.,
        Some(1) => 0.667,
        Some(2) => 0.417,
        _ => 0.333,
    }
}

pub fn minimap_max_height(timeline_height: f32) -> f32 {
    (timeline_height - 236.).max(64.)
}

/// Prefer the first visible user message, otherwise the exchange being read.
pub fn minimap_current_index(rows: &[usize], visible: Option<(usize, usize)>) -> Option<usize> {
    let (first, last) = visible?;
    let mut preceding = None;
    for (index, &row) in rows.iter().enumerate() {
        if row >= first {
            return if row <= last { Some(index) } else { preceding };
        }
        preceding = Some(index);
    }
    preceding
}

pub fn minimap_pointer_index(count: usize, y: f32, height: f32) -> Option<usize> {
    (count > 0 && height > 0.)
        .then(|| ((y / height).clamp(0., 1.) * count.saturating_sub(1) as f32).round() as usize)
}

pub fn minimap_navigation(
    current: Option<usize>,
    count: usize,
    at_end: bool,
) -> (Option<usize>, Option<usize>) {
    let current = current.filter(|&index| index < count);
    (
        current.and_then(|index| index.checked_sub(1)),
        current
            .filter(|&index| !at_end && index + 1 < count)
            .map(|index| index + 1),
    )
}

/// Last displayed response before the next user, excluding work commentary.
fn last_answer_in_exchange(
    user: usize,
    next_user: Option<usize>,
    answers: &[usize],
) -> Option<usize> {
    answers
        .iter()
        .copied()
        .rfind(|&row| row > user && next_user.is_none_or(|end| row < end))
}

#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
pub struct MinimapExchange {
    pub row: usize,
    pub answer: Option<usize>,
}

/// The map and its exchange catalog are absent below the source's minimum.
pub fn minimap_exchanges(users: &[usize], answers: &[usize]) -> Vec<MinimapExchange> {
    if users.len() < MINIMAP_MIN_ITEMS {
        return Vec::new();
    }
    users
        .iter()
        .enumerate()
        .map(|(index, &row)| MinimapExchange {
            row,
            answer: last_answer_in_exchange(row, users.get(index + 1).copied(), answers),
        })
        .collect()
}

fn html_text(html: &str) -> String {
    use crate::presentation::markdown::MARKDOWN_BLOCK_TAGS;
    use regex::{Captures, Regex};
    use std::sync::LazyLock;
    static COMMENTS: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"<!--[\s\S]*?(?:-->|$)").unwrap());
    static TAGS: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"</?((?i-u:[a-z][\w-]*))(?:[^"'<>]|"[^"]*"|'[^']*')*>"#).unwrap()
    });
    TAGS.replace_all(&COMMENTS.replace_all(html, ""), |capture: &Captures<'_>| {
        if MARKDOWN_BLOCK_TAGS.contains(&capture[1].to_ascii_lowercase().as_str()) {
            " "
        } else {
            ""
        }
    })
    .into_owned()
}

// Pinned parseThreadLinkHref accepts nonblank branded ids, and hosted ids
// additionally require the exact hosted path grammar and valid URI decoding.
fn hosted_id(segment: &str) -> bool {
    let mut bytes = Vec::new();
    let mut source = segment.bytes();
    while let Some(byte) = source.next() {
        match byte {
            b'%' => {
                let hex = |byte: u8| (byte as char).to_digit(16).map(|value| value as u8);
                let Some(value) = source.next().and_then(hex).zip(source.next().and_then(hex))
                else {
                    return false;
                };
                bytes.push(value.0 * 16 + value.1);
            }
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_' | b'-' => bytes.push(byte),
            _ => return false,
        }
    }
    String::from_utf8(bytes).is_ok_and(|id| !crate::text::trim(&id).is_empty())
}

fn thread_link(text: &str) -> bool {
    if let Some(id) = text.strip_prefix("t3-thread://v1/") {
        return !crate::text::trim(id).is_empty();
    }
    if let Some(path) = text.strip_prefix("t3-thread://v2/") {
        return path.split_once('/').is_some_and(|(environment, thread)| {
            !crate::text::trim(environment).is_empty() && !crate::text::trim(thread).is_empty()
        });
    }
    text.strip_prefix("https://app.t3.codes/threads/")
        .and_then(|path| path.split_once('/'))
        .is_some_and(|(environment, thread)| hosted_id(environment) && hosted_id(thread))
}

fn markdown_text(node: &::markdown::mdast::Node) -> String {
    use ::markdown::mdast::Node;
    match node {
        Node::Text(value) => value.value.clone(),
        Node::Code(value) => value.value.clone(),
        Node::InlineCode(value) => value.value.clone(),
        Node::Html(value) => html_text(&value.value),
        Node::Image(value) => value.alt.clone(),
        Node::ImageReference(value) => value.alt.clone(),
        Node::Break(_) => " ".into(),
        _ => {
            let separator = if matches!(
                node,
                Node::Paragraph(_)
                    | Node::Heading(_)
                    | Node::TableCell(_)
                    | Node::Emphasis(_)
                    | Node::Strong(_)
                    | Node::Delete(_)
                    | Node::Link(_)
                    | Node::LinkReference(_)
            ) {
                ""
            } else {
                " "
            };
            let text = node
                .children()
                .into_iter()
                .flatten()
                .map(markdown_text)
                .collect::<Vec<_>>()
                .join(separator);
            if matches!(node, Node::Link(_)) && thread_link(&text) {
                "a thread".into()
            } else {
                text
            }
        }
    }
}

fn compact(text: &str) -> Option<String> {
    let text = text
        .split(crate::text::is_space)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    (!text.is_empty()).then_some(text)
}

/// The user's authored text and bounded, rendered assistant text. Parsing is
/// deferred until hover, so streaming a large answer never reparses it per tick.
pub fn minimap_preview(user: &str, assistant: Option<&str>) -> (Option<String>, Option<String>) {
    let answer = assistant.and_then(|text| {
        let prefix = String::from_utf16_lossy(&text.encode_utf16().take(1500).collect::<Vec<_>>());
        let tree = ::markdown::to_mdast(&prefix, &::markdown::ParseOptions::gfm())
            .expect("GFM is infallible");
        compact(&markdown_text(&tree))
    });
    (compact(user), answer)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_exchange_and_pointer_boundaries_follow_the_pinned_contract() {
        let rows = [1, 5, 9];
        assert_eq!(minimap_current_index(&rows, None), None);
        assert_eq!(minimap_current_index(&rows, Some((0, 0))), None);
        assert_eq!(minimap_current_index(&rows, Some((2, 4))), Some(0));
        assert_eq!(minimap_current_index(&rows, Some((2, 5))), Some(1));
        assert_eq!(minimap_current_index(&rows, Some((10, 12))), Some(2));
        assert_eq!(minimap_pointer_index(0, 3., 16.), None);
        assert_eq!(minimap_pointer_index(3, 3., 0.), None);
        assert_eq!(minimap_pointer_index(3, -9., 16.), Some(0));
        assert_eq!(minimap_pointer_index(3, 4., 16.), Some(1));
        assert_eq!(minimap_pointer_index(3, 99., 16.), Some(2));
        assert_eq!(minimap_max_height(200.), 64.);
        assert_eq!(minimap_max_height(500.), 264.);
        assert_eq!(minimap_navigation(Some(0), 3, false), (None, Some(1)));
        assert_eq!(minimap_navigation(Some(0), 3, true), (None, None));
        assert_eq!(minimap_navigation(Some(2), 3, false), (Some(1), None));
        assert_eq!(minimap_navigation(None, 3, false), (None, None));
        assert_eq!(minimap_navigation(Some(3), 3, false), (None, None));
    }

    #[test]
    fn preview_uses_the_final_response_and_rendered_text_without_live_controls() {
        assert_eq!(last_answer_in_exchange(1, Some(8), &[0, 2, 4, 9]), Some(4));
        assert_eq!(last_answer_in_exchange(8, None, &[2, 4, 9]), Some(9));
        assert_eq!(last_answer_in_exchange(10, None, &[9]), None);
        assert!(minimap_exchanges(&[1], &[2]).is_empty());
        assert_eq!(
            minimap_exchanges(&[1, 8, 10], &[0, 2, 4, 9]),
            vec![
                MinimapExchange {
                    row: 1,
                    answer: Some(4)
                },
                MinimapExchange {
                    row: 8,
                    answer: Some(9)
                },
                MinimapExchange {
                    row: 10,
                    answer: None
                },
            ]
        );
        let (user, answer) = minimap_preview(
            " \u{feff}**user**\n text ",
            Some(
                "## Heading\n\n**bold** [label](https://ignored.test) ![alt](image.png)\n\n<div>one</div><p>two</p><!--secret-->",
            ),
        );
        assert_eq!(user.as_deref(), Some("**user** text"));
        assert_eq!(answer.as_deref(), Some("Heading bold label alt one two"));
        assert_eq!(
            minimap_preview("\u{0085}", Some("\u{feff}")),
            (Some("\u{0085}".into()), None)
        );
        let long = format!("{}tail", "🙂".repeat(750));
        assert_eq!(minimap_preview("", Some(&long)).1, Some("🙂".repeat(750)));
    }

    #[test]
    fn thread_autolinks_are_named_without_hiding_invalid_or_authored_labels() {
        for href in [
            "t3-thread://v1/id",
            "t3-thread://v2/environment/id",
            "https://app.t3.codes/threads/env/a%2Fb",
        ] {
            assert_eq!(
                minimap_preview("", Some(&format!("<{href}>"))).1.as_deref(),
                Some("a thread")
            );
        }
        for href in [
            "t3-thread://v1/",
            "t3-thread://v2/env/",
            "https://app.t3.codes/threads/env/%GG",
            "https://app.t3.codes/threads/env/%FF",
            "https://app.t3.codes/threads/env/%20",
            "https://app.t3.codes/threads/env/id/extra",
        ] {
            assert_eq!(
                minimap_preview("", Some(&format!("<{href}>"))).1.as_deref(),
                Some(href)
            );
        }
        assert_eq!(
            minimap_preview("", Some("[Named](t3-thread://v1/id)"))
                .1
                .as_deref(),
            Some("Named")
        );
    }
}

/// Only message roles affect exchange boundaries; viewport-specific row data stays with its owner.
#[derive(Clone, Copy)]
pub enum MinimapRow {
    User,
    Assistant,
    Other,
}

/// Last visible assistant message after this user and before the next one.
/// Tool, progress and action rows do not change the exchange's preview.
pub fn minimap_answer_row(
    user_row: usize,
    rows: impl IntoIterator<Item = MinimapRow>,
) -> Option<usize> {
    let mut users = Vec::new();
    let mut answers = Vec::new();
    for (index, role) in rows.into_iter().enumerate() {
        match role {
            MinimapRow::User => users.push(index),
            MinimapRow::Assistant => answers.push(index),
            MinimapRow::Other => {}
        }
    }
    let index = users.iter().position(|row| *row == user_row)?;
    last_answer_in_exchange(user_row, users.get(index + 1).copied(), &answers)
}

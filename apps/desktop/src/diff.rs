use gpui_kit::{
    component::{
        button::{Button, ButtonVariants},
        h_flex, v_flex,
    },
    *,
};
use similar::{ChangeTag, TextDiff};
use std::{collections::HashSet, ops::Range};

struct Row {
    text: SharedString,
    old: Option<usize>,
    new: Option<usize>,
    kind: char,
    file: usize,
    emphasis: Vec<Range<usize>>,
}
/// The patch is parsed only when its source changes. Both views reuse these rows.
pub(crate) struct DiffView {
    source: SharedString,
    rows: Vec<Row>,
    visible: Vec<usize>,
    folded: HashSet<usize>,
    list: ListState,
    scroll: bool,
    limit: Option<usize>,
}
impl DiffView {
    pub(crate) fn new(source: SharedString, scroll: bool) -> Self {
        let rows = parse(&source);
        let mut view = Self {
            source,
            rows,
            visible: Vec::new(),
            folded: HashSet::new(),
            list: ListState::new(0, ListAlignment::Top, px(200.)),
            scroll,
            limit: if scroll { None } else { Some(300) },
        };
        view.rebuild();
        view
    }
    pub(crate) fn set_source(&mut self, source: &str, cx: &mut Context<Self>) {
        if self.source.as_ref() == source {
            return;
        }
        self.rows = parse(&source);
        self.source = source.to_owned().into();
        self.rebuild();
        cx.notify();
    }
    fn rebuild(&mut self) {
        self.visible.clear();
        self.visible.extend(
            self.rows
                .iter()
                .enumerate()
                .filter(|(_, row)| row.kind == 'F' || !self.folded.contains(&row.file))
                .map(|(ix, _)| ix),
        );
        self.list.reset(self.visible.len());
    }
    fn row(&self, ix: usize, cx: &Context<Self>) -> AnyElement {
        let row = &self.rows[self.visible[ix]];
        if row.kind == 'F' {
            let file = row.file;
            let folded = self.folded.contains(&file);
            return Button::new(("file", file))
                .label(format!("{} {}", if folded { "›" } else { "⌄" }, row.text))
                .ghost()
                .on_click(cx.listener(move |s, _, _, cx| {
                    if !s.folded.remove(&file) {
                        s.folded.insert(file);
                    }
                    s.rebuild();
                    cx.notify();
                }))
                .into_any_element();
        }
        let (background, color, strong) = match row.kind {
            '+' => (0x14291d, 0x9be9a8, 0x285a35),
            '-' => (0x321c20, 0xffa2a2, 0x723137),
            '@' => (0x232b38, 0x9cc7f5, 0x232b38),
            _ => (0x202020, 0xd0d0d0, 0x202020),
        };
        h_flex()
            .w_full()
            .min_h(px(23.))
            .font_family("Menlo")
            .text_size(px(12.))
            .bg(rgb(background))
            .text_color(rgb(color))
            .child(
                div()
                    .w(px(44.))
                    .flex_shrink_0()
                    .text_color(rgb(0x8b8b8b))
                    .child(row.old.map(|n| n.to_string()).unwrap_or_default()),
            )
            .child(
                div()
                    .w(px(44.))
                    .flex_shrink_0()
                    .text_color(rgb(0x8b8b8b))
                    .child(row.new.map(|n| n.to_string()).unwrap_or_default()),
            )
            .child(StyledText::new(row.text.clone()).with_highlights(
                row.emphasis.iter().cloned().map(|range| {
                    (
                        range,
                        HighlightStyle {
                            background_color: Some(rgb(strong).into()),
                            ..Default::default()
                        },
                    )
                }),
            ))
            .into_any_element()
    }
}
impl Render for DiffView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let source = self.source.clone();
        let mut body = v_flex().gap_2().w_full().child(
            Button::new("copy-patch")
                .label("差分をコピー")
                .ghost()
                .on_click(move |_, _, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(source.to_string()))
                }),
        );
        if self.scroll {
            let entity = cx.entity().downgrade();
            body = body.h_full().child(
                list(self.list.clone(), move |ix, _, cx| {
                    entity
                        .update(cx, |s, cx| s.row(ix, cx))
                        .unwrap_or_else(|_| div().into_any_element())
                })
                .flex_1()
                .min_h_0(),
            );
        } else {
            let count = self
                .limit
                .unwrap_or(self.visible.len())
                .min(self.visible.len());
            for ix in 0..count {
                body = body.child(self.row(ix, cx));
            }
            if count < self.visible.len() {
                body = body.child(
                    Button::new("show-more")
                        .label(format!("残り {} 行を表示", self.visible.len() - count))
                        .ghost()
                        .on_click(cx.listener(|s, _, _, cx| {
                            s.limit = None;
                            cx.notify();
                        })),
                );
            }
        }
        body
    }
}
fn parse(source: &str) -> Vec<Row> {
    let mut rows = Vec::new();
    let (mut old, mut new, mut file) = (None, None, 0usize);
    for line in source.lines() {
        let kind = if line.starts_with("diff --git ") {
            file += 1;
            old = None;
            new = None;
            'F'
        } else if line.starts_with("@@ ") {
            let mut parts = line.split_whitespace();
            parts.next();
            old = parts
                .next()
                .and_then(|p| p.strip_prefix('-'))
                .and_then(|p| p.split(',').next())
                .and_then(|p| p.parse().ok());
            new = parts
                .next()
                .and_then(|p| p.strip_prefix('+'))
                .and_then(|p| p.split(',').next())
                .and_then(|p| p.parse().ok());
            '@'
        } else if old.is_some() && line.starts_with('-') {
            '-'
        } else if new.is_some() && line.starts_with('+') {
            '+'
        } else if old.is_some() && line.starts_with(' ') {
            ' '
        } else {
            'M'
        };
        let old_line = if matches!(kind, '-' | ' ') { old } else { None };
        let new_line = if matches!(kind, '+' | ' ') { new } else { None };
        if old_line.is_some() {
            old = old.map(|n| n + 1);
        }
        if new_line.is_some() {
            new = new.map(|n| n + 1);
        }
        rows.push(Row {
            text: line.to_owned().into(),
            old: old_line,
            new: new_line,
            kind,
            file,
            emphasis: Vec::new(),
        });
    }
    // Pair consecutive removed/added lines within a hunk. Whole-line additions remain colored.
    let mut start = 0;
    while start < rows.len() {
        if rows[start].kind != '-' {
            start += 1;
            continue;
        }
        let mut split = start;
        while split < rows.len() && rows[split].kind == '-' {
            split += 1;
        }
        let mut end = split;
        while end < rows.len() && rows[end].kind == '+' {
            end += 1;
        }
        for offset in 0..(split - start).min(end - split) {
            let left = start + offset;
            let right = split + offset;
            let diff = TextDiff::from_words(&rows[left].text[1..], &rows[right].text[1..]);
            let (mut l, mut r) = (1, 1);
            let (mut deleted, mut added) = (Vec::new(), Vec::new());
            for change in diff.iter_all_changes() {
                let n = change.value().len();
                match change.tag() {
                    ChangeTag::Equal => {
                        l += n;
                        r += n;
                    }
                    ChangeTag::Delete => {
                        deleted.push(l..l + n);
                        l += n;
                    }
                    ChangeTag::Insert => {
                        added.push(r..r + n);
                        r += n;
                    }
                }
            }
            rows[left].emphasis = deleted;
            rows[right].emphasis = added;
        }
        start = end;
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;
    #[test]
    fn unified_patch_preserves_lines_numbers_and_unicode_word_changes() {
        let patch = "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -3,2 +3,2 @@\n-old 日本語 text\n+new 日本語 text\n unchanged\ndiff --git a/b b/b\nnew file mode 100644\n@@ -0,0 +1 @@\n+added\n";
        let rows = parse(patch);
        assert_eq!(
            rows.iter().map(|r| r.text.as_ref()).collect::<Vec<_>>(),
            patch.lines().collect::<Vec<_>>()
        );
        assert_eq!((rows[4].old, rows[4].new), (Some(3), None));
        assert_eq!((rows[5].old, rows[5].new), (None, Some(3)));
        assert_eq!((rows[6].old, rows[6].new), (Some(4), Some(4)));
        assert_eq!(rows[10].new, Some(1));
        assert_eq!(&rows[4].text[rows[4].emphasis[0].clone()], "old");
        assert_eq!(&rows[5].text[rows[5].emphasis[0].clone()], "new");
    }
}

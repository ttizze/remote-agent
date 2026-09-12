use gpui_kit::{
    component::{
        button::{Button, ButtonVariants},
        h_flex, v_flex,
    },
    *,
};
use similar::{ChangeTag, TextDiff};
use std::{
    collections::{HashMap, HashSet},
    ops::Range,
};

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
    file_names: HashMap<usize, SharedString>,
    folded: HashSet<usize>,
    context_folds: HashMap<usize, Range<usize>>,
    expanded_context: HashSet<usize>,
    list: ListState,
    scroll: bool,
    limit: Option<usize>,
}
impl DiffView {
    pub(crate) fn new(source: SharedString, scroll: bool) -> Self {
        let (rows, file_names) = parse(&source);
        let context_folds = context_folds(&rows);
        let mut view = Self {
            source,
            rows,
            visible: Vec::new(),
            file_names,
            folded: HashSet::new(),
            context_folds,
            expanded_context: HashSet::new(),
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
        (self.rows, self.file_names) = parse(source);
        self.context_folds = context_folds(&self.rows);
        self.folded.clear();
        self.expanded_context.clear();
        self.source = source.to_owned().into();
        self.rebuild();
        cx.notify();
    }
    fn rebuild(&mut self) {
        self.visible.clear();
        let mut ix = 0;
        while ix < self.rows.len() {
            let row = &self.rows[ix];
            let redundant_header =
                row.kind == 'M' && agent_core::presentation::diff::is_file_metadata(&row.text);
            if !redundant_header && (row.kind == 'F' || !self.folded.contains(&row.file)) {
                self.visible.push(ix);
                if let Some(range) = self.context_folds.get(&ix)
                    && !self.expanded_context.contains(&ix)
                {
                    ix = range.end;
                    continue;
                }
            }
            ix += 1;
        }
        self.list.reset(self.visible.len());
    }
    pub(crate) fn reveal_path(&mut self, path: &str, cx: &mut Context<Self>) -> bool {
        let Some(file) = self
            .file_names
            .iter()
            .find_map(|(file, name)| (name.as_ref() == path).then_some(*file))
        else {
            return false;
        };
        if self.folded.remove(&file) {
            self.rebuild();
        }
        if let Some(ix) = self
            .visible
            .iter()
            .position(|ix| self.rows[*ix].file == file)
        {
            self.list.scroll_to(ListOffset {
                item_ix: ix,
                offset_in_item: px(0.),
            });
            cx.notify();
        }
        true
    }
    fn row(&self, ix: usize, cx: &Context<Self>) -> AnyElement {
        let row = &self.rows[self.visible[ix]];
        let source_ix = self.visible[ix];
        if let Some(range) = self.context_folds.get(&source_ix)
            && !self.expanded_context.contains(&source_ix)
        {
            return Button::new(("context", source_ix))
                .label(format!("{} 行の未変更部分を表示", range.len()))
                .ghost()
                .on_click(cx.listener(move |s, _, _, cx| {
                    s.expanded_context.insert(source_ix);
                    s.rebuild();
                    cx.notify();
                }))
                .into_any_element();
        }
        if row.kind == 'F' {
            let file = row.file;
            let folded = self.folded.contains(&file);
            return Button::new(("file", file))
                .label(format!(
                    "{} {}",
                    if folded { "›" } else { "⌄" },
                    self.file_names.get(&file).unwrap_or(&row.text)
                ))
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
            .child(
                StyledText::new(
                    if row.kind == 'M' && row.text.starts_with("Binary files ") {
                        "バイナリファイルが変更されました".into()
                    } else {
                        row.text.clone()
                    },
                )
                .with_highlights(row.emphasis.iter().cloned().map(|range| {
                    (
                        range,
                        HighlightStyle {
                            background_color: Some(rgb(strong).into()),
                            ..Default::default()
                        },
                    )
                })),
            )
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
fn parse(source: &str) -> (Vec<Row>, HashMap<usize, SharedString>) {
    let rows = agent_core::presentation::diff::parse(source);
    let names = agent_core::presentation::diff::file_names(&rows)
        .into_iter()
        .map(|(file, path)| (file as usize, path.into()))
        .collect();
    let mut rows: Vec<Row> = rows
        .into_iter()
        .map(|row| Row {
            text: row.text.into(),
            old: row.old.map(|n| n as usize),
            new: row.new.map(|n| n as usize),
            kind: row.kind.chars().next().unwrap_or('M'),
            file: row.file as usize,
            emphasis: Vec::new(),
        })
        .collect();
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
    (rows, names)
}

fn context_folds(rows: &[Row]) -> HashMap<usize, Range<usize>> {
    let mut folds = HashMap::new();
    let mut start = 0;
    while start < rows.len() {
        if rows[start].kind != ' ' {
            start += 1;
            continue;
        }
        let mut end = start + 1;
        while end < rows.len() && rows[end].kind == ' ' {
            end += 1;
        }
        if end - start > 6 {
            folds.insert(start + 3, start + 3..end - 3);
        }
        start = end;
    }
    folds
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;
    #[test]
    fn folding_preserves_changed_lines_and_restores_hidden_context() {
        let patch = format!(
            "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1,13 +1,13 @@\n-old\n+new\n{}",
            " context\n".repeat(12)
        );
        let mut view = DiffView::new(patch.into(), true);
        let start = *view.context_folds.keys().next().unwrap();
        assert_eq!(view.context_folds[&start].len(), 6);
        assert!(
            view.visible
                .iter()
                .any(|index| view.rows[*index].kind == '+')
        );
        assert_eq!(view.visible.len(), view.rows.len() - 7);
        view.expanded_context.insert(start);
        view.rebuild();
        assert_eq!(view.visible.len(), view.rows.len() - 2);
        view.folded.insert(1);
        view.rebuild();
        assert_eq!(view.visible.len(), 1);
        view.folded.clear();
        view.rebuild();
        assert_eq!(view.visible.len(), view.rows.len() - 2);
    }
    #[test]
    fn unified_patch_preserves_lines_numbers_and_unicode_word_changes() {
        let patch = "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -3,2 +3,2 @@\n-old 日本語 text\n+new 日本語 text\n unchanged\ndiff --git a/b b/b\nnew file mode 100644\n@@ -0,0 +1 @@\n+added\n";
        let (rows, _) = parse(patch);
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

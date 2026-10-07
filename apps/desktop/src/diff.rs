use crate::app::color;
use gpui_kit::{
    component::{
        Icon,
        button::{Button, ButtonVariants},
        h_flex, v_flex,
    },
    prelude::FluentBuilder,
    *,
};
use std::{
    collections::{HashMap, HashSet},
    ops::Range,
};

const FONT_SIZE: f32 = 12.;

fn tint(role: &str, alpha: f32) -> Hsla {
    color(role).opacity(alpha)
}
const LINE_HEIGHT: f32 = 20.;
const NUMBER_WIDTH: f32 = 44.;

struct Row {
    text: SharedString,
    old: Option<usize>,
    new: Option<usize>,
    kind: char,
    file: usize,
}

/// How changed lines are laid out: one column, or old and new side by side.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum DiffLayout {
    #[default]
    Stacked,
    Split,
}

/// One line of the split layout: a row spanning both sides, or the old and
/// new halves of a changed or unchanged line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SplitRow {
    Full(usize),
    Pair(Option<usize>, Option<usize>),
}

/// The patch is parsed only when its source changes. Both views reuse these rows.
pub(crate) struct DiffView {
    source: SharedString,
    rows: Vec<Row>,
    visible: Vec<usize>,
    split: Vec<SplitRow>,
    file_names: HashMap<usize, SharedString>,
    stats: HashMap<String, (Option<u64>, Option<u64>)>,
    folded: HashSet<usize>,
    context_folds: HashMap<usize, Range<usize>>,
    expanded_context: HashSet<usize>,
    list: ListState,
    scroll: bool,
    limit: Option<usize>,
    layout: DiffLayout,
    wrap: bool,
}
impl DiffView {
    pub(crate) fn new(source: SharedString, scroll: bool) -> Self {
        let (rows, file_names) = parse(&source);
        let context_folds = context_folds(&rows);
        let mut view = Self {
            source,
            rows,
            visible: Vec::new(),
            split: Vec::new(),
            file_names,
            stats: HashMap::new(),
            folded: HashSet::new(),
            context_folds,
            expanded_context: HashSet::new(),
            list: ListState::new(0, ListAlignment::Top, px(200.)),
            scroll,
            limit: if scroll { None } else { Some(300) },
            layout: DiffLayout::Stacked,
            wrap: false,
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
    /// Per-file line counts for the file headers, by path.
    pub(crate) fn set_stats(&mut self, stats: HashMap<String, (Option<u64>, Option<u64>)>) {
        self.stats = stats;
    }
    pub(crate) fn set_layout(&mut self, layout: DiffLayout, wrap: bool, cx: &mut Context<Self>) {
        if self.layout == layout && self.wrap == wrap {
            return;
        }
        self.layout = layout;
        self.wrap = wrap;
        self.rebuild();
        cx.notify();
    }
    pub(crate) fn file_count(&self) -> usize {
        self.file_names.len()
    }
    pub(crate) fn all_folded(&self) -> bool {
        !self.file_names.is_empty() && self.file_names.keys().all(|f| self.folded.contains(f))
    }
    /// Collapses every file, or expands them all when all are collapsed.
    pub(crate) fn toggle_all_files(&mut self, cx: &mut Context<Self>) {
        if self.all_folded() {
            self.folded.clear();
        } else {
            self.folded = self.file_names.keys().copied().collect();
        }
        self.rebuild();
        cx.notify();
    }
    /// Expands `path` and scrolls its header to the top.
    pub(crate) fn reveal(&mut self, path: &str, cx: &mut Context<Self>) {
        let Some(file) = self
            .file_names
            .iter()
            .find(|(_, name)| name.as_ref() == path)
            .map(|(file, _)| *file)
        else {
            return;
        };
        self.folded.remove(&file);
        self.rebuild();
        let header = self
            .visible
            .iter()
            .position(|ix| self.rows[*ix].kind == 'F' && self.rows[*ix].file == file);
        let item = header.map(|position| match self.layout {
            DiffLayout::Stacked => position,
            DiffLayout::Split => self
                .split
                .iter()
                .position(|row| *row == SplitRow::Full(self.visible[position]))
                .unwrap_or(0),
        });
        if let Some(item_ix) = item {
            self.list.scroll_to(ListOffset {
                item_ix,
                offset_in_item: px(0.),
            });
        }
        cx.notify();
    }
    fn folded_context(&self, ix: usize) -> Option<&Range<usize>> {
        self.context_folds
            .get(&ix)
            .filter(|_| !self.expanded_context.contains(&ix))
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
                if let Some(range) = self.folded_context(ix) {
                    ix = range.end;
                    continue;
                }
            }
            ix += 1;
        }
        let kinds: Vec<char> = self.rows.iter().map(|row| row.kind).collect();
        self.split = split_rows(&kinds, &self.visible, |ix| {
            self.folded_context(ix).is_some()
        });
        self.list.reset(match self.layout {
            DiffLayout::Stacked => self.visible.len(),
            DiffLayout::Split => self.split.len(),
        });
    }
    fn item(&self, ix: usize, cx: &Context<Self>) -> AnyElement {
        match self.layout {
            DiffLayout::Stacked => self.row(self.visible[ix], cx),
            DiffLayout::Split => match self.split[ix] {
                SplitRow::Full(row) => self.row(row, cx),
                SplitRow::Pair(old, new) => h_flex()
                    .w_full()
                    .items_stretch()
                    .child(self.half(old, true))
                    .child(div().w(px(1.)).flex_shrink_0().bg(color("border")))
                    .child(self.half(new, false))
                    .into_any_element(),
            },
        }
    }
    fn file_header(&self, row: &Row, cx: &Context<Self>) -> AnyElement {
        let file = row.file;
        let folded = self.folded.contains(&file);
        let name = self.file_names.get(&file).unwrap_or(&row.text).clone();
        let stats = self.stats.get(name.as_ref()).copied();
        h_flex()
            .id(("diff-file", file))
            .w_full()
            .h(px(36.))
            .px_2()
            .gap_2()
            .flex_shrink_0()
            .cursor_pointer()
            .bg(color("codeBackground"))
            .border_y_1()
            .border_color(color("border"))
            .text_xs()
            .on_click(cx.listener(move |view, _, _, cx| {
                if !view.folded.remove(&file) {
                    view.folded.insert(file);
                }
                view.rebuild();
                cx.notify();
            }))
            .child(
                Icon::default()
                    .path(if folded {
                        "lucide/chevron-right.svg"
                    } else {
                        "lucide/chevron-down.svg"
                    })
                    .size_4()
                    .text_color(color("textMuted")),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .font_family("Menlo")
                    .font_weight(FontWeight::MEDIUM)
                    .child(name),
            )
            .when_some(stats, |header, (additions, deletions)| {
                header.child(diff_stat(additions, deletions))
            })
            .into_any_element()
    }
    fn row(&self, source_ix: usize, cx: &Context<Self>) -> AnyElement {
        let row = &self.rows[source_ix];
        if let Some(range) = self.folded_context(source_ix) {
            let count = range.len();
            return h_flex()
                .id(("diff-context", source_ix))
                .w_full()
                .h(px(28.))
                .px_3()
                .gap_2()
                .cursor_pointer()
                .bg(tint("textMuted", 0.06))
                .text_color(color("textMuted"))
                .text_xs()
                .hover(|row| row.text_color(color("text")))
                .on_click(cx.listener(move |view, _, _, cx| {
                    view.expanded_context.insert(source_ix);
                    view.rebuild();
                    cx.notify();
                }))
                .child(
                    Icon::default()
                        .path("lucide/chevrons-up-down.svg")
                        .size_3()
                        .text_color(color("textMuted")),
                )
                .child(format!("{count} unmodified lines"))
                .into_any_element();
        }
        match row.kind {
            'F' => self.file_header(row, cx),
            '@' | 'M' => h_flex()
                .w_full()
                .min_h(px(LINE_HEIGHT + 4.))
                .px_3()
                .bg(tint("textMuted", 0.06))
                .text_color(color("textMuted"))
                .font_family("Menlo")
                .text_size(px(FONT_SIZE))
                .child(
                    div()
                        .min_w_0()
                        .when(!self.wrap, |text| text.truncate())
                        .child(
                            if row.kind == 'M' && row.text.starts_with("Binary files ") {
                                SharedString::from("Binary file not shown")
                            } else {
                                row.text.clone()
                            },
                        ),
                )
                .into_any_element(),
            kind => line(kind, [row.old, row.new], row.text.clone(), self.wrap).into_any_element(),
        }
    }
    fn half(&self, row: Option<usize>, old: bool) -> AnyElement {
        let Some(row) = row.map(|ix| &self.rows[ix]) else {
            return div()
                .flex_1()
                .min_w_0()
                .bg(tint("textMuted", 0.04))
                .into_any_element();
        };
        let kind = match row.kind {
            ' ' => ' ',
            _ if old => '-',
            _ => '+',
        };
        div()
            .flex_1()
            .min_w_0()
            .child(line(
                kind,
                [if old { row.old } else { row.new }],
                row.text.clone(),
                self.wrap,
            ))
            .into_any_element()
    }
}

/// One code line: its numbers, then its text on the change's background.
fn line<const N: usize>(
    kind: char,
    numbers: [Option<usize>; N],
    text: SharedString,
    wrap: bool,
) -> impl IntoElement {
    let (background, gutter) = match kind {
        '+' => (
            tint("successForeground", 0.10),
            tint("successForeground", 0.18),
        ),
        '-' => (tint("error", 0.10), tint("error", 0.18)),
        _ => (color("codeBackground"), color("codeBackground")),
    };
    let content: SharedString = text.get(1..).unwrap_or_default().to_owned().into();
    h_flex()
        .w_full()
        .items_start()
        .min_h(px(LINE_HEIGHT))
        .font_family("Menlo")
        .text_size(px(FONT_SIZE))
        .line_height(px(LINE_HEIGHT))
        .bg(background)
        .text_color(color("codeForeground"))
        .children(numbers.into_iter().map(move |number| {
            div()
                .w(px(NUMBER_WIDTH))
                .flex_shrink_0()
                .self_stretch()
                .pr_2()
                .flex()
                .justify_end()
                .bg(gutter)
                .text_color(match kind {
                    '+' => color("successForeground"),
                    '-' => color("errorForeground"),
                    _ => tint("textMuted", 0.7),
                })
                .child(number.map(|n| n.to_string()).unwrap_or_default())
        }))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .pl_3()
                .pr_2()
                .when(!wrap, |text| text.whitespace_nowrap().overflow_hidden())
                .child(content),
        )
}

/// `+12 −3` in the diff colors.
pub(crate) fn diff_stat(additions: Option<u64>, deletions: Option<u64>) -> impl IntoElement {
    h_flex()
        .gap_1()
        .flex_shrink_0()
        .text_size(px(11.))
        .line_height(px(16.))
        .when_some(additions, |stat, n| {
            stat.child(
                div()
                    .text_color(color("successForeground"))
                    .child(format!("+{n}")),
            )
        })
        .when_some(deletions, |stat, n| {
            stat.child(
                div()
                    .text_color(color("errorForeground"))
                    .child(format!("−{n}")),
            )
        })
}

impl Render for DiffView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.scroll {
            let entity = cx.entity().downgrade();
            return v_flex().size_full().bg(color("codeBackground")).child(
                list(self.list.clone(), move |ix, _, cx| {
                    entity
                        .update(cx, |view, cx| view.item(ix, cx))
                        .unwrap_or_else(|_| div().into_any_element())
                })
                .flex_1()
                .min_h_0(),
            );
        }
        let source = self.source.clone();
        let mut body = v_flex().gap_2().w_full().child(
            Button::new("copy-patch")
                .label("Copy diff")
                .ghost()
                .on_click(move |_, _, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(source.to_string()))
                }),
        );
        let count = self
            .limit
            .unwrap_or(self.visible.len())
            .min(self.visible.len());
        for ix in 0..count {
            body = body.child(self.row(self.visible[ix], cx));
        }
        if count < self.visible.len() {
            body = body.child(
                Button::new("show-more")
                    .label(format!("Show {} more lines", self.visible.len() - count))
                    .ghost()
                    .on_click(cx.listener(|view, _, _, cx| {
                        view.limit = None;
                        cx.notify();
                    })),
            );
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
    let rows = rows
        .into_iter()
        .map(|row| Row {
            text: row.text.into(),
            old: row.old.map(|n| n as usize),
            new: row.new.map(|n| n as usize),
            kind: row.kind.chars().next().unwrap_or('M'),
            file: row.file as usize,
        })
        .collect();
    (rows, names)
}

/// Pairs the visible rows for the split layout: removed lines meet the added
/// lines that follow them, unchanged lines show on both sides, and headers
/// and folded context span the width.
fn split_rows(kinds: &[char], visible: &[usize], spans: impl Fn(usize) -> bool) -> Vec<SplitRow> {
    let mut rows = Vec::new();
    let mut at = 0;
    while at < visible.len() {
        let ix = visible[at];
        match kinds[ix] {
            _ if spans(ix) => {
                rows.push(SplitRow::Full(ix));
                at += 1;
            }
            ' ' => {
                rows.push(SplitRow::Pair(Some(ix), Some(ix)));
                at += 1;
            }
            '-' | '+' => {
                let run = |from: usize, kind: char| {
                    visible[from..]
                        .iter()
                        .take_while(|ix| kinds[**ix] == kind && !spans(**ix))
                        .count()
                };
                let removed = run(at, '-');
                let added = run(at + removed, '+');
                for offset in 0..removed.max(added) {
                    rows.push(SplitRow::Pair(
                        (offset < removed).then(|| visible[at + offset]),
                        (offset < added).then(|| visible[at + removed + offset]),
                    ));
                }
                at += removed + added;
            }
            _ => {
                rows.push(SplitRow::Full(ix));
                at += 1;
            }
        }
    }
    rows
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
    use super::{DiffView, SplitRow, split_rows};
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
    fn split_layout_pairs_removed_lines_with_the_added_lines_after_them() {
        let kinds = ['F', '@', ' ', '-', '-', '+', ' ', '+', '-', ' '];
        let visible: Vec<usize> = (0..kinds.len()).collect();
        assert_eq!(
            split_rows(&kinds, &visible, |ix| ix == 9),
            [
                SplitRow::Full(0),
                SplitRow::Full(1),
                SplitRow::Pair(Some(2), Some(2)),
                SplitRow::Pair(Some(3), Some(5)),
                SplitRow::Pair(Some(4), None),
                SplitRow::Pair(Some(6), Some(6)),
                SplitRow::Pair(None, Some(7)),
                SplitRow::Pair(Some(8), None),
                SplitRow::Full(9),
            ]
        );
    }

    #[test]
    fn split_layout_keeps_every_visible_row_once_per_side() {
        use proptest::prelude::*;
        proptest!(|(kinds in proptest::collection::vec(prop_oneof![Just(' '), Just('-'), Just('+'), Just('@')], 0..40))| {
            let visible: Vec<usize> = (0..kinds.len()).collect();
            let rows = split_rows(&kinds, &visible, |_| false);
            let (mut left, mut right) = (vec![], vec![]);
            for row in rows {
                let (old, new) = match row {
                    SplitRow::Full(ix) => (Some(ix), Some(ix)),
                    SplitRow::Pair(old, new) => (old, new),
                };
                left.extend(old);
                right.extend(new);
            }
            let side = |hidden: char| -> Vec<usize> {
                visible.iter().copied().filter(|ix| kinds[*ix] != hidden).collect()
            };
            prop_assert_eq!(left, side('+'));
            prop_assert_eq!(right, side('-'));
        });
    }
}

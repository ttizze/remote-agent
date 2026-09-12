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
        let rows = parse(&source);
        let context_folds = context_folds(&rows);
        let file_names = file_names(&rows);
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
        self.rows = parse(source);
        self.context_folds = context_folds(&self.rows);
        self.file_names = file_names(&self.rows);
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
            let redundant_header = row.kind == 'M'
                && ["index ", "--- ", "+++ "]
                    .iter()
                    .any(|prefix| row.text.starts_with(prefix));
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

fn file_names(rows: &[Row]) -> HashMap<usize, SharedString> {
    let mut names = HashMap::new();
    for row in rows {
        let line = row.text.as_ref();
        let path = if row.kind == 'F' {
            line.strip_prefix("diff --git ").and_then(|header| {
                if header.starts_with('"') {
                    let (_, rest) = quoted_path(header)?;
                    let new = decode_path(rest.trim_start())?;
                    return new.strip_prefix("b/").map(str::to_owned);
                }
                // Unquoted binary patches have no +++/--- lines. With no rename
                // their two paths are identical; spaces inside a path are legal.
                let middle = header.len().checked_sub(1)? / 2;
                let old = header.get(..middle)?.strip_prefix("a/")?;
                let new = header.get(middle..)?.strip_prefix(" b/")?;
                (old == new).then(|| new.to_owned())
            })
        } else if row.kind == 'M' {
            if let Some(path) = line
                .strip_prefix("rename to ")
                .or_else(|| line.strip_prefix("copy to "))
            {
                decode_path(path)
            } else {
                line.strip_prefix("+++ ")
                    .or_else(|| line.strip_prefix("--- "))
                    .filter(|path| *path != "/dev/null")
                    .and_then(decode_path)
                    .map(|path| {
                        path.strip_prefix("a/")
                            .or_else(|| path.strip_prefix("b/"))
                            .unwrap_or(&path)
                            .to_owned()
                    })
            }
        } else {
            None
        };
        if let Some(path) = path {
            names.insert(row.file, path.into());
        }
    }
    names
}

fn decode_path(path: &str) -> Option<String> {
    if path.starts_with('"') {
        let (path, rest) = quoted_path(path)?;
        (rest.is_empty() || rest.starts_with('\t')).then_some(path)
    } else {
        Some(path.split('\t').next()?.into())
    }
}

fn quoted_path(path: &str) -> Option<(String, &str)> {
    let bytes = path.strip_prefix('"')?.as_bytes();
    let mut output = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => return Some((String::from_utf8(output).ok()?, &path[i + 2..])),
            b'\\' => {
                i += 1;
                let byte = *bytes.get(i)?;
                output.push(match byte {
                    b'a' => 7,
                    b'b' => 8,
                    b't' => 9,
                    b'n' => 10,
                    b'v' => 11,
                    b'f' => 12,
                    b'r' => 13,
                    b'\\' | b'"' => byte,
                    b'0'..=b'7' => {
                        let mut value = u16::from(byte - b'0');
                        for _ in 0..2 {
                            if let Some(next @ b'0'..=b'7') = bytes.get(i + 1) {
                                value = value * 8 + u16::from(next - b'0');
                                i += 1;
                            } else {
                                break;
                            }
                        }
                        u8::try_from(value).ok()?
                    }
                    _ => return None,
                });
            }
            byte => output.push(byte),
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;
    #[test]
    fn file_navigation_matches_real_git_paths_including_renames_binary_and_unicode() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .current_dir(root)
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8(output.stdout).unwrap()
        };
        git(&["init", "--quiet"]);
        let names = [
            "a.txt",
            "space name.txt",
            "日本語.txt",
            "tab\tname.txt",
            "quoted\"name.txt",
            "line\nname.txt",
        ];
        for name in names {
            std::fs::write(root.join(name), "before\n").unwrap();
        }
        std::fs::write(root.join("binary.dat"), [0, 1, 2]).unwrap();
        std::fs::write(root.join("old.txt"), "rename me\n").unwrap();
        git(&["add", "."]);
        git(&[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--quiet",
            "-m",
            "fixture",
        ]);
        for name in names {
            std::fs::write(root.join(name), "after\n").unwrap();
        }
        std::fs::write(root.join("binary.dat"), [0, 3, 4]).unwrap();
        git(&["mv", "old.txt", "renamed 日本語.txt"]);
        for quoting in ["core.quotePath=true", "core.quotePath=false"] {
            let patch = git(&[
                "-c",
                quoting,
                "diff",
                "--no-ext-diff",
                "--no-color",
                "HEAD",
                "--",
            ]);
            let rows = parse(&patch);
            for name in names
                .into_iter()
                .chain(["binary.dat", "renamed 日本語.txt"])
            {
                assert!(
                    file_names(&rows).values().any(|path| path.as_ref() == name),
                    "cannot navigate to {name:?} ({quoting})"
                );
            }
            assert!(
                !file_names(&rows)
                    .values()
                    .any(|path| path.as_ref() == "not-present.txt")
            );
        }
    }

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

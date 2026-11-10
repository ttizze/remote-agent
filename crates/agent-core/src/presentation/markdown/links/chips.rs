//! Workspace file chips: authored file links and code spans in T3 Code's
//! grammar resolve against the workspace, and chips that share a basename name
//! the shortest parent that tells them apart.
use super::super::{MarkdownBlock, MarkdownRun};
use super::*;

/// The workspace chips resolve against, as a Host path, and the Host's home.
pub(in crate::presentation::markdown) struct Workspace<'a> {
    cwd: String,
    home: Option<&'a str>,
}

impl<'a> Workspace<'a> {
    pub(in crate::presentation::markdown) fn new(cwd: &str, home: Option<&'a str>) -> Self {
        Self {
            cwd: file_host_path(cwd.into(), home.map(str::to_owned)),
            home,
        }
    }

    /// The file `target` names, its path resolved on the Host. A relative path
    /// needs a workspace, and a network path outside it is not offered.
    pub(in crate::presentation::markdown) fn file(
        &self,
        target: &str,
        syntax: Syntax,
    ) -> Option<MarkdownFileReference> {
        let mut file = file_reference(target, syntax)?;
        let path = file_host_path(
            paths::resolve(&file.path, &self.cwd)?,
            self.home.map(str::to_owned),
        );
        if agent_protocol::paths::is_network_host_path(&path)
            && paths::relative(&path, &self.cwd).is_none()
        {
            return None;
        }
        file.path = path;
        Some(file)
    }
}

/// Relabels every chip of the document: a basename, the parent that tells it
/// apart from another chip's same basename, and its position.
pub(in crate::presentation::markdown) fn label_parents(blocks: &mut [MarkdownBlock]) {
    fn chips(blocks: &mut [MarkdownBlock]) -> impl Iterator<Item = &mut MarkdownRun> {
        blocks
            .iter_mut()
            .flat_map(|block| match block {
                MarkdownBlock::Paragraph { runs, .. } => vec![runs],
                MarkdownBlock::Table { rows, .. } => rows
                    .iter_mut()
                    .flatten()
                    .map(|cell| &mut cell.runs)
                    .collect(),
                MarkdownBlock::Visualization { .. } => Vec::new(),
            })
            .flatten()
            .filter(|run| run.file.is_some())
    }
    let paths: Vec<_> = chips(blocks)
        .filter_map(|run| run.file.as_ref().map(|file| file.path.clone()))
        .collect();
    let labels = file_labels(paths.iter().map(String::as_str));
    for run in chips(blocks) {
        let file = run.file.as_mut().expect("chips have a file");
        let parent = labels.get(&file.path.replace('\\', "/"));
        file.label = file_label(
            &file.path,
            parent.map(String::as_str),
            file.line,
            file.column,
        );
        run.text = file.label.clone();
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::{MarkdownBlock, MarkdownRun, workspace_markdown_blocks};

    fn runs(source: &str, cwd: &str, home: Option<&str>) -> Vec<MarkdownRun> {
        workspace_markdown_blocks(source.into(), cwd.into(), home.map(str::to_owned))
            .into_iter()
            .flat_map(|block| match block {
                MarkdownBlock::Paragraph { runs, .. } => runs,
                _ => Vec::new(),
            })
            .collect()
    }

    fn chips(source: &str, cwd: &str, home: Option<&str>) -> Vec<MarkdownRun> {
        runs(source, cwd, home)
            .into_iter()
            .filter(|run| run.file.is_some())
            .collect()
    }

    #[test]
    fn aliases_are_resolved_before_disambiguation_without_inventing_a_home() {
        let source = "[index.ts](src/a/index.ts) [index.ts](/repo/src/a/index.ts) [index.ts](~/src/b/index.ts)";
        let chips = chips(source, "/repo", Some("/home/actual"));
        let paths: Vec<_> = chips
            .iter()
            .map(|run| run.file.as_ref().unwrap().path.as_str())
            .collect();
        assert_eq!(
            paths,
            [
                "/repo/src/a/index.ts",
                "/repo/src/a/index.ts",
                "/home/actual/src/b/index.ts"
            ]
        );
        let labels: Vec<_> = chips.iter().map(|run| run.text.as_str()).collect();
        assert_eq!(
            labels,
            ["index.ts · src/a", "index.ts · src/a", "index.ts · src/b"]
        );
        // The links keep their authored targets, which opening resolves again.
        assert_eq!(chips[0].link.as_deref(), Some("src/a/index.ts"));
        let unresolved = self::chips("[a.rs](~/a.rs)", "/Users/guessed/repo", None);
        assert_eq!(unresolved[0].file.as_ref().unwrap().path, "~/a.rs");
        assert!(self::chips("[a.rs](src/a.rs)", "", None).is_empty());
    }

    #[test]
    fn authored_captions_keep_their_inline_formatting_beside_the_chip() {
        let source = "[# *設計* `src/a.rs`](docs/a.md)";
        let runs = runs(source, "/repo", None);
        let (chip, caption) = runs.split_last().unwrap();
        assert_eq!(chip.text, "a.md");
        assert_eq!(chip.file.as_ref().unwrap().path, "/repo/docs/a.md");
        assert_eq!(
            caption
                .iter()
                .map(|run| run.text.as_str())
                .collect::<String>(),
            "# 設計 src/a.rs"
        );
        assert!(caption.iter().any(|run| run.emphasis && run.text == "設計"));
        assert!(caption.iter().any(|run| run.code && run.text == "src/a.rs"));
        // The caption opens the same file and holds no chip of its own.
        assert!(
            caption
                .iter()
                .all(|run| run.file.is_none() && run.link.as_deref() == Some("docs/a.md"))
        );
        let plain = self::runs("[src/main.rs](src/main.rs)", "/repo", None);
        assert_eq!(plain.len(), 1, "a caption that names the file is the chip");
    }

    #[test]
    fn desktop_code_spans_follow_t3_and_fences_and_bare_text_are_not_chips() {
        let source = "`src/main.rs:4` `README.md` `origin/main` `src/日本語.rs` src/c.rs\n\n```text\nsrc/d.rs\n```";
        let chips = chips(source, "/repo", None);
        assert_eq!(chips.len(), 2);
        assert_eq!(chips[0].text, "main.rs · L4");
        assert_eq!(chips[0].link.as_deref(), Some("src/main.rs:4"));
        assert!(!chips[0].code, "a chip replaces the code span");
        assert_eq!(chips[1].file.as_ref().unwrap().path, "/repo/src/日本語.rs");
    }

    #[test]
    fn home_relative_workspaces_resolve_as_their_absolute_form() {
        let source = "[../src/main.rs:7](../src/main.rs:7) [**説明**](src/main.rs) [guide.md](~/docs/guide.md)";
        for (home, relative, absolute) in [
            ("/Users/fixture", "~/project", "/Users/fixture/project"),
            (
                r"C:\Users\fixture",
                r"~\project",
                r"C:\Users\fixture\project",
            ),
        ] {
            assert_eq!(
                runs(source, relative, Some(home)),
                runs(source, absolute, Some(home))
            );
            assert_eq!(chips(source, absolute, Some(home)).len(), 3);
        }
    }

    #[test]
    fn table_cells_share_the_document_disambiguation() {
        let source = "| a |\n|---|\n| `src/a/index.ts` |\n\n`src/b/index.ts`";
        let blocks = workspace_markdown_blocks(source.into(), "/repo".into(), None);
        let MarkdownBlock::Table { rows, .. } = &blocks[0] else {
            panic!("the table is kept");
        };
        assert_eq!(rows[1][0].runs[0].text, "index.ts · src/a");
        let MarkdownBlock::Paragraph { runs, .. } = &blocks[1] else {
            panic!("the paragraph is kept");
        };
        assert_eq!(runs[0].text, "index.ts · src/b");
    }
}

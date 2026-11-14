//! A Markdown file shown rendered instead of as its source, as T3 Code's file
//! panel (`FilePreviewPanel`, `FileMarkdownPreview`).
use super::super::markdown::{
    MarkdownBlock, MarkdownFileSurface, file_basename, markdown_file_target,
    workspace_markdown_blocks,
};

/// T3 Code's `isMarkdownPreviewFile`: the files whose panel offers the rendering.
pub fn is_markdown_preview_file(path: &str) -> bool {
    path.rsplit_once('.').is_some_and(|(_, extension)| {
        ["md", "mdx", "markdown", "mdown", "mkd"]
            .iter()
            .any(|known| extension.eq_ignore_ascii_case(known))
    })
}

/// Whether the panel of `path` shows its rendering: the reader's preference,
/// except that a link to a line shows the source at that line until the reader
/// asks for the rendering again.
pub fn file_renders_markdown(path: &str, preferred: bool, line_pending: bool) -> bool {
    preferred && !line_pending && is_markdown_preview_file(path)
}

/// The rendering of the Markdown file at the Host path `path`: its leading YAML
/// frontmatter is not shown, links name files of the workspace `cwd` as a
/// message's do, and relative images are those beside the file.
pub fn file_markdown_blocks(
    source: &str,
    path: &str,
    cwd: String,
    home: Option<String>,
) -> Vec<MarkdownBlock> {
    let mut blocks = workspace_markdown_blocks(without_frontmatter(source).into(), cwd, home);
    let folder = &path[..path.len() - file_basename(path).len()];
    let runs = blocks.iter_mut().flat_map(|block| match block {
        MarkdownBlock::Paragraph { runs, .. } => runs.iter_mut().collect::<Vec<_>>(),
        MarkdownBlock::Table { rows, .. } => rows
            .iter_mut()
            .flatten()
            .flat_map(|cell| &mut cell.runs)
            .collect(),
        MarkdownBlock::Visualization { .. } => Vec::new(),
    });
    for run in runs {
        // Web and inline images name no file and stay as written.
        if let Some(file) = run.image.as_ref().and_then(|image| {
            markdown_file_target(image.clone(), folder.into(), MarkdownFileSurface::Mobile)
        }) {
            run.image = Some(super::link_destination(&file.path));
        }
    }
    blocks
}

/// `markdown` after its leading frontmatter: a first line of `---`, through the
/// next line of `---` or `...`. T3 Code blanks the block to keep source offsets;
/// nothing here edits the source through its rendering.
fn without_frontmatter(markdown: &str) -> &str {
    let delimiter =
        |line: &str, marks: &[&str]| marks.contains(&line.trim_end_matches([' ', '\t']));
    let mut rest = markdown.strip_prefix('\u{feff}').unwrap_or(markdown);
    let mut opened = false;
    while !rest.is_empty() {
        let end = rest.find(['\r', '\n']).unwrap_or(rest.len());
        let (line, after) = rest.split_at(end);
        if !opened {
            // The opening line ends in a line break: `---` alone is a rule.
            if after.is_empty() || !delimiter(line, &["---"]) {
                return markdown;
            }
            opened = true;
        } else if delimiter(line, &["---", "..."]) {
            return after;
        }
        rest = after
            .strip_prefix("\r\n")
            .or_else(|| after.strip_prefix(['\r', '\n']))
            .unwrap_or(after);
    }
    markdown
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rendering_is_offered_for_t3s_markdown_extensions_in_any_case() {
        for path in [
            "a.md",
            "/x/README.MD",
            "a.mdx",
            "a.markdown",
            "a.mdown",
            "a.mkd",
            ".md",
        ] {
            assert!(is_markdown_preview_file(path), "{path}");
        }
        for path in ["a.txt", "md", "a.md/", "a.md.bak", "a.mdc", "notes.md/b"] {
            assert!(!is_markdown_preview_file(path), "{path}");
        }
    }

    #[test]
    fn a_line_link_shows_the_source_until_the_rendering_is_asked_for() {
        assert!(file_renders_markdown("a.md", true, false));
        assert!(!file_renders_markdown("a.md", false, false));
        assert!(!file_renders_markdown("a.md", true, true));
        assert!(!file_renders_markdown("a.rs", true, false));
    }

    #[test]
    fn leading_frontmatter_is_not_rendered() {
        for (source, shown) in [
            ("---\ntitle: a\n---\n# Body", "\n# Body"),
            ("\u{feff}---  \r\ntitle: a\r\n...\t\r\nBody", "\r\nBody"),
            ("---\n---", ""),
            ("---\ra: 1\r---\rBody", "\rBody"),
            // The first closing line ends it.
            ("---\na\n---\nb\n---\n", "\nb\n---\n"),
        ] {
            assert_eq!(without_frontmatter(source), shown, "{source:?}");
        }
        for source in [
            "",
            "---",
            "# Title\n---\na\n---\n",
            "---\nnever closed\n",
            "--- x\na\n---\n",
            "---\na\n--- x\n",
            "\n---\na\n---\n",
        ] {
            assert_eq!(without_frontmatter(source), source, "{source:?}");
        }
    }

    fn images(source: &str, path: &str) -> Vec<String> {
        file_markdown_blocks(source, path, "/repo".into(), None)
            .into_iter()
            .flat_map(|block| match block {
                MarkdownBlock::Paragraph { runs, .. } => runs,
                MarkdownBlock::Table { rows, .. } => rows
                    .into_iter()
                    .flatten()
                    .flat_map(|cell| cell.runs)
                    .collect(),
                MarkdownBlock::Visualization { .. } => Vec::new(),
            })
            .filter_map(|run| run.image)
            .collect()
    }

    /// The Host path an image's source names once a message's image loads it.
    fn loaded(source: &str) -> String {
        markdown_file_target(source.into(), "/repo".into(), MarkdownFileSurface::Mobile)
            .unwrap()
            .path
    }

    #[test]
    fn relative_images_are_those_beside_the_file_not_the_workspace() {
        let shown = images(
            "---\ntitle: a\n---\n![a](img/a%20b.png)\n\n![b](/abs/b.png)\n\n\
             ![c](https://example.invalid/c.png)\n\n|t|\n|-|\n|![d](../d.png)|\n",
            "/repo/docs/guide.md",
        );
        assert_eq!(shown.len(), 4);
        assert_eq!(loaded(&shown[0]), "/repo/docs/img/a b.png");
        assert_eq!(loaded(&shown[1]), "/abs/b.png");
        assert_eq!(shown[2], "https://example.invalid/c.png");
        assert_eq!(loaded(&shown[3]), "/repo/docs/../d.png");
        assert_eq!(
            loaded(&images("![a](a.png)", r"C:\repo\docs\guide.md")[0]),
            r"C:\repo\docs\a.png"
        );
        assert_eq!(loaded(&images("![a](a.png)", "/guide.md")[0]), "/a.png");
    }

    #[test]
    fn links_name_workspace_files_as_a_messages_do() {
        let blocks =
            file_markdown_blocks("[b](src/b.rs)", "/repo/docs/guide.md", "/repo".into(), None);
        assert_eq!(
            blocks,
            workspace_markdown_blocks("[b](src/b.rs)".into(), "/repo".into(), None)
        );
    }
}

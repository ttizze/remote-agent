//! A file shown rendered instead of as its source, as T3 Code's file panel
//! (`FilePreviewPanel`): Markdown, a Mermaid diagram, or a delimited table.
use super::super::markdown::{
    MarkdownBlock, MarkdownFileSurface, file_basename, markdown_file_target,
    workspace_markdown_blocks,
};
use regex::Regex;
use std::sync::LazyLock;

/// What a file's panel can show in place of its source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileRendering {
    Mermaid,
    Markdown,
    Table,
}

fn extension_in(path: &str, known: &[&str]) -> bool {
    path.rsplit_once('.').is_some_and(|(_, extension)| {
        known
            .iter()
            .any(|known| extension.eq_ignore_ascii_case(known))
    })
}

/// The rendering the panel of `path` offers, by T3 Code's file names
/// (`isMarkdownPreviewFile`, its Mermaid pattern and `filePreviewDelimiter`).
pub fn file_rendering(path: &str) -> Option<FileRendering> {
    [
        (FileRendering::Mermaid, &["mmd", "mermaid"][..]),
        (
            FileRendering::Markdown,
            &["md", "mdx", "markdown", "mdown", "mkd"],
        ),
        (FileRendering::Table, &["csv", "tsv"]),
    ]
    .into_iter()
    .find_map(|(rendering, known)| extension_in(path, known).then_some(rendering))
}

/// Which renderings the reader prefers to the source. T3 Code starts with
/// Markdown as its source, and diagrams and tables rendered.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FileRenderingPreferences {
    markdown: bool,
    mermaid: bool,
    table: bool,
}

impl Default for FileRenderingPreferences {
    fn default() -> Self {
        Self {
            markdown: false,
            mermaid: true,
            table: true,
        }
    }
}

impl FileRenderingPreferences {
    pub fn prefers(self, rendering: FileRendering) -> bool {
        match rendering {
            FileRendering::Mermaid => self.mermaid,
            FileRendering::Markdown => self.markdown,
            FileRendering::Table => self.table,
        }
    }

    /// These preferences once the reader chose `rendered` for `rendering`.
    pub fn with(mut self, rendering: FileRendering, rendered: bool) -> Self {
        *match rendering {
            FileRendering::Mermaid => &mut self.mermaid,
            FileRendering::Markdown => &mut self.markdown,
            FileRendering::Table => &mut self.table,
        } = rendered;
        self
    }
}

/// The rendering the panel of `path` shows now, if not its source: the
/// reader's preference, except that a link to a line (`line_request`, each
/// opening its own) shows the source at that line until the reader asks for
/// the rendering of that opening (`line_left`).
pub fn file_rendered(
    path: &str,
    preferences: FileRenderingPreferences,
    line_request: Option<u64>,
    line_left: Option<u64>,
) -> Option<FileRendering> {
    let line_pending = line_request.is_some() && line_request != line_left;
    file_rendering(path).filter(|rendering| !line_pending && preferences.prefers(*rendering))
}

/// A Mermaid file as the Markdown that draws it as a message's diagram, or
/// `None` for what T3 Code's `MermaidViewer` refuses to draw: resource URLs,
/// markup, node metadata and custom configuration.
pub fn mermaid_file_markdown(text: &str) -> Option<String> {
    // T3 Code's expression runs without JavaScript's `u` flag: only ASCII
    // letters fold case and bound words, `^` also follows a lone CR and the
    // Unicode line separators, and `\s` is JavaScript's own set of spaces.
    static REFUSED: LazyLock<Regex> = LazyLock::new(|| {
        const SPACE: &str = concat!(
            r"[\t\n\x0B\x0C\r \xA0\x{1680}\x{2000}-\x{200A}",
            r"\x{2028}\x{2029}\x{202F}\x{205F}\x{3000}\x{FEFF}]*",
        );
        Regex::new(&format!(
            concat!(
                r"(?mR)%%{space}\{{|(?:^|[\x{{2028}}\x{{2029}}]){space}---",
                r"|</?[a-z]|<[!?]|@{space}\{{",
                r"|(?:^|[^0-9a-z_])(?:img|image){space}:",
                r"|url{space}\(|@import|(?:https?|data|blob|file):",
            ),
            space = SPACE,
        ))
        .unwrap()
    });
    if REFUSED.is_match(&text.to_ascii_lowercase()) {
        return None;
    }
    // A fence longer than any run of backticks in the diagram's own text.
    let longest = text
        .split(|char| char != '`')
        .map(str::len)
        .max()
        .unwrap_or_default();
    let fence = "`".repeat(longest.max(2) + 1);
    Some(format!("{fence}mermaid\n{}\n{fence}", text.trim_end()))
}

/// A delimited file as T3 Code's table: its first row heads the columns.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FileTable {
    pub rows: Vec<Vec<String>>,
    pub columns: usize,
}

/// T3 Code's `LARGE_TEXT_MAX_COLUMNS`.
const TABLE_MAX_COLUMNS: usize = 1024;
const TABLE_MIN_COLUMN: f32 = 128.0;
const TABLE_MAX_COLUMN: f32 = 320.0;
/// The column of row numbers before a table's own.
pub const TABLE_ROW_HEADER_WIDTH: f32 = 56.0;

impl FileTable {
    /// The table of the `.csv` or `.tsv` file at `path`, read as T3 Code's
    /// `parseDelimitedPreview` reads it: a quote opens a quoted cell only at
    /// the cell's start, and a doubled quote inside one is a quote.
    pub fn read(text: &str, path: &str) -> Self {
        let delimiter = if extension_in(path, &["tsv"]) {
            '\t'
        } else {
            ','
        };
        let mut rows = Vec::new();
        let mut row = Vec::new();
        let mut cell = String::new();
        let mut quoted = false;
        let mut pending = false;
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        let mut chars = text.chars().peekable();
        while let Some(char) = chars.next() {
            pending = true;
            if char == '"' {
                if quoted && chars.next_if_eq(&'"').is_some() {
                    cell.push('"');
                    continue;
                }
                if quoted || cell.is_empty() {
                    quoted = !quoted;
                    continue;
                }
            }
            if quoted || !(char == delimiter || char == '\n' || char == '\r') {
                cell.push(char);
                continue;
            }
            end_cell(&mut row, &mut cell);
            if char != delimiter {
                rows.push(std::mem::take(&mut row));
                if char == '\r' {
                    chars.next_if_eq(&'\n');
                }
                pending = false;
            }
        }
        if pending {
            end_cell(&mut row, &mut cell);
            rows.push(row);
        }
        let columns = rows.iter().map(Vec::len).max().unwrap_or_default();
        Self { rows, columns }
    }

    /// What heads `column`: the first row's cell, or where that row is
    /// shorter, the column's letters as T3 Code's `columnLabel` (A, B, … AA).
    pub fn column_title(&self, column: usize) -> String {
        if let Some(title) = self.rows.first().and_then(|head| head.get(column)) {
            return title.clone();
        }
        let mut letters = Vec::new();
        let mut value = column + 1;
        while value > 0 {
            letters.push(char::from(b'A' + ((value - 1) % 26) as u8));
            value = (value - 1) / 26;
        }
        letters.into_iter().rev().collect()
    }

    /// Each column's width in a panel `available` points wide, between T3
    /// Code's `min-w-32` and `max-w-80`: up to 30 columns fit what their head
    /// and first rows say, estimated from their characters; more share one width.
    pub fn column_widths(&self, available: f32) -> Vec<f32> {
        if self.columns > 30 {
            let shared = (available - TABLE_ROW_HEADER_WIDTH) / 6.0;
            return vec![shared.clamp(TABLE_MIN_COLUMN, TABLE_MAX_COLUMN); self.columns];
        }
        (0..self.columns)
            .map(|column| {
                let widest = self
                    .rows
                    .iter()
                    .take(20)
                    .filter_map(|row| row.get(column))
                    .flat_map(|cell| cell.lines())
                    .map(|line| {
                        line.chars()
                            .map(|char| if char.is_ascii() { 1.0 } else { 2.0 })
                            .sum::<f32>()
                    })
                    .fold(0.0, f32::max);
                // 12-point text and the cell's `px-3`.
                (widest * 7.0 + 25.0).clamp(TABLE_MIN_COLUMN, TABLE_MAX_COLUMN)
            })
            .collect()
    }
}

fn end_cell(row: &mut Vec<String>, cell: &mut String) {
    let cell = std::mem::take(cell);
    if row.len() < TABLE_MAX_COLUMNS {
        row.push(cell);
    }
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
    fn renderings_are_offered_by_t3s_file_names_in_any_case() {
        for path in [
            "a.md",
            "/x/README.MD",
            "a.mdx",
            "a.markdown",
            "a.mdown",
            "a.mkd",
            ".md",
        ] {
            assert_eq!(
                file_rendering(path),
                Some(FileRendering::Markdown),
                "{path}"
            );
        }
        for path in ["a.mmd", "flow.MERMAID"] {
            assert_eq!(file_rendering(path), Some(FileRendering::Mermaid), "{path}");
        }
        for path in ["a.csv", "/x/b.TSV"] {
            assert_eq!(file_rendering(path), Some(FileRendering::Table), "{path}");
        }
        for path in [
            "a.txt",
            "md",
            "a.md/",
            "a.md.bak",
            "a.mdc",
            "notes.md/b",
            "csv",
        ] {
            assert_eq!(file_rendering(path), None, "{path}");
        }
    }

    #[test]
    fn markdown_starts_as_its_source_and_diagrams_and_tables_rendered() {
        let preferences = FileRenderingPreferences::default();
        assert_eq!(file_rendered("a.md", preferences, None, None), None);
        assert_eq!(
            file_rendered("a.mmd", preferences, None, None),
            Some(FileRendering::Mermaid)
        );
        assert_eq!(
            file_rendered("a.csv", preferences, None, None),
            Some(FileRendering::Table)
        );
        let chosen = preferences
            .with(FileRendering::Markdown, true)
            .with(FileRendering::Table, false);
        assert_eq!(
            file_rendered("a.md", chosen, None, None),
            Some(FileRendering::Markdown)
        );
        assert_eq!(file_rendered("a.csv", chosen, None, None), None);
        assert_eq!(
            file_rendered("a.mmd", chosen, None, None),
            Some(FileRendering::Mermaid)
        );
        assert_eq!(file_rendered("a.rs", chosen, None, None), None);
        let restored: FileRenderingPreferences =
            serde_json::from_str(&serde_json::to_string(&chosen).unwrap()).unwrap();
        assert_eq!(restored, chosen);
    }

    #[test]
    fn a_line_link_shows_the_source_until_the_rendering_is_asked_for() {
        let all = FileRenderingPreferences::default().with(FileRendering::Markdown, true);
        for path in ["a.md", "a.mmd", "a.csv"] {
            assert!(file_rendered(path, all, None, None).is_some(), "{path}");
            assert_eq!(file_rendered(path, all, Some(1), None), None, "{path}");
            // The reader asked for the rendering of that opening.
            assert!(
                file_rendered(path, all, Some(1), Some(1)).is_some(),
                "{path}"
            );
            // The link opened again shows its line again.
            assert_eq!(file_rendered(path, all, Some(2), Some(1)), None, "{path}");
            // Opened without a line, the choice holds.
            assert!(file_rendered(path, all, None, Some(1)).is_some(), "{path}");
        }
    }

    #[test]
    fn a_mermaid_file_is_drawn_as_a_messages_diagram_unless_t3_refuses_it() {
        assert_eq!(
            mermaid_file_markdown("graph TD\n  A --> B\n").as_deref(),
            Some("```mermaid\ngraph TD\n  A --> B\n```")
        );
        assert_eq!(
            mermaid_file_markdown("graph TD\n  A[\"```x```\"] --> B").as_deref(),
            Some("````mermaid\ngraph TD\n  A[\"```x```\"] --> B\n````")
        );
        // Neither a word ending in `image` nor NEL, which JavaScript's `\s` leaves out.
        for drawn in ["graph TD\n  A[homeimage: x]", "graph TD\n  A[%%\u{85}{]"] {
            assert!(mermaid_file_markdown(drawn).is_some(), "{drawn:?}");
        }
        for refused in [
            "%%{init: {}}%%\ngraph TD",
            "---\ntitle: a\n---\ngraph TD",
            "graph TD\n  A[<b>x</b>]",
            "graph TD\n  A@{ shape: rect }",
            "graph TD\n  click A \"https://example.invalid\"",
            "graph TD\n  A@{ IMG: \"x\" }",
            "graph TD\n  style A fill:url(#a)",
            "graph TD\r---",
            "graph TD\u{2028}---",
            "graph TD\n  A[\u{17f}image: x]",
            "graph TD\n  A[\u{212a}IMG : x]",
            "GRAPH TD\n  STYLE A FILL:URL (#a)",
            "%%\u{feff}{init: {}}%%\ngraph TD",
            "\u{feff}---\ntitle: a\n---\ngraph TD",
            "graph TD\n  A[背景image: x]",
        ] {
            assert_eq!(mermaid_file_markdown(refused), None, "{refused:?}");
        }
    }

    #[test]
    fn delimited_files_read_as_t3s_table() {
        let rows = |text: &str, path: &str| FileTable::read(text, path).rows;
        assert_eq!(
            rows("\u{feff}a,b\r\n1,\"x, \"\"y\"\"\nz\"\n2,\n", "a.csv"),
            vec![vec!["a", "b"], vec!["1", "x, \"y\"\nz"], vec!["2", ""]]
        );
        // A quote inside an unquoted cell is itself; a last line needs no break.
        assert_eq!(
            rows("a\tb \"c\"\rd,e\tf", "a.TSV"),
            vec![vec!["a", "b \"c\""], vec!["d,e", "f"]]
        );
        assert_eq!(rows("", "a.csv"), Vec::<Vec<String>>::new());
        assert_eq!(rows("\n", "a.csv"), vec![vec![""]]);
        let wide = FileTable::read(&"x,".repeat(2000), "a.csv");
        assert_eq!((wide.rows[0].len(), wide.columns), (1024, 1024));
        let ragged = FileTable::read("a\nb,c,d\n", "a.csv");
        assert_eq!(ragged.columns, 3);
        assert_eq!(
            [0, 1, 2].map(|column| ragged.column_title(column)),
            ["a", "B", "C"]
        );
        let headless = FileTable::default();
        assert_eq!(
            [25, 26, 27, 701, 702].map(|column| headless.column_title(column)),
            ["Z", "AA", "AB", "ZZ", "AAA"]
        );
    }

    #[test]
    fn columns_are_as_wide_as_their_first_rows_within_t3s_limits() {
        let table = FileTable::read(
            &format!(
                "id,name,note\n1,日本語の名前を持つ列です,{}\n",
                "x".repeat(200)
            ),
            "a.csv",
        );
        let widths = table.column_widths(800.0);
        assert_eq!(widths[0], 128.0);
        assert!(widths[1] > 128.0 && widths[1] < 320.0, "{widths:?}");
        assert_eq!(widths[2], 320.0);
        let many = FileTable::read(&"x,".repeat(40), "a.csv");
        assert_eq!(many.column_widths(800.0), vec![128.0; 41]);
        assert_eq!(many.column_widths(2000.0), vec![320.0; 41]);
        assert_eq!(many.column_widths(1256.0), vec![200.0; 41]);
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

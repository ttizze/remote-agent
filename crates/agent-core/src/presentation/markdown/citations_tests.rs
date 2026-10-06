use super::*;

fn attributes(pairs: &[(&str, &str)]) -> DirectiveAttributes {
    pairs
        .iter()
        .map(|(name, value)| ((*name).into(), (*value).into()))
        .collect()
}

fn link(path: &str, href: &str, label: &str, line: Option<u64>) -> FileCitationLink {
    FileCitationLink {
        path: path.into(),
        href: href.into(),
        label: label.into(),
        line_range_start: line,
    }
}

#[test]
fn resolves_the_attributes_emitted_by_codex() {
    assert_eq!(
        file_citation_link(&attributes(&[
            ("path", "/workspace/outputs/issue-2387-sparse-diagonal.xlsx"),
            ("purpose", "output"),
        ])),
        Some(link(
            "/workspace/outputs/issue-2387-sparse-diagonal.xlsx",
            "/workspace/outputs/issue-2387-sparse-diagonal.xlsx",
            "issue-2387-sparse-diagonal.xlsx",
            None,
        ))
    );
}

#[test]
fn carries_the_first_cited_line_into_the_file_href() {
    assert_eq!(
        file_citation_link(&attributes(&[
            ("path", "src/main.ts"),
            ("line_range_start", "42"),
            ("line_range_end", "48"),
            ("git_url", "https://example.com/main.ts"),
        ])),
        Some(link("src/main.ts", "src/main.ts#L42", "main.ts", Some(42)))
    );
}

#[test]
fn rejects_missing_paths_and_invalid_line_numbers() {
    assert_eq!(
        file_citation_link(&attributes(&[("purpose", "output")])),
        None
    );
    assert_eq!(
        file_citation_link(&attributes(&[
            ("path", "src/main.ts"),
            ("line_range_start", "not-a-line"),
        ])),
        Some(link("src/main.ts", "src/main.ts", "main.ts", None))
    );
}

#[test]
fn preserves_url_syntax_characters_in_file_paths() {
    assert_eq!(
        file_citation_link(&attributes(&[
            ("path", "reports/100% #1? draft.md"),
            ("line_range_start", "7"),
        ])),
        Some(link(
            "reports/100% #1? draft.md",
            "reports/100%25 %231%3F draft.md#L7",
            "100% #1? draft.md",
            Some(7),
        ))
    );
}

#[test]
fn produces_a_portable_markdown_link() {
    let citation =
        file_citation_link(&attributes(&[("path", "reports/profit and loss.xlsx")])).unwrap();
    assert_eq!(
        file_citation_markdown(&citation),
        "[profit and loss.xlsx](<reports/profit and loss.xlsx>)"
    );
}

#[test]
fn escapes_markdown_syntax_in_the_visible_filename() {
    let citation =
        file_citation_link(&attributes(&[("path", "reports/*draft*_[copy]`<&.txt")])).unwrap();
    assert_eq!(
        file_citation_markdown(&citation),
        "[\\*draft\\*\\_\\[copy\\]\\`\\<\\&.txt](<reports/*draft*_[copy]`%3C&.txt>)"
    );
}

#[test]
fn reads_line_numbers_as_javascript_numbers() {
    for (value, line) in [
        (" 7 ", Some(7)),
        ("0x10", Some(16)),
        ("1e2", Some(100)),
        ("7.0", Some(7)),
        ("7.5", None),
        ("0", None),
        ("-3", None),
        ("Infinity", None),
        ("9007199254740992", None),
        ("", None),
    ] {
        let citation = file_citation_link(&attributes(&[
            ("path", "a.ts"),
            ("line_range_start", value),
        ]))
        .unwrap();
        assert_eq!(citation.line_range_start, line, "{value:?}");
    }
}

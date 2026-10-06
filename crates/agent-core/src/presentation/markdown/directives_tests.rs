use super::super::artifact_templates::ArtifactTemplateKind;
use super::super::citations::FileCitationLink;
use super::super::links::{FilePathPosition, parse_markdown_file_link};
use super::*;

const FILE_CITATION: &str = r#":codex-file-citation{path="outputs/report.xlsx" purpose="output"}"#;
const ARTIFACT_TEMPLATE: &str = r#"::artifact-template{skill_name="artifact-template-hello-world" skill_directory="/Users/test/.codex/skills/artifact-template-hello-world" display_name="Hello World" artifact_kind="document"}"#;

fn hello_world() -> ArtifactTemplate {
    ArtifactTemplate {
        artifact_kind: ArtifactTemplateKind::Document,
        display_name: "Hello World".into(),
        gallery_kind: None,
        skill_directory: "/Users/test/.codex/skills/artifact-template-hello-world".into(),
        skill_name: "artifact-template-hello-world".into(),
    }
}

fn whole(markdown: &str) -> Vec<ArtifactTemplateMarkdownSegment> {
    vec![ArtifactTemplateMarkdownSegment::Markdown {
        markdown: markdown.into(),
        source_offset: 0,
    }]
}

/// The first inline node of the first paragraph in an ordinary CommonMark parse.
fn first_link_url(markdown: &str) -> Option<String> {
    let root = parse(markdown);
    let paragraph = root.children()?.first()?;
    match paragraph.children()?.first()? {
        Node::Link(link) => Some(link.url.clone()),
        _ => None,
    }
}

#[test]
fn renders_a_file_citation_as_a_link_without_changing_its_source_position() {
    let markdown = format!("Created {FILE_CITATION}.");
    let start = markdown.find(FILE_CITATION).unwrap();
    assert_eq!(
        directive_matches(&markdown),
        [DirectiveMatch {
            start,
            end: start + FILE_CITATION.len(),
            content: DirectiveContent::FileCitation(FileCitationLink {
                path: "outputs/report.xlsx".into(),
                href: "outputs/report.xlsx".into(),
                label: "report.xlsx".into(),
                line_range_start: None,
            }),
        }]
    );
}

#[test]
fn renders_an_artifact_template_as_semantic_block_metadata() {
    assert_eq!(
        directive_matches(ARTIFACT_TEMPLATE),
        [DirectiveMatch {
            start: 0,
            end: ARTIFACT_TEMPLATE.len(),
            content: DirectiveContent::ArtifactTemplate(hello_world()),
        }]
    );
}

fn assert_ordinary(markdown: &str) {
    assert_eq!(directive_matches(markdown), [], "{markdown}");
    assert_eq!(render_directives_for_copy(markdown), markdown);
    assert_eq!(render_file_citations_as_markdown(markdown), markdown);
    assert_eq!(split_artifact_template_markdown(markdown), whole(markdown));
}

#[test]
fn does_not_change_unrelated_colon_syntax() {
    for markdown in [
        "Meeting at 10:30",
        "Open src/main.ts:42",
        "Use :hover and :tada:",
        "::note",
        ":::note\ncontent\n:::",
        r#":codex-file-citation-extra{path="outputs/report.xlsx"}"#,
        "::artifact-template-extra",
    ] {
        assert_ordinary(markdown);
    }
}

#[test]
fn keeps_malformed_supported_directives_literal() {
    for markdown in [
        r#":codex-file-citation{purpose="output"}"#,
        r#"::artifact-template{skill_name="artifact-template-hello-world"}"#,
    ] {
        assert_ordinary(markdown);
    }
}

#[test]
fn preserves_the_literal_path_and_line() {
    let renderers: [fn(&str) -> String; 2] = [
        render_directives_for_copy,
        render_file_citations_as_markdown,
    ];
    for render in renderers {
        for path in [
            r"C:\Users\test\[draft]\report.md",
            r"\\server\share\report.md",
            "outputs/report.md",
            "/tmp/report%5C.md",
        ] {
            let markdown = render(&format!(
                r#":codex-file-citation{{path="{path}" line_range_start="7"}}"#
            ));
            let url = first_link_url(&markdown).unwrap_or_else(|| panic!("{markdown}"));
            assert_eq!(
                parse_markdown_file_link(&url),
                Some(FilePathPosition {
                    path: path.into(),
                    line: Some(7),
                    column: None,
                }),
                "{markdown}"
            );
        }
    }
}

#[test]
fn uses_the_same_parser_to_render_file_citations_as_portable_links() {
    assert_eq!(
        render_file_citations_as_markdown(&format!("Created {FILE_CITATION}.")),
        "Created [report.xlsx](<outputs/report.xlsx>)."
    );
}

#[test]
fn does_not_render_excluded_citation_syntax() {
    for markdown in [
        format!("\\{FILE_CITATION}"),
        format!("`{FILE_CITATION}`"),
        format!("```text\n{FILE_CITATION}\n```"),
        format!("[See {FILE_CITATION}](https://example.com)"),
    ] {
        assert_eq!(render_file_citations_as_markdown(&markdown), markdown);
    }
}

#[test]
fn splits_artifact_cards_from_surrounding_native_markdown() {
    assert_eq!(
        split_artifact_template_markdown(&format!("Before\n\n{ARTIFACT_TEMPLATE}\n\nAfter")),
        [
            ArtifactTemplateMarkdownSegment::Markdown {
                markdown: "Before\n\n".into(),
                source_offset: 0,
            },
            ArtifactTemplateMarkdownSegment::ArtifactTemplate {
                source_offset: 8,
                template: hello_world(),
            },
            ArtifactTemplateMarkdownSegment::Markdown {
                markdown: "\n\nAfter".into(),
                source_offset: 8 + ARTIFACT_TEMPLATE.len() as u64,
            },
        ]
    );
}

#[test]
fn leaves_malformed_and_code_artifact_template_examples_in_markdown() {
    let malformed = r#"::artifact-template{display_name="Hello World"}"#;
    let code = format!("`{ARTIFACT_TEMPLATE}`");
    assert_eq!(
        split_artifact_template_markdown(malformed),
        whole(malformed)
    );
    assert_eq!(split_artifact_template_markdown(&code), whole(&code));
}

#[test]
fn copies_the_markdown_representations_shown_by_citation_chips_and_template_cards() {
    assert_eq!(
        render_directives_for_copy(&format!("Created {FILE_CITATION}.\n\n{ARTIFACT_TEMPLATE}")),
        "Created [report.xlsx](<outputs/report.xlsx>).\n\nHello World (Document template)"
    );
}

#[test]
fn leaves_excluded_and_malformed_directive_source_unchanged() {
    let markdown = format!(
        "`{FILE_CITATION}`\n\n{}",
        r#"::artifact-template{display_name="Hello World"}"#
    );
    assert_eq!(render_directives_for_copy(&markdown), markdown);
}

// From the link-repair suite: everything except the repaired first segment, whose
// repair belongs to a module this crate does not have.
#[test]
fn repairs_ordinary_segments_after_splitting_mobile_artifacts_and_preserves_copy_source() {
    let link = "[file](<./file.md)";
    let template = r#"::artifact-template{skill_name="artifact-template-example" skill_directory="/tmp/skills/example" display_name="[file](<./file.md)" artifact_kind="document"}"#;
    let citation = r#":codex-file-citation{path="./report.md"}"#;
    let source = format!("{link}\n\n{template}\n\n{citation}");
    let segments = split_artifact_template_markdown(&source);
    let rendered: Vec<_> = segments
        .iter()
        .map(|segment| match segment {
            ArtifactTemplateMarkdownSegment::Markdown {
                markdown,
                source_offset,
            } => ArtifactTemplateMarkdownSegment::Markdown {
                markdown: render_file_citations_as_markdown(markdown),
                source_offset: *source_offset,
            },
            other => other.clone(),
        })
        .collect();
    assert_eq!(rendered[1], segments[1]);
    let ArtifactTemplateMarkdownSegment::ArtifactTemplate {
        source_offset,
        template,
    } = &rendered[1]
    else {
        panic!("expected a template card: {:?}", rendered[1]);
    };
    assert_eq!(*source_offset, link.len() as u64 + 2);
    assert_eq!(template.display_name, link);
    assert_eq!(template.skill_name, "artifact-template-example");
    assert_eq!(
        rendered[2],
        ArtifactTemplateMarkdownSegment::Markdown {
            source_offset: source.find(&format!("\n\n{citation}")).unwrap() as u64,
            markdown: "\n\n[report.md](<./report.md>)".into(),
        }
    );
    assert_eq!(
        render_directives_for_copy(&source),
        format!("{link}\n\n{link} (Document template)\n\n[report.md](<./report.md>)")
    );
}

#[test]
fn finds_directives_in_lists_headings_and_after_interrupted_paragraphs() {
    let markdown = format!(
        "# Report {FILE_CITATION}\n\n- item {FILE_CITATION}\n\nIntro\n{ARTIFACT_TEMPLATE}\nOutro {FILE_CITATION}"
    );
    assert_eq!(
        render_directives_for_copy(&markdown),
        "# Report [report.xlsx](<outputs/report.xlsx>)\n\n- item [report.xlsx](<outputs/report.xlsx>)\n\nIntro\nHello World (Document template)\nOutro [report.xlsx](<outputs/report.xlsx>)"
    );
    let indented = format!("Intro\n    {ARTIFACT_TEMPLATE}");
    assert_eq!(render_directives_for_copy(&indented), indented);
}

#[test]
fn a_directive_consumes_code_and_link_syntax_inside_its_attributes() {
    // The backtick inside the first citation's path does not open a code span that
    // would hide the second citation.
    let markdown =
        r#":codex-file-citation{path="a`b.md"} then :codex-file-citation{path="c`d.md"}"#;
    assert_eq!(
        render_file_citations_as_markdown(markdown),
        "[a\\`b.md](<a`b.md>) then [c\\`d.md](<c`d.md>)"
    );
    // A code span opened before the citation still wins.
    let code = r#"`x :codex-file-citation{path="a`b.md"}"#;
    assert_eq!(render_file_citations_as_markdown(code), code);
}

#[test]
fn a_citation_after_another_colon_or_inside_an_image_stays_literal() {
    for markdown in [
        format!(":{FILE_CITATION}"),
        format!("![{FILE_CITATION}](a.png)"),
        "<https://example.com/:codex-file-citation{path=report.xlsx}>".into(),
    ] {
        assert_eq!(render_file_citations_as_markdown(&markdown), markdown);
    }
    assert_eq!(
        render_file_citations_as_markdown(&format!("\\:{FILE_CITATION}")),
        "\\:[report.xlsx](<outputs/report.xlsx>)"
    );
}

#[test]
fn attribute_values_decode_character_references_but_keep_backslashes() {
    for (raw, decoded) in [
        ("a&amp;b.md", "a&b.md"),
        ("&#x41;&#66;.md", "AB.md"),
        ("&copy.md", "©.md"),
        ("&copyx.md", "&copyx.md"),
        ("&amp=x.md", "&amp=x.md"),
        ("&#128;.md", "€.md"),
        ("&#0;.md", "\u{fffd}.md"),
        ("&unknown;.md", "&unknown;.md"),
        (r"dir\[x\].md", r"dir\[x\].md"),
    ] {
        assert_eq!(decode_attribute_references(raw), decoded, "{raw}");
    }
}

#[test]
fn template_offsets_count_utf16_code_units() {
    let markdown = format!("日本 🚀\n\n{ARTIFACT_TEMPLATE}");
    let segments = split_artifact_template_markdown(&markdown);
    assert_eq!(
        segments[1],
        ArtifactTemplateMarkdownSegment::ArtifactTemplate {
            source_offset: "日本 🚀\n\n".encode_utf16().count() as u64,
            template: hello_world(),
        }
    );
}

#[test]
fn an_indented_template_line_and_a_citation_closing_a_link_label_still_render() {
    assert_eq!(
        render_directives_for_copy(&format!(" {ARTIFACT_TEMPLATE}")),
        " Hello World (Document template)"
    );
    // The directive consumes the `]`, so the link that bracket would close never forms.
    assert_eq!(
        render_directives_for_copy(r#"[outer :codex-file-citation{path="b](u)"}"#),
        "[outer [b\\](u)](<b](u)>)"
    );
}

proptest::proptest! {
    #[test]
    fn rendered_citations_link_back_to_the_cited_path(
        name in r"[A-Za-z0-9 _.%#?\\\[\]()*`<>!-]{0,16}[A-Za-z0-9]",
        line in 1u64..100_000,
    ) {
        let path = format!("/tmp/{name}");
        let markdown = render_file_citations_as_markdown(&format!(
            r#":codex-file-citation{{path="{path}" line_range_start="{line}"}}"#
        ));
        let url = first_link_url(&markdown);
        proptest::prop_assert!(url.is_some(), "{}", markdown);
        proptest::prop_assert_eq!(
            parse_markdown_file_link(&url.unwrap_or_default()),
            Some(FilePathPosition { path, line: Some(line), column: None })
        );
    }
}

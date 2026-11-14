use super::super::MarkdownBlock;
use super::*;

// These expectations describe the pinned T3 source, including its desktop/mobile differences.
#[rstest::rstest]
#[case("src/main.rs:12:3", "src/main.rs", Some(12), Some(3))]
#[case("src/main.rs#L18C2", "src/main.rs", Some(18), Some(2))]
#[case("src/main.rs:3#L18C2", "src/main.rs", Some(3), None)]
#[case("src/main.rs:0", "src/main.rs", None, None)]
#[case("src/main.rs:0:3", "src/main.rs", None, Some(3))]
#[case("src/main.rs:12:0", "src/main.rs", Some(12), None)]
#[case("src/main.rs#L7-L10", "src/main.rs", None, None)]
#[case("file:///D:/src/main.rs#l12c3", "D:/src/main.rs", Some(12), Some(3))]
#[case("file://localhost/tmp/a%2520b.rs", "/tmp/a%20b.rs", None, None)]
#[case("file:///c%3A/Users/x/shot.png", "c:/Users/x/shot.png", None, None)]
#[case("file://server/share/a.rs", r"\\server\share\a.rs", None, None)]
#[case("docs/My%20Folder/", "docs/My Folder/", None, None)]
#[case("資料.備份", "資料.備份", None, None)]
#[case("Résumé%20final.pdf", "Résumé final.pdf", None, None)]
#[case("/tmp/notes%23one.md?view=1#L2", "/tmp/notes#one.md", Some(2), None)]
fn explicit_destinations(
    #[case] source: &str,
    #[case] path: &str,
    #[case] line: Option<u32>,
    #[case] column: Option<u32>,
) {
    let reference = file_reference(source, Syntax::Link).unwrap();
    assert_eq!(reference.path, path);
    assert_eq!((reference.line, reference.column), (line, column));
}

#[rstest::rstest]
#[case("src/main.rs", true, true)]
#[case("main.rs:12:3", true, true)]
#[case("Makefile:12", true, false)]
#[case("README.md", false, false)]
#[case("origin/main", false, false)]
// The mobile renderer enriches code with its own regex, which accepts model-like paths.
#[case("z-ai/glm-5.3", false, true)]
#[case("share/man/ls.1", true, true)]
#[case("src/main.rs/", false, false)]
#[case("./patches/", true, true)]
#[case("src/日本語.rs", true, false)]
#[case("/custom/main.rs", true, false)]
#[case("example.com/src/a.rs", false, true)]
#[case("file:///tmp/main.rs", true, false)]
#[case("/app/settings", false, false)]
#[case("`src/main.rs`", false, false)]
#[case("npm run dev", false, false)]
#[case("~", false, false)]
#[case("~1.2", false, false)]
fn code_span_grammars(#[case] text: &str, #[case] desktop: bool, #[case] mobile: bool) {
    assert_eq!(file_reference(text, Syntax::DesktopCode).is_some(), desktop);
    assert_eq!(file_reference(text, Syntax::MobileCode).is_some(), mobile);
}

#[rstest::rstest]
#[case("https://example.com/a.rs")]
#[case("mailto:a@example.com")]
#[case("javascript:alert(1)")]
#[case("#section")]
#[case("//cdn.example.com/clip.mp4")]
#[case("TODO:12")]
#[case("patches")]
#[case("/chat/settings#L3")]
#[case("src/notes%23one.md?view=1#L2")]
fn other_destinations_are_not_files(#[case] text: &str) {
    assert!(file_reference(text, Syntax::Link).is_none());
}

#[test]
fn all_extensionless_names_and_unknown_extensions_are_links() {
    for name in [
        "GNUmakefile",
        "Containerfile",
        "Brewfile",
        "Fastfile",
        "WORKSPACE",
        "CODEOWNERS",
        "README",
        "artifact.unknown",
        "installer.exe",
        "bundle.zip",
    ] {
        assert!(file_reference(name, Syntax::Link).is_some(), "{name}");
    }
}

#[test]
fn labels_require_the_path_and_explicit_position_to_match() {
    let file = file_reference("/repo/src/example.ts:12:3", Syntax::Link).unwrap();
    assert_eq!(file.label, "example.ts · L12:C3");
    for label in [
        "example.ts",
        "src/example.ts",
        "./src/example.ts:12",
        "example.ts:12:3",
    ] {
        assert!(is_file_label(label, &file), "{label}");
    }
    for label in [
        "validates the input",
        "example.ts:99",
        "example.ts:12:4",
        "other/example.ts",
    ] {
        assert!(!is_file_label(label, &file), "{label}");
    }
    let windows = file_reference(r"C:\repo\Main.rs:12", Syntax::Link).unwrap();
    assert!(is_file_label("main.RS:12", &windows));
}

#[test]
fn mobile_uses_basename_only() {
    let source = "[index.ts](src/a/index.ts) [index.ts](src/b/index.ts) [index.ts](src/a/index.ts)";
    let blocks = super::super::markdown_blocks(source.into());
    let MarkdownBlock::Paragraph { runs, .. } = &blocks[0] else {
        panic!()
    };
    assert_eq!(
        runs.iter()
            .filter_map(|run| run.file.as_ref())
            .map(|file| file.label.as_str())
            .collect::<Vec<_>>(),
        ["index.ts", "index.ts", "index.ts"]
    );
}

#[rstest::rstest]
#[case::inline("[修正版の画像](/images/exec-generated.png)", "修正版の画像")]
#[case::reference(
    "[修正版の画像][result]\n\n[result]: /images/exec-generated.png",
    "修正版の画像"
)]
#[case::formatted("[**修正版**の画像](/images/exec-generated.png)", "修正版の画像")]
#[case::filename(
    "[exec-generated.png](/images/exec-generated.png)",
    "exec-generated.png"
)]
#[case::path(
    "[/images/exec-generated.png](/images/exec-generated.png)",
    "exec-generated.png"
)]
#[case::empty("[](/images/exec-generated.png)", "exec-generated.png")]
fn native_file_links_keep_authored_descriptions(#[case] source: &str, #[case] expected: &str) {
    let blocks = super::super::markdown_blocks(source.into());
    let MarkdownBlock::Paragraph { runs, .. } = &blocks[0] else {
        panic!("paragraph")
    };
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].text, expected);
    assert_eq!(runs[0].link.as_deref(), Some("/images/exec-generated.png"));
    let file = runs[0].file.as_ref().unwrap();
    assert_eq!(file.label, expected);
    assert_eq!(file.path, "/images/exec-generated.png");
    assert_eq!(file.kind, MarkdownFileKind::Image);
}

#[test]
fn desktop_parents_expand_until_distinct_even_when_one_path_is_a_suffix() {
    let files = [
        "src/index.ts",
        "repo/src/index.ts",
        "one/a/index.ts",
        "two/a/index.ts",
    ];
    let labels = file_labels(files.into_iter());
    assert_eq!(labels["src/index.ts"], "src");
    assert_eq!(labels["repo/src/index.ts"], "repo/src");
    assert_eq!(labels["one/a/index.ts"], "one/a");
    assert_eq!(labels["two/a/index.ts"], "two/a");
}

#[test]
fn file_position_labels_keep_unicode_and_ignore_a_column_without_a_line() {
    for (source, label) in [
        ("src/資料.rs", "資料.rs"),
        ("src/資料.rs:12", "資料.rs · L12"),
        ("src/資料.rs:12:3", "資料.rs · L12:C3"),
        ("src/資料.rs:0:3", "資料.rs"),
    ] {
        assert_eq!(file_reference(source, Syntax::Link).unwrap().label, label);
    }
}

#[test]
fn disambiguation_handles_normalized_duplicates_and_identical_parent_components() {
    let labels = file_labels(
        [
            r"C:\repo\one\a\shared\index.ts",
            "C:/repo/one/a/shared/index.ts",
            "C:/repo/two/a/shared/index.ts",
            "shared/index.ts",
            "a//shared/index.ts",
            "a/shared/index.ts",
            "index.ts",
            "/single/unique.rs",
        ]
        .into_iter(),
    );
    assert_eq!(labels["C:/repo/one/a/shared/index.ts"], "one/a/shared");
    assert_eq!(labels["C:/repo/two/a/shared/index.ts"], "two/a/shared");
    assert_eq!(labels["shared/index.ts"], "shared");
    assert_eq!(labels["a//shared/index.ts"], "a/shared");
    assert_eq!(labels["a/shared/index.ts"], "a/shared");
    assert!(!labels.contains_key("index.ts"));
    assert!(!labels.contains_key("/single/unique.rs"));
}

proptest::proptest! {
    #[test]
    fn parent_labels_use_the_shortest_distinct_suffix_with_at_least_two_components(
        input in proptest::collection::vec("(?:[abcé]{1,3}/){0,5}index[01]\\.ts", 0..80),
    ) {
        let paths = input.iter().map(|path| format!("/repo/{path}")).collect::<Vec<_>>();
        let labels = file_labels(paths.iter().map(String::as_str));
        proptest::prop_assert_eq!(&labels, &file_labels(paths.iter().rev().map(String::as_str)));
        let distinct = paths.iter().collect::<std::collections::BTreeSet<_>>();
        for path in &distinct {
            let mut segments = path.split('/').filter(|part| !part.is_empty()).collect::<Vec<_>>();
            let basename = segments.pop().unwrap();
            let peers = distinct.iter().filter(|other| other != &path && paths::basename(other) == basename)
                .map(|other| {
                    let mut parts = other.split('/').filter(|part| !part.is_empty()).collect::<Vec<_>>();
                    parts.pop();
                    parts
                }).collect::<Vec<_>>();
            let Some(label) = labels.get(path.as_str()) else {
                proptest::prop_assert!(peers.is_empty());
                continue;
            };
            proptest::prop_assert!(!peers.is_empty());
            let depth = label.split('/').count();
            proptest::prop_assert!((2.min(segments.len())..=segments.len()).contains(&depth));
            proptest::prop_assert_eq!(label, &segments[segments.len()-depth..].join("/"));
            if depth < segments.len() {
                proptest::prop_assert!(peers.iter().all(|other| other[other.len().saturating_sub(depth)..] != segments[segments.len()-depth..]));
            }
            if depth > 2 {
                let shorter = depth - 1;
                proptest::prop_assert!(peers.iter().any(|other| other[other.len().saturating_sub(shorter)..] == segments[segments.len()-shorter..]));
            }
        }
    }
}

#[test]
fn descriptive_captions_are_kept_on_mobile() {
    let source = "[**設計資料**](docs/overview.md) and `src/main.rs:4` and `README.md`\n\n```rs\nsrc/main.rs\n```";
    let blocks = super::super::markdown_blocks(source.into());
    let MarkdownBlock::Paragraph { runs, .. } = &blocks[0] else {
        panic!()
    };
    assert_eq!(
        runs.iter().map(|run| run.text.as_str()).collect::<String>(),
        "設計資料 and main.rs · L4 and README.md"
    );
    assert_eq!(runs.iter().filter(|run| run.file.is_some()).count(), 2);
    assert!(
        matches!(&blocks[1], MarkdownBlock::Paragraph { runs, .. } if runs.iter().all(|run| run.file.is_none()))
    );
}

#[test]
fn host_home_paths_are_never_inferred_from_the_client_or_workspace() {
    for surface in [MarkdownFileSurface::Desktop, MarkdownFileSurface::Mobile] {
        for cwd in [
            "",
            "/Users/other/project",
            "/workspace",
            r"C:\Users\other\project",
        ] {
            let file =
                markdown_file_target("~/Movies/promo.mp4:12".into(), cwd.into(), surface).unwrap();
            assert_eq!(file.path, "~/Movies/promo.mp4");
            assert_eq!(file.line, Some(12));
        }
    }
}

#[test]
fn targets_preserve_host_path_style_and_lexical_dot_segments() {
    for (source, cwd, expected) in [
        ("src/main.rs", "/workspace", "/workspace/src/main.rs"),
        ("../docs/a.md", "/workspace", "/workspace/../docs/a.md"),
        ("src/main.rs", r"C:\workspace", r"C:\workspace\src\main.rs"),
        (r"D:\docs\a.md", r"C:\workspace", r"D:\docs\a.md"),
        ("file:///tmp/a%2520b.rs", "", "/tmp/a%20b.rs"),
    ] {
        let file =
            markdown_file_target(source.into(), cwd.into(), MarkdownFileSurface::Desktop).unwrap();
        assert_eq!(file.path, expected);
    }
}

#[test]
fn desktop_network_links_are_limited_to_the_workspace_share() {
    let cwd = r"\\server\share";
    let accepted = markdown_file_target(
        "file://server/share/a.rs".into(),
        cwd.into(),
        MarkdownFileSurface::Desktop,
    )
    .unwrap();
    assert_eq!(accepted.path, r"\\server\share\a.rs");
    for source in [
        "file://other/share/a.rs",
        r"\\other\share\a.rs",
        "file://server/share/../outside/a.rs",
    ] {
        assert!(
            markdown_file_target(source.into(), cwd.into(), MarkdownFileSurface::Desktop).is_none()
        );
    }
    assert!(
        markdown_file_target(
            "file://other/share/a.rs".into(),
            cwd.into(),
            MarkdownFileSurface::Mobile
        )
        .is_some()
    );
}

#[test]
fn workspace_share_links_ignore_windows_path_casing_and_still_reject_escapes() {
    let cwd = r"\\Server\Share\Project";
    for source in [
        "file://server/share/project/src/main.rs",
        r"\\SERVER\SHARE\PROJECT\src\main.rs",
    ] {
        let file = markdown_file_target(source.into(), cwd.into(), MarkdownFileSurface::Desktop)
            .expect("the same Windows workspace share is allowed");
        assert_eq!(
            file_workspace_relative_path(&file.path, cwd).as_deref(),
            Some("src/main.rs")
        );
        assert!(
            chips::Workspace::new(cwd, None)
                .file(source, Syntax::Link)
                .is_some()
        );
    }
    for source in [
        "file://server/share/project/../outside/main.rs",
        "file://server/share/project-other/main.rs",
        "file://server/other/project/main.rs",
    ] {
        assert!(
            markdown_file_target(source.into(), cwd.into(), MarkdownFileSurface::Desktop).is_none()
        );
        assert!(
            chips::Workspace::new(cwd, None)
                .file(source, Syntax::Link)
                .is_none()
        );
    }
    assert_eq!(file_workspace_relative_path("/Repo/a.rs", "/repo"), None);
    assert_eq!(
        file_workspace_relative_path("C:/repo/a.rs", "/C:/Repo").as_deref(),
        Some("a.rs")
    );
}

#[test]
fn malformed_percent_encoding_remains_literal_and_is_not_partially_decoded() {
    assert_eq!(
        agent_protocol::paths::decode_link_path("src/a%20b%zz.rs"),
        "src/a%20b%zz.rs"
    );
    assert_eq!(
        agent_protocol::paths::decode_link_path("src/%FF.rs"),
        "src/%FF.rs"
    );
    assert_eq!(
        agent_protocol::paths::decode_link_path("src/%E6%97%A5.rs"),
        "src/日.rs"
    );
}

#[rstest::rstest]
#[case("/workspace/readme.MD", MarkdownFileKind::Markdown)]
#[case(r"C:\host\src\main.RS", MarkdownFileKind::Code)]
#[case("~/Pictures/photo.PNG", MarkdownFileKind::Image)]
#[case("notes.txt", MarkdownFileKind::Document)]
#[case("~/Documents/", MarkdownFileKind::Folder)]
#[case("~/Documents/archive.md/", MarkdownFileKind::Folder)]
#[case("/workspace/config.json/", MarkdownFileKind::Folder)]
#[case(r"C:\host\main.rs\", MarkdownFileKind::Folder)]
#[case("~", MarkdownFileKind::File)]
#[case("notes%2Emd", MarkdownFileKind::File)]
fn literal_filename_classification(#[case] path: &str, #[case] kind: MarkdownFileKind) {
    assert_eq!(markdown_file_kind(path), kind);
}

proptest::proptest! {
    #[test]
    fn unicode_links_keep_the_literal_name_and_position(name in "[a-zあ-ん]{1,24}", line in 1u32..5000, column in 1u32..2000) {
        let source = format!("src/{name}.rs:{line}:{column}");
        let file = markdown_file_target(source, "/workspace".into(), MarkdownFileSurface::Desktop).unwrap();
        proptest::prop_assert_eq!(file.path, format!("/workspace/src/{name}.rs"));
        proptest::prop_assert_eq!((file.line, file.column), (Some(line), Some(column)));
    }
}
#[test]
fn home_paths_use_only_the_authenticated_host_home() {
    use super::file_host_path;
    assert_eq!(file_host_path("~/a.rs".into(), None), "~/a.rs");
    assert_eq!(
        file_host_path("~/a.rs".into(), Some("/actual/home".into())),
        "/actual/home/a.rs"
    );
    assert_eq!(
        file_host_path("~".into(), Some("/actual/home".into())),
        "/actual/home"
    );
    assert_eq!(
        file_host_path(r"~\src/a.rs".into(), Some(r"C:\Users\Host".into())),
        r"C:\Users\Host\src\a.rs"
    );
    assert_eq!(
        file_host_path("/else/a".into(), Some("/actual/home".into())),
        "/else/a"
    );
    assert_eq!(file_host_path("~/a".into(), Some("relative".into())), "~/a");
    assert_eq!(
        file_host_path("~//a".into(), Some("/actual/home".into())),
        "/actual/home/a"
    );
    assert_eq!(
        file_host_path(r"~\\a".into(), Some(r"C:\Users\Host".into())),
        r"C:\Users\Host\a"
    );
}

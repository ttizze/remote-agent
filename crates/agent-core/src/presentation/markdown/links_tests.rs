use super::*;

fn position(path: &str, line: Option<u64>, column: Option<u64>) -> FilePathPosition {
    FilePathPosition {
        path: path.into(),
        line,
        column,
    }
}

#[test]
fn detects_windows_drive_paths() {
    assert!(is_windows_drive_path("C:\\repo"));
    assert!(is_windows_drive_path("D:/repo"));
    assert!(!is_windows_drive_path("/repo"));
}

#[test]
fn detects_unc_paths() {
    assert!(is_unc_path("\\\\server\\share\\repo"));
    assert!(!is_unc_path("C:\\repo"));
}

#[test]
fn detects_windows_absolute_paths() {
    assert!(is_windows_absolute_path("C:\\repo"));
    assert!(is_windows_absolute_path("\\\\server\\share\\repo"));
    assert!(!is_windows_absolute_path("./repo"));
}

#[test]
fn classifies_label_for_href() {
    for (label, href, expected) in [
        ("validates the input", "/repo/src/example.ts:12", false),
        ("read src/example.ts", "/repo/src/example.ts:12", false),
        ("example.ts?why this matters", "/repo/src/example.ts", false),
        ("example.ts", "/repo/src/example.ts:12", true),
        ("example.ts:12", "/repo/src/example.ts:12", true),
        ("example.ts:99", "/repo/src/example.ts:12", false),
        ("example.ts:12:2", "/repo/src/example.ts:12:2", true),
        ("example.ts:12:3", "/repo/src/example.ts:12:2", false),
        ("example.ts:12", "/repo/src/example.ts", false),
        ("src/example.ts:12", "/repo/src/example.ts:12", true),
        ("./src/example.ts", "/repo/src/example.ts", true),
        ("/repo/src/example.ts", "/repo/src/example.ts", true),
        ("src/", "/home/me/project/src/", true),
        ("EXAMPLE.TS", "C:/repo/src/example.ts:12", true),
        ("file name.ts", "file:///repo/file%20name.ts", true),
        ("", "/repo/src/example.ts", true),
    ] {
        assert_eq!(
            is_markdown_file_link_label(label, href),
            expected,
            "{label} for {href}"
        );
    }
}

#[test]
fn distinguishes_file_paths_from_code_and_hostnames() {
    for (source, candidate) in [
        ("src\\main.ts", Some("src/main.ts")),
        (
            "C:\\Users\\demo\\image.png",
            Some("C:\\Users\\demo\\image.png"),
        ),
        (
            "\\\\server\\share\\image.png",
            Some("\\\\server\\share\\image.png"),
        ),
        ("conf.d/nginx.conf", Some("conf.d/nginx.conf")),
        ("script.pl:10", Some("script.pl:10")),
        ("node.meta", None),
        ("Recorded evidence here: /tmp/image.png", None),
        ("origin/main", None),
        ("127.0.0.1:3000", None),
        ("example.com/index.html", None),
        ("example.pl/index.html", None),
        ("z-ai/glm-5.3", None),
        ("z-ai/glm-5.3:12", None),
        ("python/3.12", None),
        ("Qwen/Qwen2.5-Coder", None),
        ("meta-llama/Llama-3.1-8B", None),
        ("share/man/ls.1", Some("share/man/ls.1")),
        ("usr/lib/libfoo.so.1", Some("usr/lib/libfoo.so.1")),
        (
            "vendor/jquery-3.6.0.min.js",
            Some("vendor/jquery-3.6.0.min.js"),
        ),
        ("./models/glm-5.3", Some("./models/glm-5.3")),
    ] {
        assert_eq!(
            inline_code_file_path_candidate(source).as_deref(),
            candidate,
            "{source}"
        );
    }
}

#[test]
fn parses_file_urls() {
    for (href, path, hash) in [
        (
            "file:///Users/julius/project/src/main.ts#L42",
            "/Users/julius/project/src/main.ts",
            "#L42",
        ),
        (
            "file:///D:/Programme/app/OpenInPicker.tsx#L69",
            "D:/Programme/app/OpenInPicker.tsx",
            "#L69",
        ),
        (
            "file://server/share/workspace-image.svg",
            "\\\\server\\share\\workspace-image.svg",
            "",
        ),
        ("file://localhost/home/me/notes.md", "/home/me/notes.md", ""),
    ] {
        assert_eq!(
            parse_file_url_href(href),
            Some(LinkPathAndHash {
                path: path.into(),
                hash: hash.into()
            }),
            "{href}"
        );
    }
}

#[test]
fn keeps_percent_encoding_so_the_caller_decodes_once() {
    assert_eq!(
        parse_file_url_href("file:///Users/julius/project/file%2520name.md").map(|url| url.path),
        Some("/Users/julius/project/file%2520name.md".into())
    );
    assert_eq!(
        parse_file_url_href("file:///c%3A/Users/x/shot.png").map(|url| url.path),
        Some("/c%3A/Users/x/shot.png".into())
    );
}

#[test]
fn rejects_non_file_urls() {
    for href in ["https://example.com/a.ts", "file://%", "/Users/julius/a.ts"] {
        assert_eq!(parse_file_url_href(href), None, "{href}");
    }
}

#[test]
fn splits_path_and_hash_positions() {
    for (path, hash, expected) in [
        ("src/main.ts", "", position("src/main.ts", None, None)),
        (
            "src/main.ts:12",
            "",
            position("src/main.ts", Some(12), None),
        ),
        (
            "src/main.ts:12:5",
            "",
            position("src/main.ts", Some(12), Some(5)),
        ),
        (
            "src/main.ts",
            "#L18C2",
            position("src/main.ts", Some(18), Some(2)),
        ),
        (
            "src/main.ts:3",
            "#L18C2",
            position("src/main.ts", Some(3), None),
        ),
        ("src/main.ts:0", "", position("src/main.ts", None, None)),
        (
            "src/main.ts",
            "#section",
            position("src/main.ts", None, None),
        ),
    ] {
        assert_eq!(
            split_file_path_position(path, hash),
            expected,
            "{path}{hash}"
        );
    }
}

// Both clients consume this table, so a path one client recognizes is one the other
// recognizes too.
#[test]
fn recognizes_href_as_a_file() {
    for (href, path) in [
        (
            "/Users/julius/project/AGENTS.md",
            "/Users/julius/project/AGENTS.md",
        ),
        ("/home/me/notes.md", "/home/me/notes.md"),
        ("/usr/local/bin/tool", "/usr/local/bin/tool"),
        ("/workspace/Makefile", "/workspace/Makefile"),
        ("/tmp/favicons/", "/tmp/favicons/"),
        (
            "C:\\Users\\mike\\project\\src\\main.ts",
            "C:\\Users\\mike\\project\\src\\main.ts",
        ),
        ("C:%5Crepo%5Cimage.png", "C:\\repo\\image.png"),
        (
            "\\\\server\\share\\image.png",
            "\\\\server\\share\\image.png",
        ),
        (
            "/D:/Programme/app/OpenInPicker.tsx",
            "D:/Programme/app/OpenInPicker.tsx",
        ),
        (
            "</D:/Programme/app/ChatMarkdown.tsx:1>",
            "D:/Programme/app/ChatMarkdown.tsx",
        ),
        (
            "file:///Users/julius/project/file%2520name.md",
            "/Users/julius/project/file%20name.md",
        ),
        (
            "file://server/share/workspace-image.svg",
            "\\\\server\\share\\workspace-image.svg",
        ),
        ("file://localhost/home/me/notes.md", "/home/me/notes.md"),
        ("apps/mobile/src/index.ts:10", "apps/mobile/src/index.ts"),
        (
            "docs/My%20Folder/checklist.xml",
            "docs/My Folder/checklist.xml",
        ),
        (
            "Updated%20cutover%20checklist.md",
            "Updated cutover checklist.md",
        ),
        ("./scripts/deploy", "./scripts/deploy"),
        ("~/notes/today.md", "~/notes/today.md"),
        ("AGENTS.md", "AGENTS.md"),
        ("script.ts:10", "script.ts"),
        ("/tmp/clip%23one.mp4#t=2", "/tmp/clip#one.mp4"),
    ] {
        assert_eq!(
            parse_markdown_file_link(href)
                .map(|link| link.path)
                .as_deref(),
            Some(path),
            "{href}"
        );
    }
}

#[test]
fn does_not_treat_href_as_a_file() {
    for href in [
        "",
        "#anchor",
        "//cdn.example.com/clip.mp4",
        "https://example.com/docs",
        "mailto:someone@example.com",
        "javascript:alert(1)",
        "/chat/settings",
        "/chat/settings#L3",
        "/app#L1",
        "readme",
        "TODO:12",
    ] {
        assert_eq!(parse_markdown_file_link(href), None, "{href}");
    }
}

#[test]
fn accepts_conventional_extensionless_names_with_or_without_a_position() {
    assert_eq!(
        parse_markdown_file_link("Makefile"),
        Some(position("Makefile", None, None))
    );
    assert_eq!(
        parse_markdown_file_link("Dockerfile:8"),
        Some(position("Dockerfile", Some(8), None))
    );
    assert_eq!(
        parse_markdown_file_link("/srv/app/Makefile"),
        Some(position("/srv/app/Makefile", None, None))
    );
}

#[test]
fn reads_positions_from_suffixes_and_line_anchors() {
    assert_eq!(
        parse_markdown_file_link("/Users/julius/project/src/main.ts#L42C7"),
        Some(position(
            "/Users/julius/project/src/main.ts",
            Some(42),
            Some(7)
        ))
    );
    assert_eq!(
        parse_markdown_file_link("file://server/share/src/main.ts#L42C7"),
        Some(position(
            "\\\\server\\share\\src\\main.ts",
            Some(42),
            Some(7)
        ))
    );
}

#[test]
fn labels_path_with_its_basename() {
    for (path, basename) in [
        ("/tmp/favicons/", "favicons"),
        ("C:\\Users\\kelchm\\.claude\\", ".claude"),
        ("/tmp/", "tmp"),
        ("AGENTS.md", "AGENTS.md"),
        ("/", "/"),
    ] {
        assert_eq!(file_basename(path), basename, "{path}");
    }
}

#[test]
fn relates_path_to_workspace_root() {
    for (path, root, relative) in [
        ("/repo/project", Some("/repo/project"), Some(".")),
        ("/repo/project/", Some("/repo/project/"), Some(".")),
        ("/", Some("/"), Some(".")),
        (
            "C:/USERS/mike/project",
            Some("c:/users/MIKE/project"),
            Some("."),
        ),
        ("C:/", Some("c:/"), Some(".")),
        (
            "/repo/project/src/main.ts",
            Some("/repo/project"),
            Some("src/main.ts"),
        ),
        (
            "/repo/project/src/main.ts",
            Some("/repo/project/"),
            Some("src/main.ts"),
        ),
        (
            "C:\\Users\\mike\\app\\apps\\web\\a.ts",
            Some("C:/Users/mike/app"),
            Some("apps/web/a.ts"),
        ),
        (
            "/C:/Users/mike/app/apps/web/a.ts",
            Some("C:/Users/mike/app"),
            Some("apps/web/a.ts"),
        ),
        ("/Repo/Project/src/main.ts", Some("/repo/project"), None),
        (
            "/tmp/case/project/probe.txt",
            Some("/tmp/case/Project"),
            None,
        ),
        (
            "//tmp/case/project/probe.txt",
            Some("//tmp/case/Project"),
            None,
        ),
        (
            "/tmp/case/Project/probe.txt",
            Some("/tmp/case/Project"),
            Some("probe.txt"),
        ),
        (
            "C:/USERS/mike/app/main.ts",
            Some("c:/users/MIKE/app"),
            Some("main.ts"),
        ),
        (
            "/C:/USERS/mike/app/main.ts",
            Some("/c:/users/MIKE/app"),
            Some("main.ts"),
        ),
        (
            "\\\\server\\share\\PROJECT\\main.ts",
            Some("\\\\Server\\Share\\Project"),
            Some("main.ts"),
        ),
        ("/tmp/repo/file.ts", Some("/"), Some("tmp/repo/file.ts")),
        (
            "C:/Users/MIKE/main.ts",
            Some("c:/"),
            Some("Users/MIKE/main.ts"),
        ),
        (
            "\\\\server\\SHARE\\file.ts",
            Some("\\\\Server\\Share\\"),
            Some("file.ts"),
        ),
        ("/tmp/repo/file.ts ", Some("/tmp/repo"), Some("file.ts ")),
        ("/tmp/report.ts", Some("/repo/project"), None),
        ("/repo/project-two/a.ts", Some("/repo/project"), None),
        ("/repo/project/a.ts", None, None),
    ] {
        assert_eq!(
            workspace_relative_file_path(path, root).as_deref(),
            relative,
            "{path} to {root:?}"
        );
    }
}

fn file_link(href: &str, icon: MarkdownFileIcon, label: &str, path: &str) -> MarkdownFileLink {
    MarkdownFileLink {
        href: href.into(),
        icon,
        label: label.into(),
        path: path.into(),
        line: None,
        column: None,
    }
}

fn file_presentation(href: &str) -> MarkdownFileLink {
    match markdown_link_presentation(href) {
        MarkdownLinkPresentation::File { link } => link,
        other => panic!("{href} is not a file: {other:?}"),
    }
}

#[test]
fn gives_github_hosts_the_brand_mark_and_everything_else_the_generic_glyph() {
    assert_eq!(
        markdown_link_icon("github.com"),
        Some(MarkdownLinkIcon::Github)
    );
    assert_eq!(
        markdown_link_icon("GitHub.com"),
        Some(MarkdownLinkIcon::Github)
    );
    assert_eq!(
        markdown_link_icon("gist.github.com"),
        Some(MarkdownLinkIcon::Github)
    );
    assert_eq!(markdown_link_icon("github.community"), None);
    assert_eq!(markdown_link_icon("notgithub.com"), None);
    assert_eq!(markdown_link_icon("example.com"), None);
}

#[test]
fn treats_protocol_relative_media_as_an_external_url_not_a_filesystem_path() {
    assert_eq!(
        markdown_link_presentation("//cdn.example.com/clip.mp4?sig=a%2fb#t=2"),
        MarkdownLinkPresentation::External {
            href: "https://cdn.example.com/clip.mp4?sig=a%2fb#t=2".into(),
            host: "cdn.example.com".into(),
        }
    );
}

#[test]
fn separates_encoded_filename_characters_from_a_video_playback_fragment() {
    let link = file_presentation("/tmp/clip%23one.mp4#t=2");
    assert_eq!(link.path, "/tmp/clip#one.mp4");
    assert_eq!(link.label, "clip#one.mp4");
    assert_eq!(link.icon, MarkdownFileIcon::Video);
}

#[test]
fn extracts_external_link_hosts() {
    assert_eq!(
        markdown_link_presentation("https://example.com/docs?q=1"),
        MarkdownLinkPresentation::External {
            href: "https://example.com/docs?q=1".into(),
            host: "example.com".into(),
        }
    );
}

#[test]
fn preserves_the_file_url_path_and_position() {
    for (href, path) in [
        (
            "file:///Users/julius/project/src/main.ts#L42C7",
            "/Users/julius/project/src/main.ts",
        ),
        (
            "file://server/share/src/main.ts#L42C7",
            "\\\\server\\share\\src\\main.ts",
        ),
    ] {
        assert_eq!(
            markdown_link_presentation(href),
            MarkdownLinkPresentation::File {
                link: MarkdownFileLink {
                    line: Some(42),
                    column: Some(7),
                    ..file_link(href, MarkdownFileIcon::Typescript, "main.ts:42:7", path)
                }
            },
            "{href}"
        );
    }
}

#[test]
fn recognizes_relative_source_paths_and_bare_filenames() {
    assert_eq!(
        file_presentation("apps/mobile/src/index.ts:10"),
        MarkdownFileLink {
            line: Some(10),
            ..file_link(
                "apps/mobile/src/index.ts:10",
                MarkdownFileIcon::Typescript,
                "index.ts:10",
                "apps/mobile/src/index.ts",
            )
        }
    );
    assert_eq!(
        file_presentation("AGENTS.md"),
        file_link(
            "AGENTS.md",
            MarkdownFileIcon::Agents,
            "AGENTS.md",
            "AGENTS.md"
        )
    );
    assert_eq!(
        file_presentation("package.json"),
        file_link(
            "package.json",
            MarkdownFileIcon::Npm,
            "package.json",
            "package.json"
        )
    );
}

#[test]
fn recognizes_a_bare_spaced_filename() {
    for extension in ["md", "html", "xml"] {
        let link = file_presentation(&format!("Updated%20cutover%20checklist.{extension}"));
        assert_eq!(link.path, format!("Updated cutover checklist.{extension}"));
        assert_eq!(link.label, format!("Updated cutover checklist.{extension}"));
    }
}

#[test]
fn recognizes_spaced_relative_paths() {
    let link = file_presentation("docs/My%20Folder/checklist.xml");
    assert_eq!(link.path, "docs/My Folder/checklist.xml");
    assert_eq!(link.label, "checklist.xml");
}

#[test]
fn extracts_line_fragments_from_relative_file_links() {
    let link = file_presentation("src/main.ts#L18C2");
    assert_eq!(link.path, "src/main.ts");
    assert_eq!((link.line, link.column), (Some(18), Some(2)));
    assert_eq!(link.label, "main.ts:18:2");
}

#[test]
fn uses_the_pierre_complete_icon_mappings() {
    for (href, icon) in [
        ("src/Button.tsx", MarkdownFileIcon::React),
        ("vite.config.ts", MarkdownFileIcon::Vite),
        ("Dockerfile", MarkdownFileIcon::Docker),
        ("pnpm-lock.yaml", MarkdownFileIcon::Pnpm),
    ] {
        assert_eq!(file_presentation(href).icon, icon, "{href}");
    }
}

#[test]
fn does_not_style_app_routes_as_file_links() {
    assert_eq!(
        markdown_link_presentation("/chat/settings"),
        MarkdownLinkPresentation::Link { href: None }
    );
}

#[test]
fn keeps_normalized_workspace_relative_paths() {
    assert_eq!(
        workspace_file_path(Some("/repo"), "./src/../src/main.ts").as_deref(),
        Some("src/main.ts")
    );
}

#[test]
fn converts_absolute_paths_inside_the_workspace() {
    assert_eq!(
        workspace_file_path(Some("/Users/julius/repo"), "/Users/julius/repo/src/main.ts")
            .as_deref(),
        Some("src/main.ts")
    );
    assert_eq!(
        workspace_file_path(Some("C:\\repo"), "c:\\repo\\src\\main.ts").as_deref(),
        Some("src/main.ts")
    );
}

#[test]
fn rejects_paths_outside_the_workspace() {
    assert_eq!(workspace_file_path(Some("/repo"), "/other/main.ts"), None);
    assert_eq!(workspace_file_path(Some("/repo"), "../other/main.ts"), None);
    assert_eq!(
        workspace_file_path(Some("/repo"), "/repo/../outside.txt"),
        None
    );
    assert_eq!(workspace_file_path(None, "/repo/main.ts"), None);
}

#[test]
fn places_a_workspace_file_under_its_project() {
    assert_eq!(
        file_header_subtitle(
            "acme",
            "apps/mobile/src/features/threads/fileChipMenu.test.ts"
        ),
        "acme · apps/mobile/src/features/threads"
    );
}

#[test]
fn shows_only_the_directory_for_a_host_file_outside_the_workspace() {
    assert_eq!(file_header_subtitle("acme", "/tmp/report.md"), "/tmp");
}

#[test]
fn shows_only_the_project_for_a_file_at_the_workspace_root() {
    assert_eq!(file_header_subtitle("acme", "README.md"), "acme");
}

#[test]
fn a_tapped_link_opens_workspace_files_host_files_or_the_web() {
    assert_eq!(
        markdown_link_action("src/main.rs#L20", Some("/repo")),
        MarkdownLinkAction::WorkspaceFile {
            path: "src/main.rs".into(),
            line: Some(20)
        }
    );
    assert_eq!(
        markdown_link_action("/repo/src/lib.rs:7", Some("/repo")),
        MarkdownLinkAction::WorkspaceFile {
            path: "src/lib.rs".into(),
            line: Some(7)
        }
    );
    assert_eq!(
        markdown_link_action("/tmp/report.md", Some("/repo")),
        MarkdownLinkAction::HostFile {
            path: "/tmp/report.md".into(),
            line: None
        }
    );
    assert_eq!(
        markdown_link_action("https://example.com/a", Some("/repo")),
        MarkdownLinkAction::External {
            url: "https://example.com/a".into()
        }
    );
    assert_eq!(
        markdown_link_action("mailto:team@example.com", None),
        MarkdownLinkAction::External {
            url: "mailto:team@example.com".into()
        }
    );
    assert_eq!(
        markdown_link_action("/chat/settings", Some("/repo")),
        MarkdownLinkAction::Nothing
    );
}

#[test]
fn rejects_oversized_markdown_line_targets_before_native_conversion() {
    assert_eq!(markdown_line_target(1, 1), Some(1));
    assert_eq!(markdown_line_target(2, 1), None);
    assert_eq!(markdown_line_target(u64::MAX, 1_000_000), None);
    assert_eq!(markdown_line_target(u64::MAX, u64::MAX), Some(u64::MAX));
}

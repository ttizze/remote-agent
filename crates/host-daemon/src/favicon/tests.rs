use super::*;
use std::sync::Arc;

fn write(root: &Path, path: &str, text: &str) {
    let path = root.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn workspace() -> (tempfile::TempDir, PathBuf) {
    let directory = tempfile::tempdir().unwrap();
    let root = dunce::canonicalize(directory.path()).unwrap();
    (directory, root)
}

/// A resolver whose clock moves only when the test advances it.
fn resolver() -> (Resolver, impl Fn(Duration)) {
    let now = Arc::new(Mutex::new(Instant::now()));
    let clock = now.clone();
    let resolver = Resolver::new(move || *clock.lock().unwrap());
    (resolver, move |by| *now.lock().unwrap() += by)
}

fn resolve(resolver: &Resolver, root: &Path, saved: Option<&str>) -> Option<PathBuf> {
    resolver.resolve(root.to_str().unwrap(), saved).unwrap()
}

fn found(root: &Path, saved: Option<&str>) -> Option<PathBuf> {
    resolve(&resolver().0, root, saved)
}

// ProjectFaviconResolver.test.ts "serves repeated resolves from cache instead of
// re-walking candidates".
#[test]
fn serves_repeated_resolves_from_the_cache_instead_of_walking_again() {
    let (_directory, root) = workspace();
    let (resolver, advance) = resolver();
    write(&root, "public/favicon.svg", "<svg>public</svg>");
    let first = resolve(&resolver, &root, None);
    assert_eq!(first, Some(root.join("public/favicon.svg")));
    // `favicon.svg` outranks `public/favicon.svg`, so only the cache keeps the
    // first answer.
    write(&root, "favicon.svg", "<svg>root</svg>");
    for _ in 0..3 {
        assert_eq!(resolve(&resolver, &root, None), first);
    }
    advance(Duration::from_secs(11 * 60));
    assert_eq!(
        resolve(&resolver, &root, None),
        Some(root.join("favicon.svg"))
    );
}

// "falls back at once when a cached favicon is deleted"
#[test]
fn falls_back_at_once_when_a_cached_favicon_is_deleted() {
    let (_directory, root) = workspace();
    let (resolver, _) = resolver();
    write(&root, "favicon.svg", "<svg>favicon</svg>");
    assert!(resolve(&resolver, &root, None).is_some());
    fs::remove_file(root.join("favicon.svg")).unwrap();
    assert_eq!(resolve(&resolver, &root, None), None);
}

// "re-probes for a favicon added after a miss once the negative TTL expires"
#[test]
fn looks_again_for_an_icon_added_after_a_miss_once_the_miss_expires() {
    let (_directory, root) = workspace();
    let (resolver, advance) = resolver();
    assert_eq!(resolve(&resolver, &root, None), None);
    write(&root, "favicon.svg", "<svg>favicon</svg>");
    assert_eq!(resolve(&resolver, &root, None), None);
    advance(Duration::from_secs(2 * 60));
    assert!(resolve(&resolver, &root, None).is_some());
}

// Failures are not cached; the least recently used entry makes room.
#[test]
fn failures_are_not_remembered_and_the_least_recently_used_answer_is_dropped() {
    let (_directory, parent) = workspace();
    let root = parent.join("app");
    let (resolver, _) = resolver();
    assert!(resolver.resolve(root.to_str().unwrap(), None).is_err());
    write(&root, "public/favicon.svg", "<svg/>");
    assert_eq!(
        resolve(&resolver, &root, None),
        Some(root.join("public/favicon.svg"))
    );
    assert_eq!(
        resolve(&resolver, &root, Some("kept.svg")),
        Some(root.join("public/favicon.svg"))
    );
    write(&root, "favicon.svg", "<svg/>");
    for index in 0..CAPACITY - 2 {
        resolve(&resolver, &root, Some(&format!("{index}.svg")));
    }
    // Using an entry keeps it; adding one more drops the oldest.
    resolve(&resolver, &root, Some("kept.svg"));
    resolve(&resolver, &root, Some("new.svg"));
    assert_eq!(
        resolve(&resolver, &root, None),
        Some(root.join("favicon.svg"))
    );
    assert_eq!(
        resolve(&resolver, &root, Some("kept.svg")),
        Some(root.join("public/favicon.svg"))
    );
}

// "prefers well-known favicon files"; the candidates are checked in order.
#[test]
fn prefers_well_known_favicon_files_in_order() {
    let (_directory, root) = workspace();
    write(
        &root,
        "index.html",
        r#"<link rel="icon" href="/brand.png">"#,
    );
    write(&root, "public/brand.png", "png");
    assert_eq!(found(&root, None), Some(root.join("public/brand.png")));
    for candidate in CANDIDATES.iter().rev() {
        write(&root, candidate, "icon");
        assert_eq!(found(&root, None), Some(root.join(candidate)));
    }
}

// "uses a saved project favicon override"
#[test]
fn uses_a_saved_icon() {
    let (_directory, root) = workspace();
    write(&root, "brand/custom.svg", "<svg>custom</svg>");
    write(&root, "favicon.svg", "<svg>automatic</svg>");
    assert_eq!(
        found(&root, Some("brand/custom.svg")),
        Some(root.join("brand/custom.svg"))
    );
}

// "uses a saved project favicon outside the workspace"
#[test]
fn uses_a_saved_icon_outside_the_workspace() {
    let (_directory, root) = workspace();
    let (_pictures, pictures) = workspace();
    write(&pictures, "custom.png", "image");
    let external = pictures.join("custom.png");
    assert_eq!(
        found(&root, Some(external.to_str().unwrap())),
        Some(external.clone())
    );
    // Only an absolute saved path may leave the workspace.
    write(&root, "favicon.svg", "<svg/>");
    let relative = pathdiff(&external, &root);
    assert!(root.join(&relative).is_file());
    assert_eq!(
        found(&root, Some(&relative)),
        Some(root.join("favicon.svg"))
    );
}

/// `path` relative to `base`, through `..`.
fn pathdiff(path: &Path, base: &Path) -> String {
    let common = path
        .components()
        .zip(base.components())
        .take_while(|(left, right)| left == right)
        .count();
    let ups = base.components().count() - common;
    let mut relative = PathBuf::new();
    for _ in 0..ups {
        relative.push("..");
    }
    relative.extend(path.components().skip(common));
    relative.to_str().unwrap().to_owned()
}

// "falls back when a saved override is missing from a checkout"
#[test]
fn falls_back_when_the_saved_icon_is_missing_from_a_checkout() {
    let (_directory, root) = workspace();
    write(&root, "favicon.svg", "<svg>automatic</svg>");
    assert_eq!(
        found(&root, Some("brand/missing.svg")),
        Some(root.join("favicon.svg"))
    );
    assert_eq!(
        found(&root, Some("/nowhere/missing.svg")),
        Some(root.join("favicon.svg"))
    );
}

// "resolves icon hrefs from project source files"
#[test]
fn resolves_icon_links_from_project_source_files() {
    let (_directory, root) = workspace();
    write(
        &root,
        "index.html",
        r#"<link rel="icon" href="/brand/logo.svg">"#,
    );
    write(&root, "public/brand/logo.svg", "<svg>brand</svg>");
    assert_eq!(found(&root, None), Some(root.join("public/brand/logo.svg")));
    // Without the public copy, the href names a file in the root itself.
    fs::remove_file(root.join("public/brand/logo.svg")).unwrap();
    write(&root, "brand/logo.svg", "<svg>brand</svg>");
    assert_eq!(found(&root, None), Some(root.join("brand/logo.svg")));
}

// "resolves icon hrefs from object-literal route metadata", "…when href precedes
// rel", "…alongside nested objects" and "skips icon metadata without an href and
// keeps scanning".
#[test]
fn resolves_icon_metadata_objects() {
    for (source, text) in [
        (
            "src/routes/__root.tsx",
            r#"export const Route = createRootRoute({
  head: () => ({
    links: [
      { rel: "stylesheet", href: "/app.css" },
      { rel: "icon", href: "/brand/logo.svg" },
    ],
  }),
});"#,
        ),
        (
            "src/root.tsx",
            r#"const links = [{ href: "/brand/logo.svg", rel: "shortcut icon" }];"#,
        ),
        (
            "src/root.tsx",
            r#"const links = [{ attributes: {}, rel: "icon", href: "/brand/logo.svg" }];"#,
        ),
        (
            "src/root.tsx",
            r#"const links = [{ rel: "icon" }, { rel: "icon", href: "/brand/logo.svg" }];"#,
        ),
    ] {
        let (_directory, root) = workspace();
        write(&root, source, text);
        write(&root, "public/brand/logo.svg", "<svg>brand</svg>");
        assert_eq!(
            found(&root, None),
            Some(root.join("public/brand/logo.svg")),
            "{text}"
        );
    }
}

// The link and metadata patterns: attributes in any order, ASCII case, the last
// href of a tag, one leading slash removed, and a query ending the path.
#[test]
fn reads_icon_links_like_the_reference_patterns() {
    let cases = [
        (
            r#"<link rel="stylesheet" href="a.css"><LINK HREF='/icon.svg?v=2' REL='Icon'>"#,
            Some("/icon.svg"),
        ),
        (
            r#"<link rel="icon" href="/a.png" href="/b.png">"#,
            Some("/b.png"),
        ),
        (r#"<link rel="icon" href="a>b.png">"#, Some("a>b.png")),
        (r#"<link rel="icon"> <link href="/a.png">"#, None),
        (r#"<link rel="icon" href="/a.png""#, None),
        (r#"<linkrel="icon" href="/a.png">"#, None),
        (r#"<link data-rel="icon" href="/a.png">"#, Some("/a.png")),
        (r#"<link rel="icon" href="">"#, None),
        (
            "{ rel\u{a0}:\u{feff}'icon', href :\n'/a.png' }",
            Some("/a.png"),
        ),
        ("{ rel\u{85}: 'icon', href: '/a.png' }", None),
        (r#"{ rel: "icon" } { href: "/other.png" }"#, None),
        ("{ rel: 'icon', href: '/a.png' }", Some("/a.png")),
    ];
    for (source, expected) in cases {
        assert_eq!(icon_href(source).as_deref(), expected, "{source}");
    }
    let (_directory, root) = workspace();
    write(
        &root,
        "index.html",
        r#"<link rel="icon" href="//brand.png">"#,
    );
    write(&root, "public/brand.png", "png");
    assert_eq!(found(&root, None), Some(root.join("public/brand.png")));
}

// "scans large icon sources without an icon in reasonable time"
#[test]
fn scans_large_icon_sources_without_an_icon_quickly() {
    let (_directory, root) = workspace();
    let filler = format!("<p>{}</p>\n", "pokopia companion guide ".repeat(24));
    write(
        &root,
        "index.html",
        &format!(
            "<!doctype html><html><head><title>guide</title></head><body>\n{}</body></html>",
            filler.repeat(1200)
        ),
    );
    let started = Instant::now();
    assert_eq!(found(&root, None), None);
    assert!(started.elapsed() < Duration::from_secs(5));
}

// "returns null when no icon is present"; a directory with an icon's name is not
// one.
#[test]
fn finds_nothing_without_an_icon_file() {
    let (_directory, root) = workspace();
    assert_eq!(found(&root, None), None);
    fs::create_dir(root.join("favicon.svg")).unwrap();
    assert_eq!(found(&root, Some("favicon.svg")), None);
}

// "preserves workspace normalization context"
#[test]
fn a_missing_root_fails() {
    let (_directory, root) = workspace();
    let error = resolver()
        .0
        .resolve(root.join("missing").to_str().unwrap(), None)
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::NotFound);
    assert!(
        error
            .to_string()
            .starts_with("Workspace root does not exist")
    );
}

// "preserves non-missing candidate stat failures" and "preserves icon source read
// failures".
#[cfg(unix)]
#[test]
fn unreadable_candidates_and_sources_fail_the_lookup() {
    use std::os::unix::fs::PermissionsExt;
    let (_directory, root) = workspace();
    fs::create_dir(root.join("public")).unwrap();
    fs::set_permissions(root.join("public"), fs::Permissions::from_mode(0o000)).unwrap();
    let error = resolver()
        .0
        .resolve(root.to_str().unwrap(), None)
        .unwrap_err();
    fs::set_permissions(root.join("public"), fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);

    write(
        &root,
        "index.html",
        r#"<link rel="icon" href="/favicon.svg">"#,
    );
    fs::set_permissions(root.join("index.html"), fs::Permissions::from_mode(0o000)).unwrap();
    let error = resolver()
        .0
        .resolve(root.to_str().unwrap(), None)
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
}

// "skips icon metadata paths outside the workspace" and "continues to later
// sources after an outside-root icon href".
#[test]
fn skips_icon_links_outside_the_workspace() {
    let (_directory, parent) = workspace();
    let root = parent.join("app");
    write(&parent, "secret.svg", "<svg>secret</svg>");
    write(
        &root,
        "index.html",
        r#"<link rel="icon" href="../../secret.svg">"#,
    );
    assert_eq!(found(&root, None), None);
    write(
        &root,
        "public/index.html",
        r#"<link rel="icon" href="/brand/../logo.svg">"#,
    );
    write(&root, "public/logo.svg", "<svg>brand</svg>");
    assert_eq!(found(&root, None), Some(root.join("public/logo.svg")));
}

fn sha256(bytes: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, bytes)
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn icon(root: &Path, saved: Option<&str>, known_hash: Option<&str>) -> Option<ProjectFavicon> {
    read_with(&resolver().0, root.to_str().unwrap(), saved, known_hash).unwrap()
}

// AssetAccess.test.ts "issues project favicon capabilities with a signed
// fallback": the name carries the content hash, new content a new hash, and a
// deleted icon the fallback. A client holding the current copy gets no bytes.
#[test]
fn names_the_icon_by_its_content_and_falls_back_when_it_is_deleted() {
    let (_directory, root) = workspace();
    let (resolver, _) = resolver();
    let read =
        |known: Option<&str>| read_with(&resolver, root.to_str().unwrap(), None, known).unwrap();
    write(&root, "favicon.svg", "<svg>a</svg>");
    let first = read(None).unwrap();
    let hash = sha256(b"<svg>a</svg>");
    assert_eq!(
        first,
        ProjectFavicon {
            file_name: format!("v{hash}-favicon.svg"),
            mime_type: "image/svg+xml".into(),
            data: Some(b"<svg>a</svg>".to_vec()),
            hash: hash.clone(),
        }
    );
    assert_eq!(read(None), Some(first.clone()));
    assert_eq!(
        read(Some(&hash)),
        Some(ProjectFavicon {
            data: None,
            ..first.clone()
        })
    );

    write(&root, "favicon.svg", "<svg>b</svg>");
    let updated = read(Some(&hash)).unwrap();
    assert_ne!(updated.file_name, first.file_name);
    assert_eq!(updated.data.as_deref(), Some(&b"<svg>b</svg>"[..]));

    fs::remove_file(root.join("favicon.svg")).unwrap();
    assert_eq!(read(Some(&updated.hash)), None);
}

#[test]
fn an_icon_file_over_four_mebibytes_shows_the_fallback() {
    let (_directory, root) = workspace();
    let limit = MAX_SOURCE_BYTES as usize;
    fs::write(root.join("favicon.png"), vec![0u8; limit]).unwrap();
    assert_eq!(icon(&root, None, None).unwrap().data.map(|data| data.len()), Some(limit));
    fs::write(root.join("favicon.png"), vec![0u8; limit + 1]).unwrap();
    assert_eq!(icon(&root, None, None), None);
}

// "issues project favicon capabilities for a saved override", "ignores a client
// favicon path hint" (the request names only the project; the saved path is the
// Host's) and "keeps automatic favicon resolution separate from a saved override".
#[test]
fn serves_the_saved_icon_and_otherwise_the_discovered_one() {
    let (_directory, root) = workspace();
    write(&root, "brand/saved.svg", "<svg>saved</svg>");
    write(&root, "brand/hint.svg", "<svg>hint</svg>");
    write(&root, "favicon.svg", "<svg>automatic</svg>");
    let saved = icon(&root, Some("brand/saved.svg"), None).unwrap();
    assert!(
        saved.file_name.ends_with("-saved.svg"),
        "{}",
        saved.file_name
    );
    assert_eq!(saved.data.as_deref(), Some(&b"<svg>saved</svg>"[..]));
    let automatic = icon(&root, None, None).unwrap();
    assert!(automatic.file_name.ends_with("-favicon.svg"));
}

// "issues an exact capability for a saved favicon outside the workspace"
#[test]
fn serves_a_saved_icon_outside_the_workspace() {
    let (_directory, root) = workspace();
    let (_pictures, pictures) = workspace();
    fs::write(pictures.join("custom.png"), [1, 2, 3]).unwrap();
    fs::write(pictures.join("sibling.png"), [4, 5, 6]).unwrap();
    let external = pictures.join("custom.png");
    let favicon = icon(&root, Some(external.to_str().unwrap()), None).unwrap();
    assert_eq!(
        favicon.file_name,
        format!("v{}-custom.png", sha256(&[1, 2, 3]))
    );
    assert_eq!(favicon.mime_type, "image/png");
    assert_eq!(favicon.data, Some(vec![1, 2, 3]));
}

// "rejects a resolved project favicon with a non-image extension"
#[test]
fn a_file_that_is_not_an_image_is_not_served() {
    let (_directory, root) = workspace();
    write(&root, "secret.txt", "not an image");
    assert_eq!(icon(&root, Some("secret.txt"), None), None);
    write(
        &root,
        "index.html",
        r#"<link rel="icon" href="/notes.txt">"#,
    );
    write(&root, "public/notes.txt", "not an image");
    assert_eq!(icon(&root, None, None), None);
}

// A link inside the workspace whose real file is outside it is not served.
#[cfg(unix)]
#[test]
fn an_icon_linked_out_of_the_workspace_is_not_served() {
    let (_directory, root) = workspace();
    let (_outside, outside) = workspace();
    fs::write(outside.join("secret.svg"), "<svg/>").unwrap();
    std::os::unix::fs::symlink(outside.join("secret.svg"), root.join("favicon.svg")).unwrap();
    assert_eq!(icon(&root, None, None), None);
}

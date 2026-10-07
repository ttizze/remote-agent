//! A project's icon: a well-known favicon or app icon file in its root, or the
//! icon an HTML or route file links to. Answers are cached, and a cached file
//! is checked again before it is returned.
use std::{
    collections::HashMap,
    path::{Component, Path, PathBuf},
    sync::{LazyLock, Mutex},
    time::{Duration, Instant},
};

const CAPACITY: usize = 512;
const FOUND_LIFETIME: Duration = Duration::from_secs(600);
const MISSING_LIFETIME: Duration = Duration::from_secs(60);

/// Checked in order.
const CANDIDATES: [&str; 21] = [
    "favicon.svg",
    "favicon.ico",
    "favicon.png",
    "public/favicon.svg",
    "public/favicon.ico",
    "public/favicon.png",
    "app/favicon.ico",
    "app/favicon.png",
    "app/icon.svg",
    "app/icon.png",
    "app/icon.ico",
    "src/favicon.ico",
    "src/favicon.svg",
    "src/app/favicon.ico",
    "src/app/icon.svg",
    "src/app/icon.png",
    "assets/icon.svg",
    "assets/icon.png",
    "assets/logo.svg",
    "assets/logo.png",
    ".idea/icon.svg",
];

/// Files that may declare an icon link.
const SOURCES: [&str; 7] = [
    "index.html",
    "public/index.html",
    "app/routes/__root.tsx",
    "src/routes/__root.tsx",
    "app/root.tsx",
    "src/root.tsx",
    "src/index.html",
];

static LINK_TAG: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r#"(?i)<link\b[^>]*>"#).expect("pattern compiles"));
static TAG_REL: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r#"(?i)\brel=["'](?:icon|shortcut icon)["']"#).expect("pattern compiles")
});
static TAG_HREF: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r#"(?i)\bhref=["']([^"'?]+)"#).expect("pattern compiles"));
static OBJECT_REL: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r#"(?i)\brel\s*:\s*["'](?:icon|shortcut icon)["']"#)
        .expect("pattern compiles")
});
static OBJECT_HREF: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r#"(?i)\bhref\s*:\s*["']([^"'?]+)"#).expect("pattern compiles")
});

static CACHE: LazyLock<Mutex<HashMap<PathBuf, (Instant, Option<PathBuf>)>>> =
    LazyLock::new(Mutex::default);

/// The icon `href` of an HTML `<link>` or of object-like icon metadata whose
/// `rel` and `href` share a brace-free run.
fn icon_href(source: &str) -> Option<String> {
    for tag in LINK_TAG.find_iter(source) {
        let tag = tag.as_str();
        if TAG_REL.is_match(tag)
            && let Some(href) = TAG_HREF.captures(tag)
        {
            return Some(href[1].to_owned());
        }
    }
    source
        .split('}')
        .filter(|run| OBJECT_REL.is_match(run))
        .find_map(|run| OBJECT_HREF.captures(run).map(|href| href[1].to_owned()))
}

/// A file inside `root`; paths that leave it are ignored.
fn file_within(root: &Path, relative: &str) -> Option<PathBuf> {
    let relative = Path::new(relative);
    if relative.is_absolute()
        || relative
            .components()
            .any(|part| matches!(part, Component::ParentDir | Component::Prefix(_)))
    {
        return None;
    }
    let path = root.join(relative);
    path.is_file().then_some(path)
}

fn discover(root: &Path) -> Option<PathBuf> {
    if let Some(found) = CANDIDATES
        .iter()
        .find_map(|candidate| file_within(root, candidate))
    {
        return Some(found);
    }
    for source in SOURCES {
        let Ok(text) = std::fs::read_to_string(root.join(source)) else {
            continue;
        };
        let Some(href) = icon_href(&text) else {
            continue;
        };
        let clean = href.trim_start_matches('/');
        if let Some(found) = [format!("public/{clean}"), clean.to_owned()]
            .iter()
            .find_map(|candidate| file_within(root, candidate))
        {
            return Some(found);
        }
    }
    None
}

/// The project's icon file, or `None`.
pub(crate) fn resolve(root: &Path) -> Option<PathBuf> {
    let cached = CACHE.lock().unwrap().get(root).cloned();
    if let Some((stored, found)) = cached {
        let lifetime = if found.is_some() {
            FOUND_LIFETIME
        } else {
            MISSING_LIFETIME
        };
        if stored.elapsed() < lifetime && found.as_ref().is_none_or(|path| path.is_file()) {
            return found;
        }
    }
    let found = discover(root);
    let mut cache = CACHE.lock().unwrap();
    if cache.len() >= CAPACITY {
        cache.retain(|_, (stored, _)| stored.elapsed() < MISSING_LIFETIME);
    }
    cache.insert(root.to_owned(), (Instant::now(), found.clone()));
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, path: &str, text: &str) {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    // ProjectFaviconResolver.ts: well-known files in order, then declared icons.
    #[test]
    fn finds_well_known_icons_before_declared_ones() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        assert_eq!(discover(root), None);
        write(
            root,
            "index.html",
            r#"<link href="/brand.png?v=2" rel="shortcut icon">"#,
        );
        write(root, "public/brand.png", "png");
        assert_eq!(discover(root), Some(root.join("public/brand.png")));
        write(root, "app/icon.svg", "svg");
        assert_eq!(discover(root), Some(root.join("app/icon.svg")));
        write(root, "favicon.ico", "ico");
        assert_eq!(discover(root), Some(root.join("favicon.ico")));
    }

    #[test]
    fn reads_icon_links_and_metadata_objects() {
        assert_eq!(
            icon_href(r#"<link rel="stylesheet" href="a.css"><link rel='icon' href='/icon.svg'>"#)
                .as_deref(),
            Some("/icon.svg")
        );
        assert_eq!(
            icon_href(r#"links: [{ rel: "preload" }, { href: "/logo.png", rel: "icon" }]"#)
                .as_deref(),
            Some("/logo.png")
        );
        assert_eq!(icon_href(r#"{ rel: "icon" } { href: "/other.png" }"#), None);
        let directory = tempfile::tempdir().unwrap();
        write(
            directory.path(),
            "index.html",
            r#"<link rel="icon" href="../outside.png">"#,
        );
        assert_eq!(discover(directory.path()), None);
    }
}

//! A project's icon: the user's saved icon file, a well-known favicon or app icon
//! file in its root, or the icon an HTML or route file links to. Answers are
//! cached, and a cached file is checked again before it is returned.
use crate::projects::normalize_lexically;
use agent_protocol::models::{ProjectFavicon, image_mime_type};
use std::{
    collections::HashMap,
    fs, io,
    path::{Component, Path, PathBuf},
    sync::{LazyLock, Mutex},
    time::{Duration, Instant},
};

const CAPACITY: usize = 512;
const FOUND_LIFETIME: Duration = Duration::from_secs(10 * 60);
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

// ASCII case and word boundaries, and the ECMAScript `\s`, as browsers' script
// patterns use them.
const SPACE: &str = r"(?u:[\t\n\x0B\x0C\r \u{a0}\u{1680}\u{2000}-\u{200a}\u{2028}\u{2029}\u{202f}\u{205f}\u{3000}\u{feff}])";
fn pattern(source: &str) -> regex::bytes::Regex {
    regex::bytes::Regex::new(&format!("(?i-u){}", source.replace("SPACE", SPACE)))
        .expect("pattern compiles")
}
static LINK: LazyLock<regex::bytes::Regex> = LazyLock::new(|| pattern(r"<link\b"));
static LINK_REL: LazyLock<regex::bytes::Regex> =
    LazyLock::new(|| pattern(r#"\brel=["'](?:icon|shortcut icon)["']"#));
static LINK_HREF: LazyLock<regex::bytes::Regex> = LazyLock::new(|| pattern(r#"\bhref=["']"#));
static OBJECT_REL: LazyLock<regex::bytes::Regex> =
    LazyLock::new(|| pattern(r#"\brelSPACE*:SPACE*["'](?:icon|shortcut icon)["']"#));
static OBJECT_HREF: LazyLock<regex::bytes::Regex> =
    LazyLock::new(|| pattern(r#"\bhrefSPACE*:SPACE*["']([^"'?]+)"#));

/// The icon `href` of the first HTML `<link>` with an icon `rel`, or of
/// object-like icon metadata whose `rel` and `href` share a brace-free run.
fn icon_href(source: &str) -> Option<String> {
    let source = source.as_bytes();
    let value = |start: usize| {
        let end = source[start..]
            .iter()
            .position(|byte| matches!(byte, b'"' | b'\'' | b'?'))
            .map_or(source.len(), |length| start + length);
        (end > start).then(|| String::from_utf8_lossy(&source[start..end]).into_owned())
    };
    for link in LINK.find_iter(source) {
        // The attributes must begin before the tag's first `>`; a value may run past it.
        let Some(close) = source[link.end()..].iter().position(|byte| *byte == b'>') else {
            break;
        };
        let attributes = &source[link.end()..link.end() + close];
        if !LINK_REL.is_match(attributes) {
            continue;
        }
        // The last `href` wins, as a greedy scan of the attributes finds it.
        let hrefs: Vec<_> = LINK_HREF.find_iter(attributes).collect();
        if let Some(href) = hrefs
            .iter()
            .rev()
            .find_map(|href| value(link.end() + href.end()))
        {
            return Some(href);
        }
    }
    source
        .split(|byte| *byte == b'}')
        .filter(|run| OBJECT_REL.is_match(run))
        .find_map(|run| {
            OBJECT_HREF
                .captures(run)
                .map(|href| String::from_utf8_lossy(&href[1]).into_owned())
        })
}

/// Absolute on this platform, or rooted (`\x` on Windows).
fn is_absolute(path: &str) -> bool {
    let path = Path::new(path);
    path.is_absolute() || path.has_root()
}

/// A path strictly inside `root`; absolute paths and paths that leave it are
/// refused.
fn within_root(root: &Path, relative: &str) -> Option<PathBuf> {
    let relative =
        relative.trim_matches(|c: char| c == '\u{feff}' || (c != '\u{85}' && c.is_whitespace()));
    if is_absolute(relative)
        || Path::new(relative)
            .components()
            .any(|part| matches!(part, Component::Prefix(_)))
    {
        return None;
    }
    let path = normalize_lexically(&root.join(relative));
    (path.starts_with(root) && path != root).then_some(path)
}

/// The project root as an absolute directory, `~` expanded.
pub(crate) fn workspace_root(root: &str) -> io::Result<PathBuf> {
    let expanded = crate::projects::expand_home(root.trim());
    let absolute = if expanded.is_absolute() {
        expanded
    } else {
        std::env::current_dir()?.join(expanded)
    };
    let root = normalize_lexically(&absolute);
    match fs::metadata(&root) {
        Ok(metadata) if metadata.is_dir() => Ok(root),
        Ok(_) => Err(io::Error::other(format!(
            "Workspace root is not a directory: {}",
            root.display()
        ))),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("Workspace root does not exist: {}", root.display()),
        )),
        Err(error) => Err(error),
    }
}

/// The first candidate that is a regular file. Only `anywhere` candidates may
/// be absolute.
fn existing_file(root: &Path, candidates: &[&str], anywhere: bool) -> io::Result<Option<PathBuf>> {
    for candidate in candidates {
        let path = if anywhere && is_absolute(candidate) {
            PathBuf::from(candidate)
        } else {
            match within_root(root, candidate) {
                Some(path) => path,
                None => continue,
            }
        };
        match fs::metadata(&path) {
            Ok(metadata) if metadata.is_file() => return Ok(Some(path)),
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(None)
}

fn discover(root: &str, saved: Option<&str>) -> io::Result<Option<PathBuf>> {
    let root = workspace_root(root)?;
    // The saved file may be missing from one checkout of the project; the others
    // keep it, and this one falls back.
    if let Some(saved) = saved
        && let Some(found) = existing_file(&root, &[saved], true)?
    {
        return Ok(Some(found));
    }
    for candidate in CANDIDATES {
        if let Some(found) = existing_file(&root, &[candidate], false)? {
            return Ok(Some(found));
        }
    }
    for source in SOURCES {
        let Some(path) = within_root(&root, source) else {
            continue;
        };
        let text = match fs::read(&path) {
            Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        let Some(href) = icon_href(&text) else {
            continue;
        };
        let clean = href.strip_prefix('/').unwrap_or(&href);
        let public = format!("public/{clean}");
        if let Some(found) = existing_file(&root, &[public.as_str(), clean], false)? {
            return Ok(Some(found));
        }
    }
    Ok(None)
}

type Key = (Option<String>, String);
struct Entry {
    found: Option<PathBuf>,
    expires: Instant,
    used: u64,
}
#[derive(Default)]
struct Entries {
    map: HashMap<Key, Entry>,
    uses: u64,
}

/// Remembers each root and saved path's icon: a found file for 10 minutes, none
/// for 1 minute, failures not at all. The least recently used of 512 entries
/// makes room for a new one.
pub(crate) struct Resolver {
    entries: Mutex<Entries>,
    now: Box<dyn Fn() -> Instant + Send + Sync>,
}
impl Resolver {
    fn new(now: impl Fn() -> Instant + Send + Sync + 'static) -> Self {
        Self {
            entries: Mutex::default(),
            now: Box::new(now),
        }
    }
    fn entries(&self) -> std::sync::MutexGuard<'_, Entries> {
        self.entries
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }
    fn cached_or_discovered(&self, key: &Key) -> io::Result<Option<PathBuf>> {
        let now = (self.now)();
        {
            let mut entries = self.entries();
            entries.uses += 1;
            let uses = entries.uses;
            match entries.map.get_mut(key) {
                Some(entry) if entry.expires > now => {
                    entry.used = uses;
                    return Ok(entry.found.clone());
                }
                Some(_) => {
                    entries.map.remove(key);
                }
                None => {}
            }
        }
        let found = discover(&key.1, key.0.as_deref())?;
        let lifetime = if found.is_some() {
            FOUND_LIFETIME
        } else {
            MISSING_LIFETIME
        };
        let mut entries = self.entries();
        if !entries.map.contains_key(key)
            && entries.map.len() >= CAPACITY
            && let Some(oldest) = entries
                .map
                .iter()
                .min_by_key(|(_, entry)| entry.used)
                .map(|(key, _)| key.clone())
        {
            entries.map.remove(&oldest);
        }
        entries.uses += 1;
        let used = entries.uses;
        entries.map.insert(
            key.clone(),
            Entry {
                found: found.clone(),
                expires: (self.now)() + lifetime,
                used,
            },
        );
        Ok(found)
    }

    /// The icon file of the project at `root`, preferring the `saved` path.
    pub(crate) fn resolve(&self, root: &str, saved: Option<&str>) -> io::Result<Option<PathBuf>> {
        let key = (saved.map(str::to_owned), root.to_owned());
        let Some(found) = self.cached_or_discovered(&key)? else {
            return Ok(None);
        };
        // One check of the remembered file instead of a full walk, so a deleted
        // icon falls back at once.
        match fs::metadata(&found) {
            Ok(metadata) if metadata.is_file() => return Ok(Some(found)),
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        self.entries().map.remove(&key);
        self.cached_or_discovered(&key)
    }
}

static RESOLVER: LazyLock<Resolver> = LazyLock::new(|| Resolver::new(Instant::now));

/// The project's icon, or `None` for the fallback icon.
pub(crate) fn read(
    root: &str,
    saved: Option<&str>,
    known_hash: Option<&str>,
) -> io::Result<Option<ProjectFavicon>> {
    read_with(&RESOLVER, root, saved, known_hash)
}

fn read_with(
    resolver: &Resolver,
    root: &str,
    saved: Option<&str>,
    known_hash: Option<&str>,
) -> io::Result<Option<ProjectFavicon>> {
    let root = workspace_root(root)?;
    let Some(found) = resolver.resolve(&root.to_string_lossy(), saved)? else {
        return Ok(None);
    };
    // An absolute saved path is served as that exact file; anything else must
    // stay inside the root.
    let external = saved.is_some_and(|saved| {
        is_absolute(saved) && normalize_lexically(Path::new(saved)) == normalize_lexically(&found)
    });
    let source = if external {
        found.clone()
    } else {
        found.strip_prefix(&root).unwrap_or(&found).to_owned()
    };
    let Some(mime_type) = image_mime_type(&source.to_string_lossy()) else {
        return Ok(None);
    };
    let canonical = |path: &Path| match dunce::canonicalize(path) {
        Ok(path) => Ok(Some(path)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    };
    let file = if external {
        canonical(&found)?
    } else {
        match (canonical(&root)?, canonical(&found)?) {
            (Some(root), Some(file)) if file.starts_with(&root) && file != root => Some(file),
            _ => None,
        }
    };
    let Some(file) = file.filter(|file| file.is_file()) else {
        return Ok(None);
    };
    let bytes = fs::read(&file)?;
    let hash: String = ring::digest::digest(&ring::digest::SHA256, &bytes)
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let name = source
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    Ok(Some(ProjectFavicon {
        file_name: format!("v{hash}-{name}"),
        mime_type: mime_type.into(),
        data: (known_hash != Some(hash.as_str())).then_some(bytes),
        hash,
    }))
}

#[cfg(test)]
mod tests;

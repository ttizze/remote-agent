use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

/// Path segment of the Host's own managed worktrees (`<repo>/.worktree/<session>`).
pub const MANAGED_WORKTREE_SEGMENT: &str = "/.worktree/";

/// Expands a leading `~` or `~/`; `~user` is left alone.
pub(crate) fn expand_home(value: &str, home: &Path) -> PathBuf {
    if value == "~" {
        return home.to_path_buf();
    }
    match value
        .strip_prefix("~/")
        .or_else(|| value.strip_prefix("~\\"))
    {
        Some(rest) => home.join(rest),
        None => PathBuf::from(value),
    }
}

/// Lexical absolute form, like Node's `path.resolve`.
pub(crate) fn resolve(path: &Path) -> PathBuf {
    let joined;
    let path = if path.is_absolute() {
        path
    } else {
        joined = std::env::current_dir().unwrap_or_default().join(path);
        &joined
    };
    let mut resolved = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => resolved.push(prefix.as_os_str()),
            Component::RootDir => resolved.push(Component::RootDir.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                resolved.pop();
            }
            Component::Normal(part) => resolved.push(part),
        }
    }
    resolved
}

fn is_windows_drive(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() >= 2
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes.len() == 2 || bytes[2] == b'/' || bytes[2] == b'\\')
}

fn is_root(value: &str) -> bool {
    value == "/"
        || value == "\\"
        || (value.len() == 3 && is_windows_drive(value) && value.as_bytes()[2] != b':')
}

/// T3 `normalizeProjectPathForComparison`.
pub(crate) fn comparison_key(path: &Path) -> String {
    let value = path.to_string_lossy();
    let value = value.trim();
    let trimmed = if value.is_empty() || is_root(value) {
        value.to_owned()
    } else {
        let stripped = if value.starts_with('/') {
            value.trim_end_matches('/')
        } else {
            value.trim_end_matches(['/', '\\'])
        };
        if stripped.is_empty() {
            value.to_owned()
        } else if stripped.len() == 2 && is_windows_drive(stripped) {
            format!("{stripped}\\")
        } else {
            stripped.to_owned()
        }
    };
    if is_windows_drive(&trimmed) || trimmed.starts_with("\\\\") {
        trimmed.replace('/', "\\").to_lowercase()
    } else {
        trimmed
    }
}

fn prefix_key(path: &Path) -> String {
    let normalized = format!("{}/", path.to_string_lossy().replace('\\', "/"));
    if cfg!(windows) {
        normalized.to_lowercase()
    } else {
        normalized
    }
}

/// Directories that are never projects: the home and temporary roots, download and
/// scratch folders, and the Host's own data and worktree directories.
pub(crate) struct Exclusions {
    roots: HashSet<String>,
    ancestors: Vec<String>,
}

impl Exclusions {
    /// Each directory is excluded under its given spelling and its real path.
    pub(crate) fn new(
        home: &Path,
        temp: &Path,
        managed: &[PathBuf],
        real_path: impl Fn(&Path) -> Option<PathBuf>,
    ) -> Self {
        let spellings = |path: &Path| {
            let resolved = resolve(path);
            let real = real_path(&resolved);
            std::iter::once(resolved).chain(real)
        };
        let roots = [home, temp, Path::new("/tmp"), Path::new("/private/tmp")]
            .into_iter()
            .flat_map(spellings)
            .map(|root| comparison_key(&root))
            .collect();
        let ancestors = [home.join("Downloads"), home.join("Documents").join("Codex")]
            .iter()
            .chain(managed)
            .flat_map(|ancestor| spellings(ancestor))
            .map(|ancestor| prefix_key(&ancestor))
            .collect();
        Self { roots, ancestors }
    }

    pub(crate) fn excluded(&self, path: &Path) -> bool {
        let key = prefix_key(path);
        self.roots.contains(&comparison_key(path))
            || self
                .ancestors
                .iter()
                .any(|ancestor| key.starts_with(ancestor.as_str()))
            || key.contains(MANAGED_WORKTREE_SEGMENT)
    }
}

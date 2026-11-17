use agent_protocol::paths::is_network_host_path;
use std::sync::LazyLock;

use regex::Regex;

static POSITION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r":([0-9]+)(?::([0-9]+))?$").unwrap());
static HASH_POSITION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^#L([0-9]+)(?:C([0-9]+))?$").unwrap());

pub(super) fn position<'a>(path: &'a str, hash: &str) -> (&'a str, Option<u32>, Option<u32>) {
    let suffix = POSITION.captures(path);
    let hash_position = HASH_POSITION.captures(hash);
    let Some(captures) = suffix.as_ref().or(hash_position.as_ref()) else {
        return (path, None, None);
    };
    let number = |index| {
        captures
            .get(index)
            .and_then(|value| value.as_str().parse::<u32>().ok())
            .filter(|value| *value > 0)
    };
    let path = suffix
        .as_ref()
        .map_or(path, |captures| &path[..captures.get(0).unwrap().start()]);
    (path, number(1), number(2))
}

pub(super) fn windows(path: &str) -> bool {
    path.starts_with(r"\\")
        || (path.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
            && path.as_bytes().get(1) == Some(&b':')
            && (path.len() == 2 || matches!(path.as_bytes().get(2), Some(b'/' | b'\\'))))
}

pub(super) fn absolute(path: &str) -> bool {
    path.starts_with('/') || windows(path)
}

pub(super) fn strip_drive_slash(path: &str) -> &str {
    if path.starts_with('/') && windows(&path[1..]) && !is_network_host_path(&path[1..]) {
        &path[1..]
    } else {
        path
    }
}

pub fn basename(path: &str) -> &str {
    let trimmed = path.trim_end_matches(['/', '\\']);
    if trimmed.is_empty() {
        return path;
    }
    trimmed.rsplit(['/', '\\']).next().unwrap_or(trimmed)
}

/// Match T3's lexical containment check; resolving filesystem aliases belongs to the Host.
pub(super) fn relative(path: &str, cwd: &str) -> Option<String> {
    if cwd.is_empty() {
        return None;
    }
    let path = path.replace('\\', "/");
    let cwd = cwd.replace('\\', "/");
    let path = strip_drive_slash(&path);
    let cwd = strip_drive_slash(&cwd).trim_end_matches('/');
    let (comparable, root) = if windows(cwd) || is_network_host_path(cwd) {
        (path.to_lowercase(), cwd.to_lowercase())
    } else {
        (path.to_owned(), cwd.to_owned())
    };
    if comparable.trim_end_matches('/') == root {
        return Some(".".into());
    }
    // A local filesystem root cannot contain a different network authority.
    if is_network_host_path(path) != is_network_host_path(cwd) {
        return None;
    }
    if !comparable.starts_with(&format!("{root}/")) {
        return None;
    }
    let relative = path.get(cwd.len() + 1..)?;
    let mut depth = 0i32;
    for segment in relative.split('/') {
        match segment {
            ".." => depth -= 1,
            "" | "." => {}
            _ => depth += 1,
        }
        if depth < 0 {
            return None;
        }
    }
    Some(relative.into())
}

pub(super) fn resolve(path: &str, cwd: &str) -> Option<String> {
    // Approved deviation from T3: only the Host knows its actual home directory.
    if absolute(path) || path.starts_with("~/") || path.starts_with("~\\") {
        return Some(path.into());
    }
    if !absolute(cwd) {
        return None;
    }
    let base = cwd.trim_end_matches(['/', '\\']);
    Some(if windows(cwd) {
        format!("{base}\\{}", path.replace('/', "\\"))
    } else {
        format!("{base}/{}", path.trim_start_matches('/'))
    })
}

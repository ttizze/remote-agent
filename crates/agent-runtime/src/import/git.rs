//! Git identity read from `.git` files without spawning git (T3 `@t3tools/shared/git`).
use super::{EntryKind, TranscriptFs, paths::resolve};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectGit {
    /// The normalized origin URL, shared by every clone of one repository.
    pub remote_key: Option<String>,
    /// GitHub `owner/name` when the origin is on GitHub.
    pub repository: Option<String>,
}

pub(crate) enum GitIdentity {
    Repository(ProjectGit),
    /// A linked worktree; its history belongs to the main checkout.
    Worktree,
    NotGit,
}

pub(crate) fn read_git_identity(fs: &dyn TranscriptFs, directory: &Path) -> GitIdentity {
    let git_path = directory.join(".git");
    let Ok(stats) = fs.stat(&git_path) else {
        return GitIdentity::NotGit;
    };
    let mut git_dir = git_path.clone();
    if stats.kind != EntryKind::Directory {
        let pointer = fs.read_to_string(&git_path).unwrap_or_default();
        let target = pointer
            .lines()
            .find_map(|line| line.strip_prefix("gitdir:"))
            .map(str::trim)
            .unwrap_or_default();
        if target.is_empty() {
            return GitIdentity::NotGit;
        }
        git_dir = resolve(&directory.join(target));
        if is_worktree_git_dir(&git_dir.to_string_lossy()) {
            return GitIdentity::Worktree;
        }
    }
    let config = fs
        .read_to_string(&git_dir.join("config"))
        .unwrap_or_default();
    let origin = parse_origin_url(&config);
    GitIdentity::Repository(ProjectGit {
        remote_key: origin.as_deref().map(normalize_remote_url),
        repository: github_repository(origin.as_deref()),
    })
}

/// `[\\/]worktrees[\\/][^\\/]+[\\/]?$`
fn is_worktree_git_dir(path: &str) -> bool {
    let path = path.strip_suffix(['/', '\\']).unwrap_or(path);
    let Some(split) = path.rfind(['/', '\\']) else {
        return false;
    };
    let (parent, name) = (&path[..split], &path[split + 1..]);
    !name.is_empty() && (parent.ends_with("/worktrees") || parent.ends_with("\\worktrees"))
}

fn config_value(raw: &str) -> String {
    let mut out = String::new();
    let mut quoted = false;
    let mut chars = raw.chars().peekable();
    while let Some(char) = chars.next() {
        if char == '\\'
            && let Some(next) = chars.next()
        {
            out.push(next);
            continue;
        }
        if char == '"' {
            quoted = !quoted;
            continue;
        }
        if !quoted && (char == '#' || char == ';') {
            break;
        }
        out.push(char);
    }
    out.trim().to_owned()
}

fn remote_section(line: &str) -> Option<Option<String>> {
    let inner = line.strip_prefix('[')?;
    let close = inner.find(']')?;
    let rest = inner[close + 1..].trim_start();
    if !(rest.is_empty() || rest.starts_with('#') || rest.starts_with(';')) {
        return None;
    }
    let header = inner[..close].trim();
    if header.len() < 6 || !header[..6].eq_ignore_ascii_case("remote") {
        return None;
    }
    let tail = &header[6..];
    if let Some(dotted) = tail.strip_prefix('.') {
        return (!dotted.is_empty() && !dotted.contains(char::is_whitespace))
            .then(|| Some(dotted.to_lowercase()));
    }
    let quoted = tail.trim_start();
    if quoted.len() == tail.len() {
        return None;
    }
    let name = quoted.strip_prefix('"')?.strip_suffix('"')?;
    (!name.is_empty() && !name.contains('"')).then(|| Some(name.to_owned()))
}

/// `remote.origin.url`, or the first remote's URL.
pub(crate) fn parse_origin_url(config: &str) -> Option<String> {
    let mut joined = String::with_capacity(config.len());
    let mut lines = config.split('\n').peekable();
    while let Some(line) = lines.next() {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if let Some(continued) = line.strip_suffix('\\') {
            joined.push_str(continued);
            if let Some(next) = lines.peek_mut() {
                *next = next.trim_start_matches([' ', '\t']);
            }
        } else {
            joined.push_str(line);
            joined.push('\n');
        }
    }
    let mut section: Option<String> = None;
    let (mut origin, mut first) = (None, None);
    for line in joined.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(header) = remote_section(line) {
            section = header;
            continue;
        }
        if line.starts_with('[') {
            section = None;
            continue;
        }
        let Some(name) = &section else { continue };
        let Some(value) = line
            .get(..3)
            .filter(|key| key.eq_ignore_ascii_case("url"))
            .and_then(|_| line[3..].trim_start().strip_prefix('='))
        else {
            continue;
        };
        let url = config_value(value);
        if url.is_empty() {
            continue;
        }
        if name == "origin" {
            origin.get_or_insert(url);
        } else {
            first.get_or_insert(url);
        }
    }
    origin.or(first)
}

fn azure_key(host: &str, segments: &[&str]) -> Option<String> {
    if host != "ssh.dev.azure.com" && host != "vs-ssh.visualstudio.com" {
        return None;
    }
    let [marker, organization, project, repository] = segments else {
        return None;
    };
    if *marker != "v3" || organization.is_empty() || project.is_empty() || repository.is_empty() {
        return None;
    }
    Some(if host == "ssh.dev.azure.com" {
        format!("dev.azure.com/{organization}/{project}/_git/{repository}")
    } else {
        format!("{organization}.visualstudio.com/{project}/_git/{repository}")
    })
}

pub(crate) fn normalize_remote_url(value: &str) -> String {
    let trimmed = value.trim().trim_end_matches('/');
    let trimmed = match trimmed.len().checked_sub(4) {
        Some(at) if trimmed.is_char_boundary(at) && trimmed[at..].eq_ignore_ascii_case(".git") => {
            &trimmed[..at]
        }
        _ => trimmed,
    };
    let normalized = trimmed.to_lowercase();
    if ["ssh://", "http://", "https://", "git://"]
        .iter()
        .any(|scheme| normalized.starts_with(scheme))
    {
        let Ok(url) = url::Url::parse(&normalized) else {
            return normalized;
        };
        let segments: Vec<&str> = url.path().split('/').filter(|s| !s.is_empty()).collect();
        if let Some(host) = url.host_str().filter(|host| !host.is_empty())
            && segments.len() > 1
        {
            return azure_key(host, &segments)
                .unwrap_or_else(|| format!("{host}/{}", segments.join("/")));
        }
    }
    if let Some((user, rest)) = normalized.split_once('@')
        && !user.is_empty()
        && user
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        && let Some((host, path)) = rest.split_once(':')
        && !host.is_empty()
        && !host.contains(['/', ' ', '\t', '\n'])
        && !path.contains(char::is_whitespace)
    {
        let segments: Vec<&str> = path.split('/').collect();
        if segments.len() > 1 && segments.iter().all(|segment| !segment.is_empty()) {
            return azure_key(host, &segments).unwrap_or_else(|| format!("{host}/{path}"));
        }
    }
    normalized
}

pub(crate) fn github_repository(url: Option<&str>) -> Option<String> {
    let trimmed = url?.trim();
    let lower = trimmed.to_lowercase();
    let prefix = [
        "git@github.com:",
        "ssh://git@github.com/",
        "ssh://github.com/",
        "https://github.com/",
        "git://github.com/",
    ]
    .into_iter()
    .find(|prefix| lower.starts_with(prefix))?;
    let rest = &trimmed[prefix.len()..];
    let rest = rest.strip_suffix('/').unwrap_or(rest);
    let rest = match rest.len().checked_sub(4) {
        Some(at) if rest.is_char_boundary(at) && rest[at..].eq_ignore_ascii_case(".git") => {
            &rest[..at]
        }
        _ => rest,
    };
    let (owner, name) = rest.split_once('/')?;
    let valid = |part: &str| {
        !part.is_empty() && !part.contains(['/']) && !part.contains(char::is_whitespace)
    };
    (valid(owner) && valid(name)).then(|| rest.to_owned())
}

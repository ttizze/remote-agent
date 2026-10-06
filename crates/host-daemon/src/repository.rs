//! A project's repository identity from its Git remotes, so clients group
//! checkouts of one repository across Hosts.
use agent_protocol::models::{RepositoryIdentity, RepositoryLocator};
use std::collections::BTreeMap;
use std::path::Path;

/// `git remote -v` fetch URLs by remote name.
fn fetch_urls(stdout: &str) -> BTreeMap<String, String> {
    stdout
        .lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let (name, url, direction) = (parts.next()?, parts.next()?, parts.next()?);
            (parts.next().is_none() && direction == "(fetch)")
                .then(|| (name.to_owned(), url.to_owned()))
        })
        .collect()
}

/// `upstream`, then `origin`, then the first remote by name.
fn primary_remote(remotes: &BTreeMap<String, String>) -> Option<(&str, &str)> {
    ["upstream", "origin"]
        .iter()
        .find_map(|name| remotes.get_key_value(*name))
        .or_else(|| remotes.iter().next())
        .map(|(name, url)| (name.as_str(), url.as_str()))
}

/// The host a remote URL points at: SCP-style and SSH remotes give the host
/// name, other URLs keep an explicit port.
fn remote_host(remote: &str) -> Option<String> {
    let trimmed = remote.trim();
    if let Some((user, rest)) = trimmed.split_once('@')
        && !user.is_empty()
        && user
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        && let Some((host, _)) = rest.split_once(':')
        && !host.is_empty()
        && !host.contains('/')
    {
        return Some(host.to_lowercase());
    }
    let url = url::Url::parse(trimmed).ok()?;
    let host = url.host_str()?.to_lowercase();
    Some(match url.port() {
        Some(port) if url.scheme() != "ssh" => format!("{host}:{port}"),
        _ => host,
    })
}

/// The provider kind.
fn provider_kind(remote: &str) -> Option<&'static str> {
    let host = remote_host(remote)?;
    let name = host
        .rsplit_once(':')
        .filter(|(_, port)| port.bytes().all(|b| b.is_ascii_digit()))
        .map_or(host.as_str(), |(name, _)| name);
    let label = |label: &str| name.split('.').any(|part| part == label);
    Some(
        if name == "codeberg.org" || label("forgejo") || label("gitea") {
            "forgejo"
        } else if name == "github.com" || label("github") {
            "github"
        } else if name == "gitlab.com" || label("gitlab") {
            "gitlab"
        } else if name == "dev.azure.com"
            || name.ends_with(".dev.azure.com")
            || name.ends_with(".visualstudio.com")
        {
            "azure-devops"
        } else if name == "bitbucket.org" || label("bitbucket") {
            "bitbucket"
        } else {
            "unknown"
        },
    )
}

pub(crate) fn identity(remote_name: &str, remote_url: &str, root_path: &str) -> RepositoryIdentity {
    let canonical_key = agent_runtime::normalize_remote_url(remote_url);
    let path = canonical_key
        .split('/')
        .skip(1)
        .collect::<Vec<_>>()
        .join("/");
    let segments: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
    RepositoryIdentity {
        locator: RepositoryLocator {
            source: "git-remote".into(),
            remote_name: remote_name.into(),
            remote_url: remote_url.into(),
        },
        web_url: None,
        root_path: Some(root_path.into()),
        display_name: (!path.is_empty()).then(|| path.clone()),
        provider: provider_kind(remote_url).map(str::to_owned),
        owner: segments.first().map(|owner| (*owner).to_owned()),
        name: segments.last().map(|name| (*name).to_owned()),
        canonical_key,
    }
}

/// The identity of the repository containing `cwd`, or `None` for a folder outside
/// Git or a repository without remotes.
pub(crate) fn resolve(cwd: &Path) -> Option<RepositoryIdentity> {
    let top = crate::git::text(cwd, &["rev-parse", "--show-toplevel"]).ok()?;
    let top = top.trim();
    if top.is_empty() {
        return None;
    }
    let remotes = crate::git::text(Path::new(top), &["remote", "-v"]).ok()?;
    let remotes = fetch_urls(&remotes);
    let (name, url) = primary_remote(&remotes)?;
    Some(identity(name, url, top))
}

#[cfg(test)]
mod tests;

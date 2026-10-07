//! Forgejo and Gitea servers recognized by the logins of their command-line
//! tools (`fj`, `tea`), which also give the repository's web address.
use super::RepositoryIdentity;
use futures_util::future::BoxFuture;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

const TEA_TIMEOUT: Duration = Duration::from_secs(30);

/// A remote's server and repository path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Remote {
    /// The host with any explicit port.
    pub(crate) host: String,
    pub(crate) hostname: String,
    pub(crate) ssh: bool,
    pub(crate) path: String,
}

pub(crate) fn parse_remote(value: &str) -> Option<Remote> {
    let lower = value.get(..8).unwrap_or(value).to_ascii_lowercase();
    if ["http://", "https://", "ssh://"]
        .iter()
        .any(|scheme| lower.starts_with(scheme))
    {
        let url = url::Url::parse(value).ok()?;
        let hostname = url.host_str().unwrap_or_default().to_lowercase();
        let path = url.path().trim_matches('/');
        return Some(Remote {
            host: match url.port() {
                Some(port) => format!("{hostname}:{port}"),
                None => hostname.clone(),
            },
            hostname,
            ssh: url.scheme() == "ssh",
            path: path.strip_suffix(".git").unwrap_or(path).to_owned(),
        });
    }
    // SCP remotes may omit the user name.
    let rest = match value.split_once('@') {
        Some((user, rest)) if !user.is_empty() && !user.contains('/') => rest,
        _ => value,
    };
    let (host, path) = rest.split_once(':')?;
    let mut characters = path.chars();
    if host.is_empty()
        || host.contains('/')
        || characters.next().is_none_or(|first| first == '/')
        || characters.any(|c| matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}'))
    {
        return None;
    }
    let host = host.to_lowercase();
    Some(Remote {
        hostname: host.clone(),
        host,
        ssh: true,
        path: path.strip_suffix(".git").unwrap_or(path).to_owned(),
    })
}

/// One server login of `fj` or `tea`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct Login {
    pub(crate) name: String,
    pub(crate) url: String,
    #[serde(default)]
    pub(crate) ssh_host: Option<String>,
    #[serde(default)]
    pub(crate) valid: Option<String>,
    pub(crate) user: String,
    pub(crate) default: String,
}

/// `tea login list --output json`; anything else lists nothing.
pub(crate) fn parse_logins(raw: &str) -> Vec<Login> {
    serde_json::from_str(raw).unwrap_or_default()
}

/// The single login serving `remote`, or the default one when every match is
/// the same server.
pub(crate) fn match_login<'a>(
    logins: &'a [Login],
    remote: &Remote,
    requested_host: Option<&str>,
    host_only: bool,
) -> Option<&'a Login> {
    let mut matches: Vec<&Login> = vec![];
    for login in logins {
        let Some(url) = parse_remote(&login.url) else {
            continue;
        };
        if requested_host.is_some_and(|host| url.host != host.to_lowercase()) {
            continue;
        }
        let ssh_host = login.ssh_host.as_deref().map(str::to_lowercase);
        let serves = if remote.ssh {
            ssh_host.as_deref() == Some(remote.host.as_str())
                || ssh_host.as_deref() == Some(remote.hostname.as_str())
                || url.hostname == remote.hostname
        } else {
            url.host == remote.host
                && ((host_only && remote.path.is_empty())
                    || url.path.is_empty()
                    || remote.path == url.path
                    || remote.path.starts_with(&format!("{}/", url.path)))
        };
        if !serves {
            continue;
        }
        // One login per name; a later login replaces an earlier one in place.
        match matches.iter_mut().find(|known| known.name == login.name) {
            Some(known) => *known = login,
            None => matches.push(login),
        }
    }
    match matches.as_slice() {
        [only] => Some(only),
        [first, rest @ ..] if rest.iter().all(|login| login.url == first.url) => {
            matches.into_iter().find(|login| login.default == "true")
        }
        _ => None,
    }
}

#[derive(Deserialize)]
struct Keys {
    hosts: BTreeMap<String, Key>,
    #[serde(default)]
    aliases: Option<BTreeMap<String, String>>,
}
#[derive(Deserialize)]
struct Key {
    #[serde(rename = "type")]
    _kind: KeyKind,
    #[serde(rename = "token")]
    _token: String,
}
#[derive(Deserialize)]
enum KeyKind {
    Application,
    OAuth,
}

/// `fj`'s stored hosts as logins. `fj` keeps no scheme, so only a matching
/// `http://` remote selects HTTP.
fn public_logins(keys: &Keys, remote_url: &str) -> Vec<Login> {
    let remote = parse_remote(remote_url);
    keys.hosts
        .keys()
        .flat_map(|host| {
            let Some(url) = parse_remote(&format!("https://{host}")) else {
                return vec![];
            };
            // fj drops URL mounts during whoami and OAuth renewal.
            if !url.path.is_empty() {
                return vec![];
            }
            let http = remote
                .as_ref()
                .is_some_and(|remote| !remote.ssh && remote.host == url.host)
                && remote_url
                    .get(..7)
                    .is_some_and(|scheme| scheme.eq_ignore_ascii_case("http://"));
            let login = Login {
                name: host.clone(),
                url: format!("{}://{host}", if http { "http" } else { "https" }),
                ssh_host: None,
                valid: None,
                user: String::new(),
                default: "false".into(),
            };
            let aliases: Vec<Login> = keys
                .aliases
                .iter()
                .flatten()
                .filter(|(_, target)| *target == host)
                .map(|(alias, _)| Login {
                    ssh_host: Some(alias.clone()),
                    ..login.clone()
                })
                .collect();
            if aliases.is_empty() {
                vec![login]
            } else {
                aliases
            }
        })
        .collect()
}

/// Where `fj` keeps its keys, newest organization name first.
fn keys_paths(
    platform: &str,
    home: &Path,
    data_home: Option<&str>,
    app_data: Option<&str>,
) -> Vec<PathBuf> {
    const ORGANIZATIONS: [&str; 2] = ["forgejo-cli", "Cyborus"];
    match platform {
        "macos" => ORGANIZATIONS
            .iter()
            .map(|organization| {
                home.join("Library/Application Support")
                    .join(format!("{organization}.forgejo-cli"))
                    .join("keys.json")
            })
            .collect(),
        "windows" => {
            let roaming = app_data
                .filter(|path| !path.is_empty())
                .map_or_else(|| home.join("AppData").join("Roaming"), PathBuf::from);
            ORGANIZATIONS
                .iter()
                .map(|organization| {
                    roaming
                        .join(organization)
                        .join("forgejo-cli")
                        .join("data")
                        .join("keys.json")
                })
                .collect()
        }
        _ => {
            let data = data_home
                .map(Path::new)
                .filter(|path| path.is_absolute())
                .map_or_else(|| home.join(".local").join("share"), Path::to_path_buf);
            vec![data.join("forgejo-cli").join("keys.json")]
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Cli {
    Fj,
    Tea,
}

/// The logins a command-line tool has; empty when they cannot be read.
pub(crate) trait Logins: Send + Sync {
    fn list<'a>(&'a self, cwd: &'a str, cli: Cli, remote_url: &'a str)
    -> BoxFuture<'a, Vec<Login>>;
}

/// The logins of the installed `fj` and `tea`.
pub(crate) struct CliLogins {
    keys: Vec<PathBuf>,
    tea: PathBuf,
}

impl CliLogins {
    pub(crate) fn system() -> Self {
        let home = directories::BaseDirs::new()
            .map(|dirs| dirs.home_dir().to_path_buf())
            .unwrap_or_default();
        Self {
            keys: keys_paths(
                std::env::consts::OS,
                &home,
                std::env::var("XDG_DATA_HOME").ok().as_deref(),
                std::env::var("APPDATA").ok().as_deref(),
            ),
            tea: "tea".into(),
        }
    }

    /// The first `fj` keys file there is; `None` when one cannot be read.
    async fn keys(&self) -> Option<Keys> {
        for path in &self.keys {
            if !tokio::fs::try_exists(path).await.ok()? {
                continue;
            }
            let raw = tokio::fs::read_to_string(path).await.ok()?;
            return serde_json::from_str(&raw).ok();
        }
        Some(Keys {
            hosts: BTreeMap::new(),
            aliases: None,
        })
    }

    async fn tea(&self, cwd: &str) -> Vec<Login> {
        let mut command = tokio::process::Command::new(&self.tea);
        command
            .args(["login", "list", "--output", "json"])
            .current_dir(cwd)
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true);
        match tokio::time::timeout(TEA_TIMEOUT, command.output()).await {
            Ok(Ok(output)) if output.status.success() => {
                parse_logins(&String::from_utf8_lossy(&output.stdout))
            }
            _ => vec![],
        }
    }
}

impl Logins for CliLogins {
    fn list<'a>(
        &'a self,
        cwd: &'a str,
        cli: Cli,
        remote_url: &'a str,
    ) -> BoxFuture<'a, Vec<Login>> {
        Box::pin(async move {
            match cli {
                Cli::Fj => self
                    .keys()
                    .await
                    .map(|keys| public_logins(&keys, remote_url))
                    .unwrap_or_default(),
                Cli::Tea => self.tea(cwd).await,
            }
        })
    }
}

/// An identity on a server `fj` or `tea` has a login for becomes a Forgejo
/// repository with its web address on that server. Other identities, and
/// remotes no login serves, stay as they are.
pub(crate) async fn refine(
    identity: RepositoryIdentity,
    logins: &dyn Logins,
) -> RepositoryIdentity {
    let Some(remote) = parse_remote(&identity.locator.remote_url) else {
        return identity;
    };
    let Some(root) = identity.root_path.as_deref() else {
        return identity;
    };
    if identity
        .provider
        .as_deref()
        .is_some_and(|provider| provider != "unknown" && provider != "forgejo")
    {
        return identity;
    }
    let mut server = None;
    for cli in [Cli::Fj, Cli::Tea] {
        let listed = logins.list(root, cli, &identity.locator.remote_url).await;
        if let Some(login) = match_login(&listed, &remote, None, false) {
            server = Some(login.url.clone());
            break;
        }
    }
    let Some(server) = server else {
        return identity;
    };
    let base = server.trim_end_matches('/');
    let Ok(base_url) = url::Url::parse(base) else {
        return identity;
    };
    let base_path = base_url.path().trim_matches('/');
    let path = match remote.path.strip_prefix(&format!("{base_path}/")) {
        Some(path) if !remote.ssh && !base_path.is_empty() => path,
        _ => &remote.path,
    };
    RepositoryIdentity {
        provider: Some("forgejo".into()),
        web_url: Some(format!("{base}/{path}")),
        ..identity
    }
}

#[cfg(test)]
mod tests;

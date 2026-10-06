//! Linked issue and pull request subjects for title generation (T3
//! `ThreadTitleLinks.ts` with the GitHub and GitLab `resolveLink`).
use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::LazyLock;
use std::time::Duration;

const MAX_LINKS: usize = 2;
const LOOKUP_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_OUTPUT_BYTES: usize = 32_000;
const TITLE_UNITS: usize = 300;
const BODY_UNITS: usize = 1_200;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Subject {
    pub title: String,
    pub body: Option<String>,
}

/// A pending subject read; `None` output means the subject could not be read.
pub(crate) type SubjectRead = BoxFuture<'static, Option<Subject>>;
/// T3 `SourceControlProviderRegistry.resolveLink`: `None` for an unsupported link.
pub(crate) type ResolveLink = dyn Fn(&url::Url, &str) -> Option<SubjectRead> + Send + Sync;

static GITHUB: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"^/([\w.-]+)/([\w.-]+)/(?:pull|issues)/([1-9][0-9]*)(?:/.*)?$")
        .expect("pattern compiles")
});
static GITLAB: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"^/(.+)/-/(merge_requests|issues)/([1-9][0-9]*)(?:/.*)?$")
        .expect("pattern compiles")
});

/// JavaScript `encodeURIComponent`.
fn encode_component(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&byte) {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

/// The subject as T3 encodes it, title first.
#[derive(Serialize)]
struct Encoded {
    title: String,
    body: String,
}

#[derive(Deserialize)]
struct Fields {
    title: String,
    body: Option<String>,
    description: Option<String>,
}

/// Runs a CLI with T3's limits; any failure reads as unavailable.
fn read_with(
    program: &'static str,
    args: Vec<String>,
    env: &'static [(&'static str, &'static str)],
    cwd: &str,
) -> SubjectRead {
    let cwd = cwd.to_owned();
    Box::pin(async move {
        let mut command = tokio::process::Command::new(program);
        command
            .args(&args)
            .envs(env.iter().copied())
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true);
        if Path::new(&cwd).is_dir() {
            command.current_dir(&cwd);
        }
        let output = command.output().await.ok()?;
        if !output.status.success() || output.stdout.len() > MAX_OUTPUT_BYTES {
            return None;
        }
        let fields: Fields = serde_json::from_slice(&output.stdout).ok()?;
        Some(Subject {
            title: fields.title,
            body: fields.body.or(fields.description),
        })
    })
}

/// Automatic enrichment never sends ambient CLI credentials to a host taken from
/// message text, so only github.com and gitlab.com links resolve.
pub(crate) fn resolve_link(url: &url::Url, cwd: &str) -> Option<SubjectRead> {
    if url.scheme() != "https" || !url.username().is_empty() || url.password().is_some() {
        return None;
    }
    match url.host_str()? {
        "github.com" => {
            let found = GITHUB.captures(url.path())?;
            let endpoint = format!("repos/{}/{}/issues/{}", &found[1], &found[2], &found[3]);
            Some(read_with(
                "gh",
                [
                    "api",
                    "--hostname",
                    "github.com",
                    &endpoint,
                    "--jq",
                    "{title, body}",
                ]
                .map(str::to_owned)
                .to_vec(),
                &[("GH_PROMPT_DISABLED", "1")],
                cwd,
            ))
        }
        "gitlab.com" => {
            let found = GITLAB.captures(url.path())?;
            let endpoint = format!(
                "projects/{}/{}/{}",
                encode_component(&found[1]),
                &found[2],
                &found[3]
            );
            Some(read_with(
                "glab",
                ["api", "--hostname", "gitlab.com", &endpoint]
                    .map(str::to_owned)
                    .to_vec(),
                &[],
                cwd,
            ))
        }
        _ => None,
    }
}

fn utf16_prefix(text: &str, units: usize) -> String {
    let mut used = 0;
    text.chars()
        .take_while(|c| {
            used += c.len_utf16();
            used <= units
        })
        .collect()
}

/// T3 `resolveThreadTitleLinks` over the runtime's link candidates: the first two
/// links a provider supports, each `url` and its encoded subject, or
/// `url: unavailable`.
pub(crate) async fn title_link_context(
    cwd: &str,
    candidates: &[String],
    resolve: &ResolveLink,
) -> Option<String> {
    let reads: Vec<(String, SubjectRead)> = candidates
        .iter()
        .filter_map(|candidate| {
            let url = url::Url::parse(candidate).ok()?;
            Some((candidate.clone(), resolve(&url, cwd)?))
        })
        .take(MAX_LINKS)
        .collect();
    if reads.is_empty() {
        return None;
    }
    let subjects =
        futures_util::future::join_all(reads.into_iter().map(|(url, read)| async move {
            match tokio::time::timeout(LOOKUP_TIMEOUT, read).await {
                Ok(Some(subject)) => {
                    let encoded = Encoded {
                        title: utf16_prefix(&subject.title, TITLE_UNITS),
                        body: subject
                            .body
                            .map(|body| utf16_prefix(&body, BODY_UNITS))
                            .unwrap_or_default(),
                    };
                    let encoded = serde_json::to_string(&encoded).unwrap_or_default();
                    format!("{url}\n{encoded}")
                }
                _ => format!("{url}: unavailable"),
            }
        }))
        .await;
    Some(subjects.join("\n\n"))
}

#[cfg(test)]
mod tests;

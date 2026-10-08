//! Small Host-owned Agent Client Protocol registry adapter.
//!
//! The registry is credential-free and only describes how an agent is
//! started. The adapter keeps that untrusted description at the boundary,
//! validates the platform command, and gives the ACP runtime a short-lived
//! stdio process for probing or a future conversation owner.

use agent_protocol::operations::{
    AcpProbeAuthMethod, AcpProbeModel, AcpProbeResult, AcpRegistryAgent, AcpRegistrySearchResult,
    AcpSessionManagement, PrepareAcpAgent, PreparedAcpAgent, ProbeAcpAgent, SearchAcpRegistry,
};
use futures_util::StreamExt;
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

const REGISTRY_URL: &str = "https://cdn.agentclientprotocol.com/registry/v1/latest/registry.json";
const MAX_REGISTRY_BYTES: usize = 1024 * 1024;
const MAX_REGISTRY_AGENTS: usize = 512;
const MAX_RESULTS: usize = 20;
const MAX_ARCHIVE_BYTES: usize = 1024 * 1024 * 1024;
const MAX_METADATA_BYTES: usize = 2_048;
const PACKAGE_INSTALL_TIMEOUT: Duration = Duration::from_secs(20 * 60);

#[derive(Debug, Deserialize)]
struct RegistryEnvelope {
    #[serde(default)]
    agents: Vec<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct RegistryAgent {
    id: String,
    name: String,
    version: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    authors: Vec<String>,
    #[serde(default)]
    license: Option<String>,
    #[serde(default)]
    website: Option<String>,
    #[serde(default)]
    repository: Option<String>,
    distribution: RegistryDistribution,
    #[serde(default)]
    icon: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct RegistryDistribution {
    #[serde(default)]
    binary: Option<BTreeMap<String, RegistryBinary>>,
    #[serde(default)]
    npx: Option<RegistryPackage>,
    #[serde(default)]
    uvx: Option<RegistryPackage>,
}

#[derive(Debug, Deserialize)]
struct RegistryBinary {
    archive: String,
    cmd: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default)]
    sha256: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RegistryPackage {
    package: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
}

fn platform_target() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "darwin-aarch64",
        ("macos", "x86_64") => "darwin-x86_64",
        ("linux", "aarch64") => "linux-aarch64",
        ("linux", "x86_64") => "linux-x86_64",
        ("windows", "aarch64") => "windows-aarch64",
        ("windows", "x86_64") => "windows-x86_64",
        _ => "",
    }
}

fn valid_token(value: &str, max: usize) -> bool {
    !value.trim().is_empty()
        && value.len() <= max
        && !value.bytes().any(|byte| byte.is_ascii_control())
}

fn valid_version(value: &str) -> bool {
    let value = value.trim_start_matches('v');
    if value.is_empty() || value.len() > 128 || value == "." || value == ".." {
        return false;
    }
    let Some(first_dot) = value.find('.') else {
        return false;
    };
    let Some(second_dot) = value[first_dot + 1..].find('.') else {
        return false;
    };
    let second_dot = first_dot + 1 + second_dot;
    let suffix_start = value[second_dot + 1..]
        .find(['-', '+'])
        .map(|index| second_dot + 1 + index)
        .unwrap_or(value.len());
    let core = [
        &value[..first_dot],
        &value[first_dot + 1..second_dot],
        &value[second_dot + 1..suffix_start],
    ];
    core.iter()
        .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
        && (suffix_start == value.len()
            || (!value[suffix_start + 1..].is_empty()
                && value[suffix_start + 1..].bytes().all(|byte| {
                    byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'+')
                })))
}

fn valid_argument(value: &str) -> bool {
    value.len() <= 1_024 && !value.bytes().any(|byte| byte.is_ascii_control())
}

fn valid_environment(environment: &BTreeMap<String, String>) -> bool {
    environment.len() <= 256
        && environment.iter().all(|(name, value)| {
            !name.is_empty()
                && name.len() <= 256
                && name.bytes().enumerate().all(|(index, byte)| {
                    byte.is_ascii_alphanumeric() || (index > 0 && byte == b'_')
                })
                && (name.as_bytes()[0].is_ascii_alphabetic() || name.as_bytes()[0] == b'_')
                && valid_argument(value)
        })
}

fn valid_https_url(value: &str) -> bool {
    if value.len() > 2_048 {
        return false;
    }
    let Ok(url) = url::Url::parse(value) else {
        return false;
    };
    url.scheme() == "https" && url.username().is_empty() && url.password().is_none()
}

fn valid_relative_command(value: &str) -> bool {
    valid_argument(value)
        && !value.is_empty()
        && !Path::new(value).is_absolute()
        && !value.replace('\\', "/").split('/').any(|part| part == "..")
}

fn valid_sha256(value: Option<&str>) -> bool {
    value
        .is_none_or(|value| value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

fn valid_agent_id(value: &str) -> bool {
    valid_token(value, 128)
        && value.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || (index > 0 && matches!(byte, b'.' | b'_' | b'-'))
        })
}

fn exact_package(package: &str, runner: &str) -> bool {
    let separator = if runner == "uvx" {
        package.rfind("==").map(|index| (index, 2))
    } else {
        package.rfind('@').map(|index| (index, 1))
    };
    let Some((index, length)) = separator else {
        return false;
    };
    let (name, version) = package.split_at(index);
    let version = &version[length..];
    let valid_name = if runner == "npx" {
        if let Some(name) = name.strip_prefix('@') {
            let mut parts = name.split('/');
            let scope = parts.next().unwrap_or_default();
            let package = parts.next();
            parts.next().is_none()
                && !scope.is_empty()
                && package.is_some_and(|package| !package.is_empty())
                && name.bytes().all(|byte| {
                    byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'.' | b'_' | b'-')
                })
        } else {
            !name.is_empty()
                && name.as_bytes()[0].is_ascii_alphanumeric()
                && !name.contains('/')
                && name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        }
    } else {
        !name.is_empty()
            && name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    };
    valid_name && valid_version(version)
}

fn package_command(package: &str, runner: &str) -> Option<String> {
    exact_package(package, runner).then(|| runner.to_owned())
}

fn package_name(package: &str) -> Option<&str> {
    let separator = package.rfind('@').or_else(|| package.rfind("=="))?;
    (separator > 0).then_some(&package[..separator])
}

fn package_version<'a>(package: &'a str, runner: &str) -> Option<&'a str> {
    let (separator, length) = if runner == "uvx" {
        (package.rfind("==")?, 2)
    } else {
        (package.rfind('@')?, 1)
    };
    (separator > 0).then_some(&package[separator + length..])
}

fn package_install_root(state_dir: &Path, agent: &AcpRegistryAgent) -> Option<PathBuf> {
    let package = agent.package_spec.as_deref()?;
    let runner = match agent.distribution.as_str() {
        "npx" => "npm",
        "uvx" => "python",
        _ => return None,
    };
    let package_component = package_name(package)?
        .replace(['/', '\\'], "_")
        .trim_start_matches('.')
        .to_owned();
    (!package_component.is_empty() && package_component != "." && package_component != "..").then(
        || {
            state_dir
                .join("acp-registry")
                .join("packages")
                .join(&agent.id)
                .join(&agent.version)
                .join(runner)
                .join(package_component)
        },
    )
}

fn executable_names(agent: &AcpRegistryAgent) -> Vec<String> {
    let package = agent
        .package_spec
        .as_deref()
        .and_then(package_name)
        .and_then(|name| name.rsplit('/').next())
        .unwrap_or_default();
    let mut names = vec![agent.id.clone(), package.to_owned()];
    if let Some(name) = package.strip_suffix("-acp") {
        names.push(name.to_owned());
    }
    names.sort();
    names.dedup();
    names
}

async fn package_executable(
    root: &Path,
    agent: &AcpRegistryAgent,
) -> Result<Option<PathBuf>, String> {
    let bin = root.join("bin");
    let package = agent.package_spec.as_deref().and_then(package_name);
    if agent.distribution == "npx"
        && let (Some(package), Some(version)) = (
            package,
            agent
                .package_spec
                .as_deref()
                .and_then(|package| package_version(package, "npx")),
        )
    {
        let package_root = [
            root.join("lib").join("node_modules").join(package),
            root.join("node_modules").join(package),
        ]
        .into_iter()
        .find(|path| path.join("package.json").is_file());
        let Some(package_root) = package_root else {
            return Ok(None);
        };
        let manifest_path = package_root.join("package.json");
        let bytes = tokio::fs::read(&manifest_path)
            .await
            .map_err(|error| format!("ACP package manifest could not be read: {error}"))?;
        if bytes.len() > MAX_METADATA_BYTES {
            return Err("ACP package manifest is too large".into());
        }
        let manifest: serde_json::Value = serde_json::from_slice(&bytes)
            .map_err(|error| format!("ACP package manifest is invalid: {error}"))?;
        if manifest["name"].as_str() != Some(package)
            || manifest["version"].as_str() != Some(version)
        {
            return Ok(None);
        }
        let command_names = match &manifest["bin"] {
            serde_json::Value::String(_) => package
                .rsplit('/')
                .next()
                .map(str::to_owned)
                .into_iter()
                .collect(),
            serde_json::Value::Object(commands) => {
                let package_name = package.rsplit('/').next().unwrap_or(package);
                let selected = if commands.len() == 1 {
                    commands.keys().next().cloned()
                } else {
                    commands
                        .get(package_name)
                        .map(|_| package_name.to_owned())
                        .or_else(|| commands.get(&agent.id).map(|_| agent.id.clone()))
                };
                selected.into_iter().collect()
            }
            _ => Vec::new(),
        };
        for name in command_names {
            for candidate in [
                bin.join(&name),
                bin.join(format!("{name}.cmd")),
                bin.join(format!("{name}.exe")),
                root.join(&name),
                root.join(format!("{name}.cmd")),
                root.join(format!("{name}.exe")),
            ] {
                if tokio::fs::metadata(&candidate)
                    .await
                    .is_ok_and(|metadata| metadata.is_file())
                {
                    return Ok(Some(candidate));
                }
            }
        }
        return Ok(None);
    }
    for name in executable_names(agent) {
        for candidate in [
            bin.join(&name),
            bin.join(format!("{name}.cmd")),
            bin.join(format!("{name}.exe")),
            root.join(&name),
        ] {
            if tokio::fs::metadata(&candidate)
                .await
                .is_ok_and(|metadata| metadata.is_file())
            {
                return Ok(Some(candidate));
            }
        }
    }
    let Some(package) = agent.package_spec.as_deref().and_then(package_name) else {
        return Ok(None);
    };
    let manifest_path = root
        .join("lib")
        .join("node_modules")
        .join(package)
        .join("package.json");
    let bytes = match tokio::fs::read(&manifest_path).await {
        Ok(bytes) => bytes,
        Err(_) => return Ok(None),
    };
    let manifest: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("ACP package manifest is invalid: {error}"))?;
    let command = match &manifest["bin"] {
        serde_json::Value::String(command) => Some(command.clone()),
        serde_json::Value::Object(commands) => commands.keys().next().cloned(),
        _ => None,
    };
    Ok(command.and_then(|command| {
        let candidate = bin.join(command);
        candidate.is_file().then_some(candidate)
    }))
}

async fn install_package(agent: &AcpRegistryAgent, state_dir: &Path) -> Result<PathBuf, String> {
    let root = package_install_root(state_dir, agent)
        .ok_or_else(|| "ACP agent does not use a package distribution".to_owned())?;
    tokio::fs::create_dir_all(&root)
        .await
        .map_err(|error| format!("ACP package directory could not be created: {error}"))?;
    if let Some(executable) = package_executable(&root, agent).await? {
        return Ok(executable);
    }
    let package = agent
        .package_spec
        .as_deref()
        .ok_or_else(|| "ACP package specification is missing".to_owned())?;
    let (program, args) = if agent.distribution == "npx" {
        (
            "npm",
            vec![
                "install".into(),
                "--global".into(),
                "--prefix".into(),
                root.to_string_lossy().into_owned(),
                "--no-audit".into(),
                "--no-fund".into(),
                "--no-update-notifier".into(),
                "--package-lock=false".into(),
                package.into(),
            ],
        )
    } else {
        (
            "uv",
            vec![
                "tool".into(),
                "install".into(),
                "--force".into(),
                "--bin-dir".into(),
                root.join("bin").to_string_lossy().into_owned(),
                package.into(),
            ],
        )
    };
    let status = tokio::time::timeout(
        PACKAGE_INSTALL_TIMEOUT,
        tokio::process::Command::new(program)
            .args(args)
            .envs(&agent.environment)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .status(),
    )
    .await
    .map_err(|_| "ACP package installation timed out".to_owned())?
    .map_err(|error| format!("ACP package manager could not start: {error}"))?;
    if !status.success() {
        return Err(format!("ACP package manager exited with {status}"));
    }
    package_executable(&root, agent)
        .await?
        .ok_or_else(|| "ACP package did not expose a runnable command".to_owned())
}

fn binary_install_root(state_dir: &Path, agent: &AcpRegistryAgent) -> PathBuf {
    state_dir
        .join("acp-registry")
        .join("binaries")
        .join(&agent.id)
        .join(&agent.version)
}

fn binary_command_candidates(root: &Path, command: &str) -> Option<Vec<PathBuf>> {
    let command = command.replace('\\', "/");
    let command = command.trim_start_matches("./");
    let name = Path::new(command).file_name()?.to_owned();
    Some(vec![
        root.join(command),
        root.join("bin").join(&name),
        root.join(name),
    ])
}

async fn binary_command_path(root: &Path, command: &str) -> Option<PathBuf> {
    let canonical_root = tokio::fs::canonicalize(root).await.ok()?;
    for candidate in binary_command_candidates(root, command)? {
        let Ok(metadata) = tokio::fs::symlink_metadata(&candidate).await else {
            continue;
        };
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            continue;
        }
        let canonical = tokio::fs::canonicalize(&candidate).await.ok()?;
        if canonical.starts_with(&canonical_root) {
            return Some(candidate);
        }
    }
    None
}

fn archive_is_safe(listing: &[u8]) -> bool {
    String::from_utf8_lossy(listing).lines().all(|entry| {
        let entry = entry.trim().replace('\\', "/");
        !entry.starts_with('/')
            && !entry.split('/').any(|component| component == "..")
            && !entry.contains('\0')
    })
}

async fn archive_listing(program: &str, args: &[String]) -> Result<Vec<u8>, String> {
    let output = tokio::time::timeout(
        Duration::from_secs(30),
        tokio::process::Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| "ACP binary archive listing timed out".to_owned())?
    .map_err(|error| format!("ACP archive tool could not start: {error}"))?;
    if !output.status.success() || output.stdout.len() > MAX_METADATA_BYTES {
        return Err("ACP binary archive could not be inspected".into());
    }
    archive_is_safe(&output.stdout)
        .then_some(output.stdout)
        .ok_or_else(|| "ACP binary archive contains an unsafe path".into())
}

async fn response_bytes(
    response: reqwest::Response,
    maximum: usize,
    label: &str,
) -> Result<Vec<u8>, String> {
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| format!("{label} response failed: {error}"))?;
        if bytes.len().saturating_add(chunk.len()) > maximum {
            return Err(format!("{label} response is too large"));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

async fn install_binary(agent: &AcpRegistryAgent, state_dir: &Path) -> Result<PathBuf, String> {
    let archive = agent
        .archive
        .as_deref()
        .ok_or_else(|| "ACP binary distribution is missing its archive".to_owned())?;
    let root = binary_install_root(state_dir, agent);
    tokio::fs::create_dir_all(&root)
        .await
        .map_err(|error| format!("ACP binary directory could not be created: {error}"))?;
    if let Some(command) = binary_command_path(&root, &agent.command).await {
        return Ok(command);
    }
    let response = tokio::time::timeout(
        PACKAGE_INSTALL_TIMEOUT,
        crate::http::client().get(archive).send(),
    )
    .await
    .map_err(|_| "ACP binary download timed out".to_owned())?
    .map_err(|error| format!("ACP binary download failed: {error}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "ACP binary download returned {}",
            response.status()
        ));
    }
    let bytes = tokio::time::timeout(
        PACKAGE_INSTALL_TIMEOUT,
        response_bytes(response, MAX_ARCHIVE_BYTES, "ACP binary archive"),
    )
    .await
    .map_err(|_| "ACP binary download timed out".to_owned())??;
    if let Some(expected) = agent.sha256.as_deref() {
        let digest = ring::digest::digest(&ring::digest::SHA256, &bytes);
        let actual = digest
            .as_ref()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        if !actual.eq_ignore_ascii_case(expected) {
            return Err("ACP binary archive checksum did not match the registry".into());
        }
    }
    let archive_path = root.join("download.archive");
    tokio::fs::write(&archive_path, &bytes)
        .await
        .map_err(|error| format!("ACP binary archive could not be saved: {error}"))?;
    let lower = archive.to_ascii_lowercase();
    let (program, args) = if lower.contains(".zip") {
        (
            "unzip",
            vec![
                "-q".into(),
                archive_path.to_string_lossy().into_owned(),
                "-d".into(),
                root.to_string_lossy().into_owned(),
            ],
        )
    } else if lower.contains(".tar") || lower.contains(".tgz") || lower.contains(".tbz2") {
        (
            "tar",
            vec![
                "-xf".into(),
                archive_path.to_string_lossy().into_owned(),
                "-C".into(),
                root.to_string_lossy().into_owned(),
            ],
        )
    } else {
        let command_path = root.join(agent.command.replace('\\', "/"));
        if let Some(parent) = command_path.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|error| {
                format!("ACP binary command directory could not be created: {error}")
            })?;
        }
        tokio::fs::write(&command_path, &bytes)
            .await
            .map_err(|error| format!("ACP binary could not be installed: {error}"))?;
        let _ = tokio::fs::remove_file(&archive_path).await;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = tokio::fs::metadata(&command_path)
                .await
                .map_err(|error| format!("ACP binary metadata could not be read: {error}"))?
                .permissions();
            permissions.set_mode(0o700);
            tokio::fs::set_permissions(&command_path, permissions)
                .await
                .map_err(|error| format!("ACP binary permissions could not be set: {error}"))?;
        }
        return binary_command_path(&root, &agent.command)
            .await
            .ok_or_else(|| "ACP binary could not be installed at its configured command".into());
    };
    let list_args = if program == "unzip" {
        vec!["-Z1".into(), archive_path.to_string_lossy().into_owned()]
    } else {
        vec!["-tf".into(), archive_path.to_string_lossy().into_owned()]
    };
    if let Err(error) = archive_listing(program, &list_args).await {
        let _ = tokio::fs::remove_file(&archive_path).await;
        return Err(error);
    }
    let status = match tokio::time::timeout(
        PACKAGE_INSTALL_TIMEOUT,
        tokio::process::Command::new(program)
            .args(args)
            .envs(&agent.environment)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .status(),
    )
    .await
    {
        Ok(Ok(status)) => status,
        Ok(Err(error)) => {
            let _ = tokio::fs::remove_file(&archive_path).await;
            return Err(format!("ACP binary archive tool could not start: {error}"));
        }
        Err(_) => {
            let _ = tokio::fs::remove_file(&archive_path).await;
            return Err("ACP binary extraction timed out".into());
        }
    };
    let _ = tokio::fs::remove_file(&archive_path).await;
    if !status.success() {
        return Err(format!("ACP binary archive tool exited with {status}"));
    }
    binary_command_path(&root, &agent.command)
        .await
        .ok_or_else(|| "ACP binary archive did not expose its configured command".into())
}

async fn runtime_agent(
    mut agent: AcpRegistryAgent,
    state_dir: &Path,
) -> Result<AcpRegistryAgent, String> {
    let executable = if agent.package_spec.is_some() {
        Some(install_package(&agent, state_dir).await?)
    } else if agent.distribution == "binary" {
        Some(install_binary(&agent, state_dir).await?)
    } else {
        None
    };
    if let Some(executable) = executable {
        agent.command = executable.to_string_lossy().into_owned();
        agent.args = agent.runtime_args.clone();
    }
    Ok(agent)
}

fn official_icon(id: &str, icon: Option<String>) -> Option<String> {
    let expected = format!("https://cdn.agentclientprotocol.com/registry/v1/latest/{id}.svg");
    icon.filter(|icon| icon == &expected)
}

fn bounded_authors(authors: Vec<String>) -> Option<Vec<String>> {
    (authors.len() <= 16 && authors.iter().all(|author| valid_token(author, 256)))
        .then_some(authors)
}

fn bounded_url(value: Option<String>) -> Option<String> {
    value.filter(|value| valid_https_url(value))
}

fn into_agent(raw: RegistryAgent) -> Option<AcpRegistryAgent> {
    if !valid_agent_id(&raw.id)
        || !valid_token(&raw.name, 160)
        || !valid_version(&raw.version)
        || raw.description.len() > 1_024
    {
        return None;
    }
    let authors = bounded_authors(raw.authors)?;
    let website = bounded_url(raw.website);
    let repository = bounded_url(raw.repository);
    let icon = official_icon(&raw.id, raw.icon);
    let target = platform_target();
    if let Some(binary) = raw
        .distribution
        .binary
        .as_ref()
        .and_then(|binary| binary.get(target))
        && valid_https_url(&binary.archive)
        && valid_relative_command(&binary.cmd)
        && binary.args.len() <= 64
        && binary.args.iter().all(|argument| valid_argument(argument))
        && valid_environment(&binary.env)
        && valid_sha256(binary.sha256.as_deref())
    {
        return Some(AcpRegistryAgent {
            id: raw.id,
            name: raw.name,
            version: raw.version,
            description: raw.description,
            authors,
            license: raw.license.filter(|license| valid_token(license, 128)),
            website,
            repository,
            icon: icon.clone(),
            distribution: "binary".into(),
            command: binary.cmd.clone(),
            args: binary.args.clone(),
            environment: binary.env.clone(),
            integrity: if binary.sha256.is_some() {
                "sha256".into()
            } else {
                "registry".into()
            },
            archive: Some(binary.archive.clone()),
            sha256: binary.sha256.clone(),
            package_spec: None,
            runtime_args: binary.args.clone(),
        });
    }
    if let Some(package) = raw.distribution.npx.as_ref()
        && valid_token(&package.package, 256)
        && package_command(&package.package, "npx").is_some()
        && package.args.len() <= 64
        && package.args.iter().all(|argument| valid_argument(argument))
        && valid_environment(&package.env)
    {
        let mut args = vec!["--yes".into(), package.package.clone()];
        args.extend(package.args.clone());
        return Some(AcpRegistryAgent {
            id: raw.id,
            name: raw.name,
            version: raw.version,
            description: raw.description,
            authors,
            license: raw.license.filter(|license| valid_token(license, 128)),
            website,
            repository,
            icon: icon.clone(),
            distribution: "npx".into(),
            command: "npx".into(),
            args,
            environment: package.env.clone(),
            integrity: "registry".into(),
            archive: None,
            sha256: None,
            package_spec: Some(package.package.clone()),
            runtime_args: package.args.clone(),
        });
    }
    if let Some(package) = raw.distribution.uvx.as_ref()
        && valid_token(&package.package, 256)
        && package_command(&package.package, "uvx").is_some()
        && package.args.len() <= 64
        && package.args.iter().all(|argument| valid_argument(argument))
        && valid_environment(&package.env)
    {
        let mut args = vec![package.package.clone()];
        args.extend(package.args.clone());
        return Some(AcpRegistryAgent {
            id: raw.id,
            name: raw.name,
            version: raw.version,
            description: raw.description,
            authors,
            license: raw.license.filter(|license| valid_token(license, 128)),
            website,
            repository,
            icon,
            distribution: "uvx".into(),
            command: "uvx".into(),
            args,
            environment: package.env.clone(),
            integrity: "registry".into(),
            archive: None,
            sha256: None,
            package_spec: Some(package.package.clone()),
            runtime_args: package.args.clone(),
        });
    }
    None
}

async fn fetch_agents() -> Result<Vec<AcpRegistryAgent>, String> {
    let response = tokio::time::timeout(
        Duration::from_secs(30),
        crate::http::client().get(REGISTRY_URL).send(),
    )
    .await
    .map_err(|_| "ACP registry request timed out".to_owned())?
    .map_err(|error| format!("ACP registry request failed: {error}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "ACP registry request returned {}",
            response.status()
        ));
    }
    let bytes = tokio::time::timeout(
        Duration::from_secs(30),
        response_bytes(response, MAX_REGISTRY_BYTES, "ACP registry"),
    )
    .await
    .map_err(|_| "ACP registry response timed out".to_owned())??;
    let envelope: RegistryEnvelope = serde_json::from_slice(&bytes)
        .map_err(|error| format!("ACP registry response is invalid: {error}"))?;
    if envelope.agents.len() > MAX_REGISTRY_AGENTS {
        return Err("ACP registry contains too many agents".into());
    }
    Ok(envelope
        .agents
        .into_iter()
        .filter_map(|value| serde_json::from_value::<RegistryAgent>(value).ok())
        .filter_map(into_agent)
        .collect())
}

fn rank(agent: &AcpRegistryAgent, query: &str) -> Option<u8> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return Some(100);
    }
    let id = agent.id.to_lowercase();
    let name = agent.name.to_lowercase();
    let authors = agent.authors.join(" ").to_lowercase();
    let description = agent.description.to_lowercase();
    let terms: Vec<_> = query.split_whitespace().collect();
    if id == query || name == query {
        return Some(100);
    }
    if id.starts_with(&query) || name.starts_with(&query) {
        return Some(90);
    }
    let identity = format!("{id} {name}");
    let identity_tokens: Vec<_> = identity
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|token| !token.is_empty())
        .collect();
    if terms
        .iter()
        .all(|term| identity_tokens.iter().any(|token| token.starts_with(term)))
    {
        return Some(80);
    }
    if terms
        .iter()
        .all(|term| id.contains(term) || name.contains(term))
    {
        return Some(70);
    }
    if terms.iter().all(|term| authors.contains(term)) {
        return Some(60);
    }
    if terms
        .iter()
        .all(|term| format!("{id} {name} {authors} {description}").contains(term))
    {
        return Some(50);
    }
    None
}

pub(crate) async fn search(params: &SearchAcpRegistry) -> Result<AcpRegistrySearchResult, String> {
    if params.query.trim().len() > 120 {
        return Err("ACP registry search query is too long".into());
    }
    let mut agents = fetch_agents().await?;
    agents.retain(|agent| rank(agent, &params.query).is_some());
    agents.sort_by(|left, right| {
        rank(right, &params.query)
            .cmp(&rank(left, &params.query))
            .then_with(|| left.name.cmp(&right.name))
            .then_with(|| left.id.cmp(&right.id))
    });
    agents.truncate(MAX_RESULTS);
    Ok(AcpRegistrySearchResult { agents })
}

async fn find(agent_id: &str) -> Result<AcpRegistryAgent, String> {
    if !valid_agent_id(agent_id) {
        return Err("invalid ACP agent id".into());
    }
    fetch_agents()
        .await?
        .into_iter()
        .find(|agent| agent.id == agent_id)
        .ok_or_else(|| format!("ACP agent {agent_id} was not found"))
}

pub(crate) async fn prepare(
    params: &PrepareAcpAgent,
    state_dir: &Path,
) -> Result<PreparedAcpAgent, String> {
    let agent = runtime_agent(find(&params.agent_id).await?, state_dir).await?;
    let mut command = tokio::process::Command::new(&agent.command);
    command
        .args(&agent.args)
        .arg("--version")
        .envs(&agent.environment)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let status = tokio::time::timeout(Duration::from_secs(60), command.status())
        .await
        .map_err(|_| "ACP agent preparation timed out".to_owned())?
        .map_err(|error| format!("ACP agent could not be prepared: {error}"))?;
    if !status.success() {
        return Err(format!("ACP agent preparation exited with {status}"));
    }
    Ok(PreparedAcpAgent {
        agent_id: agent.id,
        version: agent.version,
        distribution: agent.distribution,
        prepared: true,
    })
}

pub(crate) async fn uninstall_managed_binary(
    agent_id: &str,
    state_dir: &Path,
) -> Result<agent_protocol::operations::UninstalledAcpAgent, String> {
    if !valid_agent_id(agent_id) {
        return Err("invalid ACP agent id".into());
    }
    let root = state_dir
        .join("acp-registry")
        .join("binaries")
        .join(agent_id);
    let metadata = match tokio::fs::symlink_metadata(&root).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(agent_protocol::operations::UninstalledAcpAgent {
                agent_id: agent_id.to_owned(),
                removed: false,
            });
        }
        Err(error) => return Err(format!("ACP binary cache could not be inspected: {error}")),
    };
    if metadata.file_type().is_symlink() || metadata.is_file() {
        tokio::fs::remove_file(&root)
            .await
            .map_err(|error| format!("ACP binary cache could not be removed: {error}"))?;
    } else {
        tokio::fs::remove_dir_all(&root)
            .await
            .map_err(|error| format!("ACP binary cache could not be removed: {error}"))?;
    }
    Ok(agent_protocol::operations::UninstalledAcpAgent {
        agent_id: agent_id.to_owned(),
        removed: true,
    })
}

fn bounded_value(value: &serde_json::Value, key: &str, maximum: usize) -> Option<String> {
    value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty() && value.len() <= maximum)
        .map(str::to_owned)
}

fn shell_display_token(value: &str) -> String {
    if !value.is_empty()
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(
                    byte,
                    b'_' | b'@' | b'%' | b'+' | b'=' | b',' | b':' | b'.' | b'/' | b'-'
                )
        })
    {
        value.to_owned()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

fn terminal_auth_command(agent: &AcpRegistryAgent, method: &serde_json::Value) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(environment) = method.get("env").and_then(serde_json::Value::as_object) {
        for (name, value) in environment {
            let value = value.as_str()?;
            if !valid_token(name, 128) || value.len() > 1_024 {
                return None;
            }
            parts.push(format!("{}={}", name, shell_display_token(value)));
        }
    }
    parts.push(shell_display_token(&agent.command));
    parts.extend(
        agent
            .args
            .iter()
            .map(|argument| shell_display_token(argument)),
    );
    if let Some(arguments) = method.get("args").and_then(serde_json::Value::as_array) {
        for argument in arguments.iter().take(64) {
            let argument = argument.as_str()?;
            if !valid_argument(argument) {
                return None;
            }
            parts.push(shell_display_token(argument));
        }
        if arguments.len() > 64 {
            return None;
        }
    }
    let command = parts.join(" ");
    (command.len() <= 2_048).then_some(command)
}

fn normalize_auth_methods(
    value: &serde_json::Value,
    agent: &AcpRegistryAgent,
) -> Vec<AcpProbeAuthMethod> {
    value
        .get("authMethods")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|method| {
            let id = bounded_value(method, "id", 256)?;
            let name = bounded_value(method, "name", 256).unwrap_or_else(|| id.clone());
            let auth_type = match bounded_value(method, "type", 32).as_deref() {
                Some("env_var") => "env_var",
                Some("terminal") => "terminal",
                _ => "agent",
            }
            .to_owned();
            let env_var_names = if auth_type == "env_var" {
                method
                    .get("vars")
                    .and_then(serde_json::Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|variable| bounded_value(variable, "name", 128))
                    .take(16)
                    .collect()
            } else {
                Vec::new()
            };
            let link = (auth_type == "env_var")
                .then(|| bounded_value(method, "link", 2_048))
                .flatten()
                .filter(|link| valid_https_url(link));
            let command = (auth_type == "terminal")
                .then(|| terminal_auth_command(agent, method))
                .flatten();
            Some(AcpProbeAuthMethod {
                id,
                name,
                description: bounded_value(method, "description", 1_024),
                auth_type,
                command,
                env_var_names,
                link,
            })
        })
        .take(32)
        .collect()
}

fn option_description(value: &serde_json::Value) -> Option<String> {
    bounded_value(value, "description", 1_024)
}

fn normalize_option_choices(value: &serde_json::Value) -> Vec<agent_domain::OptionChoice> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|choice| {
            if let Some(id) = bounded_value(choice, "value", 128) {
                let label = bounded_value(choice, "name", 160)
                    .or_else(|| bounded_value(choice, "label", 160))
                    .unwrap_or_else(|| id.clone());
                return vec![agent_domain::OptionChoice {
                    id,
                    label,
                    description: option_description(choice),
                    is_default: choice
                        .get("isDefault")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false),
                }];
            }
            normalize_option_choices(choice.get("options").unwrap_or(&serde_json::Value::Null))
        })
        .take(256)
        .collect()
}

fn normalize_config_options(
    value: &serde_json::Value,
) -> (
    Vec<AcpProbeModel>,
    Option<String>,
    Vec<agent_domain::OptionDescriptor>,
) {
    let mut models: Vec<AcpProbeModel> = Vec::new();
    let mut current_model_id = None;
    let mut descriptors = Vec::new();
    let options = value
        .get("configOptions")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten();
    for option in options {
        let Some(id) = bounded_value(option, "id", 128) else {
            continue;
        };
        let label = bounded_value(option, "name", 160)
            .or_else(|| bounded_value(option, "label", 160))
            .unwrap_or_else(|| id.clone());
        let description = option_description(option);
        let category = option
            .get("category")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        match bounded_value(option, "type", 32).as_deref() {
            Some("select") => {
                let choices = normalize_option_choices(
                    option.get("options").unwrap_or(&serde_json::Value::Null),
                );
                let current_value = bounded_value(option, "currentValue", 128)
                    .filter(|value| choices.iter().any(|choice| &choice.id == value));
                if category == "model" {
                    if current_value.is_some() {
                        current_model_id = current_value.clone();
                    }
                    for choice in choices {
                        if models.iter().any(|model| model.id == choice.id) {
                            continue;
                        }
                        models.push(AcpProbeModel {
                            id: choice.id,
                            name: choice.label,
                            description: choice.description,
                        });
                        if models.len() == 256 {
                            break;
                        }
                    }
                } else if category != "collaboration_mode" && !choices.is_empty() {
                    descriptors.push(agent_domain::OptionDescriptor::Select(
                        agent_domain::SelectOption {
                            id,
                            label,
                            description,
                            options: choices,
                            current_value,
                            prompt_injected_values: vec![],
                        },
                    ));
                }
            }
            Some("boolean") if category != "collaboration_mode" => {
                descriptors.push(agent_domain::OptionDescriptor::Boolean(
                    agent_domain::BooleanOption {
                        id,
                        label,
                        description,
                        current_value: option
                            .get("currentValue")
                            .and_then(serde_json::Value::as_bool),
                    },
                ));
            }
            _ => {}
        }
        if descriptors.len() >= 16 {
            break;
        }
    }
    models.truncate(256);
    let current_model_id = current_model_id.filter(|id| models.iter().any(|model| &model.id == id));
    (models, current_model_id, descriptors)
}

fn session_management(initialize: &serde_json::Value) -> AcpSessionManagement {
    let capabilities = &initialize["agentCapabilities"];
    let sessions = &capabilities["sessionCapabilities"];
    AcpSessionManagement {
        can_list: sessions.get("list").is_some_and(|value| !value.is_null()),
        can_load: capabilities["loadSession"] == serde_json::Value::Bool(true),
        can_resume: sessions.get("resume").is_some_and(|value| !value.is_null()),
        can_logout: capabilities["auth"]["logout"].is_object()
            || capabilities["auth"]["logout"] == serde_json::Value::Bool(true),
        can_delete: sessions.get("delete").is_some_and(|value| !value.is_null()),
        can_configure_providers: capabilities
            .get("providers")
            .is_some_and(|value| !value.is_null()),
    }
}

pub(crate) async fn probe(
    params: &ProbeAcpAgent,
    state_dir: &Path,
) -> Result<AcpProbeResult, String> {
    let agent = find(&params.agent_id).await?;
    let cwd = Path::new(&params.cwd);
    if !cwd.is_absolute()
        || !tokio::fs::metadata(cwd)
            .await
            .is_ok_and(|metadata| metadata.is_dir())
    {
        return Err("ACP probe cwd must be an existing absolute directory".into());
    }
    let agent = runtime_agent(agent, state_dir).await?;
    let runtime = crate::acp_runtime::AcpSessionRuntime::spawn(
        &agent.command,
        &agent.args,
        &agent.environment,
        Some(cwd),
    )
    .await?;
    let result = tokio::time::timeout(Duration::from_secs(20), runtime.initialize()).await;
    let initialize = match result {
        Ok(Ok(value)) => value,
        Ok(Err(error)) => {
            runtime.shutdown().await;
            return Err(error);
        }
        Err(_) => {
            runtime.shutdown().await;
            return Err("ACP initialize timed out".into());
        }
    };
    let session = tokio::time::timeout(Duration::from_secs(20), runtime.new_session(cwd)).await;
    runtime.shutdown().await;
    let session = match session {
        Ok(Ok(value)) => value,
        Ok(Err(error)) => return Err(error),
        Err(_) => return Err("ACP session creation timed out".into()),
    };
    let (models, current_model_id, config_options) = normalize_config_options(&session);
    Ok(AcpProbeResult {
        agent_id: params.agent_id.clone(),
        ready: true,
        icon: agent.icon.clone(),
        auth_methods: normalize_auth_methods(&initialize, &agent),
        models,
        current_model_id,
        config_options,
        session_management: session_management(&initialize),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(id: &str, name: &str, description: &str) -> AcpRegistryAgent {
        AcpRegistryAgent {
            id: id.into(),
            name: name.into(),
            version: "1.0.0".into(),
            description: description.into(),
            authors: vec![],
            license: None,
            website: None,
            repository: None,
            icon: None,
            distribution: "npx".into(),
            integrity: "registry".into(),
            command: "npx".into(),
            args: vec![],
            environment: BTreeMap::new(),
            archive: None,
            sha256: None,
            package_spec: None,
            runtime_args: vec![],
        }
    }

    // Reference: apps/server/src/provider/acp/AcpRegistrySupport.ts:569-592.
    // The exact and prefix ID/name tiers intentionally have equal rank.
    #[test]
    fn search_ranking_matches_reference_tiers() {
        let query = "alpha";
        assert_eq!(
            rank(&agent("alpha", "Other", ""), query),
            rank(&agent("other", "Alpha", ""), query)
        );
        assert_eq!(
            rank(&agent("alpha-agent", "Other", ""), query),
            rank(&agent("other", "Alpha Agent", ""), query)
        );
        assert!(
            rank(&agent("alpha-agent-other", "Other", ""), "alpha agent")
                > rank(&agent("other", "Other", "alpha agent"), "alpha agent")
        );
        assert_eq!(rank(&agent("other", "MÜNCHEN", ""), "münchen"), Some(100));
        assert_eq!(rank(&agent("other", "Other", ""), query), None);
    }

    #[test]
    fn search_ranking_supports_identity_terms_and_authors() {
        let mut authored = agent("other", "Other", "Description");
        authored.authors = vec!["Alpha Labs".into()];
        assert!(rank(&authored, "alpha labs").is_some());
        assert!(rank(&agent("alpha-agent", "Other", ""), "alpha ag").is_some());
        assert_eq!(
            rank(&agent("other", "Other", "Description"), "missing terms"),
            None
        );
    }

    #[test]
    fn package_and_version_validation_requires_exact_runner_versions() {
        assert!(exact_package("@scope/agent@1.2.3", "npx"));
        assert!(exact_package("agent==v1.2.3-beta.1", "uvx"));
        assert_eq!(package_version("@scope/agent@1.2.3", "npx"), Some("1.2.3"));
        assert_eq!(package_version("agent==v1.2.3", "uvx"), Some("v1.2.3"));
        assert!(!exact_package("agent", "npx"));
        assert!(!exact_package("agent@latest", "npx"));
        assert!(!exact_package("../agent@1.2.3", "npx"));
        assert!(valid_version("1.2.3+build.4"));
        assert!(!valid_version("1.2"));
        assert!(!valid_version("1.2.3.4"));
    }

    #[tokio::test]
    async fn npm_package_resolution_requires_the_registry_version() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory
            .path()
            .join("lib")
            .join("node_modules")
            .join("@scope")
            .join("agent");
        tokio::fs::create_dir_all(directory.path().join("bin"))
            .await
            .unwrap();
        tokio::fs::create_dir_all(&root).await.unwrap();
        tokio::fs::write(
            root.join("package.json"),
            r#"{"name":"@scope/agent","version":"1.2.3","bin":{"agent":"bin/agent"}}"#,
        )
        .await
        .unwrap();
        tokio::fs::write(directory.path().join("bin").join("agent"), b"agent")
            .await
            .unwrap();
        let mut configured = agent("alpha", "Alpha", "test");
        configured.package_spec = Some("@scope/agent@1.2.3".into());
        assert_eq!(
            package_executable(directory.path(), &configured)
                .await
                .unwrap(),
            Some(directory.path().join("bin").join("agent"))
        );
        tokio::fs::write(
            root.join("package.json"),
            r#"{"name":"@scope/agent","version":"1.2.4","bin":{"agent":"bin/agent"}}"#,
        )
        .await
        .unwrap();
        assert!(
            package_executable(directory.path(), &configured)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn registry_wire_metadata_does_not_expose_launch_recipe() {
        let agent = agent("alpha", "Alpha", "test");
        let value = serde_json::to_value(agent).unwrap();
        assert_eq!(value["distribution"], "npx");
        assert_eq!(value["integrity"], "registry");
        assert!(value.get("command").is_none());
        assert!(value.get("environment").is_none());
    }

    #[test]
    fn registry_entries_without_optional_links_are_kept() {
        let parsed = into_agent(RegistryAgent {
            id: "alpha".into(),
            name: "Alpha".into(),
            version: "1.0.0".into(),
            description: "test".into(),
            authors: vec![],
            license: None,
            website: None,
            repository: None,
            distribution: RegistryDistribution {
                npx: Some(RegistryPackage {
                    package: "alpha@1.0.0".into(),
                    args: vec![],
                    env: BTreeMap::new(),
                }),
                ..Default::default()
            },
            icon: None,
        });
        assert!(parsed.is_some());
        assert_eq!(parsed.and_then(|agent| agent.website), None);
    }

    #[test]
    fn probe_model_options_are_deduplicated_and_not_repeated_as_descriptors() {
        let (models, current, descriptors) = normalize_config_options(&serde_json::json!({
            "configOptions": [
                {
                    "id": "model",
                    "name": "Model",
                    "category": "model",
                    "type": "select",
                    "currentValue": "sonnet",
                    "options": [
                        {"value": "sonnet", "name": "Sonnet"},
                        {"value": "haiku", "name": "Haiku"},
                        {"value": "sonnet", "name": "Duplicate"}
                    ]
                },
                {
                    "id": "mode",
                    "name": "Mode",
                    "type": "select",
                    "options": [{"value": "safe", "name": "Safe"}]
                }
            ]
        }));
        assert_eq!(current.as_deref(), Some("sonnet"));
        assert_eq!(
            models
                .iter()
                .map(|model| model.id.as_str())
                .collect::<Vec<_>>(),
            ["sonnet", "haiku"]
        );
        assert_eq!(descriptors.len(), 1);
        assert_eq!(descriptors[0].id(), "mode");
    }

    #[test]
    fn terminal_auth_commands_include_the_prepared_spawn_and_quote_arguments() {
        let agent = AcpRegistryAgent {
            command: "/opt/agent".into(),
            args: vec!["--acp".into()],
            ..agent("alpha", "Alpha", "test")
        };
        let methods = normalize_auth_methods(
            &serde_json::json!({
                "authMethods": [{
                    "id": "login",
                    "name": "Terminal login",
                    "type": "terminal",
                    "args": ["auth", "log in"],
                    "env": {"FORCE_TTY": "1"}
                }]
            }),
            &agent,
        );
        assert_eq!(
            methods[0].command.as_deref(),
            Some("FORCE_TTY=1 /opt/agent --acp auth 'log in'")
        );
    }

    #[test]
    fn archive_path_validation_rejects_traversal_before_extraction() {
        assert!(archive_is_safe(b"bin/agent\n./\n"));
        assert!(!archive_is_safe(b"../outside\n"));
        assert!(!archive_is_safe(b"/absolute\n"));
    }

    #[tokio::test]
    async fn managed_binary_uninstall_is_idempotent_and_scoped_to_agent_id() {
        let state = tempfile::tempdir().unwrap();
        let root = state
            .path()
            .join("acp-registry")
            .join("binaries")
            .join("example-agent")
            .join("1.0.0");
        tokio::fs::create_dir_all(&root).await.unwrap();
        tokio::fs::write(root.join("agent"), b"binary")
            .await
            .unwrap();

        let removed = uninstall_managed_binary("example-agent", state.path())
            .await
            .unwrap();
        assert_eq!(removed.agent_id, "example-agent");
        assert!(removed.removed);
        assert!(
            !state
                .path()
                .join("acp-registry")
                .join("binaries")
                .join("example-agent")
                .exists()
        );

        let second = uninstall_managed_binary("example-agent", state.path())
            .await
            .unwrap();
        assert!(!second.removed);
        assert!(
            uninstall_managed_binary("../outside", state.path())
                .await
                .is_err()
        );
    }
}

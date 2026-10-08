//! Host-owned provider package maintenance.
//!
//! The updater only runs a command whose installer can be derived from the
//! executable being updated. A provider setting can select a binary and a
//! home, but it cannot make an unrelated package manager mutate an unproven
//! installation.

use agent_domain::Driver;
use agent_protocol::{
    models::{ProviderVersionAdvisory, ProviderVersionAdvisoryStatus},
    operations::{ProviderUpdate, ProviderUpdateStatus},
};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};
use tokio::io::AsyncRead;

const MAX_OUTPUT_BYTES: usize = 10_000;
const UPDATE_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const VERSION_LOOKUP_TIMEOUT: Duration = Duration::from_secs(4);
const VERSION_CACHE_TTL: Duration = Duration::from_secs(5 * 60);

static NPM_VERSION_CACHE: OnceLock<Mutex<HashMap<String, (Instant, Option<String>)>>> =
    OnceLock::new();
static HOMEBREW_VERSION_CACHE: OnceLock<Mutex<HashMap<String, (Instant, Option<String>)>>> =
    OnceLock::new();

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UpdateCommand {
    pub(crate) executable: PathBuf,
    pub(crate) args: Vec<String>,
    pub(crate) environment: Vec<(String, String)>,
}

fn package_name(driver: Driver) -> &'static str {
    match driver {
        Driver::Codex => "@openai/codex",
        Driver::Claude => "@anthropic-ai/claude-code",
    }
}

fn normalized(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "/")
        .to_ascii_lowercase()
}

fn homebrew_prefix(path: &Path) -> Option<PathBuf> {
    let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
    let slash_path = path.to_string_lossy().replace('\\', "/");
    let components: Vec<_> = slash_path.split('/').collect();
    let index = components.iter().position(|component| {
        component.eq_ignore_ascii_case("cellar") || component.eq_ignore_ascii_case("caskroom")
    })?;
    if components.len() <= index + 3 {
        return None;
    }
    let prefix = components[..index].join("/");
    (!prefix.is_empty()).then(|| PathBuf::from(prefix))
}

pub(crate) fn installation_lock_key(driver: Driver, binary: &Path, home: Option<&Path>) -> String {
    let path = std::fs::canonicalize(binary)
        .unwrap_or_else(|_| binary.to_owned())
        .to_string_lossy()
        .replace('\\', "/");
    let home = home
        .map(|home| {
            std::fs::canonicalize(home)
                .unwrap_or_else(|_| home.to_owned())
                .to_string_lossy()
                .replace('\\', "/")
        })
        .unwrap_or_default();
    format!("{driver:?}:{path}:{home}")
}

fn npm_prefix(path: &Path, package: &str) -> Option<PathBuf> {
    let original = path.to_string_lossy();
    let real = normalized(path);
    let marker = format!("/lib/node_modules/{}/", package.to_ascii_lowercase());
    let index = real.rfind(&marker)?;
    let prefix = &original[..index];
    if real[..index].contains("/node_modules/") {
        return None;
    }
    if let Some(rest) = real[..index].split_once("/mise/installs/").map(|(_, rest)| rest)
        && rest.split('/').next().is_some_and(|tool| tool != "node")
    {
        return None;
    }
    Some(PathBuf::from(prefix))
}

/// Windows npm writes a command shim beside `node_modules` instead of
/// symlinking it through `lib/node_modules`. The package manifest is the
/// ownership proof for that layout; a project-local node_modules directory is
/// rejected because it is not a global installation.
fn npm_windows_prefix(path: &Path, package: &str) -> Option<PathBuf> {
    let prefix = path.parent()?.to_owned();
    let manifest = prefix
        .join("node_modules")
        .join(package)
        .join("package.json");
    manifest.is_file().then_some(prefix)
}

fn manager_path(path: &str, marker: &str) -> bool {
    path.contains(marker)
}

fn manager_command(
    executable: &str,
    args: &[&str],
    package: &str,
    target_version: Option<&str>,
    environment: &[(String, String)],
) -> UpdateCommand {
    let package = target_version
        .map(|version| format!("{package}@{version}"))
        .unwrap_or_else(|| format!("{package}@latest"));
    let args = args
        .iter()
        .map(|arg| (*arg).to_owned())
        .chain(std::iter::once(package))
        .collect();
    UpdateCommand {
        executable: PathBuf::from(if cfg!(windows) && executable == "npm" {
            "npm.cmd"
        } else {
            executable
        }),
        args,
        environment: environment.to_vec(),
    }
}

/// Derive the command from the actual executable path. This is deliberately a
/// pure function so ownership decisions remain reviewable and testable.
pub(crate) fn update_command(
    driver: Driver,
    binary: &Path,
    home: Option<&Path>,
    target_version: Option<&str>,
) -> Result<UpdateCommand, String> {
    if let Some(version) = target_version
        && !valid_version(version)
    {
        return Err("provider versions must use x.y.z form".into());
    }
    let package = package_name(driver);
    let resolved = std::fs::canonicalize(binary).ok();
    let real = resolved.as_deref().map(normalized);
    let environment = home
        .map(|home| {
            let variable = match driver {
                Driver::Codex => "CODEX_HOME",
                Driver::Claude => "CLAUDE_CONFIG_DIR",
            };
            vec![(variable.into(), home.to_string_lossy().into_owned())]
        })
        .unwrap_or_default();

    // Built-in provider commands are resolved by the Host's prepared PATH at
    // startup. A bare `codex` or `claude` therefore has the same ownership
    // proof as the executable used for the running provider.
    if resolved.is_none()
        && binary
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| {
                (driver == Driver::Codex && name.eq_ignore_ascii_case("codex"))
                    || (driver == Driver::Claude && name.eq_ignore_ascii_case("claude"))
            })
    {
        if target_version.is_some() {
            return Err("the native provider updater does not accept a target version".into());
        }
        return Ok(UpdateCommand {
            executable: binary.to_owned(),
            args: vec!["update".into()],
            environment,
        });
    }

    // Codex's standalone tree is owned by `codex update`; the shared CODEX_HOME
    // is passed explicitly because provider instances may use an auth overlay.
    if driver == Driver::Codex
        && real
            .as_deref()
            .is_some_and(|path| path.contains("/packages/standalone/"))
    {
        if target_version.is_some() {
            return Err("the Codex standalone updater does not accept a target version".into());
        }
        return Ok(UpdateCommand {
            executable: binary.to_owned(),
            args: vec!["update".into()],
            environment,
        });
    }

    // Claude's native installer owns its local binary tree and can select the
    // right channel itself. It cannot safely install an arbitrary exact
    // version, so targeted updates stay available only for proven npm roots.
    if driver == Driver::Claude
        && real.as_deref().is_some_and(|path| {
            path.ends_with("/.local/bin/claude") || path.contains("/.local/share/claude/")
        })
    {
        if target_version.is_some() {
            return Err("the Claude native updater does not accept a target version".into());
        }
        return Ok(UpdateCommand {
            executable: binary.to_owned(),
            args: vec!["update".into()],
            environment,
        });
    }

    if let Some(prefix) = resolved.as_deref().and_then(|path| {
        npm_prefix(path, package)
            .or_else(|| cfg!(windows).then(|| npm_windows_prefix(Path::new(path), package))?)
    }) {
        let version = target_version.unwrap_or("latest");
        return Ok(UpdateCommand {
            executable: PathBuf::from(if cfg!(windows) { "npm.cmd" } else { "npm" }),
            args: vec![
                "install".into(),
                "-g".into(),
                "--prefix".into(),
                prefix.to_string_lossy().into_owned(),
                format!("--allow-scripts={package}"),
                format!("{package}@{version}"),
            ],
            environment,
        });
    }

    let Some(real) = real.as_deref() else {
        return Err(format!(
            "the {} installation is not owned by a supported updater",
            package
        ));
    };

    // These managers expose a global bin directory whose path proves that the
    // package manager, rather than a project, owns the provider executable.
    // Keep npm above these branches: Homebrew's Node keg can contain npm
    // globals, and that package ownership is stronger evidence than the keg.
    if manager_path(real, "/.bun/bin/") {
        return Ok(manager_command(
            "bun",
            &["install", "--global"],
            package,
            target_version,
            &environment,
        ));
    }
    if manager_path(real, "/.local/share/pnpm/")
        || manager_path(real, "/library/pnpm/")
        || manager_path(real, "/appdata/local/pnpm/")
        || manager_path(real, "/pnpm/global/")
    {
        return Ok(manager_command(
            "pnpm",
            &["add", "--global"],
            package,
            target_version,
            &environment,
        ));
    }
    if manager_path(real, "/yarn/") && manager_path(real, "/global/") {
        return Ok(manager_command(
            "yarn",
            &["global", "add"],
            package,
            target_version,
            &environment,
        ));
    }
    if manager_path(real, "/.vite-plus/bin/") {
        return Ok(manager_command(
            "vp",
            &["install", "--global"],
            package,
            target_version,
            &environment,
        ));
    }
    if manager_path(real, "/.volta/") && volta_package_installation(binary, real, package) {
        return Ok(manager_command(
            "volta",
            &["install"],
            package,
            target_version,
            &environment,
        ));
    }
    // mise owns the version pin in its config. It has no safe package update
    // command for this generic provider contract, so surface that fact to the
    // caller rather than running the provider's native updater against a shim.
    if mise_managed(binary, real) {
        return Err("provider installation is managed by mise; update it with mise".into());
    }

    // A versioned Homebrew keg/cask proves the provider's package ownership.
    // The selected `brew` on PATH still owns the prefix at runtime; the
    // command itself is intentionally target-version agnostic.
    let segments: Vec<_> = real.split('/').collect();
    if let Some(index) = segments
        .iter()
        .position(|segment| *segment == "cellar" || *segment == "caskroom")
        && segments.len() > index + 3
    {
        if target_version.is_some() {
            return Err("Homebrew updates do not accept a target version".into());
        }
        let name = segments[index + 1];
        let args = if segments[index] == "caskroom" {
            vec!["upgrade".into(), "--cask".into(), name.into()]
        } else {
            vec!["upgrade".into(), name.into()]
        };
        return Ok(UpdateCommand {
            executable: "brew".into(),
            args,
            environment,
        });
    }

    // The provider's own installer is the safe fallback for an explicit
    // executable whose path is not a package-manager tree. Its update command
    // performs the provider-specific ownership check itself. A path that
    // contains node_modules remains manual-only because it may belong to a
    // project or another package.
    if ((driver == Driver::Codex && real.ends_with("/.local/bin/codex"))
        || (driver == Driver::Claude
            && (real.ends_with("/.local/bin/claude") || real.contains("/.local/share/claude/"))))
    {
        if target_version.is_some() {
            return Err("the native provider updater does not accept a target version".into());
        }
        return Ok(UpdateCommand {
            executable: binary.to_owned(),
            args: vec!["update".into()],
            environment,
        });
    }

    Err(format!(
        "the {} installation is not owned by a supported updater",
        package
    ))
}

pub(crate) fn valid_version(version: &str) -> bool {
    let mut parts = version.split('.');
    parts.clone().count() == 3
        && parts.all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
}

fn compare_versions(current: &str, latest: &str) -> Option<std::cmp::Ordering> {
    if !valid_version(current) || !valid_version(latest) {
        return None;
    }
    let current = current
        .split('.')
        .map(|part| part.parse::<u64>().ok())
        .collect::<Option<Vec<_>>>()?;
    let latest = latest
        .split('.')
        .map(|part| part.parse::<u64>().ok())
        .collect::<Option<Vec<_>>>()?;
    Some(current.cmp(&latest))
}

fn shell_word(value: &str) -> String {
    if value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || b"-_=./:@".contains(&byte))
    {
        value.to_owned()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

fn command_text(command: &UpdateCommand) -> String {
    std::iter::once(shell_word(&command.executable.to_string_lossy()))
        .chain(command.args.iter().map(|arg| shell_word(arg)))
        .collect::<Vec<_>>()
        .join(" ")
}

fn homebrew_name(command: &UpdateCommand) -> Option<(&'static str, &str)> {
    if command.executable != Path::new("brew") || command.args.first()? != "upgrade" {
        return None;
    }
    if command.args.get(1).is_some_and(|arg| arg == "--cask") {
        Some(("casks", command.args.get(2)?))
    } else {
        Some(("formulae", command.args.get(1)?))
    }
}

fn parse_homebrew_latest(value: &serde_json::Value, collection: &str) -> Option<String> {
    let raw = match collection {
        "formulae" => value[collection][0]["versions"]["stable"].as_str(),
        "casks" => value[collection][0]["version"].as_str(),
        _ => None,
    }?;
    let version = raw.split(',').next()?.trim();
    valid_version(version).then(|| version.to_owned())
}

async fn latest_homebrew_version(
    binary: &Path,
    command: &UpdateCommand,
    environment: &[(String, String)],
) -> Option<String> {
    let (collection, name) = homebrew_name(command)?;
    let prefix = homebrew_prefix(binary)?;
    let cache_key = format!("{}:{collection}:{name}", prefix.display());
    let cache = HOMEBREW_VERSION_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some((at, value)) = cache
        .lock()
        .ok()
        .and_then(|cache| cache.get(&cache_key).cloned())
        && at.elapsed() < VERSION_CACHE_TTL
    {
        return value;
    }
    let brew = homebrew_brew_path(binary)?;
    let result = async {
        let output = tokio::time::timeout(
            VERSION_LOOKUP_TIMEOUT,
            tokio::process::Command::new(&brew)
                .arg("--json=v2")
                .arg(collection.strip_suffix("s").unwrap_or(collection))
                .arg(name)
                .envs(environment.iter().map(|(key, value)| (key, value)))
                .stdin(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .output(),
        )
        .await
        .ok()?
        .ok()?;
        if !output.status.success() {
            return None;
        }
        let value = serde_json::from_slice(&output.stdout).ok()?;
        parse_homebrew_latest(&value, collection)
    }
    .await;
    if let Ok(mut cache) = cache.lock() {
        cache.insert(cache_key, (Instant::now(), result.clone()));
    }
    result
}

fn homebrew_brew_path(binary: &Path) -> Option<PathBuf> {
    let prefix = homebrew_prefix(binary)?;
    let preferred = prefix.join("bin").join("brew");
    Some(if preferred.is_file() {
        preferred
    } else {
        PathBuf::from("brew")
    })
}

async fn homebrew_prefix_matches(
    binary: &Path,
    environment: &[(String, String)],
) -> Result<(), String> {
    let expected = homebrew_prefix(binary)
        .ok_or_else(|| "Homebrew provider path has no valid prefix".to_owned())?;
    let brew = homebrew_brew_path(binary)
        .ok_or_else(|| "Homebrew provider path has no valid prefix".to_owned())?;
    let output = tokio::time::timeout(
        Duration::from_secs(10),
        tokio::process::Command::new(&brew)
            .arg("--prefix")
            .envs(environment.iter().map(|(key, value)| (key, value)))
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output(),
    )
    .await
    .map_err(|_| "Homebrew prefix check timed out".to_owned())?
    .map_err(|error| format!("Homebrew prefix check could not start: {error}"))?;
    if !output.status.success() {
        return Err("Homebrew prefix check failed".into());
    }
    let reported = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    let reported = std::fs::canonicalize(&reported).unwrap_or_else(|_| PathBuf::from(&reported));
    let expected = std::fs::canonicalize(&expected).unwrap_or(expected);
    if normalized(&reported) != normalized(&expected) {
        return Err(format!(
            "Homebrew prefix {} does not own provider executable {}",
            reported.display(),
            binary.display()
        ));
    }
    Ok(())
}

async fn latest_npm_version(package: &str) -> Result<String, String> {
    let cache = NPM_VERSION_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some((at, value)) = cache
        .lock()
        .ok()
        .and_then(|cache| cache.get(package).cloned())
        && at.elapsed() < VERSION_CACHE_TTL
    {
        return value.ok_or_else(|| "provider registry version is unavailable".to_owned());
    }
    let encoded = package.replace('/', "%2F");
    let response = tokio::time::timeout(
        VERSION_LOOKUP_TIMEOUT,
        reqwest::get(format!("https://registry.npmjs.org/{encoded}/latest")),
    )
    .await
    .map_err(|_| "provider version lookup timed out".to_owned())?
    .map_err(|error| format!("provider version lookup failed: {error}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "provider version lookup returned {}",
            response.status()
        ));
    }
    let value: serde_json::Value = response
        .json()
        .await
        .map_err(|error| format!("provider version response was invalid: {error}"))?;
    let version = value["version"]
        .as_str()
        .map(str::trim)
        .filter(|version| valid_version(version))
        .ok_or_else(|| "provider registry did not return a stable version".to_owned())?;
    let version = version.to_owned();
    if let Ok(mut cache) = cache.lock() {
        cache.insert(package.to_owned(), (Instant::now(), Some(version.clone())));
    }
    Ok(version)
}

fn volta_package_installation(binary: &Path, real: &str, package: &str) -> bool {
    let package_marker = format!(
        "/tools/image/packages/{}/",
        package.replace('\\', "/").to_ascii_lowercase()
    );
    if real.contains(&package_marker) {
        return true;
    }
    if !real.ends_with("/volta-shim") && !real.contains("/.volta/bin/") {
        return false;
    }
    let Some(volta_home) = binary.parent().and_then(Path::parent) else {
        return false;
    };
    let mut package_dir = volta_home.join("tools").join("image").join("packages");
    for part in package.split('/') {
        package_dir.push(part);
    }
    package_dir.is_dir()
}

fn mise_managed(binary: &Path, real: &str) -> bool {
    if real.contains("/mise/installs/") || real.contains("/mise/shims/") {
        return true;
    }
    let Ok(metadata) = std::fs::metadata(binary) else {
        return false;
    };
    if metadata.len() > 64 * 1024 {
        return false;
    }
    std::fs::read_to_string(binary).is_ok_and(|script| {
        script.starts_with("#!") && (script.contains("mise x") || script.contains("mise exec"))
    })
}

/// Publish the same ownership proof used by the update endpoint as a compact
/// client-facing advisory. Version lookups are bounded and cached; a failed
/// lookup leaves update capabilities visible while making the comparison
/// unknown.
pub(crate) async fn advisory(
    driver: Driver,
    binary: &Path,
    home: Option<&Path>,
    environment: &[(String, String)],
    current_version: Option<String>,
    check_for_updates: bool,
) -> ProviderVersionAdvisory {
    let package = package_name(driver);
    let resolved = update_command(driver, binary, home, None);
    let (mut display_command, mut can_update, mut can_install_version, mut ownership_message) =
        match resolved {
            Ok(command) => (
                Some(command_text(&command)),
                true,
                update_command(driver, binary, home, Some("0.0.0")).is_ok(),
                None,
            ),
            Err(error) => (None, false, false, Some(error)),
        };
    if can_update
        && display_command
            .as_deref()
            .is_some_and(|command| command.starts_with("brew "))
        && let Err(error) = homebrew_prefix_matches(binary, environment).await
    {
        display_command = None;
        can_update = false;
        can_install_version = false;
        ownership_message = Some(error);
    }
    let latest_version = if check_for_updates && can_update && current_version.is_some() {
        match update_command(driver, binary, home, None) {
            Ok(command) if command.args.iter().any(|arg| arg == &format!("{package}@latest")) => {
                latest_npm_version(package).await.ok()
            }
            Ok(command) if command.executable == Path::new("brew") => {
                latest_homebrew_version(binary, &command, environment).await
            }
            _ => None,
        }
    } else {
        None
    };
    let status = match (current_version.as_deref(), latest_version.as_deref()) {
        (Some(current), Some(latest)) => match compare_versions(current, latest) {
            Some(std::cmp::Ordering::Less) => ProviderVersionAdvisoryStatus::BehindLatest,
            Some(_) => ProviderVersionAdvisoryStatus::Current,
            None => ProviderVersionAdvisoryStatus::Unknown,
        },
        _ => ProviderVersionAdvisoryStatus::Unknown,
    };
    let message = ownership_message.or_else(|| {
        (status == ProviderVersionAdvisoryStatus::BehindLatest)
            .then_some("A newer provider version is available.".to_owned())
    });
    ProviderVersionAdvisory {
        status,
        current_version,
        latest_version,
        update_command: display_command,
        can_update,
        can_install_version,
        message,
    }
}

fn shorten_output(output: &[u8]) -> String {
    let text = String::from_utf8_lossy(&output[..output.len().min(MAX_OUTPUT_BYTES)]);
    text.trim().to_owned()
}

fn reported_version(output: &str) -> Option<String> {
    output.split_whitespace().find_map(|token| {
        let token = token.trim_matches(|character: char| !character.is_ascii_digit());
        valid_version(token).then(|| token.to_owned())
    })
}

async fn verify_provider_version(
    binary: &Path,
    environment: &[(String, String)],
) -> Option<String> {
    let output = tokio::time::timeout(
        Duration::from_secs(30),
        tokio::process::Command::new(binary)
            .arg("--version")
            .envs(environment.iter().map(|(key, value)| (key, value)))
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .ok()?
    .ok()?;
    output.status.success().then(|| {
        reported_version(&format!(
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ))
    })?
}

async fn read_capped<R: AsyncRead + Unpin>(mut reader: R) -> Result<Vec<u8>, String> {
    use tokio::io::AsyncReadExt;
    let mut kept = Vec::with_capacity(MAX_OUTPUT_BYTES);
    let mut buffer = [0u8; 4096];
    loop {
        let read = reader
            .read(&mut buffer)
            .await
            .map_err(|error| format!("provider updater output could not be read: {error}"))?;
        if read == 0 {
            return Ok(kept);
        }
        let remaining = MAX_OUTPUT_BYTES.saturating_sub(kept.len());
        kept.extend_from_slice(&buffer[..read.min(remaining)]);
    }
}

pub(crate) async fn update(
    instance: String,
    driver: Driver,
    binary: PathBuf,
    home: Option<PathBuf>,
    environment: Vec<(String, String)>,
    target_version: Option<String>,
) -> Result<ProviderUpdate, String> {
    let package = package_name(driver);
    if target_version
        .as_deref()
        .is_some_and(|version| !valid_version(version))
    {
        return Err("provider versions must use x.y.z form".into());
    }
    let mut command = update_command(driver, &binary, home.as_deref(), target_version.as_deref())?;
    let version = match target_version.as_deref() {
        Some(version) => version.to_owned(),
        None if command
            .args
            .iter()
            .any(|arg| arg == &format!("{package}@latest")) =>
        {
            latest_npm_version(package).await?
        }
        None => "unknown".into(),
    };
    // The command resolver owns the provider home; user environment values are
    // layered only after the protected updater variables have been selected.
    // Provider home variables are selected from the provider instance and must
    // not be replaced by an arbitrary environment entry. Other settings are
    // safe to layer for package-manager proxies and user-selected registries.
    command
        .environment
        .extend(environment.into_iter().filter(|(key, _)| {
            !key.eq_ignore_ascii_case("CODEX_HOME")
                && !key.eq_ignore_ascii_case("CLAUDE_CONFIG_DIR")
        }));
    let executable = if command.executable == Path::new("brew") {
        homebrew_brew_path(&binary).unwrap_or_else(|| command.executable.clone())
    } else {
        command.executable.clone()
    };
    if command.executable == Path::new("brew") {
        homebrew_prefix_matches(&binary, &command.environment).await?;
    }
    let mut child = tokio::process::Command::new(&executable)
        .args(&command.args)
        .envs(command.environment.iter().map(|(key, value)| (key, value)))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| format!("provider updater could not start: {error}"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "provider updater stdout is unavailable".to_owned())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "provider updater stderr is unavailable".to_owned())?;
    let result = tokio::time::timeout(UPDATE_TIMEOUT, async {
        let (stdout, stderr, status) =
            tokio::join!(read_capped(stdout), read_capped(stderr), child.wait(),);
        Ok::<_, String>((
            stdout?,
            stderr?,
            status.map_err(|error| format!("provider updater failed: {error}"))?,
        ))
    })
    .await
    .map_err(|_| "provider update timed out".to_owned())??;
    let output = shorten_output(&[result.1.as_slice(), result.0.as_slice()].concat());
    if !result.2.success() {
        return Ok(ProviderUpdate {
            instance,
            status: ProviderUpdateStatus::Failed,
            version: None,
            message: format!("provider updater exited with {}", result.2),
            output: (!output.is_empty()).then_some(output),
        });
    }
    let reported = verify_provider_version(&binary, &command.environment).await;
    if target_version
        .as_deref()
        .is_some_and(|target| reported.as_deref() != Some(target))
    {
        return Ok(ProviderUpdate {
            instance,
            status: ProviderUpdateStatus::Unchanged,
            version: reported,
            message:
                "Update command completed, but the requested provider version was not verified."
                    .into(),
            output: (!output.is_empty()).then_some(output),
        });
    }
    let verified = reported.is_some();
    Ok(ProviderUpdate {
        instance,
        status: ProviderUpdateStatus::Updated,
        version: reported.or_else(|| (version != "unknown").then_some(version)),
        message: if verified {
            "Provider updated.".into()
        } else {
            "Provider update command completed.".into()
        },
        output: (!output.is_empty()).then_some(output),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn npm_binary(root: &Path, package: &str) -> PathBuf {
        let path = root
            .join("lib")
            .join("node_modules")
            .join(package)
            .join("bin")
            .join("provider.js");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "#!/bin/sh\n").unwrap();
        path
    }

    #[test]
    fn exact_npm_update_is_derived_only_from_a_global_package_path() {
        let directory = tempfile::tempdir().unwrap();
        let binary = npm_binary(directory.path(), "@anthropic-ai/claude-code");
        let command = update_command(Driver::Claude, &binary, None, Some("2.1.300")).unwrap();
        assert_eq!(command.executable, Path::new("npm"));
        assert_eq!(
            command.args.last().unwrap(),
            "@anthropic-ai/claude-code@2.1.300"
        );
        let nested = directory.path().join("repo").join("node_modules");
        let nested_binary = npm_binary(&nested, "@anthropic-ai/claude-code");
        assert!(update_command(Driver::Claude, &nested_binary, None, Some("2.1.300")).is_err());
    }

    #[test]
    fn native_updates_reject_targeted_versions() {
        let directory = tempfile::tempdir().unwrap();
        let binary = directory.path().join(".local/bin/claude");
        std::fs::create_dir_all(binary.parent().unwrap()).unwrap();
        std::fs::write(&binary, "#!/bin/sh\n").unwrap();
        assert!(update_command(Driver::Claude, &binary, None, Some("2.1.300"),).is_err());
    }

    #[test]
    fn global_manager_paths_keep_updates_with_the_manager_that_owns_them() {
        let directory = tempfile::tempdir().unwrap();
        for (relative, executable, expected) in [
            (".bun/bin/codex", "bun", "@openai/codex@2.1.300"),
            (".local/share/pnpm/codex", "pnpm", "@openai/codex@2.1.300"),
            (
                ".config/yarn/global/node_modules/codex",
                "yarn",
                "@openai/codex@2.1.300",
            ),
            (".vite-plus/bin/codex", "vp", "@openai/codex@2.1.300"),
            (".volta/bin/codex", "volta", "@openai/codex@2.1.300"),
        ] {
            let binary = directory.path().join(relative);
            std::fs::create_dir_all(binary.parent().unwrap()).unwrap();
            std::fs::write(&binary, "provider").unwrap();
            if executable == "volta" {
                std::fs::create_dir_all(
                    directory
                        .path()
                        .join(".volta/tools/image/packages/@openai/codex"),
                )
                .unwrap();
            }
            let command = update_command(Driver::Codex, &binary, None, Some("2.1.300"))
                .unwrap_or_else(|error| panic!("{relative}: {error}"));
            assert_eq!(command.executable, Path::new(executable));
            assert_eq!(command.args.last().unwrap(), expected);
        }
    }

    #[test]
    fn homebrew_kegs_have_an_explicit_upgrade_command_and_mise_is_manual() {
        let directory = tempfile::tempdir().unwrap();
        let brew = directory
            .path()
            .join("Cellar")
            .join("codex")
            .join("1.2.3")
            .join("bin")
            .join("codex");
        std::fs::create_dir_all(brew.parent().unwrap()).unwrap();
        std::fs::write(&brew, "provider").unwrap();
        let command = update_command(Driver::Codex, &brew, None, None).unwrap();
        assert_eq!(command.executable, Path::new("brew"));
        assert_eq!(command.args, ["upgrade", "codex"]);
        assert_eq!(
            homebrew_prefix(&brew),
            Some(
                directory
                    .path()
                    .join("Cellar")
                    .parent()
                    .unwrap()
                    .to_path_buf()
            )
        );

        let mise = directory
            .path()
            .join("mise")
            .join("installs")
            .join("node")
            .join("20.0.0")
            .join("bin")
            .join("codex");
        std::fs::create_dir_all(mise.parent().unwrap()).unwrap();
        std::fs::write(&mise, "provider").unwrap();
        let error = update_command(Driver::Codex, &mise, None, None).unwrap_err();
        assert!(error.contains("managed by mise"));
    }

    #[test]
    fn homebrew_advisory_reads_formula_and_cask_versions() {
        let formula: serde_json::Value = serde_json::json!({
            "formulae": [{"versions": {"stable": "1.2.3"}}]
        });
        let cask: serde_json::Value = serde_json::json!({
            "casks": [{"version": "1.2.3,456"}]
        });
        assert_eq!(parse_homebrew_latest(&formula, "formulae"), Some("1.2.3".into()));
        assert_eq!(parse_homebrew_latest(&cask, "casks"), Some("1.2.3".into()));
        assert_eq!(parse_homebrew_latest(&formula, "casks"), None);
    }

    #[test]
    fn version_advisory_comparison_requires_stable_three_part_versions() {
        assert_eq!(
            compare_versions("1.2.3", "1.3.0"),
            Some(std::cmp::Ordering::Less)
        );
        assert_eq!(compare_versions("1.2", "1.3.0"), None);
        assert_eq!(compare_versions("1.2.3", "latest"), None);
    }

    #[test]
    fn reported_version_ignores_non_semver_output() {
        assert_eq!(
            reported_version("provider 2.1.300\n"),
            Some("2.1.300".into())
        );
        assert_eq!(reported_version("provider latest"), None);
        assert_eq!(reported_version("provider 2.1"), None);
    }
}

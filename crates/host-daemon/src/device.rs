//! Host-owned simulator and emulator support.
//!
//! Device discovery and lifecycle stay here so native clients never derive
//! command lines or talk to a device directly.  The only commands exposed to
//! callers are the typed operations in `agent_protocol::device`; action
//! command construction is kept pure and is covered with fake-runner tests.
use agent_domain::ThreadId;
use agent_protocol::device::{
    self, DeviceActionInput, DeviceActionKind, DeviceAppearance, DeviceColorFilter,
    DeviceConfigureInput, DeviceDetail, DeviceDetailInput, DeviceEvent, DeviceForegroundApp,
    DeviceHostConfig, DeviceHostKind, DeviceHostStatus, DeviceHostStatusRecord, DeviceHostSummary,
    DeviceHostsInput, DeviceListInput, DeviceOpenInput, DeviceOrientation, DevicePermission,
    DevicePermissionDecision, DevicePlatform, DevicePlatformAvailability, DeviceScreenshot,
    DeviceScreenshotInput, DeviceServiceState, DeviceSession, DeviceSettings, DeviceShutdownInput,
    DeviceSummary, DeviceTextSize, DeviceToolVersion, DeviceToolVersions, LOCAL_DEVICE_HOST_ID,
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::{
    net::{TcpListener, TcpStream},
    process::Command,
    sync::{Mutex, RwLock, broadcast},
};

const ANDROID_BOOT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
const ANDROID_BOOT_POLL: std::time::Duration = std::time::Duration::from_millis(500);
const IOS_BOOT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

const HUB_PACKAGE: &str = "expo-device-hub";
const HUB_VERSION: &str = "0.12.0";
const AGENT_PACKAGE: &str = "agent-device";
const AGENT_VERSION: &str = "0.21.12";
const HUB_ENTRY: &[&str] = &["dist", "server", "cli.mjs"];
const AGENT_ENTRY: &[&str] = &["bin", "agent-device.mjs"];
const HOST_COMMAND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(600);
const HOST_OUTPUT_LIMIT: usize = 8 * 1024 * 1024;

/// Non-interactive SSH sessions do not source a user's shell profile. Keep
/// the fixed environment lookup in the Host runner so discovery, actions and
/// the pinned lifecycle see the same Node/Android tools.
const REMOTE_DEVICE_ENVIRONMENT: &str = r#"export PATH="$HOME/.local/bin:/opt/homebrew/bin:/usr/local/bin:$PATH"
if [ -z "$ANDROID_HOME" ]; then
  if [ -d "$HOME/Library/Android/sdk" ]; then export ANDROID_HOME="$HOME/Library/Android/sdk";
  elif [ -d "$HOME/Android/Sdk" ]; then export ANDROID_HOME="$HOME/Android/Sdk"; fi
fi
if [ -n "$ANDROID_HOME" ]; then export PATH="$ANDROID_HOME/platform-tools:$ANDROID_HOME/emulator:$ANDROID_HOME/cmdline-tools/latest/bin:$PATH"; fi
if [ -z "$JAVA_HOME" ] && ! command -v java >/dev/null 2>&1; then
  for device_java_home in "$HOME/.local/opt/android-studio/jbr" /opt/android-studio/jbr "/Applications/Android Studio.app/Contents/jbr" "$HOME/Applications/Android Studio.app/Contents/jbr"; do
    if [ -x "$device_java_home/bin/java" ]; then export JAVA_HOME="$device_java_home"; break; fi
  done
fi
if [ -n "$JAVA_HOME" ]; then export PATH="$JAVA_HOME/bin:$PATH"; fi
"#;

/// The SSH lifecycle is one fixed, audited script.  It installs only the two
/// pinned packages, writes state below the remote user's private directory,
/// and returns bounded JSON.  Device actions still travel through the typed
/// allowlist below; this script is never populated from a caller command.
const REMOTE_DEVICE_LIFECYCLE: &str = r#"
const fs = require('node:fs');
const path = require('node:path');
const os = require('node:os');
const net = require('node:net');
const { spawnSync, spawn } = require('node:child_process');
const mode = process.argv[2];
const owner = process.argv[3];
const hubVersion = '0.12.0';
const agentVersion = '0.21.12';
if (!/^[A-Za-z0-9_-]{1,128}$/.test(owner || '')) throw Error('invalid device host owner');
const root = path.join(os.homedir(), '.remote-agent', 'device', 'hosts', owner);
const tools = path.join(root, 'tools');
const state = path.join(root, 'state');
const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
const read = file => { try { return JSON.parse(fs.readFileSync(file, 'utf8')); } catch { return null; } };
const write = (file, value) => {
  fs.mkdirSync(path.dirname(file), { recursive: true, mode: 0o700 });
  const temporary = file + '.' + process.pid;
  fs.writeFileSync(temporary, JSON.stringify(value), { mode: 0o600 });
  fs.renameSync(temporary, file);
};
const run = (command, args, options = {}) => spawnSync(command, args, { encoding: 'utf8', timeout: 600000, maxBuffer: 8 * 1024 * 1024, ...options });
const entryFor = (name, version, suffix) => path.join(tools, name + '@' + version, 'node_modules', name, ...suffix);
const install = (name, version, suffix) => {
  const directory = path.join(tools, name + '@' + version);
  const entry = entryFor(name, version, suffix);
  const complete = () => fs.existsSync(entry) && fs.readFileSync(path.join(directory, '.install-complete'), 'utf8').trim() === version;
  if (complete()) return entry;
  fs.mkdirSync(tools, { recursive: true, mode: 0o700 });
  fs.rmSync(directory, { recursive: true, force: true });
  const staging = fs.mkdtempSync(path.join(tools, '.install-'));
  try {
    const result = run('npm', ['install', '--prefix', staging, '--no-fund', '--no-audit', name + '@' + version]);
    if (result.status !== 0) throw Error('Installing ' + name + ': ' + (result.error?.message || result.stderr?.slice(-2000) || 'npm failed'));
    if (!fs.existsSync(path.join(staging, 'node_modules', name, ...suffix))) throw Error('Missing installed entry for ' + name);
    fs.writeFileSync(path.join(staging, '.install-complete'), version);
    fs.renameSync(staging, directory);
    return entry;
  } finally {
    fs.rmSync(staging, { recursive: true, force: true });
  }
};
const healthy = async (port, route) => {
  try { return (await fetch('http://127.0.0.1:' + port + route, { signal: AbortSignal.timeout(2000) })).ok; }
  catch { return false; }
};
const freePort = () => new Promise((resolve, reject) => {
  const server = net.createServer();
  server.once('error', reject);
  server.listen(0, '127.0.0.1', () => { const value = server.address().port; server.close(() => resolve(value)); });
});
const stop = record => {
  if (!record?.pid || !record?.entryPath) return;
  const command = run('ps', ['-p', String(record.pid), '-o', 'command=']).stdout || '';
  if (command.includes(record.entryPath)) { try { process.kill(record.pid, 'SIGTERM'); } catch {} }
};
const startHub = async () => {
  const entry = install('expo-device-hub', hubVersion, ['dist', 'server', 'cli.mjs']);
  const file = path.join(state, 'hub.json');
  let current = read(file);
  if (current && current.version === hubVersion && current.entryPath === entry && await healthy(current.port, '/readyz')) return current;
  stop(current);
  const port = await freePort();
  const child = spawn(process.execPath, [entry, '--port', String(port), '--host', '127.0.0.1', '--hide-sidebar', '--hide-boot-device'], { cwd: state, detached: true, stdio: 'ignore', env: { ...process.env, FORCE_COLOR: '0', NO_COLOR: '1' } });
  child.unref();
  current = { pid: child.pid, port, entryPath: entry, version: hubVersion };
  write(file, current);
  const deadline = Date.now() + 30000;
  while (Date.now() < deadline) {
    if (await healthy(port, '/readyz')) return current;
    await sleep(200);
  }
  stop(current);
  throw Error('device hub did not become ready');
};
const startAgent = async () => {
  const entry = install('agent-device', agentVersion, ['bin', 'agent-device.mjs']);
  const file = path.join(state, 'daemon.json');
  const agentFile = path.join(state, 'agent.json');
  const recorded = read(agentFile);
  let current = read(file);
  if (current?.httpPort && current.token && recorded?.version === agentVersion && recorded.entryPath === entry && await healthy(current.httpPort, '/health')) return { ...current, entryPath: entry, version: agentVersion };
  if (current) run(process.execPath, [recorded?.entryPath || entry, 'daemon', 'stop', '--state-dir', state]);
  try { fs.rmSync(file, { force: true }); } catch {}
  const env = { ...process.env, AGENT_DEVICE_STATE_DIR: state, AGENT_DEVICE_DAEMON_SERVER_MODE: 'http', AGENT_DEVICE_DAEMON_IDLE_TIMEOUT_MS: '0', AGENT_DEVICE_NO_UPDATE_NOTIFIER: '1' };
  delete env.AGENT_DEVICE_DAEMON_BASE_URL; delete env.AGENT_DEVICE_DAEMON_AUTH_TOKEN; delete env.AGENT_DEVICE_CONFIG;
  const result = run(process.execPath, [entry, 'devices', '--json'], { env, timeout: 60000 });
  if (result.status !== 0 && !read(file)) throw Error('agent-device daemon failed to start: ' + (result.error?.message || result.stderr?.slice(-2000) || 'unknown error'));
  const deadline = Date.now() + 30000;
  while (Date.now() < deadline) {
    current = read(file);
    if (current?.httpPort && current.token && await healthy(current.httpPort, '/health')) {
      write(agentFile, { entryPath: entry, version: agentVersion });
      return { ...current, entryPath: entry, version: agentVersion };
    }
    await sleep(200);
  }
  throw Error('agent-device daemon did not become ready');
};
const platformState = () => {
  const ios = process.platform === 'darwin' && run('xcrun', ['simctl', 'help']).status === 0;
  // SSH hosts may expose physical Android devices through adb without an
  // emulator or avdmanager. Keep remote availability aligned with the
  // reference probe; offline AVD boot reports its own actionable failure.
  const android = run('adb', ['version']).status === 0;
  return [
    { platform: 'Ios', available: ios, ...(ios ? {} : { reason: 'iOS needs macOS with Xcode and working xcrun simctl.' }) },
    { platform: 'Android', available: android, ...(android ? {} : { reason: 'Android SDK missing. Set ANDROID_HOME or put adb on the SSH PATH.' }) },
  ];
};
const toolState = () => {
  const installed = (name, version, suffix) => {
    let names = [];
    try { names = fs.readdirSync(tools); } catch (error) { if (error.code !== 'ENOENT') throw error; }
    const prefix = name + '@';
    const installedVersions = names
      .filter(candidate => candidate.startsWith(prefix))
      .map(candidate => candidate.slice(prefix.length))
      .filter(candidate => /^[0-9]+\.[0-9]+\.[0-9]+(?:-[A-Za-z0-9.-]+)?$/.test(candidate))
      .filter(candidate => {
        const directory = path.join(tools, prefix + candidate);
        const entry = entryFor(name, candidate, suffix);
        try {
          return fs.existsSync(entry)
            && fs.readFileSync(path.join(directory, '.install-complete'), 'utf8').trim() === candidate;
        } catch { return false; }
      })
      .sort();
    return { required_version: version, installed_versions: installedVersions, running_version: null };
  };
  return {
    hub: installed('expo-device-hub', hubVersion, ['dist', 'server', 'cli.mjs']),
    agent: installed('agent-device', agentVersion, ['bin', 'agent-device.mjs']),
  };
};
const pruneTools = required => {
  const scan = run('ps', ['-ax', '-o', 'command=']);
  if (scan.status !== 0 || !scan.stdout) return;
  for (const [name, version, suffix] of required) {
    let names = [];
    try { names = fs.readdirSync(tools); } catch { continue; }
    const completed = names
      .filter(candidate => candidate.startsWith(name + '@'))
      .map(candidate => ({ version: candidate.slice(name.length + 1), directory: path.join(tools, candidate) }))
      .filter(candidate => /^[0-9]+\.[0-9]+\.[0-9]+(?:-[A-Za-z0-9.-]+)?$/.test(candidate.version))
      .filter(candidate => {
        try {
          return fs.statSync(candidate.directory).isDirectory()
            && fs.readFileSync(path.join(candidate.directory, '.install-complete'), 'utf8').trim() === candidate.version
            && fs.existsSync(path.join(candidate.directory, 'node_modules', name, ...suffix));
        } catch { return false; }
      })
      .map(candidate => {
        try { return { ...candidate, modified: fs.statSync(path.join(candidate.directory, '.install-complete')).mtimeMs }; }
        catch { return null; }
      })
      .filter(Boolean);
    if (!completed.some(candidate => candidate.version === version)) continue;
    const previous = completed
      .filter(candidate => candidate.version !== version)
      .sort((left, right) => right.modified - left.modified || right.version.localeCompare(left.version, 'en', { numeric: true }))[0]?.version;
    for (const candidate of completed) {
      if (candidate.version === version || candidate.version === previous || scan.stdout.includes(candidate.directory + path.sep)) continue;
      fs.rmSync(candidate.directory, { recursive: true, force: true });
    }
  }
};
const acquireLock = async () => {
  const lock = path.join(state, 'runtime.lock');
  const deadline = Date.now() + 600000;
  const token = process.pid + ':' + require('node:crypto').randomUUID();
  const owner = () => {
    try { return fs.readlinkSync(lock); }
    catch (error) { if (error.code === 'ENOENT') return null; throw error; }
  };
  while (true) {
    try {
      fs.symlinkSync(token, lock);
      return () => { if (owner() === token) fs.unlinkSync(lock); };
    } catch (error) {
      if (error.code !== 'EEXIST') throw error;
      const previous = owner();
      if (previous === null) continue;
      const pid = Number(previous.split(':')[0]);
      if (!Number.isSafeInteger(pid) || pid <= 0) throw Error('Invalid device lock at ' + lock);
      try { process.kill(pid, 0); } catch (probe) {
        if (probe.code === 'ESRCH' && owner() === previous) {
          try { fs.unlinkSync(lock); } catch (error) { if (error.code !== 'ENOENT') throw error; }
          continue;
        }
      }
      if (Date.now() >= deadline) throw Error('Device host lifecycle is locked at ' + lock);
      await sleep(500);
    }
  }
};
const main = async () => {
  fs.mkdirSync(state, { recursive: true, mode: 0o700 });
  const release = await acquireLock();
  try {
  if (mode === 'stop' || mode === 'stop-agent') {
    if (mode === 'stop') {
      const hubFile = path.join(state, 'hub.json');
      const hub = read(hubFile); stop(hub); fs.rmSync(hubFile, { force: true });
    }
    const entry = entryFor('agent-device', agentVersion, ['bin', 'agent-device.mjs']);
    if (fs.existsSync(entry)) run(process.execPath, [entry, 'daemon', 'stop', '--state-dir', state]);
    fs.rmSync(path.join(state, 'daemon.json'), { force: true });
    fs.rmSync(path.join(state, 'agent.json'), { force: true });
    return;
  }
  if (Number(process.versions.node.split('.')[0]) < 22) throw Error('Node 22 or newer is required on the device host.');
  const platforms = platformState();
  if (mode === 'probe') {
    if (run('npm', ['--version']).status !== 0) throw Error('npm is missing from the non-interactive SSH PATH.');
    const tools = toolState();
    console.log(JSON.stringify({ platforms, tools, hubInstalled: tools.hub.installed_versions.includes(hubVersion), agentDeviceInstalled: tools.agent.installed_versions.includes(agentVersion) }));
    return;
  }
  if (mode === 'hub-install') {
    install('expo-device-hub', hubVersion, ['dist', 'server', 'cli.mjs']);
    const tools = toolState();
    console.log(JSON.stringify({ platforms, tools, hubInstalled: true, agentDeviceInstalled: tools.agent.installed_versions.includes(agentVersion) }));
    return;
  }
  if (mode === 'agent-install') {
    install('agent-device', agentVersion, ['bin', 'agent-device.mjs']);
    const tools = toolState();
    console.log(JSON.stringify({ platforms, tools, hubInstalled: tools.hub.installed_versions.includes(hubVersion), agentDeviceInstalled: true }));
    return;
  }
  if (!platforms.some(platform => platform.available)) {
    throw Error(platforms.map(platform => platform.reason || (platform.platform + ' is unavailable')).join(' '));
  }
  if (mode === 'hub') {
    const hub = await startHub();
    pruneTools([['expo-device-hub', hubVersion, ['dist', 'server', 'cli.mjs']]]);
    const vendor = path.resolve(path.dirname(hub.entryPath), '../../vendor/serve-sim/dist');
    const tools = toolState();
    console.log(JSON.stringify({ ...hub, platforms, tools, hubInstalled: true, agentDeviceInstalled: tools.agent.installed_versions.includes(agentVersion), helpers: {
      serveSimAxSettings: fs.existsSync(path.join(vendor, 'simax', 'serve-sim-ax-settings')) ? path.join(vendor, 'simax', 'serve-sim-ax-settings') : null,
      serveSimCli: fs.existsSync(path.join(vendor, 'serve-sim.js')) ? path.join(vendor, 'serve-sim.js') : null,
    }}));
    return;
  }
  if (mode === 'agent') {
    const result = await startAgent();
    pruneTools([
      ['expo-device-hub', hubVersion, ['dist', 'server', 'cli.mjs']],
      ['agent-device', agentVersion, ['bin', 'agent-device.mjs']],
    ]);
    return console.log(JSON.stringify(result));
  }
  throw Error('unknown device lifecycle mode');
  } finally {
    release();
  }
};
main().catch(error => { console.error(error.message); process.exitCode = 1; });
"#;

#[derive(Debug, Clone, Copy)]
enum DeviceHelper {
    ServeSimAxSettings,
    ServeSimCli,
}

#[derive(Debug, Clone)]
struct HostOutput {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    code: i32,
}

#[async_trait]
trait DeviceHostRunner: Send + Sync {
    fn id(&self) -> &str;
    fn kind(&self) -> DeviceHostKind;
    fn label(&self) -> &str;
    fn target(&self) -> Option<&str> {
        None
    }
    fn identity_file(&self) -> Option<&str> {
        None
    }
    fn port(&self) -> Option<u16> {
        None
    }
    fn helper_path(&self, helper: DeviceHelper) -> Option<String>;
    fn set_helper_paths(&self, _helpers: SshHelperPaths) {}
    fn set_probe(&self, _probe: RemoteHostProbe) {}
    fn probe(&self) -> Option<RemoteHostProbe> {
        None
    }
    fn set_probe_error(&self, _error: Option<String>) {}
    fn probe_error(&self) -> Option<String> {
        None
    }
    fn config_matches(&self, _config: &DeviceHostConfig) -> bool {
        false
    }
    fn configure_process(&self, _process: &mut Command) {}
    async fn run(
        &self,
        command: &str,
        args: &[String],
        stdin: Option<&[u8]>,
    ) -> Result<HostOutput, String>;
    async fn start(&self, command: &str, args: &[String]) -> Result<(), String>;
    async fn forward(&self, remote_port: u16) -> Result<Option<ForwardedPort>, String>;
    async fn lifecycle(&self, mode: &str) -> Result<Option<HostOutput>, String>;
}

struct ForwardedPort {
    local_port: u16,
    child: tokio::process::Child,
}

struct LocalDeviceHost {
    label: String,
    state_root: PathBuf,
    android: AndroidToolPaths,
}

#[derive(Debug, Clone, Default)]
struct AndroidToolPaths {
    root: Option<PathBuf>,
    adb: Option<PathBuf>,
    emulator: Option<PathBuf>,
    avdmanager: Option<PathBuf>,
    legacy_avdmanager: Option<PathBuf>,
}

impl AndroidToolPaths {
    fn command(&self, command: &str) -> Option<&Path> {
        match command {
            "adb" => self.adb.as_deref(),
            "emulator" => self.emulator.as_deref(),
            _ => None,
        }
    }

    fn availability_reason(&self) -> Option<String> {
        let Some(root) = self.root.as_ref() else {
            return Some(
                "Android SDK was not found. Install it with Android Studio or set ANDROID_HOME to your SDK directory."
                    .into(),
            );
        };
        if self.adb.is_none() {
            return Some(format!(
                "Android SDK Platform-Tools are missing from {}. Install them in Android Studio's SDK Manager.",
                root.display()
            ));
        }
        if self.emulator.is_none() {
            return Some(format!(
                "Android Emulator is missing from {}. Install it in Android Studio's SDK Manager.",
                root.display()
            ));
        }
        if self.avdmanager.is_none() {
            return Some(if self.legacy_avdmanager.is_some() {
                format!(
                    "The Android SDK command-line tools in {} appear to be an older, unsupported version. Install Android SDK Command-line Tools (latest) in Android Studio's SDK Manager under SDK Tools.",
                    root.display()
                )
            } else {
                format!(
                    "Android SDK Command-line Tools (latest) are missing from {}. Install them in Android Studio's SDK Manager.",
                    root.display()
                )
            });
        }
        None
    }
}

#[async_trait]
impl DeviceHostRunner for LocalDeviceHost {
    fn id(&self) -> &str {
        LOCAL_DEVICE_HOST_ID
    }
    fn kind(&self) -> DeviceHostKind {
        DeviceHostKind::Local
    }
    fn label(&self) -> &str {
        &self.label
    }
    fn helper_path(&self, helper: DeviceHelper) -> Option<String> {
        let entry = match helper {
            DeviceHelper::ServeSimAxSettings => [
                "vendor",
                "serve-sim",
                "dist",
                "simax",
                "serve-sim-ax-settings",
            ]
            .as_slice(),
            DeviceHelper::ServeSimCli => ["vendor", "serve-sim", "dist", "serve-sim.js"].as_slice(),
        };
        let mut path = self
            .state_root
            .join("tools")
            .join(HUB_PACKAGE)
            .join(HUB_VERSION)
            .join("node_modules")
            .join(HUB_PACKAGE);
        for part in entry {
            path.push(part);
        }
        std::fs::metadata(&path)
            .ok()
            .filter(|metadata| metadata.is_file())
            .map(|_| path.to_string_lossy().into_owned())
    }

    fn configure_process(&self, process: &mut Command) {
        configure_android_environment(process, &self.android);
    }

    async fn run(
        &self,
        command: &str,
        args: &[String],
        stdin: Option<&[u8]>,
    ) -> Result<HostOutput, String> {
        let resolved = self
            .android
            .command(command)
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_else(|| {
                if command == "node" {
                    node_command().to_string_lossy().into_owned()
                } else {
                    command.to_owned()
                }
            });
        run_process(&resolved, args, stdin, Some(&self.android)).await
    }

    async fn start(&self, command: &str, args: &[String]) -> Result<(), String> {
        let resolved = self
            .android
            .command(command)
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_else(|| command.to_owned());
        start_process(&resolved, args, Some(&self.android)).await
    }

    async fn forward(&self, _remote_port: u16) -> Result<Option<ForwardedPort>, String> {
        Ok(None)
    }

    async fn lifecycle(&self, _mode: &str) -> Result<Option<HostOutput>, String> {
        Ok(None)
    }
}

struct SshDeviceHost {
    config: DeviceHostConfig,
    owner: String,
    helpers: std::sync::RwLock<Option<SshHelperPaths>>,
    probe: std::sync::RwLock<Option<RemoteHostProbe>>,
    probe_error: std::sync::RwLock<Option<String>>,
}

#[derive(Debug, Clone)]
struct SshHelperPaths {
    serve_sim_ax_settings: Option<String>,
    serve_sim_cli: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct RemoteHostProbe {
    #[serde(default)]
    platforms: Vec<DevicePlatformAvailability>,
    #[serde(default)]
    tools: Option<DeviceToolVersions>,
    #[serde(rename = "hubInstalled", default)]
    hub_installed: bool,
    #[serde(rename = "agentDeviceInstalled", default)]
    agent_device_installed: bool,
}

#[async_trait]
impl DeviceHostRunner for SshDeviceHost {
    fn id(&self) -> &str {
        &self.config.id
    }
    fn kind(&self) -> DeviceHostKind {
        DeviceHostKind::Ssh
    }
    fn label(&self) -> &str {
        &self.config.label
    }
    fn target(&self) -> Option<&str> {
        Some(&self.config.target)
    }
    fn identity_file(&self) -> Option<&str> {
        self.config.identity_file.as_deref()
    }
    fn port(&self) -> Option<u16> {
        self.config.port
    }
    fn helper_path(&self, helper: DeviceHelper) -> Option<String> {
        let helpers = self.helpers.read().ok()?.clone()?;
        match helper {
            DeviceHelper::ServeSimAxSettings => helpers.serve_sim_ax_settings,
            DeviceHelper::ServeSimCli => helpers.serve_sim_cli,
        }
    }
    fn set_helper_paths(&self, helpers: SshHelperPaths) {
        if let Ok(mut current) = self.helpers.write() {
            *current = Some(helpers);
        }
    }
    fn set_probe(&self, probe: RemoteHostProbe) {
        if let Ok(mut current) = self.probe.write() {
            *current = Some(probe);
        }
        self.set_probe_error(None);
    }
    fn probe(&self) -> Option<RemoteHostProbe> {
        self.probe.read().ok()?.clone()
    }
    fn set_probe_error(&self, error: Option<String>) {
        if let Ok(mut current) = self.probe_error.write() {
            *current = error;
        }
    }
    fn probe_error(&self) -> Option<String> {
        self.probe_error.read().ok()?.clone()
    }
    fn config_matches(&self, config: &DeviceHostConfig) -> bool {
        &self.config == config
    }

    async fn run(
        &self,
        command: &str,
        args: &[String],
        stdin: Option<&[u8]>,
    ) -> Result<HostOutput, String> {
        let mut ssh_args = vec!["-T".to_owned(), "-o".to_owned(), "BatchMode=yes".to_owned()];
        if let Some(port) = self.config.port {
            ssh_args.extend(["-p".into(), port.to_string()]);
        }
        if let Some(identity) = &self.config.identity_file {
            ssh_args.extend(["-i".into(), identity.clone()]);
        }
        ssh_args.push(self.config.target.clone());
        ssh_args.push(remote_shell_command(command, args, false));
        run_process("ssh", &ssh_args, stdin, None).await
    }

    async fn start(&self, command: &str, args: &[String]) -> Result<(), String> {
        let mut ssh_args = vec!["-T".to_owned(), "-o".to_owned(), "BatchMode=yes".to_owned()];
        if let Some(port) = self.config.port {
            ssh_args.extend(["-p".into(), port.to_string()]);
        }
        if let Some(identity) = &self.config.identity_file {
            ssh_args.extend(["-i".into(), identity.clone()]);
        }
        ssh_args.push(self.config.target.clone());
        let remote = remote_shell_command(command, args, true);
        let output = run_process(
            "ssh",
            &ssh_args_with_command(ssh_args, vec![remote]),
            None,
            None,
        )
        .await?;
        if output.code == 0 {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&output.stderr).trim().to_owned())
        }
    }

    async fn forward(&self, remote_port: u16) -> Result<Option<ForwardedPort>, String> {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .map_err(|error| format!("could not reserve a local device tunnel port: {error}"))?;
        let local_port = listener
            .local_addr()
            .map_err(|error| error.to_string())?
            .port();
        drop(listener);
        let mut args = vec![
            "-T".into(),
            "-N".into(),
            "-o".into(),
            "BatchMode=yes".into(),
            "-o".into(),
            "ExitOnForwardFailure=yes".into(),
        ];
        if let Some(port) = self.config.port {
            args.extend(["-p".into(), port.to_string()]);
        }
        if let Some(identity) = &self.config.identity_file {
            args.extend(["-i".into(), identity.clone()]);
        }
        args.extend([
            "-L".into(),
            format!("{local_port}:127.0.0.1:{remote_port}"),
            self.config.target.clone(),
        ]);
        let mut child = Command::new("ssh")
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|error| format!("could not start the device tunnel: {error}"))?;
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            if TcpStream::connect(("127.0.0.1", local_port)).await.is_ok() {
                return Ok(Some(ForwardedPort { local_port, child }));
            }
            if let Some(status) = child.try_wait().map_err(|error| error.to_string())? {
                return Err(format!(
                    "device tunnel exited before becoming ready ({status})"
                ));
            }
            if tokio::time::Instant::now() >= deadline {
                let _ = child.kill().await;
                return Err("device tunnel did not become ready within 10 seconds".into());
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    }

    async fn lifecycle(&self, mode: &str) -> Result<Option<HostOutput>, String> {
        if !matches!(
            mode,
            "probe" | "hub-install" | "agent-install" | "hub" | "agent" | "stop" | "stop-agent"
        ) {
            return Err("invalid remote device lifecycle mode".into());
        }
        let output = self
            .run(
                "node",
                &["-".into(), mode.into(), self.owner.clone()],
                Some(REMOTE_DEVICE_LIFECYCLE.as_bytes()),
            )
            .await?;
        Ok(Some(output))
    }
}

fn ssh_args_with_command(mut args: Vec<String>, remote: Vec<String>) -> Vec<String> {
    args.push(remote.join(" "));
    args
}

fn remote_shell_command(command: &str, args: &[String], detached: bool) -> String {
    let mut invocation = String::from(REMOTE_DEVICE_ENVIRONMENT);
    invocation.push_str("exec ");
    invocation.push_str(&shell_quote(command));
    for arg in args {
        invocation.push(' ');
        invocation.push_str(&shell_quote(arg));
    }
    if detached {
        format!(
            "nohup sh -lc {} </dev/null >/dev/null 2>&1 &",
            shell_quote(&invocation)
        )
    } else {
        format!("sh -lc {}", shell_quote(&invocation))
    }
}

async fn run_process(
    command: &str,
    args: &[String],
    stdin: Option<&[u8]>,
    android: Option<&AndroidToolPaths>,
) -> Result<HostOutput, String> {
    use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};

    async fn read_limited<R: AsyncRead + Unpin>(mut reader: R) -> Result<(Vec<u8>, bool), String> {
        let mut output = Vec::new();
        let mut buffer = [0_u8; 16 * 1024];
        let mut truncated = false;
        loop {
            let size = reader
                .read(&mut buffer)
                .await
                .map_err(|error| error.to_string())?;
            if size == 0 {
                break;
            }
            let remaining = HOST_OUTPUT_LIMIT.saturating_sub(output.len());
            let copy = remaining.min(size);
            output.extend_from_slice(&buffer[..copy]);
            truncated |= copy != size;
        }
        Ok((output, truncated))
    }

    let mut process = Command::new(command);
    if let Some(android) = android {
        configure_android_environment(&mut process, android);
    }
    process.args(args);
    if stdin.is_some() {
        process.stdin(std::process::Stdio::piped());
    }
    let mut child = process
        .kill_on_drop(true)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|error| format!("could not start {command}: {error}"))?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| format!("{command} did not expose stdout"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| format!("{command} did not expose stderr"))?;
    let stdout_task = tokio::spawn(read_limited(stdout));
    let stderr_task = tokio::spawn(read_limited(stderr));

    if let Some(input) = stdin {
        if let Some(mut writer) = child.stdin.take() {
            if let Err(error) = writer.write_all(input).await {
                let _ = child.kill().await;
                let _ = child.wait().await;
                let _ = stdout_task.await;
                let _ = stderr_task.await;
                return Err(error.to_string());
            }
        }
    }

    let status = match tokio::time::timeout(HOST_COMMAND_TIMEOUT, child.wait()).await {
        Ok(result) => result.map_err(|error| error.to_string())?,
        Err(_) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            let _ = stdout_task.await;
            let _ = stderr_task.await;
            return Err(format!(
                "{command} did not finish within {} seconds",
                HOST_COMMAND_TIMEOUT.as_secs()
            ));
        }
    };
    let (stdout, stdout_truncated) = stdout_task.await.map_err(|error| error.to_string())??;
    let (stderr, stderr_truncated) = stderr_task.await.map_err(|error| error.to_string())??;
    if stdout_truncated || stderr_truncated {
        return Err(format!(
            "{command} produced more than {HOST_OUTPUT_LIMIT} bytes of output"
        ));
    }
    Ok(HostOutput {
        stdout,
        stderr,
        code: status.code().unwrap_or(-1),
    })
}

async fn reap_stale_hub(path: &Path, host: &dyn DeviceHostRunner) {
    let state = tokio::fs::read(path)
        .await
        .ok()
        .and_then(|bytes| serde_json::from_slice::<PersistedHubState>(&bytes).ok());
    let Some(state) = state else {
        let _ = tokio::fs::remove_file(path).await;
        return;
    };
    let output = host
        .run(
            "ps",
            &[
                "-p".into(),
                state.pid.to_string(),
                "-o".into(),
                "command=".into(),
            ],
            None,
        )
        .await;
    if output.as_ref().is_ok_and(|output| {
        output.code == 0 && String::from_utf8_lossy(&output.stdout).contains(&state.entry_path)
    }) {
        terminate_process(state.pid).await;
    }
    let _ = tokio::fs::remove_file(path).await;
}

async fn terminate_process(pid: u32) {
    #[cfg(unix)]
    {
        let _ = Command::new("kill")
            .args(["-TERM", &pid.to_string()])
            .output()
            .await;
    }
    #[cfg(windows)]
    {
        let _ = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .output()
            .await;
    }
}

async fn write_private_json<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let content = serde_json::to_vec(value).map_err(|error| error.to_string())?;
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|error| error.to_string())?;
    }
    let temporary = path.with_extension("json.tmp");
    tokio::fs::write(&temporary, content)
        .await
        .map_err(|error| error.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = tokio::fs::metadata(&temporary)
            .await
            .map_err(|error| error.to_string())?
            .permissions();
        permissions.set_mode(0o600);
        tokio::fs::set_permissions(&temporary, permissions)
            .await
            .map_err(|error| error.to_string())?;
    }
    if let Err(error) = tokio::fs::rename(&temporary, path).await {
        let _ = tokio::fs::remove_file(&temporary).await;
        return Err(error.to_string());
    }
    Ok(())
}

async fn start_process(
    command: &str,
    args: &[String],
    android: Option<&AndroidToolPaths>,
) -> Result<(), String> {
    let mut process = Command::new(command);
    if let Some(android) = android {
        configure_android_environment(&mut process, android);
    }
    process
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("could not start {command}: {error}"))
}

fn configure_android_environment(process: &mut Command, android: &AndroidToolPaths) {
    let Some(root) = android.root.as_ref() else {
        return;
    };
    process.env("ANDROID_HOME", root);
    process.env("ANDROID_SDK_ROOT", root);
    let mut paths = vec![
        root.join("platform-tools"),
        root.join("emulator"),
        root.join("cmdline-tools").join("latest").join("bin"),
    ];
    if let Some(existing) = std::env::var_os("PATH") {
        paths.extend(std::env::split_paths(&existing));
    }
    if let Ok(path) = std::env::join_paths(paths) {
        process.env("PATH", path);
    }
}

fn tool_entry(root: &Path, package: &str, version: &str, entry: &[&str]) -> PathBuf {
    entry.iter().fold(
        root.join("tools")
            .join(package)
            .join(version)
            .join("node_modules")
            .join(package),
        |path, part| path.join(part),
    )
}

async fn ensure_pinned_tool(
    host: &dyn DeviceHostRunner,
    root: &Path,
    package: &str,
    version: &str,
    entry: &[&str],
) -> Result<PathBuf, String> {
    let install = root.join("tools").join(package).join(version);
    let entry_path = tool_entry(root, package, version, entry);
    let complete = install.join(".install-complete");
    let installed = tokio::fs::metadata(&entry_path).await.is_ok()
        && tokio::fs::read_to_string(&complete)
            .await
            .ok()
            .is_some_and(|value| value.trim() == version);
    if installed {
        prune_tool_versions(host, root, package, version).await;
        return Ok(entry_path);
    }
    let tools = root.join("tools").join(package);
    tokio::fs::create_dir_all(&tools)
        .await
        .map_err(|error| error.to_string())?;
    let _ = tokio::fs::remove_dir_all(&install).await;
    let staging = tools.join(format!(".staging-{}", uuid::Uuid::new_v4()));
    let package_version = format!("{package}@{version}");
    let npm_args = vec![
        "install".into(),
        "--prefix".into(),
        staging.to_string_lossy().into_owned(),
        "--no-fund".into(),
        "--no-audit".into(),
        package_version.clone(),
    ];
    let npm = tokio::time::timeout(
        std::time::Duration::from_secs(600),
        host.run("npm", &npm_args, None),
    )
    .await
    .map_err(|_| format!("{package} installation timed out"))?;
    let output = match npm {
        Ok(output)
            if output.code != 0
                && String::from_utf8_lossy(&output.stderr).contains("could not start npm") =>
        {
            match host
                .run(
                    "pnpm",
                    &[
                        "--package=npm@11".into(),
                        "dlx".into(),
                        "npm".into(),
                        "install".into(),
                        "--prefix".into(),
                        staging.to_string_lossy().into_owned(),
                        "--no-fund".into(),
                        "--no-audit".into(),
                        package_version,
                    ],
                    None,
                )
                .await
            {
                Ok(output) => output,
                Err(error) => {
                    let _ = tokio::fs::remove_dir_all(&staging).await;
                    return Err(error);
                }
            }
        }
        Ok(output) => output,
        Err(error) if error.contains("could not start npm") => {
            match host
                .run(
                    "pnpm",
                    &[
                        "--package=npm@11".into(),
                        "dlx".into(),
                        "npm".into(),
                        "install".into(),
                        "--prefix".into(),
                        staging.to_string_lossy().into_owned(),
                        "--no-fund".into(),
                        "--no-audit".into(),
                        package_version,
                    ],
                    None,
                )
                .await
            {
                Ok(output) => output,
                Err(error) => {
                    let _ = tokio::fs::remove_dir_all(&staging).await;
                    return Err(error);
                }
            }
        }
        Err(error) => {
            let _ = tokio::fs::remove_dir_all(&staging).await;
            return Err(error);
        }
    };
    if output.code != 0 {
        let _ = tokio::fs::remove_dir_all(&staging).await;
        return Err(format!(
            "{package} installation failed ({}): {}",
            output.code,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let staged_entry = entry
        .iter()
        .fold(staging.join("node_modules").join(package), |path, part| {
            path.join(part)
        });
    if tokio::fs::metadata(&staged_entry).await.is_err() {
        let _ = tokio::fs::remove_dir_all(&staging).await;
        return Err(format!(
            "{package} installation did not contain its entry point"
        ));
    }
    if let Err(error) =
        tokio::fs::write(staging.join(".install-complete"), format!("{version}\n")).await
    {
        let _ = tokio::fs::remove_dir_all(&staging).await;
        return Err(error.to_string());
    }
    if let Err(error) = tokio::fs::rename(&staging, &install).await {
        let _ = tokio::fs::remove_dir_all(&staging).await;
        return Err(error.to_string());
    }
    prune_tool_versions(host, root, package, version).await;
    Ok(entry_path)
}

async fn prune_tool_versions(
    host: &dyn DeviceHostRunner,
    root: &Path,
    package: &str,
    required: &str,
) {
    let output = match host
        .run("ps", &["-ax".into(), "-o".into(), "command=".into()], None)
        .await
    {
        Ok(output) if output.code == 0 => output,
        _ => return,
    };
    let running = String::from_utf8_lossy(&output.stdout);
    let parent = root.join("tools").join(package);
    let Ok(mut entries) = tokio::fs::read_dir(&parent).await else {
        return;
    };
    let mut completed = Vec::new();
    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name().to_string_lossy().into_owned();
        let version = name.as_str();
        if !is_tool_version(version) {
            continue;
        }
        let directory = entry.path();
        let Ok(metadata) = tokio::fs::metadata(&directory).await else {
            continue;
        };
        if !metadata.is_dir() {
            continue;
        }
        let sentinel = directory.join(".install-complete");
        let Ok(contents) = tokio::fs::read_to_string(&sentinel).await else {
            continue;
        };
        if contents.trim() != version {
            continue;
        }
        let modified = tokio::fs::metadata(&sentinel)
            .await
            .ok()
            .and_then(|metadata| metadata.modified().ok())
            .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
            .map(|duration| duration.as_millis())
            .unwrap_or_default();
        completed.push((version.to_owned(), directory, modified));
    }
    if !completed.iter().any(|(version, _, _)| version == required) {
        return;
    }
    let previous = completed
        .iter()
        .filter(|(version, _, _)| version != required)
        .max_by(|left, right| left.2.cmp(&right.2).then_with(|| left.0.cmp(&right.0)))
        .map(|(version, _, _)| version.clone());
    for (version, directory, _) in completed {
        if version == required
            || previous.as_deref() == Some(version.as_str())
            || running.contains(&format!(
                "{}{}",
                directory.to_string_lossy(),
                std::path::MAIN_SEPARATOR
            ))
        {
            continue;
        }
        let _ = tokio::fs::remove_dir_all(directory).await;
    }
}

fn shell_quote(value: &str) -> String {
    if value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || b"._/-:".contains(&byte))
    {
        return value.to_owned();
    }
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceCommand {
    pub program: String,
    pub args: Vec<String>,
}

fn command(program: &str, args: impl IntoIterator<Item = String>) -> DeviceCommand {
    DeviceCommand {
        program: program.into(),
        args: args.into_iter().collect(),
    }
}

macro_rules! argv {
    ($($arg:expr),* $(,)?) => { vec![$($arg.to_string()),*] };
}

/// The allowlisted commands for one typed action.  No caller-supplied command
/// or shell fragment reaches a host runner.
pub fn action_commands(
    platform: DevicePlatform,
    device_id: &str,
    action: &DeviceActionKind,
) -> Result<Vec<DeviceCommand>, String> {
    action_commands_with_helpers(platform, device_id, action, None, None)
}

fn action_commands_with_helpers(
    platform: DevicePlatform,
    device_id: &str,
    action: &DeviceActionKind,
    serve_sim_ax_settings: Option<&str>,
    serve_sim_cli: Option<&str>,
) -> Result<Vec<DeviceCommand>, String> {
    let mut commands = Vec::new();
    match (platform, action) {
        (DevicePlatform::Ios, DeviceActionKind::SetAppearance(value)) => commands.push(command(
            "xcrun",
            argv![
                "simctl",
                "ui",
                device_id,
                "appearance",
                match value {
                    DeviceAppearance::Light => "light",
                    DeviceAppearance::Dark => "dark",
                }
            ],
        )),
        (DevicePlatform::Ios, DeviceActionKind::SetTextSize(value)) => commands.push(command(
            "xcrun",
            argv![
                "simctl",
                "ui",
                device_id,
                "content_size",
                ios_text_size(*value)
            ],
        )),
        (DevicePlatform::Ios, DeviceActionKind::SetToggle { setting, value })
            if setting == "increaseContrast" =>
        {
            commands.push(command(
                "xcrun",
                argv![
                    "simctl",
                    "ui",
                    device_id,
                    "increase_contrast",
                    if *value { "enabled" } else { "disabled" }
                ],
            ))
        }
        (DevicePlatform::Ios, DeviceActionKind::SetToggle { setting, value })
            if [
                "reduceMotion",
                "reduceTransparency",
                "showBorders",
                "voiceOver",
            ]
            .contains(&setting.as_str()) =>
        {
            let helper =
                serve_sim_ax_settings.ok_or("iOS accessibility helper is not installed")?;
            commands.push(command(
                "xcrun",
                argv![
                    "simctl",
                    "spawn",
                    device_id,
                    helper,
                    "set",
                    ios_toggle(setting),
                    if *value { "on" } else { "off" }
                ],
            ));
        }
        (DevicePlatform::Ios, DeviceActionKind::SetLiquidGlass(value)) => {
            let helper =
                serve_sim_ax_settings.ok_or("iOS accessibility helper is not installed")?;
            commands.push(command(
                "xcrun",
                argv![
                    "simctl",
                    "spawn",
                    device_id,
                    helper,
                    "set",
                    "liquid-glass",
                    value
                ],
            ));
        }
        (DevicePlatform::Ios, DeviceActionKind::SetColorFilter(value)) => {
            let helper =
                serve_sim_ax_settings.ok_or("iOS accessibility helper is not installed")?;
            commands.push(command(
                "xcrun",
                argv![
                    "simctl",
                    "spawn",
                    device_id,
                    helper,
                    "set",
                    "color-filter",
                    ios_color_filter(*value)
                ],
            ));
        }
        (
            DevicePlatform::Ios,
            DeviceActionKind::SetLocation {
                latitude,
                longitude,
            },
        ) => commands.push(command(
            "xcrun",
            argv![
                "simctl",
                "location",
                device_id,
                "set",
                format!("{latitude},{longitude}")
            ],
        )),
        (DevicePlatform::Ios, DeviceActionKind::ClearLocation) => commands.push(command(
            "xcrun",
            argv!["simctl", "location", device_id, "clear"],
        )),
        (
            DevicePlatform::Ios,
            DeviceActionKind::SetPermission {
                app_id,
                permission: device::DevicePermission::Notifications,
                decision,
            },
        ) => {
            let helper = serve_sim_cli.ok_or("iOS device helper is not installed")?;
            commands.push(command(
                "node",
                argv![
                    helper,
                    "permissions",
                    ios_permission_decision(*decision),
                    "notifications",
                    app_id,
                    "-d",
                    device_id
                ],
            ));
        }
        (
            DevicePlatform::Ios,
            DeviceActionKind::SetPermission {
                app_id,
                permission,
                decision,
            },
        ) => commands.push(command(
            "xcrun",
            argv![
                "simctl",
                "privacy",
                ios_permission_decision(*decision),
                ios_permission(*permission)?,
                app_id
            ],
        )),
        (DevicePlatform::Ios, DeviceActionKind::OpenUrl(url)) => {
            commands.push(command("xcrun", argv!["simctl", "openurl", device_id, url]))
        }
        (DevicePlatform::Ios, DeviceActionKind::LaunchApp(app)) => {
            commands.push(command("xcrun", argv!["simctl", "launch", device_id, app]))
        }
        (DevicePlatform::Ios, DeviceActionKind::TerminateApp(app)) => commands.push(command(
            "xcrun",
            argv!["simctl", "terminate", device_id, app],
        )),
        (DevicePlatform::Ios, DeviceActionKind::SendPush { app_id, .. }) => commands.push(command(
            "xcrun",
            argv!["simctl", "push", device_id, app_id, "-"],
        )),
        (DevicePlatform::Android, DeviceActionKind::SetAppearance(value)) => {
            commands.push(adb_shell(
                device_id,
                argv![
                    "cmd",
                    "uimode",
                    "night",
                    if matches!(value, DeviceAppearance::Dark) {
                        "yes"
                    } else {
                        "no"
                    }
                ],
            ))
        }
        (DevicePlatform::Android, DeviceActionKind::SetTextSize(value)) => {
            commands.push(adb_shell(
                device_id,
                argv![
                    "settings",
                    "put",
                    "system",
                    "font_scale",
                    android_text_size(*value)
                ],
            ))
        }
        (DevicePlatform::Android, DeviceActionKind::SetToggle { setting, value })
            if setting == "networkEnabled" =>
        {
            let enabled = if *value { "enable" } else { "disable" };
            commands.push(adb_shell(device_id, argv!["svc", "wifi", enabled]));
            commands.push(adb_shell(device_id, argv!["svc", "data", enabled]));
        }
        (DevicePlatform::Android, DeviceActionKind::SetToggle { setting, value })
            if setting == "reduceMotion" =>
        {
            let scale = if *value { "0" } else { "1" };
            for key in [
                "animator_duration_scale",
                "transition_animation_scale",
                "window_animation_scale",
            ] {
                commands.push(adb_shell(
                    device_id,
                    argv!["settings", "put", "global", key, scale],
                ));
            }
        }
        (DevicePlatform::Android, DeviceActionKind::SetOrientation(value)) => {
            if device_id.starts_with("emulator-") {
                commands.push(adb_shell(
                    device_id,
                    argv!["settings", "put", "system", "accelerometer_rotation", "1"],
                ));
                commands.push(adb_shell(
                    device_id,
                    argv!["cmd", "window", "user-rotation", "free"],
                ));
                commands.push(command(
                    "adb",
                    argv![
                        "-s",
                        device_id,
                        "emu",
                        "sensor",
                        "set",
                        "acceleration",
                        android_gravity(*value)
                    ],
                ));
            } else {
                commands.push(adb_shell(
                    device_id,
                    argv![
                        "cmd",
                        "window",
                        "user-rotation",
                        "lock",
                        android_rotation(*value)
                    ],
                ));
            }
        }
        (
            DevicePlatform::Android,
            DeviceActionKind::SetLocation {
                latitude,
                longitude,
            },
        ) => commands.push(command(
            "adb",
            argv!["-s", device_id, "emu", "geo", "fix", longitude, latitude],
        )),
        (DevicePlatform::Android, DeviceActionKind::ClearLocation) => {}
        (
            DevicePlatform::Android,
            DeviceActionKind::SetPermission {
                app_id,
                permission,
                decision,
            },
        ) => {
            let verb = if matches!(decision, device::DevicePermissionDecision::Grant) {
                "grant"
            } else {
                "revoke"
            };
            let permissions = android_permissions(*permission);
            if permissions.is_empty() {
                return Err("permission is unsupported on Android".into());
            }
            for permission in permissions {
                commands.push(adb_shell(device_id, argv!["pm", verb, app_id, permission]));
            }
        }
        (DevicePlatform::Android, DeviceActionKind::OpenUrl(url)) => commands.push(adb_shell(
            device_id,
            argv!["am", "start", "-a", "android.intent.action.VIEW", "-d", url],
        )),
        (DevicePlatform::Android, DeviceActionKind::LaunchApp(app)) => commands.push(adb_shell(
            device_id,
            argv![
                "monkey",
                "-p",
                app,
                "-c",
                "android.intent.category.LAUNCHER",
                "1"
            ],
        )),
        (DevicePlatform::Android, DeviceActionKind::TerminateApp(app)) => {
            commands.push(adb_shell(device_id, argv!["am", "force-stop", app]))
        }
        (_, DeviceActionKind::SetToggle { .. }) => {
            return Err("action setting is unsupported on this platform".into());
        }
        (
            _,
            DeviceActionKind::SetLiquidGlass(_)
            | DeviceActionKind::SetColorFilter(_)
            | DeviceActionKind::Shake
            | DeviceActionKind::SendPush { .. },
        ) => return Err("action is unsupported on this platform".into()),
        (DevicePlatform::Ios, DeviceActionKind::SetOrientation(_)) => {
            return Err("action is unsupported on this platform".into());
        }
    }
    Ok(commands)
}

fn adb_shell(device: &str, args: impl IntoIterator<Item = String>) -> DeviceCommand {
    let mut command = vec!["-s".to_owned(), device.to_owned(), "shell".to_owned()];
    command.extend(args);
    DeviceCommand {
        program: "adb".into(),
        args: command,
    }
}

fn ios_text_size(value: DeviceTextSize) -> &'static str {
    match value {
        DeviceTextSize::Small => "small",
        DeviceTextSize::Default => "large",
        DeviceTextSize::Large => "extra-extra-large",
        DeviceTextSize::ExtraLarge => "accessibility-large",
    }
}
fn android_text_size(value: DeviceTextSize) -> &'static str {
    match value {
        DeviceTextSize::Small => "0.85",
        DeviceTextSize::Default => "1.0",
        DeviceTextSize::Large => "1.15",
        DeviceTextSize::ExtraLarge => "1.3",
    }
}
fn ios_toggle(value: &str) -> &'static str {
    match value {
        "reduceMotion" => "reduce-motion",
        "reduceTransparency" => "reduce-transparency",
        "showBorders" => "show-borders",
        "voiceOver" => "voiceover",
        _ => "increase-contrast",
    }
}
fn ios_color_filter(value: DeviceColorFilter) -> &'static str {
    match value {
        DeviceColorFilter::None => "none",
        DeviceColorFilter::Grayscale => "grayscale",
        DeviceColorFilter::RedGreen => "red-green",
        DeviceColorFilter::GreenRed => "green-red",
        DeviceColorFilter::BlueYellow => "blue-yellow",
    }
}
fn android_rotation(value: DeviceOrientation) -> &'static str {
    match value {
        DeviceOrientation::Portrait => "0",
        DeviceOrientation::LandscapeLeft => "1",
        DeviceOrientation::PortraitUpsideDown => "2",
        DeviceOrientation::LandscapeRight => "3",
    }
}
fn android_gravity(value: DeviceOrientation) -> &'static str {
    match value {
        DeviceOrientation::Portrait => "0:9.81:0",
        DeviceOrientation::LandscapeLeft => "9.81:0:0",
        DeviceOrientation::PortraitUpsideDown => "0:-9.81:0",
        DeviceOrientation::LandscapeRight => "-9.81:0:0",
    }
}
fn ios_permission_decision(value: device::DevicePermissionDecision) -> &'static str {
    match value {
        device::DevicePermissionDecision::Grant => "grant",
        device::DevicePermissionDecision::Revoke => "revoke",
        device::DevicePermissionDecision::Reset => "reset",
    }
}
fn ios_permission(value: device::DevicePermission) -> Result<&'static str, String> {
    Ok(match value {
        device::DevicePermission::Camera => "camera",
        device::DevicePermission::Microphone => "microphone",
        device::DevicePermission::Photos => "photos",
        device::DevicePermission::Contacts => "contacts",
        device::DevicePermission::Calendar => "calendar",
        device::DevicePermission::Reminders => "reminders",
        device::DevicePermission::Location => "location",
        device::DevicePermission::Motion => "motion",
        device::DevicePermission::MediaLibrary => "media-library",
        device::DevicePermission::FaceId => "faceid",
        device::DevicePermission::Notifications => {
            return Err("notifications need the platform helper".into());
        }
    })
}
fn android_permissions(value: device::DevicePermission) -> &'static [&'static str] {
    match value {
        device::DevicePermission::Camera => &["android.permission.CAMERA"],
        device::DevicePermission::Microphone => &["android.permission.RECORD_AUDIO"],
        device::DevicePermission::Photos => &[
            "android.permission.READ_MEDIA_IMAGES",
            "android.permission.READ_EXTERNAL_STORAGE",
        ],
        device::DevicePermission::Contacts => &[
            "android.permission.READ_CONTACTS",
            "android.permission.WRITE_CONTACTS",
        ],
        device::DevicePermission::Calendar => &[
            "android.permission.READ_CALENDAR",
            "android.permission.WRITE_CALENDAR",
        ],
        device::DevicePermission::Location => &[
            "android.permission.ACCESS_FINE_LOCATION",
            "android.permission.ACCESS_COARSE_LOCATION",
        ],
        device::DevicePermission::Notifications => &["android.permission.POST_NOTIFICATIONS"],
        device::DevicePermission::Motion => &["android.permission.ACTIVITY_RECOGNITION"],
        _ => &[],
    }
}

#[derive(Debug, Deserialize)]
struct SimctlList {
    devices: BTreeMap<String, Vec<SimctlDevice>>,
}
#[derive(Debug, Deserialize)]
struct SimctlDevice {
    name: String,
    udid: String,
    state: String,
    #[serde(rename = "isAvailable")]
    is_available: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct HubDeviceList {
    #[serde(default)]
    simulators: Vec<HubDevice>,
    #[serde(default)]
    emulators: Vec<HubDevice>,
}

#[derive(Debug, Deserialize)]
struct HubDevice {
    id: String,
    name: String,
    version: String,
    platform: String,
    booted: bool,
    physical: bool,
}

#[derive(Debug, Deserialize)]
struct HubActionResult {
    ok: bool,
    id: Option<String>,
    serial: Option<String>,
    error: Option<String>,
}

#[derive(Clone)]
pub struct DeviceService {
    inner: Arc<Inner>,
}

struct Inner {
    config_path: PathBuf,
    settings_path: PathBuf,
    hosts: RwLock<BTreeMap<String, Arc<dyn DeviceHostRunner>>>,
    state: RwLock<DeviceServiceState>,
    events: broadcast::Sender<DeviceEvent>,
    frame_sequence: AtomicU64,
    operation: Mutex<()>,
    tool_install: Mutex<()>,
    hub: Mutex<Option<RunningHub>>,
    remote_hubs: Mutex<BTreeMap<String, RemoteHubRuntime>>,
    agents: Mutex<BTreeMap<String, AgentRuntime>>,
}

struct RunningHub {
    port: u16,
    child: tokio::process::Child,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct PersistedHubState {
    pid: u32,
    port: u16,
    entry_path: String,
    version: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct PersistedAgentState {
    entry_path: String,
    version: String,
}

struct RemoteHubRuntime {
    local_port: u16,
    tunnel: ForwardedPort,
}

struct AgentRuntime {
    host_id: String,
    state_dir: PathBuf,
    entry: PathBuf,
    tunnel: Option<ForwardedPort>,
    configured: bool,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
struct PersistedDeviceSettings {
    enabled: bool,
    agent_access_enabled: bool,
    onboarding_completed: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct AgentDaemonState {
    #[serde(rename = "httpPort")]
    http_port: u16,
    token: String,
}

#[derive(Debug, Deserialize)]
struct RemoteHubState {
    #[serde(default)]
    port: Option<u16>,
    #[serde(default)]
    helpers: Option<RemoteHubHelpers>,
    #[serde(default)]
    platforms: Vec<DevicePlatformAvailability>,
    #[serde(default)]
    tools: Option<DeviceToolVersions>,
    #[serde(rename = "hubInstalled", default)]
    hub_installed: bool,
    #[serde(rename = "agentDeviceInstalled", default)]
    agent_device_installed: bool,
}

#[derive(Debug, Deserialize)]
struct RemoteHubHelpers {
    #[serde(rename = "serveSimAxSettings")]
    serve_sim_ax_settings: Option<String>,
    #[serde(rename = "serveSimCli")]
    serve_sim_cli: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct AgentDeviceConfig {
    #[serde(rename = "daemonBaseUrl")]
    daemon_base_url: String,
    #[serde(rename = "daemonAuthToken")]
    daemon_auth_token: String,
}

#[derive(Debug, Clone)]
pub struct AgentDeviceTarget {
    pub command: String,
    pub target_args: Vec<String>,
}

impl DeviceService {
    pub fn new(state_root: PathBuf) -> Arc<Self> {
        let (events, _) = broadcast::channel(128);
        let local: Arc<dyn DeviceHostRunner> = Arc::new(LocalDeviceHost {
            label: "This machine".into(),
            state_root: state_root.clone(),
            android: discover_android_tools(),
        });
        let mut hosts = BTreeMap::new();
        hosts.insert(LOCAL_DEVICE_HOST_ID.into(), local);
        let mut state = DeviceServiceState::default();
        state.hosts = vec![local_summary(&state_root)];
        if let Ok(bytes) = std::fs::read(state_root.join("settings.json"))
            && let Ok(settings) = serde_json::from_slice::<PersistedDeviceSettings>(&bytes)
        {
            state.host_status = if settings.enabled {
                DeviceHostStatus::Idle
            } else {
                DeviceHostStatus::Disabled
            };
            state.agent_access_enabled = settings.agent_access_enabled;
            state.onboarding_completed = settings.onboarding_completed;
        }
        Arc::new(Self {
            inner: Arc::new(Inner {
                config_path: state_root.join("device-hosts.json"),
                settings_path: state_root.join("settings.json"),
                hosts: RwLock::new(hosts),
                state: RwLock::new(state),
                events,
                frame_sequence: AtomicU64::new(0),
                operation: Mutex::new(()),
                tool_install: Mutex::new(()),
                hub: Mutex::new(None),
                remote_hubs: Mutex::new(BTreeMap::new()),
                agents: Mutex::new(BTreeMap::new()),
            }),
        })
    }

    pub async fn state_async(&self) -> DeviceServiceState {
        self.inner.state.read().await.clone()
    }

    /// Starts the pinned agent-device daemon for a host and returns a local
    /// launcher plus a private endpoint config. SSH endpoints are forwarded
    /// through a supervised tunnel, so the provider never receives a remote
    /// shell command or a daemon token in its argv.
    pub async fn agent_device_target(
        &self,
        host_id: &str,
        thread_id: &ThreadId,
        device_id: &str,
    ) -> Result<Option<AgentDeviceTarget>, String> {
        let _guard = self.inner.operation.lock().await;
        let state = self.state_async().await;
        if state.host_status == DeviceHostStatus::Disabled {
            return Err("Device support is off. Enable it in the Device panel first.".into());
        }
        if !state.agent_access_enabled {
            return Err("Agent device access is off. Enable it in the Device panel first.".into());
        }
        let host = self.host(host_id).await?;
        let local_host = self.host(LOCAL_DEVICE_HOST_ID).await?;
        let root = self
            .inner
            .config_path
            .parent()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));

        let reusable = {
            let mut agents = self.inner.agents.lock().await;
            let alive = agents.get_mut(host_id).is_some_and(|runtime| {
                runtime
                    .tunnel
                    .as_mut()
                    .map(|tunnel| tunnel.child.try_wait().ok().flatten().is_none())
                    .unwrap_or(true)
            });
            agents
                .get(host_id)
                .filter(|runtime| runtime.configured && alive)
                .map(|runtime| (runtime.state_dir.clone(), runtime.entry.clone()))
        };
        if let Some((state_dir, entry)) = reusable {
            let healthy = agent_config_healthy(&state_dir).await;
            if healthy {
                let node = node_command();
                let command_path = write_agent_launcher(&root, &entry, &node).await?;
                let config_path = state_dir.join("config.json");
                return Ok(Some(AgentDeviceTarget {
                    command: command_path.to_string_lossy().into_owned(),
                    target_args: vec![
                        "--config".into(),
                        config_path.to_string_lossy().into_owned(),
                        "--session".into(),
                        agent_device_session(thread_id, host_id, device_id),
                    ],
                }));
            }
        }

        let previous = self.inner.agents.lock().await.remove(host_id);
        if let Some(mut previous) = previous {
            if let Some(mut tunnel) = previous.tunnel.take() {
                let _ = tunnel.child.kill().await;
                let _ = tunnel.child.wait().await;
            }
        }

        let _tool_guard = self.inner.tool_install.lock().await;
        let entry = ensure_pinned_tool(
            local_host.as_ref(),
            &root,
            AGENT_PACKAGE,
            AGENT_VERSION,
            AGENT_ENTRY,
        )
        .await?;
        drop(_tool_guard);

        let state_dir = root.join("agent-device").join("hosts").join(host_id);
        let (local_port, token, tunnel) = match host.kind() {
            DeviceHostKind::Local => {
                let daemon = ensure_agent_daemon(local_host.as_ref(), &entry, &state_dir).await?;
                (daemon.http_port, daemon.token, None)
            }
            DeviceHostKind::Ssh => {
                let output = host.lifecycle("agent").await?.ok_or_else(|| {
                    "SSH device host does not support helper lifecycle".to_owned()
                })?;
                if output.code != 0 {
                    return Err(format!(
                        "remote agent-device startup failed: {}",
                        String::from_utf8_lossy(&output.stderr).trim()
                    ));
                }
                let daemon: AgentDaemonState = match serde_json::from_slice(&output.stdout) {
                    Ok(daemon) => daemon,
                    Err(error) => {
                        self.cleanup_agent_activation(host.as_ref(), &entry, &state_dir, None)
                            .await;
                        return Err(format!(
                            "remote agent-device returned invalid state: {error}"
                        ));
                    }
                };
                let tunnel = match host.forward(daemon.http_port).await {
                    Ok(Some(tunnel)) => tunnel,
                    Ok(None) => {
                        self.cleanup_agent_activation(host.as_ref(), &entry, &state_dir, None)
                            .await;
                        return Err("SSH device host did not create an agent tunnel".to_owned());
                    }
                    Err(error) => {
                        self.cleanup_agent_activation(host.as_ref(), &entry, &state_dir, None)
                            .await;
                        return Err(error);
                    }
                };
                (tunnel.local_port, daemon.token, Some(tunnel))
            }
        };
        if host.kind() == DeviceHostKind::Ssh
            && let Some(mut probe) = host.probe()
        {
            probe.agent_device_installed = true;
            host.set_probe(probe);
        }
        let config_path = state_dir.join("config.json");
        if let Err(error) = write_agent_config(&config_path, local_port, &token).await {
            self.cleanup_agent_activation(host.as_ref(), &entry, &state_dir, tunnel)
                .await;
            return Err(error);
        }
        let node = node_command();
        let command_path = match write_agent_launcher(&root, &entry, &node).await {
            Ok(path) => path,
            Err(error) => {
                self.cleanup_agent_activation(host.as_ref(), &entry, &state_dir, tunnel)
                    .await;
                return Err(error);
            }
        };
        self.inner.agents.lock().await.insert(
            host_id.to_owned(),
            AgentRuntime {
                host_id: host_id.to_owned(),
                state_dir,
                entry,
                tunnel,
                configured: true,
            },
        );

        let mut next = self.state_async().await;
        next.hosts = next
            .hosts
            .into_iter()
            .map(|summary| {
                if summary.id == LOCAL_DEVICE_HOST_ID {
                    local_summary(&root)
                } else if summary.id == host_id {
                    host_summary(host.as_ref())
                } else {
                    summary
                }
            })
            .collect();
        self.publish_state(next).await;
        Ok(Some(AgentDeviceTarget {
            command: command_path.to_string_lossy().into_owned(),
            target_args: vec![
                "--config".into(),
                config_path.to_string_lossy().into_owned(),
                "--session".into(),
                agent_device_session(thread_id, host_id, device_id),
            ],
        }))
    }

    async fn cleanup_agent_activation(
        &self,
        host: &dyn DeviceHostRunner,
        entry: &Path,
        state_dir: &Path,
        mut tunnel: Option<ForwardedPort>,
    ) {
        if let Some(mut tunnel) = tunnel.take() {
            let _ = tunnel.child.kill().await;
            let _ = tunnel.child.wait().await;
        }
        match host.kind() {
            DeviceHostKind::Local => {
                let args = vec![
                    entry.to_string_lossy().into_owned(),
                    "daemon".into(),
                    "stop".into(),
                    "--state-dir".into(),
                    state_dir.to_string_lossy().into_owned(),
                ];
                let node = node_command();
                let node = node.to_string_lossy().into_owned();
                let _ = host.run(&node, &args, None).await;
            }
            DeviceHostKind::Ssh => {
                let _ = host.lifecycle("stop-agent").await;
            }
        }
    }

    async fn ensure_remote_hub_running(
        &self,
        host: &Arc<dyn DeviceHostRunner>,
    ) -> Result<(), String> {
        if host.kind() != DeviceHostKind::Ssh {
            return Ok(());
        }
        let stale = {
            let mut hubs = self.inner.remote_hubs.lock().await;
            let live = hubs
                .get_mut(host.id())
                .is_some_and(|runtime| runtime.tunnel.child.try_wait().ok().flatten().is_none());
            if live {
                return Ok(());
            }
            hubs.remove(host.id())
        };
        if let Some(mut stale) = stale {
            let _ = stale.tunnel.child.kill().await;
            let _ = stale.tunnel.child.wait().await;
        }
        let output = host
            .lifecycle("hub")
            .await?
            .ok_or_else(|| "SSH device host does not support helper lifecycle".to_owned())?;
        if output.code != 0 {
            return Err(format!(
                "remote device hub startup failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        let remote = serde_json::from_slice::<RemoteHubState>(&output.stdout)
            .map_err(|error| format!("remote device hub returned invalid state: {error}"))?;
        if let Some(helpers) = remote.helpers {
            host.set_helper_paths(SshHelperPaths {
                serve_sim_ax_settings: helpers.serve_sim_ax_settings,
                serve_sim_cli: helpers.serve_sim_cli,
            });
        }
        host.set_probe(RemoteHostProbe {
            platforms: remote.platforms,
            tools: remote.tools,
            hub_installed: remote.hub_installed,
            agent_device_installed: remote.agent_device_installed,
        });
        let remote_port = remote
            .port
            .ok_or_else(|| "remote device hub did not report a port".to_owned())?;
        let mut tunnel = host
            .forward(remote_port)
            .await?
            .ok_or_else(|| "SSH device host did not create a device hub tunnel".to_owned())?;
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(15);
        loop {
            if loopback_http_ok(tunnel.local_port, "/readyz").await {
                break;
            }
            let tunnel_status = match tunnel.child.try_wait() {
                Ok(status) => status,
                Err(error) => {
                    let _ = tunnel.child.kill().await;
                    let _ = tunnel.child.wait().await;
                    return Err(format!("device hub tunnel failed: {error}"));
                }
            };
            if let Some(status) = tunnel_status {
                let _ = tunnel.child.kill().await;
                let _ = tunnel.child.wait().await;
                return Err(format!(
                    "device hub tunnel exited before becoming ready ({status})"
                ));
            }
            if tokio::time::Instant::now() >= deadline {
                let _ = tunnel.child.kill().await;
                let _ = tunnel.child.wait().await;
                return Err("forwarded device hub did not become ready within 15 seconds".into());
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        let mut hubs = self.inner.remote_hubs.lock().await;
        let duplicate = hubs
            .get_mut(host.id())
            .is_some_and(|current| current.tunnel.child.try_wait().ok().flatten().is_none());
        if duplicate {
            drop(hubs);
            let mut tunnel = tunnel;
            let _ = tunnel.child.kill().await;
            let _ = tunnel.child.wait().await;
            return Ok(());
        }
        hubs.insert(
            host.id().to_owned(),
            RemoteHubRuntime {
                local_port: tunnel.local_port,
                tunnel,
            },
        );
        Ok(())
    }

    async fn ensure_hub_tool(
        &self,
        host: &Arc<dyn DeviceHostRunner>,
        start: bool,
    ) -> Result<(), String> {
        if host.kind() != DeviceHostKind::Local {
            let probe = host
                .lifecycle("probe")
                .await?
                .ok_or_else(|| "SSH device host does not support helper inspection".to_owned())?;
            if probe.code != 0 {
                return Err(format!(
                    "remote device host inspection failed: {}",
                    String::from_utf8_lossy(&probe.stderr).trim()
                ));
            }
            if let Ok(probe) = serde_json::from_slice::<RemoteHostProbe>(&probe.stdout) {
                host.set_probe(probe);
            }
            if start {
                self.ensure_remote_hub_running(host).await?;
                return Ok(());
            }
            let output = host
                .lifecycle("hub-install")
                .await?
                .ok_or_else(|| "SSH device host does not support helper lifecycle".to_owned())?;
            if output.code != 0 {
                return Err(format!(
                    "remote device hub installation failed: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                ));
            }
            if let Ok(remote) = serde_json::from_slice::<RemoteHubState>(&output.stdout) {
                if let Some(helpers) = remote.helpers {
                    host.set_helper_paths(SshHelperPaths {
                        serve_sim_ax_settings: helpers.serve_sim_ax_settings,
                        serve_sim_cli: helpers.serve_sim_cli,
                    });
                }
                host.set_probe(RemoteHostProbe {
                    platforms: remote.platforms,
                    tools: remote.tools,
                    hub_installed: remote.hub_installed,
                    agent_device_installed: remote.agent_device_installed,
                });
            }
            return Ok(());
        }
        let _guard = self.inner.tool_install.lock().await;
        let root = self
            .inner
            .config_path
            .parent()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        let _ =
            ensure_pinned_tool(host.as_ref(), &root, HUB_PACKAGE, HUB_VERSION, HUB_ENTRY).await?;
        Ok(())
    }

    async fn ensure_agent_tool(
        &self,
        host: &Arc<dyn DeviceHostRunner>,
        start: bool,
    ) -> Result<(), String> {
        if host.kind() == DeviceHostKind::Ssh {
            let output = host
                .lifecycle(if start { "agent" } else { "agent-install" })
                .await?
                .ok_or_else(|| "SSH device host does not support helper lifecycle".to_owned())?;
            if output.code != 0 {
                return Err(format!(
                    "remote agent-device startup failed: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                ));
            }
            if start {
                // `agent` returns only the daemon endpoint. Preserve the
                // inventory/tool probe collected before startup and mark the
                // pinned agent as running instead of decoding the endpoint
                // through a probe shape with defaulted fields.
                if let Some(mut probe) = host.probe() {
                    probe.agent_device_installed = true;
                    host.set_probe(probe);
                }
            } else if let Ok(probe) = serde_json::from_slice::<RemoteHostProbe>(&output.stdout) {
                host.set_probe(probe);
            }
            return Ok(());
        }
        let _guard = self.inner.tool_install.lock().await;
        let root = self
            .inner
            .config_path
            .parent()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        let _ = ensure_pinned_tool(
            host.as_ref(),
            &root,
            AGENT_PACKAGE,
            AGENT_VERSION,
            AGENT_ENTRY,
        )
        .await?;
        Ok(())
    }

    /// Consent for agent access starts the pinned daemon for every supported
    /// host. Device targets later add the private config and per-thread
    /// session, while this readiness step makes the host state match the
    /// reference service immediately after the separate permission is granted.
    async fn ensure_agent_running(&self, host: &Arc<dyn DeviceHostRunner>) -> Result<(), String> {
        let root = self
            .inner
            .config_path
            .parent()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        let existing = self
            .inner
            .agents
            .lock()
            .await
            .get(host.id())
            .map(|runtime| (runtime.state_dir.clone(), runtime.entry.clone()));
        if let Some((state_dir, mut entry)) = existing {
            self.ensure_hub_tool(host, true).await?;
            if host.kind() == DeviceHostKind::Local {
                self.ensure_hub_running(host).await?;
                if entry.as_os_str().is_empty() {
                    let _guard = self.inner.tool_install.lock().await;
                    entry = ensure_pinned_tool(
                        host.as_ref(),
                        &root,
                        AGENT_PACKAGE,
                        AGENT_VERSION,
                        AGENT_ENTRY,
                    )
                    .await?;
                }
                let _ = ensure_agent_daemon(host.as_ref(), &entry, &state_dir).await?;
            } else {
                self.ensure_agent_tool(host, true).await?;
                if let Some(mut probe) = host.probe() {
                    probe.agent_device_installed = true;
                    host.set_probe(probe);
                }
            }
            let mut state = self.state_async().await;
            state.hosts = state
                .hosts
                .into_iter()
                .map(|summary| {
                    if summary.id == host.id() {
                        if host.kind() == DeviceHostKind::Local {
                            local_summary(&root)
                        } else {
                            host_summary(host.as_ref())
                        }
                    } else {
                        summary
                    }
                })
                .collect();
            self.publish_state(state).await;
            return Ok(());
        }
        self.ensure_hub_tool(host, true).await?;
        if host.kind() == DeviceHostKind::Local {
            self.ensure_hub_running(host).await?;
            let _guard = self.inner.tool_install.lock().await;
            let entry = ensure_pinned_tool(
                host.as_ref(),
                &root,
                AGENT_PACKAGE,
                AGENT_VERSION,
                AGENT_ENTRY,
            )
            .await?;
            drop(_guard);
            let state_dir = root.join("agent-device").join("hosts").join(host.id());
            let _ = ensure_agent_daemon(host.as_ref(), &entry, &state_dir).await?;
            self.inner.agents.lock().await.insert(
                host.id().to_owned(),
                AgentRuntime {
                    host_id: host.id().to_owned(),
                    state_dir,
                    entry,
                    tunnel: None,
                    configured: false,
                },
            );
        } else {
            self.ensure_agent_tool(host, true).await?;
            if let Some(mut probe) = host.probe() {
                probe.agent_device_installed = true;
                host.set_probe(probe);
            }
            self.inner.agents.lock().await.insert(
                host.id().to_owned(),
                AgentRuntime {
                    host_id: host.id().to_owned(),
                    state_dir: root.join("agent-device").join("hosts").join(host.id()),
                    entry: PathBuf::new(),
                    tunnel: None,
                    configured: false,
                },
            );
        }
        let mut state = self.state_async().await;
        state.hosts = state
            .hosts
            .into_iter()
            .map(|summary| {
                if summary.id == host.id() {
                    if host.kind() == DeviceHostKind::Local {
                        local_summary(&root)
                    } else {
                        host_summary(host.as_ref())
                    }
                } else {
                    summary
                }
            })
            .collect();
        self.publish_state(state).await;
        Ok(())
    }

    async fn ensure_hub_running(&self, host: &Arc<dyn DeviceHostRunner>) -> Result<(), String> {
        if host.kind() != DeviceHostKind::Local {
            return self.ensure_remote_hub_running(host).await;
        }
        let root = self
            .inner
            .config_path
            .parent()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        let entry = tool_entry(&root, HUB_PACKAGE, HUB_VERSION, HUB_ENTRY);
        let persisted_path = root.join("hub.json");
        let mut running = self.inner.hub.lock().await;
        if let Some(current) = running.as_mut() {
            if current
                .child
                .try_wait()
                .map_err(|error| error.to_string())?
                .is_none()
            {
                return Ok(());
            }
            *running = None;
        }
        reap_stale_hub(&persisted_path, host.as_ref()).await;
        for attempt in 0..5 {
            let listener = TcpListener::bind(("127.0.0.1", 0))
                .await
                .map_err(|error| format!("could not reserve the device hub port: {error}"))?;
            let port = listener
                .local_addr()
                .map_err(|error| error.to_string())?
                .port();
            drop(listener);
            let node = node_command();
            let mut child = Command::new(&node);
            child
                .arg(&entry)
                .args([
                    "--port".to_owned(),
                    port.to_string(),
                    "--host".to_owned(),
                    "127.0.0.1".to_owned(),
                    "--hide-sidebar".to_owned(),
                    "--hide-boot-device".to_owned(),
                ])
                .current_dir(&root)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
            host.configure_process(&mut child);
            let mut child = child
                .spawn()
                .map_err(|error| format!("could not start the device hub: {error}"))?;
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
            loop {
                if loopback_http_ok(port, "/readyz").await {
                    let pid = child.id().unwrap_or_default();
                    let state = PersistedHubState {
                        pid,
                        port,
                        entry_path: entry.to_string_lossy().into_owned(),
                        version: HUB_VERSION.into(),
                    };
                    let _ = write_private_json(&persisted_path, &state).await;
                    *running = Some(RunningHub { port, child });
                    return Ok(());
                }
                if let Some(status) = child.try_wait().map_err(|error| error.to_string())? {
                    if attempt == 4 {
                        return Err(format!(
                            "device hub exited before becoming ready ({status})"
                        ));
                    }
                    break;
                }
                if tokio::time::Instant::now() >= deadline {
                    let _ = child.kill().await;
                    let _ = child.wait().await;
                    if attempt == 4 {
                        return Err("device hub did not become ready within 30 seconds".into());
                    }
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        }
        Err("device hub did not become ready".into())
    }

    pub async fn shutdown_owned(&self) {
        let root = self
            .inner
            .config_path
            .parent()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        let mut running = self.inner.hub.lock().await;
        if let Some(mut hub) = running.take() {
            let _ = hub.child.kill().await;
            let _ = hub.child.wait().await;
        }
        let _ = tokio::fs::remove_file(root.join("hub.json")).await;
        drop(running);
        self.shutdown_agents().await;
        let remote_hubs = {
            let mut hubs = self.inner.remote_hubs.lock().await;
            std::mem::take(&mut *hubs)
        };
        for (_, mut hub) in remote_hubs {
            let _ = hub.tunnel.child.kill().await;
            let _ = hub.tunnel.child.wait().await;
        }
        let hosts = self
            .inner
            .hosts
            .read()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for host in hosts {
            if host.kind() == DeviceHostKind::Ssh {
                let _ = host.lifecycle("stop").await;
            }
        }
    }

    pub async fn shutdown_agents(&self) {
        let agents = {
            let mut running = self.inner.agents.lock().await;
            std::mem::take(&mut *running)
        };
        for (_, mut agent) in agents {
            if let Some(mut tunnel) = agent.tunnel.take() {
                let _ = tunnel.child.kill().await;
                let _ = tunnel.child.wait().await;
            }
            if let Ok(host) = self.host(&agent.host_id).await {
                match host.kind() {
                    DeviceHostKind::Local => {
                        let args = vec![
                            agent.entry.to_string_lossy().into_owned(),
                            "daemon".into(),
                            "stop".into(),
                            "--state-dir".into(),
                            agent.state_dir.to_string_lossy().into_owned(),
                        ];
                        let node = node_command();
                        let node = node.to_string_lossy().into_owned();
                        let _ = host.run(&node, &args, None).await;
                    }
                    DeviceHostKind::Ssh => {
                        let _ = host.lifecycle("stop-agent").await;
                    }
                }
            }
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<DeviceEvent> {
        self.inner.events.subscribe()
    }

    async fn publish_state(&self, mut state: DeviceServiceState) -> DeviceServiceState {
        let mut current = self.inner.state.write().await;
        state.revision = current.revision.saturating_add(1);
        *current = state.clone();
        drop(current);
        let _ = self.inner.events.send(DeviceEvent::State(state.clone()));
        state
    }

    async fn persist_settings(&self, state: &DeviceServiceState) -> Result<(), String> {
        let settings = PersistedDeviceSettings {
            enabled: state.host_status != DeviceHostStatus::Disabled,
            agent_access_enabled: state.agent_access_enabled,
            onboarding_completed: state.onboarding_completed,
        };
        let bytes = serde_json::to_vec(&settings).map_err(|error| error.to_string())?;
        if let Some(parent) = self.inner.settings_path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|error| error.to_string())?;
        }
        let temporary = self.inner.settings_path.with_extension("json.tmp");
        tokio::fs::write(&temporary, bytes)
            .await
            .map_err(|error| error.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = tokio::fs::metadata(&temporary)
                .await
                .map_err(|error| error.to_string())?
                .permissions();
            permissions.set_mode(0o600);
            tokio::fs::set_permissions(&temporary, permissions)
                .await
                .map_err(|error| error.to_string())?;
        }
        #[cfg(windows)]
        let _ = tokio::fs::remove_file(&self.inner.settings_path).await;
        if let Err(error) = tokio::fs::rename(&temporary, &self.inner.settings_path).await {
            let _ = tokio::fs::remove_file(&temporary).await;
            return Err(error.to_string());
        }
        Ok(())
    }

    async fn load_hosts(&self) {
        let Ok(bytes) = tokio::fs::read(&self.inner.config_path).await else {
            return;
        };
        let Ok(configs) = serde_json::from_slice::<Vec<DeviceHostConfig>>(&bytes) else {
            return;
        };
        let configs = configs
            .into_iter()
            .map(DeviceHostConfig::normalized)
            .collect::<Vec<_>>();
        if configs.iter().any(|config| config.validate().is_err())
            || configs
                .iter()
                .map(|config| &config.id)
                .collect::<BTreeSet<_>>()
                .len()
                != configs.len()
        {
            return;
        }
        let owner_root = self
            .inner
            .config_path
            .parent()
            .unwrap_or_else(|| Path::new("."));
        let mut hosts = self.inner.hosts.write().await;
        let ids = configs
            .iter()
            .map(|config| config.id.as_str())
            .collect::<BTreeSet<_>>();
        hosts.retain(|id, _| id == LOCAL_DEVICE_HOST_ID || ids.contains(id.as_str()));
        for config in configs {
            let retain = hosts
                .get(&config.id)
                .is_some_and(|host| host.config_matches(&config));
            if !retain {
                hosts.insert(
                    config.id.clone(),
                    Arc::new(SshDeviceHost {
                        owner: device_host_owner(owner_root, &config.id),
                        config,
                        helpers: std::sync::RwLock::new(None),
                        probe: std::sync::RwLock::new(None),
                        probe_error: std::sync::RwLock::new(None),
                    }),
                );
            }
        }
        let mut state = self.inner.state.write().await;
        let root = self
            .inner
            .config_path
            .parent()
            .unwrap_or_else(|| Path::new("."));
        let local = local_summary(root);
        state.hosts = std::iter::once(local)
            .chain(
                hosts
                    .values()
                    .filter(|host| host.id() != LOCAL_DEVICE_HOST_ID)
                    .map(|host| host_summary(host.as_ref())),
            )
            .collect();
    }

    pub async fn configure(
        &self,
        input: DeviceConfigureInput,
    ) -> Result<DeviceServiceState, String> {
        let refresh = input.enabled == Some(true);
        let disable_support = input.enabled == Some(false);
        let disable_agent = input.agent_access_enabled == Some(false);
        let guard = self.inner.operation.lock().await;
        let mut state = self.inner.state.read().await.clone();
        if let Some(enabled) = input.enabled {
            state.host_status = if enabled {
                DeviceHostStatus::Idle
            } else {
                DeviceHostStatus::Disabled
            };
            state.host_status_detail = None;
            state.host_statuses.clear();
            if !enabled {
                state.devices.clear();
                state.sessions.clear();
                state.booting_devices.clear();
            }
        }
        if let Some(enabled) = input.agent_access_enabled {
            state.agent_access_enabled = enabled;
        }
        if let Some(completed) = input.onboarding_completed {
            state.onboarding_completed = completed;
        }
        self.persist_settings(&state).await?;
        let start_agents = state.host_status != DeviceHostStatus::Disabled
            && state.agent_access_enabled
            && input.agent_access_enabled == Some(true);
        let published = self.publish_state(state).await;
        drop(guard);
        if disable_support {
            self.shutdown_owned().await;
        } else if disable_agent {
            self.shutdown_agents().await;
        }
        if start_agents {
            let hosts = self
                .inner
                .hosts
                .read()
                .await
                .values()
                .cloned()
                .collect::<Vec<_>>();
            for host in hosts {
                if host.kind() == DeviceHostKind::Local {
                    let root = self
                        .inner
                        .config_path
                        .parent()
                        .map(PathBuf::from)
                        .unwrap_or_else(|| PathBuf::from("."));
                    let summary = local_summary(&root);
                    if !summary.platforms.iter().any(|platform| platform.available) {
                        continue;
                    }
                }
                if let Err(error) = self.ensure_agent_running(&host).await {
                    let mut failed = self.state_async().await;
                    failed.host_status = DeviceHostStatus::Failed;
                    failed.host_status_detail = Some(error.clone());
                    failed.host_statuses.insert(
                        host.id().into(),
                        DeviceHostStatusRecord {
                            status: DeviceHostStatus::Failed,
                            detail: Some(error.clone()),
                        },
                    );
                    self.publish_state(failed).await;
                    return Err(error);
                }
            }
        }
        if refresh || start_agents {
            self.list(DeviceListInput::default()).await
        } else {
            Ok(published)
        }
    }

    pub async fn update_hosts(
        &self,
        input: DeviceHostsInput,
    ) -> Result<DeviceServiceState, String> {
        let input = DeviceHostsInput {
            hosts: input
                .hosts
                .into_iter()
                .map(DeviceHostConfig::normalized)
                .collect(),
        };
        input
            .validate()
            .map_err(|error| format!("invalid device host configuration: {error}"))?;
        let _guard = self.inner.operation.lock().await;
        self.shutdown_owned().await;
        let bytes = serde_json::to_vec_pretty(&input.hosts).map_err(|error| error.to_string())?;
        if let Some(parent) = self.inner.config_path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|error| error.to_string())?;
        }
        let temporary = self.inner.config_path.with_extension("json.tmp");
        tokio::fs::write(&temporary, bytes)
            .await
            .map_err(|error| error.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = tokio::fs::metadata(&temporary)
                .await
                .map_err(|error| error.to_string())?
                .permissions();
            permissions.set_mode(0o600);
            tokio::fs::set_permissions(&temporary, permissions)
                .await
                .map_err(|error| error.to_string())?;
        }
        #[cfg(windows)]
        let _ = tokio::fs::remove_file(&self.inner.config_path).await;
        if let Err(error) = tokio::fs::rename(&temporary, &self.inner.config_path).await {
            let _ = tokio::fs::remove_file(&temporary).await;
            return Err(error.to_string());
        }
        self.load_hosts().await;
        let configured = input
            .hosts
            .iter()
            .map(|host| host.id.as_str())
            .collect::<BTreeSet<_>>();
        let mut state = self.state_async().await;
        state.devices.retain(|device| {
            device.host_id == LOCAL_DEVICE_HOST_ID || configured.contains(device.host_id.as_str())
        });
        state.sessions.retain(|session| {
            session.host_id == LOCAL_DEVICE_HOST_ID || configured.contains(session.host_id.as_str())
        });
        state.booting_devices.retain(|booting| {
            booting.device.host_id == LOCAL_DEVICE_HOST_ID
                || configured.contains(booting.device.host_id.as_str())
        });
        state
            .host_statuses
            .retain(|id, _| id == LOCAL_DEVICE_HOST_ID || configured.contains(id.as_str()));
        Ok(self.publish_state(state).await)
    }

    pub async fn list(&self, input: DeviceListInput) -> Result<DeviceServiceState, String> {
        let _guard = self.inner.operation.lock().await;
        self.list_inner(input).await
    }

    async fn list_inner(&self, input: DeviceListInput) -> Result<DeviceServiceState, String> {
        let DeviceListInput {
            update_tool,
            inspect_only,
            retry_host_id,
        } = input;
        self.load_hosts().await;
        let current = self.inner.state.read().await.clone();
        if current.host_status == DeviceHostStatus::Disabled
            && !inspect_only
            && update_tool.is_none()
        {
            return Ok(current);
        }
        if inspect_only {
            let root = self
                .inner
                .config_path
                .parent()
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."));
            let mut inspected = current;
            let hosts = self
                .inner
                .hosts
                .read()
                .await
                .values()
                .cloned()
                .collect::<Vec<_>>();
            for host in &hosts {
                if host.kind() != DeviceHostKind::Ssh {
                    continue;
                }
                match host.lifecycle("probe").await {
                    Ok(Some(output)) if output.code == 0 => {
                        match serde_json::from_slice::<RemoteHostProbe>(&output.stdout) {
                            Ok(probe) => host.set_probe(probe),
                            Err(error) => host.set_probe_error(Some(format!(
                                "Cannot check versions. The host returned invalid inspection data: {error}"
                            ))),
                        }
                    }
                    Ok(Some(output)) => host.set_probe_error(Some(format!(
                        "Cannot check versions. Reconnect the host and check again: {}",
                        String::from_utf8_lossy(&output.stderr).trim()
                    ))),
                    Ok(None) => host.set_probe_error(Some(
                        "Cannot check versions. This host does not support inspection.".into(),
                    )),
                    Err(error) => host.set_probe_error(Some(format!(
                        "Cannot check versions. Reconnect the host and check again: {error}"
                    ))),
                }
            }
            inspected.hosts = hosts
                .iter()
                .map(|host| {
                    if host.kind() == DeviceHostKind::Local {
                        local_summary(&root)
                    } else {
                        host_summary(host.as_ref())
                    }
                })
                .collect();
            return Ok(self.publish_state(inspected).await);
        }
        let tool_update = update_tool.is_some();
        let ids: Vec<String> = retry_host_id.clone().into_iter().collect();
        if let Some(id) = ids.first()
            && !self.inner.hosts.read().await.contains_key(id)
        {
            return Err(format!("unknown device host {id}"));
        }
        if !tool_update {
            let mut starting = current.clone();
            starting.host_status = DeviceHostStatus::Starting;
            self.publish_state(starting).await;
        }
        let hosts: Vec<Arc<dyn DeviceHostRunner>> = {
            let map = self.inner.hosts.read().await;
            if ids.is_empty() {
                map.values().cloned().collect()
            } else {
                ids.iter().filter_map(|id| map.get(id).cloned()).collect()
            }
        };
        let mut devices = Vec::new();
        let mut summaries = Vec::new();
        let mut statuses = BTreeMap::new();
        for host in hosts {
            let mut summary = if host.kind() == DeviceHostKind::Local {
                let root = self
                    .inner
                    .config_path
                    .parent()
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from("."));
                local_summary(&root)
            } else {
                host_summary(host.as_ref())
            };
            // Match the reference service's readinessIfSupported behavior:
            // the local host can still be shown with actionable reasons when
            // no simulator toolchain exists, but listing must not install or
            // start the pinned helpers in that state.
            if host.kind() == DeviceHostKind::Local
                && update_tool.is_none()
                && !summary.platforms.iter().any(|platform| platform.available)
            {
                let detail = summary
                    .platforms
                    .iter()
                    .filter_map(|platform| platform.reason.clone())
                    .collect::<Vec<_>>()
                    .join(" ");
                statuses.insert(
                    host.id().into(),
                    DeviceHostStatusRecord {
                        status: DeviceHostStatus::Idle,
                        detail: (!detail.is_empty()).then_some(detail),
                    },
                );
                summaries.push(summary);
                continue;
            }
            let mut host_ready = false;
            if retry_host_id.is_some() && update_tool.is_none() && current.agent_access_enabled {
                if let Err(error) = self.ensure_agent_running(&host).await {
                    statuses.insert(
                        host.id().into(),
                        DeviceHostStatusRecord {
                            status: DeviceHostStatus::Failed,
                            detail: Some(error),
                        },
                    );
                    summaries.push(summary);
                    continue;
                }
                summary = if host.kind() == DeviceHostKind::Local {
                    let root = self
                        .inner
                        .config_path
                        .parent()
                        .map(PathBuf::from)
                        .unwrap_or_else(|| PathBuf::from("."));
                    local_summary(&root)
                } else {
                    host_summary(host.as_ref())
                };
                host_ready = true;
            }
            if !host_ready && update_tool != Some(agent_protocol::device::DeviceTool::Agent) {
                if let Err(error) = self.ensure_hub_tool(&host, !tool_update).await {
                    statuses.insert(
                        host.id().into(),
                        DeviceHostStatusRecord {
                            status: DeviceHostStatus::Failed,
                            detail: Some(error),
                        },
                    );
                    summaries.push(summary);
                    continue;
                }
            }
            if host.kind() == DeviceHostKind::Local
                && update_tool != Some(agent_protocol::device::DeviceTool::Agent)
            {
                let root = self
                    .inner
                    .config_path
                    .parent()
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from("."));
                summary = local_summary(&root);
            } else if host.kind() == DeviceHostKind::Ssh
                && update_tool != Some(agent_protocol::device::DeviceTool::Agent)
            {
                summary = host_summary(host.as_ref());
            }
            if update_tool == Some(agent_protocol::device::DeviceTool::Agent) {
                if let Err(error) = self.ensure_agent_tool(&host, false).await {
                    statuses.insert(
                        host.id().into(),
                        DeviceHostStatusRecord {
                            status: DeviceHostStatus::Failed,
                            detail: Some(error),
                        },
                    );
                    summaries.push(summary);
                    continue;
                }
                if host.kind() == DeviceHostKind::Ssh {
                    if let Some(mut probe) = host.probe() {
                        probe.agent_device_installed = true;
                        host.set_probe(probe);
                        summary = host_summary(host.as_ref());
                    }
                }
                summary.agent_device_installed = true;
            }
            if tool_update {
                summaries.push(summary);
                continue;
            }
            if let Err(error) = self.ensure_hub_running(&host).await {
                statuses.insert(
                    host.id().into(),
                    DeviceHostStatusRecord {
                        status: DeviceHostStatus::Failed,
                        detail: Some(error),
                    },
                );
                summaries.push(summary);
                continue;
            }
            let hub_port = self.hub_port(host.id()).await;
            match discover_host(host.clone(), hub_port).await {
                Ok(found) => {
                    devices.extend(found);
                    statuses.insert(
                        host.id().into(),
                        DeviceHostStatusRecord {
                            status: DeviceHostStatus::Ready,
                            detail: None,
                        },
                    );
                }
                Err(error) => {
                    statuses.insert(
                        host.id().into(),
                        DeviceHostStatusRecord {
                            status: DeviceHostStatus::Failed,
                            detail: Some(error.clone()),
                        },
                    );
                }
            }
            summaries.push(summary);
        }
        let mut state = self.inner.state.read().await.clone();
        state.hosts = if ids.is_empty() {
            summaries
        } else {
            state
                .hosts
                .into_iter()
                .filter(|host| !ids.contains(&host.id))
                .chain(summaries)
                .collect()
        };
        state.devices = if ids.is_empty() {
            devices
        } else {
            state
                .devices
                .into_iter()
                .filter(|device| !ids.contains(&device.host_id))
                .chain(devices)
                .collect()
        };
        if ids.is_empty() {
            state.host_statuses = statuses;
        } else {
            state.host_statuses.retain(|id, _| !ids.contains(id));
            state.host_statuses.extend(statuses);
        }
        let failure = state.host_statuses.values().find_map(|status| {
            (status.status == DeviceHostStatus::Failed)
                .then(|| status.detail.clone())
                .flatten()
        });
        let local_status = state.host_statuses.get(LOCAL_DEVICE_HOST_ID).cloned();
        state.host_status = if tool_update {
            current.host_status
        } else if failure.is_some() {
            DeviceHostStatus::Failed
        } else if let Some(status) = local_status.as_ref() {
            status.status
        } else {
            DeviceHostStatus::Ready
        };
        state.host_status_detail =
            failure.or_else(|| local_status.and_then(|status| status.detail));
        Ok(self.publish_state(state).await)
    }

    pub async fn open(&self, mut input: DeviceOpenInput) -> Result<DeviceSession, String> {
        let _guard = self.inner.operation.lock().await;
        let host_id = input
            .host_id
            .take()
            .unwrap_or_else(|| LOCAL_DEVICE_HOST_ID.into());
        let host = self.host(&host_id).await?;
        // Refresh before every open so an AVD that booted or was removed
        // outside this process cannot leave a stale session in the panel.
        self.list_inner(DeviceListInput {
            retry_host_id: Some(host_id.clone()),
            ..Default::default()
        })
        .await?;
        let mut state = self.inner.state.read().await.clone();
        let mut device = state
            .devices
            .iter()
            .find(|device| device.host_id == host_id && device.id == input.device_id)
            .cloned()
            .ok_or_else(|| format!("device {} was not found on host {host_id}", input.device_id))?;
        if device.platform != input.platform {
            return Err("device platform does not match the requested platform".into());
        }
        if !device.booted && input.boot {
            let requested_id = device.id.clone();
            let requested_name = device.name.clone();
            let booting = agent_protocol::device::BootingDevice {
                device: device.clone(),
                thread_id: input.thread_id.clone(),
            };
            state.booting_devices.retain(|entry| {
                entry.device.host_id != booting.device.host_id
                    || entry.device.id != booting.device.id
            });
            state.booting_devices.push(booting);
            self.publish_state(state.clone()).await;
            let mut booted_through_hub = false;
            if let Some(port) = self.hub_port(&host_id).await
                && let Ok(result) = hub_action(
                    port,
                    "/api/devices/boot",
                    serde_json::json!({
                        "platform": match device.platform { DevicePlatform::Ios => "ios", DevicePlatform::Android => "android" },
                        "id": device.id.clone(),
                        "name": device.name.clone(),
                    }),
                )
                .await
                && result.ok
            {
                booted_through_hub = true;
                if device.platform == DevicePlatform::Android {
                    device.id = result
                        .serial
                        .or(result.id)
                        .unwrap_or_else(|| device.id.clone());
                } else {
                    let attached = match hub_action(
                        port,
                        "/vendor/serve-sim/grid/api/start",
                        serde_json::json!({ "udid": device.id }),
                    )
                    .await {
                        Ok(attached) => attached,
                        Err(error) => {
                            self.clear_booting(&input.thread_id, &host_id, &input.device_id).await;
                            return Err(error);
                        }
                    };
                    if !attached.ok {
                        self.clear_booting(&input.thread_id, &host_id, &input.device_id).await;
                        return Err(attached
                            .error
                            .unwrap_or_else(|| "device hub could not attach the iOS stream".into()));
                    }
                }
            }
            if !booted_through_hub {
                let device_command = match device.platform {
                    DevicePlatform::Ios => command("xcrun", argv!["simctl", "boot", device.id]),
                    DevicePlatform::Android => command("emulator", argv!["-avd", device.id]),
                };
                if device.platform == DevicePlatform::Android {
                    if let Err(error) = host
                        .start(&device_command.program, &device_command.args)
                        .await
                    {
                        let mut failed = self.inner.state.read().await.clone();
                        failed.booting_devices.retain(|entry| {
                            entry.thread_id != input.thread_id
                                || entry.device.host_id != host_id
                                || entry.device.id != input.device_id
                        });
                        self.publish_state(failed).await;
                        return Err(error);
                    }
                    let avd_name = device.name.clone();
                    let serial = match wait_for_android_emulator(host.as_ref(), &avd_name).await {
                        Ok(serial) => serial,
                        Err(error) => {
                            stop_android_emulator(host.as_ref(), &avd_name).await;
                            let mut failed = self.inner.state.read().await.clone();
                            failed.booting_devices.retain(|entry| {
                                entry.thread_id != input.thread_id
                                    || entry.device.host_id != host_id
                                    || entry.device.id != input.device_id
                            });
                            self.publish_state(failed).await;
                            return Err(error);
                        }
                    };
                    device.id = serial;
                } else {
                    if let Err(error) =
                        run_device_command(host.as_ref(), &device_command, None).await
                    {
                        let mut failed = self.inner.state.read().await.clone();
                        failed.booting_devices.retain(|entry| {
                            entry.thread_id != input.thread_id
                                || entry.device.host_id != host_id
                                || entry.device.id != input.device_id
                        });
                        self.publish_state(failed).await;
                        return Err(error);
                    }
                    if let Err(error) = wait_for_ios_boot(host.as_ref(), &device.id).await {
                        let _ = run_device_command(
                            host.as_ref(),
                            &command("xcrun", argv!["simctl", "shutdown", device.id.clone()]),
                            None,
                        )
                        .await;
                        let mut failed = self.inner.state.read().await.clone();
                        failed.booting_devices.retain(|entry| {
                            entry.thread_id != input.thread_id
                                || entry.device.host_id != host_id
                                || entry.device.id != input.device_id
                        });
                        self.publish_state(failed).await;
                        return Err(error);
                    }
                }
            }
            // Android exposes an AVD name before boot and an emulator serial
            // after boot. Refresh the hub inventory before publishing the
            // session so callers never retain the stale pre-boot id.
            let refreshed = match self
                .list_inner(DeviceListInput {
                    retry_host_id: Some(host_id.clone()),
                    ..Default::default()
                })
                .await
            {
                Ok(refreshed) => refreshed,
                Err(error) => {
                    self.clear_booting(&input.thread_id, &host_id, &input.device_id)
                        .await;
                    return Err(error);
                }
            };
            if let Some(authoritative) = refreshed.devices.into_iter().find(|candidate| {
                candidate.host_id == host_id
                    && candidate.platform == device.platform
                    && candidate.booted
                    && (candidate.id == device.id
                        || candidate.id == requested_id
                        || candidate.name == requested_name)
            }) {
                device = authoritative;
            }
            device.booted = true;
            state = self.inner.state.read().await.clone();
            state.devices.retain(|candidate| {
                !(candidate.host_id == host_id
                    && (candidate.id == input.device_id || candidate.id == device.id))
            });
            state.devices.push(device.clone());
            state.booting_devices.retain(|entry| {
                entry.thread_id != input.thread_id
                    || entry.device.host_id != host_id
                    || entry.device.id != input.device_id
            });
        } else if device.platform == DevicePlatform::Ios && device.booted {
            // An iOS simulator booted outside this service has no serve-sim
            // capture session yet. Attach the helper during open just like
            // the reference service does after an already-booted refresh.
            if let Some(port) = self.hub_port(&host_id).await {
                let attached = hub_action(
                    port,
                    "/vendor/serve-sim/grid/api/start",
                    serde_json::json!({ "udid": device.id }),
                )
                .await?;
                if !attached.ok {
                    return Err(attached
                        .error
                        .unwrap_or_else(|| "device hub could not attach the iOS stream".into()));
                }
            }
        }
        let session = DeviceSession {
            thread_id: input.thread_id,
            host_id: host_id.clone(),
            device_id: device.id.clone(),
            platform: device.platform,
            opened_at: now_iso(),
        };
        state.sessions.retain(|existing| {
            !(existing.thread_id == session.thread_id
                && existing.host_id == session.host_id
                && existing.device_id == session.device_id)
        });
        state.sessions.push(session.clone());
        self.publish_state(state).await;
        Ok(session)
    }

    async fn clear_booting(&self, thread_id: &ThreadId, host_id: &str, device_id: &str) {
        let mut state = self.inner.state.read().await.clone();
        state.booting_devices.retain(|entry| {
            &entry.thread_id != thread_id
                || entry.device.host_id != host_id
                || entry.device.id != device_id
        });
        self.publish_state(state).await;
    }

    pub async fn close(
        &self,
        input: agent_protocol::device::DeviceCloseInput,
    ) -> Result<(), String> {
        let _guard = self.inner.operation.lock().await;
        let current = self.inner.state.read().await.clone();
        let closing: Vec<DeviceSession> = current
            .sessions
            .iter()
            .filter(|session| {
                session.thread_id == input.thread_id
                    && input
                        .host_id
                        .as_ref()
                        .is_none_or(|id| id == &session.host_id)
                    && input
                        .device_id
                        .as_ref()
                        .is_none_or(|id| id == &session.device_id)
            })
            .cloned()
            .collect();
        if closing.is_empty() {
            return Ok(());
        }
        // Publish the session removal before asking the host to power devices
        // down.  This keeps the thread contract deterministic when a shutdown
        // command fails, matching the reference service's close semantics.
        let mut next = current;
        next.sessions
            .retain(|session| !closing.iter().any(|closing| closing == session));
        self.publish_state(next).await;
        if input.shutdown {
            for session in &closing {
                self.shutdown_device(
                    session.host_id.clone(),
                    session.device_id.clone(),
                    session.platform,
                )
                .await?;
            }
        }
        Ok(())
    }

    pub async fn shutdown(&self, input: DeviceShutdownInput) -> Result<(), String> {
        let _guard = self.inner.operation.lock().await;
        self.shutdown_device(
            input.host_id.unwrap_or_else(|| LOCAL_DEVICE_HOST_ID.into()),
            input.device_id,
            input.platform,
        )
        .await
    }

    async fn shutdown_device(
        &self,
        host_id: String,
        device_id: String,
        platform: DevicePlatform,
    ) -> Result<(), String> {
        let host = self.host(&host_id).await?;
        let mut shutdown_through_hub = false;
        if let Some(port) = self.hub_port(&host_id).await {
            let (path, body) = match platform {
                DevicePlatform::Ios => (
                    "/vendor/serve-sim/grid/api/shutdown",
                    serde_json::json!({ "udid": device_id.clone() }),
                ),
                DevicePlatform::Android => (
                    "/api/devices/shutdown",
                    serde_json::json!({ "platform": "android", "id": device_id.clone() }),
                ),
            };
            shutdown_through_hub = hub_action(port, path, body)
                .await
                .is_ok_and(|result| result.ok);
        }
        if !shutdown_through_hub {
            let command = match platform {
                DevicePlatform::Ios => command("xcrun", argv!["simctl", "shutdown", device_id]),
                DevicePlatform::Android => command("adb", argv!["-s", device_id, "emu", "kill"]),
            };
            if let Err(error) = run_device_command(host.as_ref(), &command, None).await {
                let observed = discover_host(host.clone(), self.hub_port(&host_id).await)
                    .await
                    .ok()
                    .and_then(|devices| devices.into_iter().find(|device| device.id == device_id));
                match observed {
                    Some(device) if !device.booted => {}
                    _ => return Err(error),
                }
            }
        }
        let mut state = self.inner.state.read().await.clone();
        state
            .sessions
            .retain(|session| !(session.host_id == host_id && session.device_id == device_id));
        state.devices = state
            .devices
            .into_iter()
            .map(|device| {
                if device.host_id == host_id && device.id == device_id {
                    DeviceSummary {
                        booted: false,
                        ..device
                    }
                } else {
                    device
                }
            })
            .collect();
        self.publish_state(state).await;
        Ok(())
    }

    pub async fn detail(&self, input: DeviceDetailInput) -> Result<DeviceDetail, String> {
        let _guard = self.inner.operation.lock().await;
        self.detail_inner(input).await
    }

    async fn detail_inner(&self, input: DeviceDetailInput) -> Result<DeviceDetail, String> {
        let host_id = input.host_id.unwrap_or_else(|| LOCAL_DEVICE_HOST_ID.into());
        let host = self.host(&host_id).await?;
        let device = self.find_device(&host_id, &input.device_id).await?;
        let (settings, foreground_app) =
            read_settings(host.as_ref(), device.platform, &device.id).await;
        Ok(DeviceDetail {
            host_id,
            device_id: device.id,
            settings,
            foreground_app,
            read_at: now_iso(),
        })
    }

    pub async fn action(&self, input: DeviceActionInput) -> Result<DeviceDetail, String> {
        input.validate()?;
        let _guard = self.inner.operation.lock().await;
        let host_id = input
            .host_id
            .clone()
            .unwrap_or_else(|| LOCAL_DEVICE_HOST_ID.into());
        let host = self.host(&host_id).await?;
        let device = self.find_device(&host_id, &input.device_id).await?;
        let push_payload = match &input.action {
            DeviceActionKind::SendPush { payload, .. } => {
                let payload = if let Some(alert) = payload.as_str() {
                    serde_json::json!({"aps": {"alert": alert}})
                } else {
                    payload.clone()
                };
                Some(serde_json::to_vec(&payload).map_err(|error| error.to_string())?)
            }
            _ => None,
        };
        let ax_helper = host.helper_path(DeviceHelper::ServeSimAxSettings);
        let cli_helper = host.helper_path(DeviceHelper::ServeSimCli);
        let ignore_permission_failures = matches!(
            (&device.platform, &input.action),
            (
                DevicePlatform::Android,
                DeviceActionKind::SetPermission { .. }
            )
        );
        for planned in action_commands_with_helpers(
            device.platform,
            &device.id,
            &input.action,
            ax_helper.as_deref(),
            cli_helper.as_deref(),
        )? {
            if let Err(error) =
                run_device_command(host.as_ref(), &planned, push_payload.as_deref()).await
                && !ignore_permission_failures
            {
                return Err(error);
            }
        }
        self.detail_inner(DeviceDetailInput {
            host_id: Some(host_id),
            device_id: device.id,
        })
        .await
    }

    pub async fn screenshot(
        &self,
        input: DeviceScreenshotInput,
    ) -> Result<DeviceScreenshot, String> {
        let _guard = self.inner.operation.lock().await;
        let host_id = input.host_id.unwrap_or_else(|| LOCAL_DEVICE_HOST_ID.into());
        let host = self.host(&host_id).await?;
        let device = self.find_device(&host_id, &input.device_id).await?;
        if let Some(port) = self.hub_port(&host_id).await
            && let Ok(png) = hub_screenshot(port, device.platform, &device.id).await
        {
            let (width, height) = device::png_dimensions(&png);
            return Ok(DeviceScreenshot {
                device,
                png,
                width,
                height,
            });
        }
        let command = match device.platform {
            DevicePlatform::Ios => {
                command("xcrun", argv!["simctl", "io", device.id, "screenshot", "-"])
            }
            DevicePlatform::Android => {
                command("adb", argv!["-s", device.id, "exec-out", "screencap", "-p"])
            }
        };
        let png = tokio::time::timeout(
            std::time::Duration::from_secs(20),
            run_device_command(host.as_ref(), &command, None),
        )
        .await
        .map_err(|_| "device screenshot timed out".to_owned())??;
        let (width, height) = device::png_dimensions(&png);
        Ok(DeviceScreenshot {
            device,
            png,
            width,
            height,
        })
    }

    pub async fn sessions_for_thread(&self, thread: &ThreadId) -> Vec<DeviceSession> {
        self.inner
            .state
            .read()
            .await
            .sessions
            .iter()
            .filter(|session| &session.thread_id == thread)
            .cloned()
            .collect()
    }

    pub async fn frames_for_thread(&self, thread: &ThreadId) -> Vec<DeviceEvent> {
        let sessions = self.sessions_for_thread(thread).await;
        let mut frames = Vec::new();
        for session in sessions {
            if let Ok(screenshot) = self
                .screenshot(DeviceScreenshotInput {
                    host_id: Some(session.host_id.clone()),
                    device_id: session.device_id.clone(),
                })
                .await
            {
                frames.push(DeviceEvent::Frame(agent_protocol::device::DeviceFrame {
                    thread_id: thread.clone(),
                    device: screenshot.device,
                    png: screenshot.png,
                    width: screenshot.width,
                    height: screenshot.height,
                    sequence: self
                        .inner
                        .frame_sequence
                        .fetch_add(1, Ordering::Relaxed)
                        .saturating_add(1),
                }));
            }
        }
        frames
    }

    async fn hub_port(&self, host_id: &str) -> Option<u16> {
        if host_id == LOCAL_DEVICE_HOST_ID {
            return self.inner.hub.lock().await.as_ref().map(|hub| hub.port);
        }
        self.inner
            .remote_hubs
            .lock()
            .await
            .get(host_id)
            .map(|hub| hub.local_port)
    }

    async fn host(&self, id: &str) -> Result<Arc<dyn DeviceHostRunner>, String> {
        self.inner
            .hosts
            .read()
            .await
            .get(id)
            .cloned()
            .ok_or_else(|| format!("unknown device host {id}"))
    }
    async fn find_device(&self, host_id: &str, device_id: &str) -> Result<DeviceSummary, String> {
        if let Some(device) = self
            .inner
            .state
            .read()
            .await
            .devices
            .iter()
            .find(|device| device.host_id == host_id && device.id == device_id)
            .cloned()
        {
            return Ok(device);
        }
        // A Host restart clears the in-memory inventory. Explicit detail,
        // action and screenshot calls still have an authoritative device id,
        // so refresh only the requested host before reporting not-found.
        self.list_inner(DeviceListInput {
            retry_host_id: Some(host_id.to_owned()),
            ..Default::default()
        })
        .await?;
        self.inner
            .state
            .read()
            .await
            .devices
            .iter()
            .find(|device| device.host_id == host_id && device.id == device_id)
            .cloned()
            .ok_or_else(|| format!("device {device_id} was not found on host {host_id}"))
    }
}

async fn loopback_http_ok(port: u16, route: &str) -> bool {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let Ok(mut stream) = TcpStream::connect(("127.0.0.1", port)).await else {
        return false;
    };
    let request = format!("GET {route} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n");
    if stream.write_all(request.as_bytes()).await.is_err() {
        return false;
    }
    let mut response = [0; 256];
    let Ok(size) = stream.read(&mut response).await else {
        return false;
    };
    let response = String::from_utf8_lossy(&response[..size]);
    response.starts_with("HTTP/1.1 200 ") || response.starts_with("HTTP/1.0 200 ")
}

async fn agent_config_healthy(state_dir: &Path) -> bool {
    let Ok(bytes) = tokio::fs::read(state_dir.join("config.json")).await else {
        return false;
    };
    let Ok(config) = serde_json::from_slice::<AgentDeviceConfig>(&bytes) else {
        return false;
    };
    let Some(port) = config
        .daemon_base_url
        .strip_prefix("http://127.0.0.1:")
        .and_then(|value| value.parse::<u16>().ok())
    else {
        return false;
    };
    loopback_http_ok(port, "/health").await
}

async fn ensure_agent_daemon(
    host: &dyn DeviceHostRunner,
    entry: &Path,
    state_dir: &Path,
) -> Result<AgentDaemonState, String> {
    tokio::fs::create_dir_all(state_dir)
        .await
        .map_err(|error| error.to_string())?;
    let entry_string = entry.to_string_lossy().into_owned();
    let daemon_file = state_dir.join("daemon.json");
    let agent_file = state_dir.join("agent.json");
    let recorded = tokio::fs::read(&agent_file)
        .await
        .ok()
        .and_then(|bytes| serde_json::from_slice::<PersistedAgentState>(&bytes).ok());
    if let Ok(bytes) = tokio::fs::read(&daemon_file).await {
        if let Ok(daemon) = serde_json::from_slice::<AgentDaemonState>(&bytes)
            && recorded.as_ref().is_some_and(|record| {
                record.version == AGENT_VERSION && record.entry_path == entry_string
            })
            && loopback_http_ok(daemon.http_port, "/health").await
        {
            return Ok(daemon);
        }
        // A previous Host can leave the daemon process alive after its
        // endpoint file becomes stale. Ask the pinned CLI to stop only its
        // own state directory before starting a replacement.
        let node = node_command();
        let node = node.to_string_lossy().into_owned();
        let _ = host
            .run(
                &node,
                &[
                    recorded
                        .as_ref()
                        .map(|record| record.entry_path.clone())
                        .unwrap_or_else(|| entry.to_string_lossy().into_owned()),
                    "daemon".into(),
                    "stop".into(),
                    "--state-dir".into(),
                    state_dir.to_string_lossy().into_owned(),
                ],
                None,
            )
            .await;
        let _ = tokio::fs::remove_file(&daemon_file).await;
    }
    let env_state = state_dir.to_string_lossy().into_owned();
    let mut process = Command::new(node_command());
    host.configure_process(&mut process);
    process
        .arg(entry)
        .args(["devices", "--json"])
        .env("AGENT_DEVICE_STATE_DIR", &env_state)
        .env("AGENT_DEVICE_DAEMON_SERVER_MODE", "http")
        .env("AGENT_DEVICE_DAEMON_IDLE_TIMEOUT_MS", "0")
        .env("AGENT_DEVICE_NO_UPDATE_NOTIFIER", "1")
        .env_remove("AGENT_DEVICE_DAEMON_BASE_URL")
        .env_remove("AGENT_DEVICE_DAEMON_AUTH_TOKEN")
        .env_remove("AGENT_DEVICE_CONFIG")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let mut child = process
        .spawn()
        .map_err(|error| format!("could not start agent-device daemon: {error}"))?;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        if let Ok(bytes) = tokio::fs::read(&daemon_file).await {
            if let Ok(daemon) = serde_json::from_slice::<AgentDaemonState>(&bytes)
                && loopback_http_ok(daemon.http_port, "/health").await
            {
                write_private_json(
                    &agent_file,
                    &PersistedAgentState {
                        entry_path: entry_string.clone(),
                        version: AGENT_VERSION.into(),
                    },
                )
                .await?;
                tokio::spawn(async move {
                    let _ = child.wait().await;
                });
                return Ok(daemon);
            }
        }
        if let Some(status) = child.try_wait().map_err(|error| error.to_string())? {
            return Err(format!(
                "agent-device daemon exited before becoming ready ({status})"
            ));
        }
        if tokio::time::Instant::now() >= deadline {
            let _ = child.kill().await;
            let _ = child.wait().await;
            return Err("agent-device daemon did not become ready within 30 seconds".into());
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

async fn write_agent_config(path: &Path, local_port: u16, token: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|error| error.to_string())?;
    }
    let content = serde_json::to_vec(&AgentDeviceConfig {
        daemon_base_url: format!("http://127.0.0.1:{local_port}"),
        daemon_auth_token: token.to_owned(),
    })
    .map_err(|error| error.to_string())?;
    let temporary = path.with_extension("json.tmp");
    tokio::fs::write(&temporary, content)
        .await
        .map_err(|error| error.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = tokio::fs::metadata(&temporary)
            .await
            .map_err(|error| error.to_string())?
            .permissions();
        permissions.set_mode(0o600);
        tokio::fs::set_permissions(&temporary, permissions)
            .await
            .map_err(|error| error.to_string())?;
    }
    if let Err(error) = tokio::fs::rename(&temporary, path).await {
        let _ = tokio::fs::remove_file(&temporary).await;
        return Err(error.to_string());
    }
    Ok(())
}

async fn write_agent_launcher(
    root: &Path,
    entry: &Path,
    node_path: &Path,
) -> Result<PathBuf, String> {
    let bin = root.join("bin");
    tokio::fs::create_dir_all(&bin)
        .await
        .map_err(|error| error.to_string())?;
    let launcher = bin.join("agent-device-launcher.mjs");
    let node = serde_json::to_string(node_path.to_string_lossy().as_ref())
        .map_err(|error| error.to_string())?;
    let entry = serde_json::to_string(entry.to_string_lossy().as_ref())
        .map_err(|error| error.to_string())?;
    let script = format!(
        "import {{ spawn }} from 'node:child_process';\nconst args = process.argv.slice(2);\nconst informational = args.length === 1 && ['help', '--help', '-h', '--version', 'version'].includes(args[0]);\nconst hasValue = flag => {{ const index = args.indexOf(flag); return index >= 0 && !!args[index + 1] && !args[index + 1].startsWith('--'); }};\nif (!informational && !(hasValue('--config') && hasValue('--session'))) {{ console.error('Call device_open first and include its --config and --session flags.'); process.exit(1); }}\nconst env = {{ ...process.env }};\ndelete env.AGENT_DEVICE_DAEMON_BASE_URL;\ndelete env.AGENT_DEVICE_DAEMON_AUTH_TOKEN;\ndelete env.AGENT_DEVICE_CONFIG;\nconst child = spawn({node}, [{entry}, ...args], {{ stdio: 'inherit', env }});\nchild.on('error', error => {{ console.error(error.message); process.exitCode = 1; }});\nchild.on('exit', code => {{ process.exitCode = code ?? 1; }});\n"
    );
    tokio::fs::write(&launcher, script)
        .await
        .map_err(|error| error.to_string())?;
    #[cfg(windows)]
    {
        let command = bin.join("agent-device.cmd");
        tokio::fs::write(
            &command,
            format!(
                "@echo off\r\n\"{}\" \"{}\" %*\r\n",
                node_path.display(),
                launcher.display()
            ),
        )
        .await
        .map_err(|error| error.to_string())?;
        return Ok(command);
    }
    #[cfg(not(windows))]
    {
        let command = bin.join("agent-device");
        let script = format!(
            "#!/bin/sh\nexec {} {} \"$@\"\n",
            shell_quote(node_path.to_string_lossy().as_ref()),
            shell_quote(launcher.to_string_lossy().as_ref())
        );
        tokio::fs::write(&command, script)
            .await
            .map_err(|error| error.to_string())?;
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = tokio::fs::metadata(&command)
            .await
            .map_err(|error| error.to_string())?
            .permissions();
        permissions.set_mode(0o755);
        tokio::fs::set_permissions(&command, permissions)
            .await
            .map_err(|error| error.to_string())?;
        Ok(command)
    }
}

fn agent_device_session(thread_id: &ThreadId, host_id: &str, device_id: &str) -> String {
    let key = format!("{thread_id}\0{host_id}\0{device_id}");
    format!(
        "device-{}",
        uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_URL, key.as_bytes()).simple()
    )
}

async fn run_device_command(
    host: &dyn DeviceHostRunner,
    command: &DeviceCommand,
    stdin: Option<&[u8]>,
) -> Result<Vec<u8>, String> {
    let output = host.run(&command.program, &command.args, stdin).await?;
    if output.code != 0 {
        return Err(format!(
            "{} failed ({}): {}",
            command.program,
            output.code,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(output.stdout)
}

fn local_summary(state_root: &Path) -> DeviceHostSummary {
    let tools = inspect_toolchain(state_root);
    let ios_available = cfg!(target_os = "macos") && executable_available("xcrun");
    let android = discover_android_tools();
    let android_reason = android.availability_reason();
    DeviceHostSummary {
        id: LOCAL_DEVICE_HOST_ID.into(),
        kind: DeviceHostKind::Local,
        label: "This machine".into(),
        target: None,
        identity_file: None,
        port: None,
        platforms: vec![
            DevicePlatformAvailability {
                platform: DevicePlatform::Ios,
                available: ios_available,
                reason: (!ios_available)
                    .then_some("iOS simulators require macOS with xcrun.".into()),
            },
            DevicePlatformAvailability {
                platform: DevicePlatform::Android,
                available: android_reason.is_none(),
                reason: android_reason,
            },
        ],
        hub_installed: tools.0,
        agent_device_installed: tools.1,
        tools: Some(tools.2),
        tool_inspection_error: None,
    }
}

fn executable_available(name: &str) -> bool {
    executable_path_in_path(name).is_some()
}

fn executable_path_in_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).find_map(|directory| {
        let candidate = directory.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
        if cfg!(windows) {
            [".exe", ".cmd", ".bat"].iter().find_map(|suffix| {
                let candidate = directory.join(format!("{name}{suffix}"));
                candidate.is_file().then_some(candidate)
            })
        } else {
            None
        }
    })
}

fn node_command() -> PathBuf {
    executable_path_in_path("node").unwrap_or_else(|| PathBuf::from("node"))
}

fn android_executable_name(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_owned()
    }
}

fn android_tool_paths_at(root: &Path) -> AndroidToolPaths {
    let adb = root
        .join("platform-tools")
        .join(android_executable_name("adb"));
    let emulator = root
        .join("emulator")
        .join(android_executable_name("emulator"));
    let avdmanager = root
        .join("cmdline-tools")
        .join("latest")
        .join("bin")
        .join(if cfg!(windows) {
            "avdmanager.bat"
        } else {
            "avdmanager"
        });
    let legacy_avdmanager = root.join("tools").join("bin").join(if cfg!(windows) {
        "avdmanager.bat"
    } else {
        "avdmanager"
    });
    AndroidToolPaths {
        root: Some(root.to_owned()),
        adb: adb.is_file().then_some(adb),
        emulator: emulator.is_file().then_some(emulator),
        avdmanager: avdmanager.is_file().then_some(avdmanager),
        legacy_avdmanager: legacy_avdmanager.is_file().then_some(legacy_avdmanager),
    }
}

fn discover_android_tools() -> AndroidToolPaths {
    let explicit = ["ANDROID_HOME", "ANDROID_SDK_ROOT"]
        .into_iter()
        .find_map(|name| {
            std::env::var_os(name).and_then(|value| {
                let value = value.to_string_lossy().trim().to_owned();
                (!value.is_empty()).then_some(PathBuf::from(value))
            })
        });
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_default();
    let mut candidates = explicit.clone().into_iter().collect::<Vec<_>>();
    if explicit.is_none() {
        candidates.extend([
            home.join("Library").join("Android").join("sdk"),
            home.join("Android").join("Sdk"),
            std::env::var_os("LOCALAPPDATA")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join("AppData").join("Local"))
                .join("Android")
                .join("Sdk"),
        ]);
        if let Some(adb) = executable_path_in_path("adb")
            && let Some(root) = adb.parent().and_then(|directory| directory.parent())
        {
            candidates.push(root.to_owned());
        }
    }
    let mut seen = BTreeSet::new();
    for root in candidates {
        if !seen.insert(root.clone()) {
            continue;
        }
        let tools = android_tool_paths_at(&root);
        if explicit.is_some() || tools.adb.is_some() || tools.emulator.is_some() {
            return tools;
        }
    }
    AndroidToolPaths::default()
}

fn host_summary(host: &dyn DeviceHostRunner) -> DeviceHostSummary {
    let probe = host.probe();
    let platforms = probe
        .as_ref()
        .filter(|probe| !probe.platforms.is_empty())
        .map(|probe| probe.platforms.clone())
        .unwrap_or_else(|| {
            vec![
                DevicePlatformAvailability {
                    platform: DevicePlatform::Ios,
                    available: true,
                    reason: None,
                },
                DevicePlatformAvailability {
                    platform: DevicePlatform::Android,
                    available: true,
                    reason: None,
                },
            ]
        });
    let tools = probe
        .as_ref()
        .and_then(|probe| probe.tools.clone())
        .or_else(|| {
            Some(DeviceToolVersions {
                hub: DeviceToolVersion {
                    required_version: HUB_VERSION.into(),
                    installed_versions: vec![],
                    running_version: None,
                },
                agent: DeviceToolVersion {
                    required_version: AGENT_VERSION.into(),
                    installed_versions: vec![],
                    running_version: None,
                },
            })
        });
    let hub_installed = probe.as_ref().is_some_and(|probe| probe.hub_installed);
    let agent_device_installed = probe
        .as_ref()
        .is_some_and(|probe| probe.agent_device_installed);
    DeviceHostSummary {
        id: host.id().into(),
        kind: host.kind(),
        label: host.label().into(),
        target: host.target().map(str::to_owned),
        identity_file: host.identity_file().map(str::to_owned),
        port: host.port(),
        platforms,
        tools,
        tool_inspection_error: host.probe_error(),
        hub_installed,
        agent_device_installed,
    }
}

/// Reads the completed helper trees without installing or starting them. The
/// paths and versions remain part of typed state so the helper lifecycle can
/// be upgraded without changing clients.
fn inspect_toolchain(state_root: &Path) -> (bool, bool, DeviceToolVersions) {
    let root = if state_root
        .file_name()
        .is_some_and(|name| name == "device-hosts.json")
    {
        state_root
            .parent()
            .map(PathBuf::from)
            .unwrap_or_else(|| state_root.to_path_buf())
    } else {
        state_root.to_path_buf()
    };
    let hub_versions = installed_tool_versions(&root, HUB_PACKAGE, &["dist", "server", "cli.mjs"]);
    let agent_versions =
        installed_tool_versions(&root, AGENT_PACKAGE, &["bin", "agent-device.mjs"]);
    let hub_installed = hub_versions.iter().any(|version| version == HUB_VERSION);
    let agent_installed = agent_versions
        .iter()
        .any(|version| version == AGENT_VERSION);
    (
        hub_installed,
        agent_installed,
        DeviceToolVersions {
            hub: DeviceToolVersion {
                required_version: HUB_VERSION.into(),
                installed_versions: hub_versions,
                running_version: None,
            },
            agent: DeviceToolVersion {
                required_version: AGENT_VERSION.into(),
                installed_versions: agent_versions,
                running_version: None,
            },
        },
    )
}

fn installed_tool_versions(root: &Path, package: &str, entry: &[&str]) -> Vec<String> {
    let directory = root.join("tools").join(package);
    let Ok(entries) = std::fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut versions = entries
        .filter_map(Result::ok)
        .filter_map(|directory_entry| {
            let version = directory_entry.file_name().into_string().ok()?;
            if !is_tool_version(&version) {
                return None;
            }
            let path = directory_entry.path();
            if !path.is_dir()
                || std::fs::read_to_string(path.join(".install-complete"))
                    .ok()?
                    .trim()
                    != version
            {
                return None;
            }
            let entry_path = entry.iter().fold(path, |path, part| path.join(part));
            entry_path.is_file().then_some(version)
        })
        .collect::<Vec<_>>();
    versions.sort_by(|left, right| version_sort_key(left).cmp(&version_sort_key(right)));
    versions
}

fn version_sort_key(value: &str) -> Vec<u64> {
    value
        .split_once('-')
        .map(|(version, _)| version)
        .unwrap_or(value)
        .split('.')
        .map(|part| part.parse().unwrap_or(0))
        .collect()
}

fn is_tool_version(value: &str) -> bool {
    let (core, prerelease) = value
        .split_once('-')
        .map_or((value, None), |(core, suffix)| (core, Some(suffix)));
    let core_parts = core.split('.').collect::<Vec<_>>();
    core_parts.len() == 3
        && core_parts.iter().all(|part| {
            !part.is_empty() && part.chars().all(|character| character.is_ascii_digit())
        })
        && prerelease.is_none_or(|suffix| {
            !suffix.is_empty()
                && suffix.chars().all(|character| {
                    character.is_ascii_alphanumeric() || character == '.' || character == '-'
                })
        })
}

fn ios_runtime_label(runtime: &str) -> String {
    let runtime = runtime
        .rsplit_once('.')
        .map(|(_, runtime)| runtime)
        .unwrap_or(runtime);
    let mut parts = runtime.split('-');
    let platform = parts.next().unwrap_or("iOS");
    let version = parts.collect::<Vec<_>>();
    if version.is_empty() {
        runtime.replace('-', " ")
    } else {
        format!("{platform} {}", version.join("."))
    }
}

async fn hub_devices(port: u16, host_id: &str) -> Result<Vec<DeviceSummary>, String> {
    let response = reqwest::Client::new()
        .get(format!("http://127.0.0.1:{port}/api/devices"))
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .map_err(|error| format!("device hub discovery failed: {error}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "device hub discovery returned {}",
            response.status()
        ));
    }
    let list = response
        .json::<HubDeviceList>()
        .await
        .map_err(|error| format!("device hub returned invalid device inventory: {error}"))?;
    list.simulators
        .into_iter()
        .chain(list.emulators)
        .map(|device| {
            let platform = match device.platform.to_ascii_lowercase().as_str() {
                "ios" => DevicePlatform::Ios,
                "android" => DevicePlatform::Android,
                other => return Err(format!("device hub returned unsupported platform {other}")),
            };
            Ok(DeviceSummary {
                host_id: host_id.to_owned(),
                id: device.id,
                platform,
                name: device.name,
                version: device.version,
                booted: device.booted,
                physical: device.physical,
            })
        })
        .collect()
}

async fn hub_screenshot(
    port: u16,
    platform: DevicePlatform,
    device_id: &str,
) -> Result<Vec<u8>, String> {
    let vendor = match platform {
        DevicePlatform::Ios => "serve-sim",
        DevicePlatform::Android => "serve-emu",
    };
    let response = reqwest::Client::new()
        .post(format!(
            "http://127.0.0.1:{port}/vendor/{vendor}/api/screenshot"
        ))
        .query(&[("device", device_id)])
        .timeout(std::time::Duration::from_secs(20))
        .send()
        .await
        .map_err(|error| format!("device hub screenshot failed: {error}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "device hub screenshot returned {}",
            response.status()
        ));
    }
    response
        .bytes()
        .await
        .map(|bytes| bytes.to_vec())
        .map_err(|error| format!("device hub screenshot body failed: {error}"))
}

async fn hub_action(
    port: u16,
    path: &str,
    body: serde_json::Value,
) -> Result<HubActionResult, String> {
    let response = reqwest::Client::new()
        .post(format!("http://127.0.0.1:{port}{path}"))
        .json(&body)
        .timeout(std::time::Duration::from_secs(180))
        .send()
        .await
        .map_err(|error| format!("device hub action failed: {error}"))?;
    if !response.status().is_success() {
        return Err(format!("device hub action returned {}", response.status()));
    }
    response
        .json::<HubActionResult>()
        .await
        .map_err(|error| format!("device hub returned invalid action result: {error}"))
}

async fn discover_host(
    host: Arc<dyn DeviceHostRunner>,
    hub_port: Option<u16>,
) -> Result<Vec<DeviceSummary>, String> {
    if let Some(port) = hub_port
        && let Ok(mut devices) = hub_devices(port, host.id()).await
    {
        let mut active_avds = BTreeSet::new();
        for device in &devices {
            if device.platform == DevicePlatform::Android
                && device.booted
                && !device.physical
                && let Some(name) = android_avd_name(host.as_ref(), &device.id).await
            {
                active_avds.insert(name);
            }
        }
        if let Ok(output) = host.run("emulator", &["-list-avds".into()], None).await
            && output.code == 0
        {
            for name in String::from_utf8_lossy(&output.stdout)
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
            {
                if !active_avds.contains(name)
                    && !devices.iter().any(|device| {
                        device.platform == DevicePlatform::Android && device.name == name
                    })
                {
                    devices.push(DeviceSummary {
                        host_id: host.id().into(),
                        id: name.into(),
                        platform: DevicePlatform::Android,
                        name: name.into(),
                        version: "Android".into(),
                        booted: false,
                        physical: false,
                    });
                }
            }
        }
        return Ok(devices);
    }
    let mut devices = Vec::new();
    let mut tool_seen = false;
    if let Ok(output) = host
        .run(
            "xcrun",
            &[
                "simctl".into(),
                "list".into(),
                "devices".into(),
                "--json".into(),
            ],
            None,
        )
        .await
        && output.code == 0
    {
        tool_seen = true;
        if let Ok(list) = serde_json::from_slice::<SimctlList>(&output.stdout) {
            for (runtime, values) in list.devices {
                for value in values
                    .into_iter()
                    .filter(|value| value.is_available != Some(false))
                {
                    devices.push(DeviceSummary {
                        host_id: host.id().into(),
                        id: value.udid,
                        platform: DevicePlatform::Ios,
                        name: value.name,
                        version: ios_runtime_label(&runtime),
                        booted: value.state.eq_ignore_ascii_case("booted"),
                        physical: false,
                    });
                }
            }
        }
    }
    let mut active_avds = BTreeSet::new();
    if let Ok(output) = host
        .run("adb", &["devices".into(), "-l".into()], None)
        .await
        && output.code == 0
    {
        tool_seen = true;
        for line in String::from_utf8_lossy(&output.stdout).lines().skip(1) {
            let mut parts = line.split_whitespace();
            let Some(id) = parts.next() else {
                continue;
            };
            let Some(status) = parts.next() else {
                continue;
            };
            if status != "device" {
                continue;
            }
            let physical = !id.starts_with("emulator-");
            let avd_name = if !physical {
                let name = android_avd_name(host.as_ref(), id).await;
                if let Some(name) = &name {
                    active_avds.insert(name.clone());
                }
                name
            } else {
                None
            };
            let model = parts
                .find_map(|part| part.strip_prefix("model:"))
                .unwrap_or(id)
                .replace('_', " ");
            devices.push(DeviceSummary {
                host_id: host.id().into(),
                id: id.into(),
                platform: DevicePlatform::Android,
                name: avd_name.unwrap_or(model),
                version: "Android".into(),
                booted: true,
                physical,
            });
        }
    }
    if let Ok(output) = host.run("emulator", &["-list-avds".into()], None).await
        && output.code == 0
    {
        tool_seen = true;
        for name in String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
        {
            if !active_avds.contains(name)
                && !devices
                    .iter()
                    .any(|device| device.platform == DevicePlatform::Android && device.name == name)
            {
                devices.push(DeviceSummary {
                    host_id: host.id().into(),
                    id: name.into(),
                    platform: DevicePlatform::Android,
                    name: name.into(),
                    version: "Android".into(),
                    booted: false,
                    physical: false,
                });
            }
        }
    }
    if devices.is_empty() && !tool_seen {
        Err("No simulator or emulator toolchain was found on this host.".into())
    } else {
        Ok(devices)
    }
}

async fn android_avd_name(host: &dyn DeviceHostRunner, serial: &str) -> Option<String> {
    let output = host
        .run(
            "adb",
            &[
                "-s".into(),
                serial.into(),
                "emu".into(),
                "avd".into(),
                "name".into(),
            ],
            None,
        )
        .await
        .ok()?;
    (output.code == 0)
        .then(|| {
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .map(str::trim)
                .find(|line| !line.is_empty() && !line.eq_ignore_ascii_case("ok"))
                .unwrap_or_default()
                .to_owned()
        })
        .filter(|name| !name.is_empty())
}

async fn wait_for_android_emulator(
    host: &dyn DeviceHostRunner,
    avd_name: &str,
) -> Result<String, String> {
    let deadline = tokio::time::Instant::now() + ANDROID_BOOT_TIMEOUT;
    loop {
        if let Ok(output) = host
            .run("adb", &["devices".into(), "-l".into()], None)
            .await
            && output.code == 0
        {
            for line in String::from_utf8_lossy(&output.stdout).lines().skip(1) {
                let mut parts = line.split_whitespace();
                let Some(serial) = parts.next() else {
                    continue;
                };
                let Some(status) = parts.next() else {
                    continue;
                };
                if status == "device"
                    && serial.starts_with("emulator-")
                    && android_avd_name(host, serial).await.as_deref() == Some(avd_name)
                {
                    return Ok(serial.to_owned());
                }
            }
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(format!(
                "Android emulator {avd_name} did not become ready within {} seconds",
                ANDROID_BOOT_TIMEOUT.as_secs()
            ));
        }
        tokio::time::sleep(ANDROID_BOOT_POLL).await;
    }
}

async fn stop_android_emulator(host: &dyn DeviceHostRunner, avd_name: &str) {
    let Ok(output) = host
        .run("adb", &["devices".into(), "-l".into()], None)
        .await
    else {
        return;
    };
    if output.code != 0 {
        return;
    }
    for line in String::from_utf8_lossy(&output.stdout).lines().skip(1) {
        let mut parts = line.split_whitespace();
        let Some(serial) = parts.next() else { continue };
        if !serial.starts_with("emulator-") {
            continue;
        }
        if android_avd_name(host, serial).await.as_deref() != Some(avd_name) {
            continue;
        }
        let _ = host
            .run(
                "adb",
                &["-s".into(), serial.into(), "emu".into(), "kill".into()],
                None,
            )
            .await;
    }
}

async fn wait_for_ios_boot(host: &dyn DeviceHostRunner, device_id: &str) -> Result<(), String> {
    let command = command("xcrun", argv!["simctl", "bootstatus", device_id, "-b"]);
    tokio::time::timeout(IOS_BOOT_TIMEOUT, run_device_command(host, &command, None))
        .await
        .map_err(|_| {
            format!(
                "iOS simulator {device_id} did not become ready within {} seconds",
                IOS_BOOT_TIMEOUT.as_secs()
            )
        })?
        .map(|_| ())
}

async fn read_settings(
    host: &dyn DeviceHostRunner,
    platform: DevicePlatform,
    device_id: &str,
) -> (DeviceSettings, Option<DeviceForegroundApp>) {
    match platform {
        DevicePlatform::Ios => {
            let appearance = host
                .run(
                    "xcrun",
                    &[
                        "simctl".into(),
                        "ui".into(),
                        device_id.into(),
                        "appearance".into(),
                    ],
                    None,
                )
                .await
                .ok()
                .and_then(command_stdout)
                .map(|value| {
                    if value.trim().eq_ignore_ascii_case("dark") {
                        DeviceAppearance::Dark
                    } else {
                        DeviceAppearance::Light
                    }
                });
            let text_size = host
                .run(
                    "xcrun",
                    &[
                        "simctl".into(),
                        "ui".into(),
                        device_id.into(),
                        "content_size".into(),
                    ],
                    None,
                )
                .await
                .ok()
                .and_then(command_stdout)
                .and_then(|value| ios_text_size_from(value.trim()));
            let increase_contrast = host
                .run(
                    "xcrun",
                    &[
                        "simctl".into(),
                        "ui".into(),
                        device_id.into(),
                        "increase_contrast".into(),
                    ],
                    None,
                )
                .await
                .ok()
                .and_then(command_stdout)
                .map(|value| value.trim().eq_ignore_ascii_case("enabled"));
            let ax = if let Some(helper) = host.helper_path(DeviceHelper::ServeSimAxSettings) {
                host.run(
                    "xcrun",
                    &[
                        "simctl".into(),
                        "spawn".into(),
                        device_id.into(),
                        helper,
                        "status".into(),
                    ],
                    None,
                )
                .await
                .ok()
                .filter(|output| output.code == 0)
                .and_then(|output| {
                    serde_json::from_slice::<BTreeMap<String, String>>(&output.stdout).ok()
                })
            } else {
                None
            };
            let on_off = |name: &str| {
                ax.as_ref()
                    .and_then(|values| values.get(name))
                    .and_then(|value| match value.as_str() {
                        "on" => Some(true),
                        "off" => Some(false),
                        _ => None,
                    })
            };
            let color_filter = ax
                .as_ref()
                .and_then(|values| values.get("color-filter"))
                .and_then(|value| match value.as_str() {
                    "none" => Some(DeviceColorFilter::None),
                    "grayscale" => Some(DeviceColorFilter::Grayscale),
                    "red-green" => Some(DeviceColorFilter::RedGreen),
                    "green-red" => Some(DeviceColorFilter::GreenRed),
                    "blue-yellow" => Some(DeviceColorFilter::BlueYellow),
                    _ => None,
                });
            let settings = DeviceSettings {
                appearance,
                text_size,
                increase_contrast,
                reduce_motion: on_off("reduce-motion"),
                reduce_transparency: on_off("reduce-transparency"),
                show_borders: on_off("show-borders"),
                voice_over: on_off("voiceover"),
                liquid_glass: ax
                    .as_ref()
                    .and_then(|values| values.get("liquid-glass"))
                    .and_then(|value| {
                        matches!(value.as_str(), "clear" | "tinted").then(|| value.clone())
                    }),
                color_filter,
                ..Default::default()
            };
            (settings, None)
        }
        DevicePlatform::Android => {
            let output = host
                .run(
                    "adb",
                    &[
                        "-s".into(),
                        device_id.into(),
                        "shell".into(),
                        "cmd".into(),
                        "uimode".into(),
                        "night".into(),
                    ],
                    None,
                )
                .await
                .ok();
            let appearance = output.and_then(command_stdout).and_then(|value| {
                if value.contains("yes") {
                    Some(DeviceAppearance::Dark)
                } else if value.contains("no") {
                    Some(DeviceAppearance::Light)
                } else {
                    None
                }
            });
            let text_size = host
                .run(
                    "adb",
                    &[
                        "-s".into(),
                        device_id.into(),
                        "shell".into(),
                        "settings".into(),
                        "get".into(),
                        "system".into(),
                        "font_scale".into(),
                    ],
                    None,
                )
                .await
                .ok()
                .and_then(command_stdout)
                .and_then(|value| value.trim().parse::<f64>().ok())
                .and_then(android_text_size_from);
            let reduce_motion = host
                .run(
                    "adb",
                    &[
                        "-s".into(),
                        device_id.into(),
                        "shell".into(),
                        "settings".into(),
                        "get".into(),
                        "global".into(),
                        "animator_duration_scale".into(),
                    ],
                    None,
                )
                .await
                .ok()
                .and_then(command_stdout)
                .and_then(|value| value.trim().parse::<f64>().ok())
                .map(|value| value == 0.0);
            let network_enabled = host
                .run(
                    "adb",
                    &[
                        "-s".into(),
                        device_id.into(),
                        "shell".into(),
                        "settings".into(),
                        "get".into(),
                        "global".into(),
                        "wifi_on".into(),
                    ],
                    None,
                )
                .await
                .ok()
                .and_then(command_stdout)
                .and_then(|value| match value.trim() {
                    "1" => Some(true),
                    "0" => Some(false),
                    _ => None,
                });
            let foreground_app = host
                .run(
                    "adb",
                    &[
                        "-s".into(),
                        device_id.into(),
                        "shell".into(),
                        "dumpsys".into(),
                        "window".into(),
                    ],
                    None,
                )
                .await
                .ok()
                .and_then(command_stdout)
                .and_then(|value| android_foreground_app(&value));
            (
                DeviceSettings {
                    appearance,
                    text_size,
                    reduce_motion,
                    network_enabled,
                    ..Default::default()
                },
                foreground_app,
            )
        }
    }
}

fn command_stdout(output: HostOutput) -> Option<String> {
    (output.code == 0)
        .then(|| String::from_utf8(output.stdout).ok())
        .flatten()
}

fn ios_text_size_from(value: &str) -> Option<DeviceTextSize> {
    Some(match value.to_ascii_lowercase().as_str() {
        "small" | "extra-small" | "medium" => DeviceTextSize::Small,
        "large" => DeviceTextSize::Default,
        value if value.starts_with("accessibility") => DeviceTextSize::ExtraLarge,
        value if value.contains("extra") => DeviceTextSize::Large,
        _ => return None,
    })
}

fn android_text_size_from(value: f64) -> Option<DeviceTextSize> {
    Some(if value <= 0.9 {
        DeviceTextSize::Small
    } else if value >= 1.25 {
        DeviceTextSize::ExtraLarge
    } else if value >= 1.1 {
        DeviceTextSize::Large
    } else {
        DeviceTextSize::Default
    })
}

fn android_foreground_app(value: &str) -> Option<DeviceForegroundApp> {
    let mut tokens = value.split_whitespace();
    while let Some(token) = tokens.next() {
        if token.starts_with('u')
            && token[1..]
                .chars()
                .all(|character| character.is_ascii_digit())
        {
            let package = tokens.next()?.split('/').next()?.trim();
            if !package.is_empty() {
                return Some(DeviceForegroundApp {
                    id: package.into(),
                    name: None,
                    version: None,
                });
            }
        }
    }
    None
}

fn now_iso() -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    format!("{millis}")
}

fn device_host_owner(root: &Path, host_id: &str) -> String {
    let seed = format!("{}\0{host_id}", root.to_string_lossy());
    uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_URL, seed.as_bytes())
        .simple()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_commands_are_typed_and_platform_specific() {
        let ios = action_commands(
            DevicePlatform::Ios,
            "sim",
            &DeviceActionKind::SetAppearance(DeviceAppearance::Dark),
        )
        .unwrap();
        assert_eq!(
            ios,
            vec![DeviceCommand {
                program: "xcrun".into(),
                args: vec![
                    "simctl".into(),
                    "ui".into(),
                    "sim".into(),
                    "appearance".into(),
                    "dark".into()
                ]
            }]
        );
        let push = action_commands(
            DevicePlatform::Ios,
            "sim",
            &DeviceActionKind::SendPush {
                app_id: "app.example".into(),
                payload: serde_json::json!({"aps": {"alert": "hello"}}),
            },
        )
        .unwrap();
        assert_eq!(push[0].program, "xcrun");
        assert_eq!(
            push[0].args,
            vec![
                "simctl".into(),
                "push".into(),
                "sim".into(),
                "app.example".into(),
                "-".into()
            ]
        );
        let android = action_commands(
            DevicePlatform::Android,
            "emu",
            &DeviceActionKind::SetToggle {
                setting: "reduceMotion".into(),
                value: true,
            },
        )
        .unwrap();
        assert_eq!(android.len(), 3);
        let orientation = action_commands(
            DevicePlatform::Android,
            "emulator-5554",
            &DeviceActionKind::SetOrientation(DeviceOrientation::LandscapeLeft),
        )
        .unwrap();
        assert_eq!(orientation.len(), 3);
        assert!(
            action_commands(
                DevicePlatform::Android,
                "emu",
                &DeviceActionKind::SetColorFilter(DeviceColorFilter::None)
            )
            .is_err()
        );
    }

    #[test]
    fn helper_backed_actions_never_fall_back_to_an_untyped_command() {
        let action = DeviceActionKind::SetToggle {
            setting: "reduceMotion".into(),
            value: true,
        };
        assert!(
            action_commands_with_helpers(DevicePlatform::Ios, "sim", &action, None, None).is_err()
        );
        let commands = action_commands_with_helpers(
            DevicePlatform::Ios,
            "sim",
            &action,
            Some("/hub/serve-sim-ax-settings"),
            None,
        )
        .unwrap();
        assert_eq!(commands[0].program, "xcrun");
        assert_eq!(
            commands[0].args,
            vec![
                "simctl".into(),
                "spawn".into(),
                "sim".into(),
                "/hub/serve-sim-ax-settings".into(),
                "set".into(),
                "reduce-motion".into(),
                "on".into(),
            ]
        );

        let permissions = action_commands(
            DevicePlatform::Android,
            "emulator-5554",
            &DeviceActionKind::SetPermission {
                app_id: "app.example".into(),
                permission: DevicePermission::Photos,
                decision: DevicePermissionDecision::Grant,
            },
        )
        .unwrap();
        assert_eq!(permissions.len(), 2);
    }

    #[test]
    fn ssh_arguments_are_shell_quoted_without_an_arbitrary_exec_escape() {
        assert_eq!(shell_quote("safe/path"), "safe/path");
        assert_eq!(shell_quote("a; echo bad"), "'a; echo bad'");
        let command = remote_shell_command("adb", &["devices".into()], false);
        assert!(command.starts_with("sh -lc 'export PATH="));
        assert!(command.contains("exec adb devices"));
    }

    #[test]
    fn agent_session_identity_includes_the_host_and_device() {
        let thread = ThreadId::new("thread").unwrap();
        assert_eq!(
            agent_device_session(&thread, "host", "device"),
            agent_device_session(&thread, "host", "device")
        );
        assert_ne!(
            agent_device_session(&thread, "host", "device"),
            agent_device_session(&thread, "other", "device")
        );
        assert_ne!(
            agent_device_session(&thread, "host", "device"),
            agent_device_session(&thread, "host", "other")
        );
    }

    #[test]
    fn ios_runtime_labels_keep_the_platform_and_version() {
        assert_eq!(
            ios_runtime_label("com.apple.CoreSimulator.SimRuntime.iOS-18-0"),
            "iOS 18.0"
        );
        assert_eq!(
            ios_runtime_label("com.apple.CoreSimulator.SimRuntime.iOS-17-5"),
            "iOS 17.5"
        );
    }

    #[test]
    fn ios_accessibility_text_sizes_are_not_collapsed_into_extra_large() {
        assert_eq!(
            ios_text_size_from("accessibility-extra-extra-large"),
            Some(DeviceTextSize::ExtraLarge)
        );
        assert_eq!(
            ios_text_size_from("extra-extra-large"),
            Some(DeviceTextSize::Large)
        );
    }

    #[test]
    fn android_sdk_commands_are_resolved_under_the_sdk_root() {
        let tools = android_tool_paths_at(Path::new("/sdk"));
        assert_eq!(tools.root.as_deref(), Some(Path::new("/sdk")));
        assert_eq!(
            tools.command("adb"),
            Some(Path::new("/sdk/platform-tools/adb"))
        );
        assert_eq!(
            tools.command("emulator"),
            Some(Path::new("/sdk/emulator/emulator"))
        );
        assert!(tools.command("xcrun").is_none());
    }

    #[test]
    fn installed_tool_versions_accept_pinned_prereleases_only() {
        assert!(is_tool_version("0.12.0"));
        assert!(is_tool_version("0.12.0-beta.1"));
        assert!(!is_tool_version("0.12"));
        assert!(!is_tool_version("0.12.0+local"));
    }

    #[test]
    fn remote_owner_is_stable_per_state_root_and_host() {
        let root = Path::new("/state/device");
        assert_eq!(
            device_host_owner(root, "one"),
            device_host_owner(root, "one")
        );
        assert_ne!(
            device_host_owner(root, "one"),
            device_host_owner(root, "two")
        );
        assert_ne!(
            device_host_owner(root, "one"),
            device_host_owner(Path::new("/other"), "one")
        );
    }
}

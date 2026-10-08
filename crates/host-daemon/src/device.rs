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
    DeviceSummary, DeviceTextSize, DeviceToolVersion, DeviceToolVersions, DeviceAccessibilityInput,
    DeviceAccessibilityTree,
    DeviceEventLogEntry, DeviceEventLogInput, DeviceFrameEncoding, DeviceInput, DeviceInputKind,
    DeviceRecording, DeviceRecordingStartInput, DeviceRecordingStatus, DeviceRecordingStopInput,
    DeviceScreenConfig, DeviceTouchPhase, DeviceVideoFrame, DeviceHardwareButton,
    DeviceRecordingFormat, LOCAL_DEVICE_HOST_ID,
    DeviceDuoCommand, DeviceDuoPhysical, DeviceDuoPose, DeviceFoldPosture,
};
use async_trait::async_trait;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::{Arc, atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering}},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::{net::{TcpListener, TcpStream}, process::Command, sync::{broadcast, mpsc, Mutex, RwLock}};
use tokio_util::sync::CancellationToken;
use crate::device_stream::{jpeg_bounds, parse_semu_packet, AvccChunkKind, AvccDemuxer, Mp4Recorder, TransportFrame, MAX_STREAM_CHUNK};

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
const DEVICE_RECORDING_FILE_NAME: &str = "device-recording.mp4";
const DEVICE_RECORDING_MIME_TYPE: &str = "video/mp4";

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
    const stateFile = name === 'expo-device-hub' ? path.join(state, 'hub.json') : path.join(state, 'agent.json');
    const recorded = read(stateFile);
    let runningVersion = null;
    if (recorded?.version && recorded.version === version && recorded.entryPath) {
      const pid = recorded.pid || read(path.join(state, name === 'expo-device-hub' ? 'hub.json' : 'daemon.json'))?.pid;
      const command = pid ? run('ps', ['-p', String(pid), '-o', 'command=']).stdout || '' : '';
      if (command.includes(recorded.entryPath)) runningVersion = recorded.version;
    }
    return { required_version: version, installed_versions: installedVersions, running_version: runningVersion };
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

impl SshDeviceHost {
    fn ssh_args(&self) -> Vec<String> {
        let mut args = vec![
            "-T".into(),
            "-o".into(),
            "BatchMode=yes".into(),
            "-o".into(),
            "ServerAliveInterval=10".into(),
            "-o".into(),
            "ServerAliveCountMax=3".into(),
        ];
        if let Some(port) = self.config.port {
            args.extend(["-p".into(), port.to_string()]);
        }
        if let Some(identity) = &self.config.identity_file {
            args.extend(["-i".into(), identity.clone()]);
        }
        args.push(self.config.target.clone());
        args
    }
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

    async fn run(&self, command: &str, args: &[String], stdin: Option<&[u8]>) -> Result<HostOutput, String> {
        let mut ssh_args = self.ssh_args();
        ssh_args.push(remote_shell_command(command, args, false));
        run_process("ssh", &ssh_args, stdin, None).await
    }

    async fn start(&self, command: &str, args: &[String]) -> Result<(), String> {
        let ssh_args = self.ssh_args();
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
        let mut last_error = None;
        for attempt in 0..3 {
            // The listener only reserves a candidate port; SSH must claim it
            // after the listener is dropped. ExitOnForwardFailure plus a
            // bounded retry closes the unavoidable local-port TOCTOU race.
            let listener = TcpListener::bind(("127.0.0.1", 0))
                .await
                .map_err(|error| format!("could not reserve a local device tunnel port: {error}"))?;
            let local_port = listener
                .local_addr()
                .map_err(|error| error.to_string())?
                .port();
            drop(listener);
            let mut args = self.ssh_args();
            let target = args.pop().expect("SSH target is always present");
            args.insert(1, "-N".into());
            args.extend(["-o".into(), "ExitOnForwardFailure=yes".into(), "-L".into(), format!("{local_port}:127.0.0.1:{remote_port}"), target]);
            let mut child = match Command::new("ssh")
                .args(args)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
            {
                Ok(child) => child,
                Err(error) => return Err(format!("could not start the device tunnel: {error}")),
            };
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
            loop {
                if TcpStream::connect(("127.0.0.1", local_port)).await.is_ok() {
                    return Ok(Some(ForwardedPort { local_port, child }));
                }
                match child.try_wait() {
                    Ok(Some(status)) => {
                        last_error = Some(format!("device tunnel exited before becoming ready ({status})"));
                        break;
                    }
                    Ok(None) => {}
                    Err(error) => {
                        last_error = Some(format!("could not inspect device tunnel: {error}"));
                        break;
                    }
                }
                if tokio::time::Instant::now() >= deadline {
                    last_error = Some("device tunnel did not become ready within 5 seconds".into());
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
            let _ = child.kill().await;
            let _ = child.wait().await;
            if attempt < 2 {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        }
        Err(last_error.unwrap_or_else(|| "device tunnel could not be established".into()))
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
    use tokio::sync::Notify;

    async fn stop_child(child: &mut tokio::process::Child) {
        let _ = tokio::time::timeout(Duration::from_secs(1), child.kill()).await;
        let _ = tokio::time::timeout(Duration::from_secs(1), child.wait()).await;
    }

    async fn read_limited<R: AsyncRead + Unpin>(mut reader: R, overflow: Arc<Notify>) -> Result<(Vec<u8>, bool), String> {
        let mut output = Vec::new();
        let mut buffer = [0_u8; 16 * 1024];
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
            if copy != size {
                overflow.notify_one();
                return Ok((output, true));
            }
        }
        Ok((output, false))
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
    let overflow = Arc::new(Notify::new());
    let stdout_task = tokio::spawn(read_limited(stdout, overflow.clone()));
    let stderr_task = tokio::spawn(read_limited(stderr, overflow.clone()));

    if let Some(input) = stdin {
        if let Some(mut writer) = child.stdin.take() {
            if let Err(error) = writer.write_all(input).await {
                stop_child(&mut child).await;
                stdout_task.abort();
                stderr_task.abort();
                return Err(error.to_string());
            }
        }
    }

    let status = tokio::select! {
        result = tokio::time::timeout(HOST_COMMAND_TIMEOUT, child.wait()) => match result {
            Ok(result) => match result {
                Ok(status) => status,
                Err(error) => {
                    stdout_task.abort();
                    stderr_task.abort();
                    return Err(error.to_string());
                }
            },
            Err(_) => {
                stop_child(&mut child).await;
                stdout_task.abort();
                stderr_task.abort();
                return Err(format!("{command} did not finish within {} seconds", HOST_COMMAND_TIMEOUT.as_secs()));
            }
        },
        _ = overflow.notified() => {
            stop_child(&mut child).await;
            stdout_task.abort();
            stderr_task.abort();
            return Err(format!("{command} produced more than {HOST_OUTPUT_LIMIT} bytes of output"));
        }
    };
    let (stdout, stdout_truncated) = match tokio::time::timeout(Duration::from_secs(1), &mut stdout_task).await {
        Ok(result) => result.map_err(|error| error.to_string())??,
        Err(_) => {
            stdout_task.abort();
            stderr_task.abort();
            return Err(format!("{command} stdout did not close after the process exited"));
        }
    };
    let (stderr, stderr_truncated) = match tokio::time::timeout(Duration::from_secs(1), &mut stderr_task).await {
        Ok(result) => result.map_err(|error| error.to_string())??,
        Err(_) => {
            stderr_task.abort();
            return Err(format!("{command} stderr did not close after the process exited"));
        }
    };
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
        (DevicePlatform::Android, DeviceActionKind::OpenUrl(url)) => commands.push(adb_shell(device_id, argv!["am", "start", "-a", "android.intent.action.VIEW", "-d", url])),
        (DevicePlatform::Android, DeviceActionKind::LaunchApp(app)) => commands.push(adb_shell(device_id, argv!["monkey", "-p", app, "-c", "android.intent.category.LAUNCHER", "1"])),
        (DevicePlatform::Android, DeviceActionKind::TerminateApp(app)) => commands.push(adb_shell(device_id, argv!["am", "force-stop", app])),
        (_, DeviceActionKind::SetToggle { .. }) => return Err("action setting is unsupported on this platform".into()),
        (_, DeviceActionKind::SetLiquidGlass(_)
        | DeviceActionKind::SetColorFilter(_)
        | DeviceActionKind::Shake
        | DeviceActionKind::SendPush { .. }
        | DeviceActionKind::Input(_)) => return Err("action is unsupported by the command runner".into()),
        (DevicePlatform::Ios, DeviceActionKind::SetOrientation(_)) => return Err("action is unsupported on this platform".into()),
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

fn next_orientation(value: DeviceOrientation) -> DeviceOrientation {
    match value {
        DeviceOrientation::Portrait => DeviceOrientation::LandscapeLeft,
        DeviceOrientation::LandscapeLeft => DeviceOrientation::PortraitUpsideDown,
        DeviceOrientation::PortraitUpsideDown => DeviceOrientation::LandscapeRight,
        DeviceOrientation::LandscapeRight => DeviceOrientation::Portrait,
    }
}

fn orientation_wire(value: DeviceOrientation) -> &'static str {
    match value {
        DeviceOrientation::Portrait => "portrait",
        DeviceOrientation::PortraitUpsideDown => "portrait_upside_down",
        DeviceOrientation::LandscapeLeft => "landscape_left",
        DeviceOrientation::LandscapeRight => "landscape_right",
    }
}

fn fold_posture_wire(value: DeviceFoldPosture) -> &'static str {
    match value {
        DeviceFoldPosture::Closed => "closed",
        DeviceFoldPosture::Opened => "opened",
    }
}

fn duo_pose_wire(value: DeviceDuoPose) -> &'static str {
    match value {
        DeviceDuoPose::Closed => "closed",
        DeviceDuoPose::Book => "book",
        DeviceDuoPose::Open => "open",
        DeviceDuoPose::Laptop => "laptop",
        DeviceDuoPose::Tent => "tent",
    }
}

fn duo_physical_wire(value: DeviceDuoPhysical) -> &'static str {
    match value {
        DeviceDuoPhysical::Faceup => "faceup",
        DeviceDuoPhysical::Facedown => "facedown",
    }
}

fn duo_command_wire(command: &DeviceDuoCommand) -> serde_json::Value {
    match command {
        DeviceDuoCommand::Angle { value } => serde_json::json!({"control": "angle", "value": value}),
        DeviceDuoCommand::Pose { value } => serde_json::json!({"control": "pose", "value": duo_pose_wire(*value)}),
        DeviceDuoCommand::Table { value } => serde_json::json!({"control": "table", "value": value}),
        DeviceDuoCommand::Physical { value } => serde_json::json!({"control": "physical", "value": duo_physical_wire(*value)}),
        DeviceDuoCommand::Orientation { value } => serde_json::json!({"control": "orientation", "value": orientation_wire(*value)}),
    }
}

fn rotate_touch(screen: Option<&DeviceScreenConfig>, x: f32, y: f32) -> (f32, f32) {
    let Some(screen) = screen else { return (x, y); };
    if screen.width > screen.height { return (x, y); }
    match screen.orientation {
        DeviceOrientation::LandscapeLeft => (y, 1.0 - x),
        DeviceOrientation::LandscapeRight => (1.0 - y, x),
        DeviceOrientation::PortraitUpsideDown => (1.0 - x, 1.0 - y),
        DeviceOrientation::Portrait => (x, y),
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
    frame_sequences: Mutex<BTreeMap<(String, String, Option<u8>), u64>>,
    session_epoch: AtomicU64,
    control_sequence: AtomicU64,
    operation: Mutex<()>,
    tool_install: Mutex<()>,
    hub: Mutex<Option<RunningHub>>,
    remote_hub_operation: Mutex<()>,
    remote_hubs: Mutex<BTreeMap<String, RemoteHubRuntime>>,
    agents: Mutex<BTreeMap<String, AgentRuntime>>,
    recordings: Mutex<BTreeMap<(ThreadId, String, String), ActiveDeviceRecording>>,
    capture_sources: Mutex<BTreeMap<(String, String), CaptureSource>>,
    event_log_tasks: Mutex<BTreeMap<(String, String), EventLogTask>>,
    recovery_tasks: Mutex<BTreeMap<String, tokio::task::JoinHandle<()>>>,
}

struct CaptureSource {
    references: Arc<AtomicUsize>,
    prefer_mjpeg: Arc<AtomicBool>,
    cancel: CancellationToken,
    task: tokio::task::JoinHandle<()>,
}

enum SourceMessage {
    Frame { epoch: String, frame: TransportFrame, width: u32, height: u32 },
    Screen { epoch: String, screen: DeviceScreenConfig },
    Foreground { epoch: String, app: Option<DeviceForegroundApp> },
    TransportEnded { epoch: String },
}

struct EventLogTask {
    port: u16,
    epoch: String,
    alive: Arc<AtomicBool>,
    task: tokio::task::JoinHandle<()>,
}

pub(crate) struct DeviceStreamLease {
    references: Vec<Arc<AtomicUsize>>,
}

impl Drop for DeviceStreamLease {
    fn drop(&mut self) {
        for references in &self.references {
            release_reference(references);
        }
    }
}

fn release_reference(references: &AtomicUsize) {
    let mut current = references.load(Ordering::Acquire);
    while current != 0 {
        match references.compare_exchange_weak(
            current,
            current - 1,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => return,
            Err(next) => current = next,
        }
    }
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

struct ActiveDeviceRecording {
    recorder: Mp4Recorder,
    error: Option<String>,
    session_epoch: Option<String>,
}

fn finish_recording(
    thread_id: ThreadId,
    host_id: String,
    device_id: String,
    mut recording: ActiveDeviceRecording,
) -> DeviceRecording {
    recording.recorder.finish();
    if let Some(error) = recording.recorder.finish_error() {
        if recording.error.is_none() {
            recording.error = Some(error.to_owned());
        }
    }
    let status = DeviceRecordingStatus {
        thread_id,
        host_id,
        device_id,
        format: recording.recorder.format(),
        file_name: DEVICE_RECORDING_FILE_NAME.into(),
        mime_type: DEVICE_RECORDING_MIME_TYPE.into(),
        active: false,
        started_at: recording.recorder.started_at().to_owned(),
        frame_count: recording.recorder.frame_count(),
        byte_count: recording.recorder.byte_count(),
        error: recording.error,
    };
    DeviceRecording { status, bytes: recording.recorder.into_bytes() }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
struct PersistedDeviceSettings {
    enabled: bool,
    agent_access_enabled: bool,
    onboarding_completed: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
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
    Arc::new(Self { inner: Arc::new(Inner { config_path: state_root.join("device-hosts.json"), settings_path: state_root.join("settings.json"), hosts: RwLock::new(hosts), state: RwLock::new(state), events, frame_sequences: Mutex::new(BTreeMap::new()), session_epoch: AtomicU64::new(0), control_sequence: AtomicU64::new(0), operation: Mutex::new(()), tool_install: Mutex::new(()), hub: Mutex::new(None), remote_hub_operation: Mutex::new(()), remote_hubs: Mutex::new(BTreeMap::new()), agents: Mutex::new(BTreeMap::new()), recordings: Mutex::new(BTreeMap::new()), capture_sources: Mutex::new(BTreeMap::new()), event_log_tasks: Mutex::new(BTreeMap::new()), recovery_tasks: Mutex::new(BTreeMap::new()) }) })
    }

    pub async fn state_async(&self) -> DeviceServiceState {
        self.inner.state.read().await.clone()
    }

    fn next_session_epoch(&self) -> String {
        let sequence = self
            .inner
            .session_epoch
            .fetch_add(1, Ordering::Relaxed)
            .saturating_add(1);
        format!("{}-{sequence}", now_iso())
    }

    /// Reports work that must settle before the Host hands its process to an
    /// update. Device discovery helpers can stay warm, but an open session,
    /// boot transition, or active recording owns live device state and cannot
    /// be interrupted by that handoff.
    pub fn has_active_tasks(&self) -> bool {
        let state = match self.inner.state.try_read() {
            Ok(state) => state,
            Err(_) => return true,
        };
        if !state.sessions.is_empty()
            || !state.booting_devices.is_empty()
            || matches!(state.host_status, DeviceHostStatus::Installing | DeviceHostStatus::Starting)
        {
            return true;
        }
        match self.inner.recordings.try_lock() {
            Ok(recordings) if !recordings.is_empty() => true,
            Ok(_) => {
                match self.inner.recovery_tasks.try_lock() {
                    Ok(tasks) if tasks.values().any(|task| !task.is_finished()) => return true,
                    Ok(_) => {}
                    Err(_) => return true,
                }
                match self.inner.capture_sources.try_lock() {
                    Ok(sources) => sources.values().any(|source| {
                        source.references.load(Ordering::Acquire) != 0 || !source.task.is_finished()
                    }),
                    Err(_) => true,
                }
            }
            Err(_) => true,
        }
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

        let state_dir = root.join("agent-device").join("hosts").join(host_id);

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

        let (local_port, token, tunnel) = match host.kind() {
            DeviceHostKind::Local => {
                let daemon = ensure_agent_daemon(local_host.as_ref(), &entry, &state_dir).await?;
                (daemon.http_port, daemon.token, None)
            }
            DeviceHostKind::Ssh => {
                if let Some((local_port, token, tunnel)) = persisted_remote_agent(host.as_ref(), &state_dir).await {
                    (local_port, token, Some(tunnel))
                } else {
                    let output = host
                        .lifecycle("agent")
                        .await?
                        .ok_or_else(|| "SSH device host does not support helper lifecycle".to_owned())?;
                    if output.code != 0 {
                        return Err(format!(
                            "remote agent-device startup failed: {}",
                            String::from_utf8_lossy(&output.stderr).trim()
                        ));
                    }
                    let remote: serde_json::Value = match serde_json::from_slice(&output.stdout) {
                        Ok(remote) => remote,
                        Err(error) => {
                            self.cleanup_agent_activation(host.as_ref(), &entry, &state_dir, None).await;
                            return Err(format!("remote agent-device returned invalid state: {error}"));
                        }
                    };
                    let daemon: AgentDaemonState = match serde_json::from_value(remote.clone()) {
                        Ok(daemon) if daemon.http_port != 0 && !daemon.token.is_empty() => daemon,
                        Ok(_) => {
                            self.cleanup_agent_activation(host.as_ref(), &entry, &state_dir, None).await;
                            return Err("remote agent-device returned an incomplete endpoint".into());
                        }
                        Err(error) => {
                            self.cleanup_agent_activation(host.as_ref(), &entry, &state_dir, None).await;
                            return Err(format!("remote agent-device returned invalid endpoint: {error}"));
                        }
                    };
                    if let Err(error) = write_private_json(&state_dir.join("daemon.json"), &daemon).await {
                        self.cleanup_agent_activation(host.as_ref(), &entry, &state_dir, None).await;
                        return Err(error);
                    }
                    if let Err(error) = write_private_json(
                        &state_dir.join("agent.json"),
                        &PersistedAgentState {
                            entry_path: remote
                                .get("entryPath")
                                .and_then(serde_json::Value::as_str)
                                .unwrap_or_default()
                                .to_owned(),
                            version: remote
                                .get("version")
                                .and_then(serde_json::Value::as_str)
                                .unwrap_or(AGENT_VERSION)
                                .to_owned(),
                        },
                    )
                    .await {
                        self.cleanup_agent_activation(host.as_ref(), &entry, &state_dir, None).await;
                        return Err(error);
                    }
                    let tunnel = match host.forward(daemon.http_port).await {
                        Ok(Some(tunnel)) => tunnel,
                        Ok(None) => {
                            self.cleanup_agent_activation(host.as_ref(), &entry, &state_dir, None).await;
                            return Err("SSH device host did not create an agent tunnel".to_owned());
                        }
                        Err(error) => {
                            self.cleanup_agent_activation(host.as_ref(), &entry, &state_dir, None).await;
                            return Err(error);
                        }
                    };
                    (tunnel.local_port, daemon.token, Some(tunnel))
                }
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
        let _start_guard = self.inner.remote_hub_operation.lock().await;
        let stale = {
            let mut hubs = self.inner.remote_hubs.lock().await;
            let live_port = hubs.get_mut(host.id()).and_then(|runtime| {
                runtime
                    .tunnel
                    .child
                    .try_wait()
                    .ok()
                    .flatten()
                    .is_none()
                    .then_some(runtime.local_port)
            });
            if let Some(port) = live_port {
                drop(hubs);
                if loopback_http_ok(port, "/readyz").await {
                    self.ensure_remote_recovery_task(host.id()).await;
                    return Ok(());
                }
                hubs = self.inner.remote_hubs.lock().await;
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
        drop(hubs);
        self.ensure_remote_recovery_task(host.id()).await;
        Ok(())
    }

    async fn ensure_remote_recovery_task(&self, host_id: &str) {
        let mut tasks = self.inner.recovery_tasks.lock().await;
        if tasks.contains_key(host_id) { return; }
        let service = self.clone();
        let id = host_id.to_owned();
        let task_id = id.clone();
        tasks.insert(task_id, tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                let Some(host) = service.inner.hosts.read().await.get(&id).cloned() else { break; };
                let port = service.inner.remote_hubs.lock().await.get(&id).map(|runtime| runtime.local_port);
                let healthy = match port {
                    Some(port) => loopback_http_ok(port, "/readyz").await,
                    None => false,
                };
                if healthy { continue; }
                host.set_probe_error(Some("SSH device hub tunnel is unavailable; reconnecting".into()));
                let recovered = service.ensure_remote_hub_running(&host).await.is_ok();
                if recovered {
                    host.set_probe_error(None);
                }
                let mut state = service.state_async().await;
                if let Some(summary) = state.host_statuses.get_mut(&id) {
                    summary.status = if recovered { DeviceHostStatus::Ready } else { DeviceHostStatus::Starting };
                    summary.detail = host.probe_error();
                }
                service.publish_state(state).await;
            }
        }));
    }

    async fn ensure_hub_tool(&self, host: &Arc<dyn DeviceHostRunner>, start: bool) -> Result<(), String> {
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
                let remote: serde_json::Value = match serde_json::from_slice(&output.stdout) {
                    Ok(remote) => remote,
                    Err(error) => {
                        let _ = host.lifecycle("stop-agent").await;
                        return Err(format!("remote agent-device returned invalid state: {error}"));
                    }
                };
                let daemon: AgentDaemonState = match serde_json::from_value(remote.clone()) {
                    Ok(daemon) => daemon,
                    Err(error) => {
                        let _ = host.lifecycle("stop-agent").await;
                        return Err(format!("remote agent-device returned invalid endpoint: {error}"));
                    }
                };
                if daemon.http_port == 0 || daemon.token.is_empty() {
                    let _ = host.lifecycle("stop-agent").await;
                    return Err("remote agent-device returned an incomplete endpoint".into());
                }
                let root = self
                    .inner
                    .config_path
                    .parent()
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from("."));
                let state_dir = root.join("agent-device").join("hosts").join(host.id());
                if let Err(error) = write_private_json(&state_dir.join("daemon.json"), &daemon).await {
                    let _ = host.lifecycle("stop-agent").await;
                    return Err(error);
                }
                if let Err(error) = write_private_json(
                    &state_dir.join("agent.json"),
                    &PersistedAgentState {
                        entry_path: remote
                            .get("entryPath")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or_default()
                            .to_owned(),
                        version: remote
                            .get("version")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or(AGENT_VERSION)
                            .to_owned(),
                    },
                )
                .await {
                    let _ = host.lifecycle("stop-agent").await;
                    return Err(error);
                }
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
        let recovery_tasks = {
            let mut tasks = self.inner.recovery_tasks.lock().await;
            std::mem::take(&mut *tasks)
        };
        for (_, task) in recovery_tasks { task.abort(); }
        let event_log_tasks = {
            let mut tasks = self.inner.event_log_tasks.lock().await;
            std::mem::take(&mut *tasks)
        };
        for (_, task) in event_log_tasks {
            task.alive.store(false, Ordering::Release);
            task.task.abort();
        }
        let completed_recordings = {
            let mut recordings = self.inner.recordings.lock().await;
            std::mem::take(&mut *recordings)
                .into_iter()
                .map(|((thread, host_id, device_id), recording)| finish_recording(thread, host_id, device_id, recording))
                .collect::<Vec<_>>()
        };
        for recording in completed_recordings {
            let _ = self.inner.events.send(DeviceEvent::Recording(recording.status.clone()));
            let _ = self.inner.events.send(DeviceEvent::RecordingComplete(recording));
        }
        let capture_sources = {
            let mut sources = self.inner.capture_sources.lock().await;
            std::mem::take(&mut *sources)
        };
        for (_, source) in capture_sources {
            source.cancel.cancel();
            let mut task = source.task;
            if tokio::time::timeout(Duration::from_secs(2), &mut task).await.is_err() {
                task.abort();
                let _ = task.await;
            }
        }
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
        let session = DeviceSession { thread_id: input.thread_id, host_id: host_id.clone(), device_id: device.id.clone(), platform: device.platform, opened_at: now_iso(), session_epoch: self.next_session_epoch() };
        let already_open = state.sessions.iter().any(|existing| {
            existing.thread_id == session.thread_id
                && existing.host_id == session.host_id
                && existing.device_id == session.device_id
        });
        state.sessions.retain(|existing| !(existing.thread_id == session.thread_id && existing.host_id == session.host_id && existing.device_id == session.device_id));
        state.sessions.push(session.clone());
        self.publish_state(state).await;
        let source_missing = self
            .inner
            .capture_sources
            .lock()
            .await
            .get(&(session.host_id.clone(), session.device_id.clone()))
            .is_none_or(|source| source.task.is_finished());
        if !already_open || source_missing {
            let _ = self.retain_capture_source(&session, false).await;
        }
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
        next.sessions.retain(|session| !closing.iter().any(|closing| closing == session));
        let completed_recordings = {
            let mut recordings = self.inner.recordings.lock().await;
            let current = std::mem::take(&mut *recordings);
            let mut completed = Vec::new();
            let mut remaining = BTreeMap::new();
            for ((thread, host_id, device_id), recording) in current {
                if closing.iter().any(|session| &session.thread_id == &thread && session.host_id == host_id && session.device_id == device_id) {
                    completed.push(((host_id.clone(), device_id.clone()), finish_recording(thread, host_id, device_id, recording)));
                } else {
                    remaining.insert((thread, host_id, device_id), recording);
                }
            }
            *recordings = remaining;
            completed
        };
        let retained_devices = next
            .sessions
            .iter()
            .map(|session| (session.host_id.clone(), session.device_id.clone()))
            .collect::<BTreeSet<_>>();
        let mut event_log_tasks = self.inner.event_log_tasks.lock().await;
        for session in &closing {
            let key = (session.host_id.clone(), session.device_id.clone());
            if !retained_devices.contains(&key) && let Some(task) = event_log_tasks.remove(&key) {
                task.alive.store(false, Ordering::Release);
                task.task.abort();
            }
        }
        drop(event_log_tasks);
        self.publish_state(next).await;
        for session in &closing {
            self.release_capture_source(&(session.host_id.clone(), session.device_id.clone())).await;
        }
        for (key, recording) in completed_recordings {
            self.release_capture_source(&key).await;
            let _ = self.inner.events.send(DeviceEvent::Recording(recording.status.clone()));
            let _ = self.inner.events.send(DeviceEvent::RecordingComplete(recording));
        }
        // Closing a session is an accepted state transition even when the
        // device was already powered off or its helper has just disappeared.
        // The reference service records the close first and deliberately
        // discards shutdown failures so a stale helper cannot resurrect the
        // session in the client.
        if input.shutdown {
            for session in &closing {
                let _ = self
                    .shutdown_device(session.host_id.clone(), session.device_id.clone(), session.platform)
                    .await;
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
        if let DeviceActionKind::Input(device_input) = &input.action {
            let port = self.hub_port(&host_id).await.ok_or_else(|| "device hub is not running".to_owned())?;
            let request_id = self.next_control_request_id(device_input);
            hub_input(port, device.platform, &device.id, device_input, request_id).await?;
            return self.detail_inner(DeviceDetailInput { host_id: Some(host_id), device_id: device.id }).await;
        }
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

    /// Retains one capture source per host/device resource. The source is
    /// owned by the Host and keeps recording even when a viewer disconnects;
    /// the lease releases only the viewer's reference.
    pub(crate) async fn acquire_stream(&self, thread: &ThreadId, prefer_mjpeg: bool) -> DeviceStreamLease {
        let sessions = self.sessions_for_thread(thread).await;
        let mut references = Vec::with_capacity(sessions.len());
        for session in sessions {
            references.push(self.retain_capture_source(&session, prefer_mjpeg).await);
        }
        DeviceStreamLease { references }
    }

    async fn retain_capture_source(&self, session: &DeviceSession, prefer_mjpeg: bool) -> Arc<AtomicUsize> {
        let key = (session.host_id.clone(), session.device_id.clone());
        let stale = {
            let mut sources = self.inner.capture_sources.lock().await;
            if let Some(source) = sources.get(&key)
                && !source.task.is_finished()
            {
                if prefer_mjpeg {
                    source.prefer_mjpeg.store(true, Ordering::Release);
                }
                source.references.fetch_add(1, Ordering::AcqRel);
                return source.references.clone();
            }
            sources.remove(&key)
        };
        if let Some(stale) = stale {
            stale.cancel.cancel();
            let mut task = stale.task;
            if tokio::time::timeout(Duration::from_secs(2), &mut task).await.is_err() {
                task.abort();
                let _ = task.await;
            }
        }
        let references = Arc::new(AtomicUsize::new(1));
        let source_preference = Arc::new(AtomicBool::new(prefer_mjpeg));
        let service = self.clone();
        let captured = session.clone();
        let cancel = CancellationToken::new();
        let task_cancel = cancel.clone();
        let task_references = references.clone();
        let task_preference = source_preference.clone();
        let task = tokio::spawn(async move {
            service
                .capture_source_loop(captured, task_references, task_preference, task_cancel)
                .await;
        });
        let mut sources = self.inner.capture_sources.lock().await;
        if let Some(source) = sources.get(&key)
            && !source.task.is_finished()
        {
            if prefer_mjpeg {
                source.prefer_mjpeg.store(true, Ordering::Release);
            }
            source.references.fetch_add(1, Ordering::AcqRel);
            let existing_references = source.references.clone();
            drop(sources);
            cancel.cancel();
            let mut task = task;
            if tokio::time::timeout(Duration::from_secs(2), &mut task).await.is_err() {
                task.abort();
                let _ = task.await;
            }
            return existing_references;
        }
        sources.insert(key, CaptureSource { references: references.clone(), prefer_mjpeg: source_preference, cancel, task });
        references
    }

    async fn release_capture_source(&self, key: &(String, String)) {
        if let Some(source) = self.inner.capture_sources.lock().await.get(key) {
            release_reference(&source.references);
        }
    }

    async fn recording_is_active(&self, key: &(String, String)) -> bool {
        self.inner.recordings.lock().await.keys().any(|(_, host_id, device_id)| {
            host_id == &key.0 && device_id == &key.1
        })
    }

    async fn recording_targets_for_source(
        &self,
        key: &(String, String),
        owner: &DeviceSession,
        generation_sessions: &BTreeMap<String, String>,
    ) -> Vec<(DeviceSession, DeviceRecordingFormat)> {
        self.inner
            .recordings
            .lock()
            .await
            .iter()
            .filter(|((_, host_id, device_id), _)| host_id == &key.0 && device_id == &key.1)
            .filter(|((thread_id, _, _), recording)| {
                recording
                    .session_epoch
                    .as_ref()
                    .is_none_or(|epoch| generation_sessions.get(thread_id.as_str()) == Some(epoch))
            })
            .map(|((thread_id, host_id, device_id), recording)| {
                (
                    DeviceSession {
                        thread_id: thread_id.clone(),
                        host_id: host_id.clone(),
                        device_id: device_id.clone(),
                        platform: owner.platform,
                        opened_at: owner.opened_at.clone(),
                        session_epoch: recording
                            .session_epoch
                            .clone()
                            .unwrap_or_else(|| owner.session_epoch.clone()),
                    },
                    recording.recorder.format(),
                )
            })
            .collect()
    }

    pub(crate) fn event_belongs_to_thread(event: &DeviceEvent, thread: &ThreadId) -> bool {
        match event {
            DeviceEvent::Frame(frame) => &frame.thread_id == thread,
            DeviceEvent::Video(frame) => &frame.thread_id == thread,
            DeviceEvent::Screen(screen) => screen.thread_id.as_ref().is_none_or(|id| id == thread),
            DeviceEvent::Recording(status) => &status.thread_id == thread,
            DeviceEvent::RecordingComplete(recording) => &recording.status.thread_id == thread,
            DeviceEvent::State(_)
            | DeviceEvent::Accessibility(_)
            | DeviceEvent::Foreground(_)
            | DeviceEvent::EventLog(_) => true,
        }
    }

    async fn capture_source_loop(
        &self,
        template: DeviceSession,
        references: Arc<AtomicUsize>,
        preference: Arc<AtomicBool>,
        cancel: CancellationToken,
    ) {
        let key = (template.host_id.clone(), template.device_id.clone());
        loop {
            if cancel.is_cancelled() {
                break;
            }
            let owner = self
                .inner
                .state
                .read()
                .await
                .sessions
                .iter()
                .find(|session| session.host_id == key.0 && session.device_id == key.1)
                .cloned()
                .unwrap_or_else(|| template.clone());
            let generation_sessions = self
                .inner
                .state
                .read()
                .await
                .sessions
                .iter()
                .filter(|session| session.host_id == key.0 && session.device_id == key.1)
                .map(|session| (session.thread_id.to_string(), session.session_epoch.clone()))
                .collect::<BTreeMap<_, _>>();
            if references.load(Ordering::Acquire) == 0 && !self.recording_is_active(&key).await {
                break;
            }
            let device = match self.find_device(&owner.host_id, &owner.device_id).await {
                Ok(device) => device,
                Err(_) => {
                    if !sleep_until_cancelled(&cancel, Duration::from_millis(250)).await {
                        break;
                    }
                    continue;
                }
            };
            let Some(port) = self.source_hub_port(&owner.host_id, &cancel).await else {
                if !sleep_until_cancelled(&cancel, Duration::from_millis(500)).await {
                    break;
                }
                continue;
            };
            let screen = self.screen_config(&owner.host_id, &device).await.ok();
            if device.platform == DevicePlatform::Ios {
                self.ensure_event_log_task(&owner.host_id, &device.id, port, &owner.session_epoch).await;
            }
            let generation_cancel = cancel.child_token();
            let (sender, mut receiver) = mpsc::channel(64);
            let mut children = Vec::new();
            let (width, height) = screen.as_ref().map_or((1, 1), |screen| (screen.width, screen.height));

            match device.platform {
                DevicePlatform::Ios => {
                    let panel_ids = if screen.as_ref().is_some_and(|screen| screen.supports_hinge_angle && !screen.supports_physical_orientation) {
                        vec![None, Some(1), Some(3)]
                    } else {
                        vec![None]
                    };
                    for panel_id in panel_ids {
                        let sender = sender.clone();
                        let child_cancel = generation_cancel.clone();
                        let epoch = owner.session_epoch.clone();
                        let device_id = device.id.clone();
                        let width = if panel_id.is_some() { width.max(1) } else { width };
                        let height = if panel_id.is_some() { height.max(1) } else { height };
                        children.push(tokio::spawn(async move {
                            persistent_avcc_reader(port, device_id, panel_id, epoch, width, height, sender, child_cancel).await;
                        }));
                    }
                    let sender = sender.clone();
                    let child_cancel = generation_cancel.clone();
                    let epoch = owner.session_epoch.clone();
                    let device_id = device.id.clone();
                    children.push(tokio::spawn(async move {
                        persistent_mjpeg_reader(port, device_id, epoch, width, height, sender, child_cancel).await;
                    }));
                    let sender = sender.clone();
                    let child_cancel = generation_cancel.clone();
                    let epoch = owner.session_epoch.clone();
                    let device_id = device.id.clone();
                    children.push(tokio::spawn(async move {
                        foreground_reader(port, device_id, epoch, sender, child_cancel).await;
                    }));
                    let sender = sender.clone();
                    let child_cancel = generation_cancel.clone();
                    let epoch = owner.session_epoch.clone();
                    let device_id = device.id.clone();
                    let platform = device.platform;
                    children.push(tokio::spawn(async move {
                        screen_config_reader(port, platform, device_id, epoch, sender, child_cancel).await;
                    }));
                }
                DevicePlatform::Android => {
                    let sender = sender.clone();
                    let child_cancel = generation_cancel.clone();
                    let epoch = owner.session_epoch.clone();
                    let device_id = device.id.clone();
                    children.push(tokio::spawn(async move {
                        persistent_semu_reader(port, device_id, epoch, width, height, sender, child_cancel).await;
                    }));
                    let sender = sender.clone();
                    let child_cancel = generation_cancel.clone();
                    let epoch = owner.session_epoch.clone();
                    let device_id = device.id.clone();
                    let platform = device.platform;
                    children.push(tokio::spawn(async move {
                        screen_config_reader(port, platform, device_id, epoch, sender, child_cancel).await;
                    }));
                }
            }
            drop(sender);

            let mut epoch_check = tokio::time::interval(Duration::from_millis(250));
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    _ = epoch_check.tick() => {
                        let current_sessions = {
                            let state = self.inner.state.read().await;
                            let sessions = state.sessions.iter()
                                .filter(|session| session.host_id == key.0 && session.device_id == key.1)
                                .map(|session| (session.thread_id.to_string(), session.session_epoch.clone()))
                                .collect::<BTreeMap<_, _>>();
                            (!sessions.is_empty()).then_some(sessions)
                        };
                        if current_sessions.as_ref().is_some_and(|sessions| sessions != &generation_sessions)
                            || (current_sessions.is_none() && !self.recording_is_active(&key).await)
                            || (references.load(Ordering::Acquire) == 0 && !self.recording_is_active(&key).await)
                        {
                            break;
                        }
                    }
                    message = receiver.recv() => {
                        let Some(message) = message else { break };
                        match message {
                            SourceMessage::Frame { epoch, frame, width, height } if epoch == owner.session_epoch => {
                                self.publish_source_frame(&key, &owner, &device, &generation_sessions, frame, width, height, preference.load(Ordering::Acquire)).await;
                            }
                            SourceMessage::Screen { epoch, screen } if epoch == owner.session_epoch => {
                                self.publish_source_screen(&key, &generation_sessions, screen).await;
                            }
                            SourceMessage::Foreground { epoch, app } if epoch == owner.session_epoch => {
                                self.publish_source_foreground(&key, &generation_sessions, app).await;
                            }
                            SourceMessage::TransportEnded { epoch } if epoch == owner.session_epoch => break,
                            _ => {}
                        }
                    }
                }
            }
            generation_cancel.cancel();
            for mut child in children {
                if tokio::time::timeout(Duration::from_secs(2), &mut child).await.is_err() {
                    child.abort();
                    let _ = child.await;
                }
            }
            if cancel.is_cancelled() {
                break;
            }
            if !sleep_until_cancelled(&cancel, Duration::from_millis(100)).await {
                break;
            }
        }
    }

    async fn source_hub_port(&self, host_id: &str, cancel: &CancellationToken) -> Option<u16> {
        if let Some(port) = self.hub_port(host_id).await {
            return Some(port);
        }
        if let Ok(host) = self.host(host_id).await {
            let _ = if host.kind() == DeviceHostKind::Ssh {
                self.ensure_remote_hub_running(&host).await
            } else {
                self.ensure_hub_running(&host).await
            };
        }
        if cancel.is_cancelled() {
            None
        } else {
            self.hub_port(host_id).await
        }
    }

    async fn publish_source_frame(
        &self,
        key: &(String, String),
        owner: &DeviceSession,
        device: &DeviceSummary,
        generation_sessions: &BTreeMap<String, String>,
        frame: TransportFrame,
        width: u32,
        height: u32,
        prefer_mjpeg: bool,
    ) {
        let sessions = self
            .inner
            .state
            .read()
            .await
            .sessions
            .iter()
            .filter(|session| session.host_id == key.0 && session.device_id == key.1)
            .cloned()
            .collect::<Vec<_>>();
        let should_publish = if prefer_mjpeg {
            matches!(frame.encoding, DeviceFrameEncoding::Mjpeg | DeviceFrameEncoding::Jpeg)
        } else {
            matches!(frame.encoding, DeviceFrameEncoding::AvccDescription | DeviceFrameEncoding::H264 | DeviceFrameEncoding::Semu)
        };
        self.append_source_recordings(key, owner, generation_sessions, std::slice::from_ref(&frame)).await;
        if !should_publish {
            return;
        }
        let sequence = self.next_frame_sequence(&owner.host_id, &owner.device_id, frame.screen_id).await;
        for session in sessions {
            if !session_matches_generation(&generation_sessions, &session) {
                continue;
            }
            let _ = self.inner.events.send(DeviceEvent::Video(DeviceVideoFrame {
                thread_id: session.thread_id,
                session_epoch: session.session_epoch,
                device: device.clone(),
                payload: frame.payload.clone(),
                encoding: frame.encoding,
                width,
                height,
                sequence,
                timestamp_us: frame.timestamp_us,
                keyframe: frame.keyframe,
                screen_id: frame.screen_id,
            }));
        }
    }

    async fn publish_source_screen(&self, key: &(String, String), generation_sessions: &BTreeMap<String, String>, mut screen: DeviceScreenConfig) {
        let sessions = self
            .inner
            .state
            .read()
            .await
            .sessions
            .iter()
            .filter(|session| session.host_id == key.0 && session.device_id == key.1)
            .cloned()
            .collect::<Vec<_>>();
        for session in sessions {
            if !session_matches_generation(&generation_sessions, &session) {
                continue;
            }
            screen.thread_id = Some(session.thread_id.clone());
            screen.session_epoch = session.session_epoch.clone();
            screen.host_id = Some(session.host_id.clone());
            screen.device_id = Some(session.device_id.clone());
            let _ = self.inner.events.send(DeviceEvent::Screen(screen.clone()));
        }
    }

    async fn publish_source_foreground(&self, key: &(String, String), generation_sessions: &BTreeMap<String, String>, app: Option<DeviceForegroundApp>) {
        let sessions = self
            .inner
            .state
            .read()
            .await
            .sessions
            .iter()
            .filter(|session| session.host_id == key.0 && session.device_id == key.1 && session_matches_generation(generation_sessions, session))
            .cloned()
            .collect::<Vec<_>>();
        for session in sessions {
            let _ = self.inner.events.send(DeviceEvent::Foreground(DeviceForegroundUpdate {
                host_id: key.0.clone(),
                device_id: key.1.clone(),
                session_epoch: session.session_epoch,
                app,
                received_at: now_iso(),
            }));
        }
    }

    async fn append_source_recordings(&self, key: &(String, String), owner: &DeviceSession, generation_sessions: &BTreeMap<String, String>, frames: &[TransportFrame]) {
        for (recording_session, format) in self.recording_targets_for_source(key, owner, generation_sessions).await {
            if format != DeviceRecordingFormat::Mp4 {
                continue;
            }
            for frame in frames {
                if matches!(frame.encoding, DeviceFrameEncoding::AvccDescription | DeviceFrameEncoding::H264 | DeviceFrameEncoding::Semu) {
                    self.append_recording(&recording_session.thread_id, &recording_session, frame).await;
                }
            }
        }
    }

    async fn screen_config(&self, host_id: &str, device: &DeviceSummary) -> Result<DeviceScreenConfig, String> {
        let port = self.hub_port(host_id).await.ok_or_else(|| "device hub is not running".to_owned())?;
        hub_screen_config(port, device.platform, &device.id).await
    }

    async fn ensure_event_log_task(&self, host_id: &str, device_id: &str, port: u16, epoch: &str) {
        let key = (host_id.to_owned(), device_id.to_owned());
        let events = self.inner.events.clone();
        let host = host_id.to_owned();
        let device = device_id.to_owned();
        let epoch = epoch.to_owned();
        let service = self.clone();
        let mut tasks = self.inner.event_log_tasks.lock().await;
        if tasks
            .get(&key)
            .is_some_and(|current| current.port == port && current.epoch == epoch && !current.task.is_finished())
        {
            return;
        }
        if let Some(task) = tasks.remove(&key) {
            task.alive.store(false, Ordering::Release);
            task.task.abort();
        }
        let alive = Arc::new(AtomicBool::new(true));
        let task_alive = alive.clone();
        let task = tokio::spawn(async move {
            loop {
                if !task_alive.load(Ordering::Acquire) || !service.session_epoch_is_current(&host, &device, &epoch).await {
                    break;
                }
                let response = match reqwest::Client::new()
                    .get(format!("http://127.0.0.1:{port}/vendor/serve-sim/api/event-log/events"))
                    .query(&[("device", device.as_str()), ("limit", "100")])
                    .send()
                    .await
                {
                    Ok(response) => response,
                    Err(error) => {
                        if task_alive.load(Ordering::Acquire) && service.session_epoch_is_current(&host, &device, &epoch).await {
                        let _ = events.send(event_log_error(&host, &device, &epoch, format!("event log request failed: {error}")));
                        }
                        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                        continue;
                    }
                };
                if !response.status().is_success() {
                    if task_alive.load(Ordering::Acquire) && service.session_epoch_is_current(&host, &device, &epoch).await {
                        let _ = events.send(event_log_error(&host, &device, &epoch, format!("event log request returned {}", response.status())));
                    }
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                    continue;
                }
                let mut stream = response.bytes_stream();
                let mut pending = String::new();
                while let Some(chunk) = stream.next().await {
                    let chunk = match chunk {
                        Ok(chunk) => chunk,
                        Err(error) => {
                            if task_alive.load(Ordering::Acquire) && service.session_epoch_is_current(&host, &device, &epoch).await {
                                let _ = events.send(event_log_error(&host, &device, &epoch, format!("event log stream failed: {error}")));
                            }
                            break;
                        }
                    };
                    if pending.len().saturating_add(chunk.len()) > MAX_STREAM_CHUNK {
                        // Drop this response and reconnect with a fresh
                        // bounded buffer.  Leaving a completed task in the
                        // ownership map would silently disable event updates
                        // until the device session is reopened.
                        pending.clear();
                        if task_alive.load(Ordering::Acquire) && service.session_epoch_is_current(&host, &device, &epoch).await {
                            let _ = events.send(event_log_error(&host, &device, &epoch, "event log response exceeded the Host limit"));
                        }
                        break;
                    }
                    pending.push_str(&String::from_utf8_lossy(&chunk));
                    while let Some(end) = pending.find('\n') {
                        let line = pending[..end].trim_end_matches('\r').to_owned();
                        pending.drain(..=end);
                        let Some(data) = line.strip_prefix("data:") else { continue; };
                        let raw = match serde_json::from_str::<serde_json::Value>(data.trim()) {
                            Ok(raw) => raw,
                            Err(error) => {
                                if task_alive.load(Ordering::Acquire) && service.session_epoch_is_current(&host, &device, &epoch).await {
                                    let _ = events.send(event_log_error(&host, &device, &epoch, format!("event log event was invalid: {error}")));
                                }
                                continue;
                            }
                        };
                        if !task_alive.load(Ordering::Acquire) || !service.session_epoch_is_current(&host, &device, &epoch).await {
                            return;
                        }
                        for entry in normalize_event_log(&host, &device, &epoch, raw) {
                            if !task_alive.load(Ordering::Acquire) {
                                return;
                            }
                            let _ = events.send(DeviceEvent::EventLog(entry));
                        }
                    }
                }
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
        });
        tasks.insert(key, EventLogTask { port, epoch, alive, task });
    }

    async fn session_epoch_is_current(&self, host_id: &str, device_id: &str, epoch: &str) -> bool {
        self.inner.state.read().await.sessions.iter().any(|session| {
            session.host_id == host_id && session.device_id == device_id && session.session_epoch == epoch
        })
    }

    async fn session_epoch_for_device(&self, host_id: &str, device_id: &str) -> String {
        self.inner
            .state
            .read()
            .await
            .sessions
            .iter()
            .find(|session| session.host_id == host_id && session.device_id == device_id)
            .map(|session| session.session_epoch.clone())
            .unwrap_or_default()
    }

    async fn append_recording(&self, thread: &ThreadId, session: &DeviceSession, frame: &TransportFrame) {
        let key = (thread.clone(), session.host_id.clone(), session.device_id.clone());
        let mut recordings = self.inner.recordings.lock().await;
        let status = if let Some(recording) = recordings.get_mut(&key) {
            let accepted = recording.recorder.format() == DeviceRecordingFormat::Mp4
                && matches!(frame.encoding, DeviceFrameEncoding::AvccDescription | DeviceFrameEncoding::H264 | DeviceFrameEncoding::Semu);
            if recording.error.is_none() {
                if !accepted {
                    recording.error = Some(format!("device {:?} recording cannot accept {:?} frames", recording.recorder.format(), frame.encoding));
                } else if let Err(error) = recording.recorder.push(frame) {
                    recording.error = Some(error);
                }
            }
            Some(DeviceRecordingStatus {
                thread_id: thread.clone(),
                host_id: session.host_id.clone(),
                device_id: session.device_id.clone(),
                format: recording.recorder.format(),
                file_name: DEVICE_RECORDING_FILE_NAME.into(),
                mime_type: DEVICE_RECORDING_MIME_TYPE.into(),
                active: true,
                started_at: recording.recorder.started_at().to_owned(),
                frame_count: recording.recorder.frame_count(),
                byte_count: recording.recorder.byte_count(),
                error: recording.error.clone(),
            })
        } else {
            None
        };
        drop(recordings);
        if let Some(status) = status {
            let _ = self.inner.events.send(DeviceEvent::Recording(status));
        }
    }

    pub async fn input(&self, input: DeviceInput) -> Result<(), String> {
        input.validate()?;
        let host_id = input.host_id.clone().unwrap_or_else(|| LOCAL_DEVICE_HOST_ID.into());
        let _guard = self.inner.operation.lock().await;
        let host = self.host(&host_id).await?;
        let device = self.find_device(&host_id, &input.device_id).await?;
        if let Some(port) = self.hub_port(&host_id).await {
            let screen = self.screen_config(&host_id, &device).await.ok();
            if device.platform == DevicePlatform::Ios && matches!(&input.input, DeviceInputKind::Fold { .. } | DeviceInputKind::Duo { .. }) {
                let Some(screen) = screen.as_ref() else {
                    return Err("Duo control state is unavailable".into());
                };
                if !screen.supports_hinge_angle {
                    return Err("device does not expose a Duo hinge".into());
                }
                if matches!(&input.input, DeviceInputKind::Duo { command: DeviceDuoCommand::Physical { .. } })
                    && !screen.supports_physical_orientation
                {
                    return Err("device does not expose physical Duo orientation".into());
                }
            }
            let input = match &input.input {
                DeviceInputKind::Rotate => {
                    let orientation = next_orientation(screen.as_ref().map(|screen| screen.orientation).unwrap_or(DeviceOrientation::Portrait));
                    if device.platform == DevicePlatform::Ios && screen.as_ref().is_some_and(|screen| screen.supports_hinge_angle) {
                        DeviceInputKind::Duo {
                            command: DeviceDuoCommand::Orientation { value: orientation },
                        }
                    } else {
                        DeviceInputKind::SetOrientation(orientation)
                    }
                }
                DeviceInputKind::Touch { phase, x, y } if device.platform == DevicePlatform::Ios => {
                    let (x, y) = rotate_touch(screen.as_ref(), *x, *y);
                    DeviceInputKind::Touch { phase: *phase, x, y }
                }
                _ => input.input.clone(),
            };
            let request_id = self.next_control_request_id(&input);
            if hub_input(port, device.platform, &device.id, &input, request_id).await.is_ok() {
                return Ok(());
            }
        }
        Err(format!("live input is unavailable for device {} on host {}", device.id, host.label()))
    }

    fn next_control_request_id(&self, input: &DeviceInputKind) -> u64 {
        if matches!(input, DeviceInputKind::Fold { .. } | DeviceInputKind::Duo { .. }) {
            self.inner.control_sequence.fetch_add(1, Ordering::Relaxed).saturating_add(1)
        } else {
            0
        }
    }

    pub async fn accessibility(&self, input: DeviceAccessibilityInput) -> Result<DeviceAccessibilityTree, String> {
        let _guard = self.inner.operation.lock().await;
        let host_id = input.host_id.unwrap_or_else(|| LOCAL_DEVICE_HOST_ID.into());
        let device = self.find_device(&host_id, &input.device_id).await?;
        let port = self.hub_port(&host_id).await.ok_or_else(|| "device hub is not running".to_owned())?;
        let epoch = self.session_epoch_for_device(&host_id, &device.id).await;
        let raw = match device.platform {
            DevicePlatform::Ios => hub_json_get(port, &format!("/vendor/serve-sim/helper/{}/ax", hub_device_component(&device.id)), &[]).await?,
            DevicePlatform::Android => hub_json_get(port, "/vendor/serve-emu/api/accessibility", &[("device", device.id.as_str())]).await?,
        };
        if epoch != self.session_epoch_for_device(&host_id, &device.id).await {
            return Err("device session changed while reading accessibility".into());
        }
        Ok(normalize_accessibility(&host_id, &device.id, &epoch, device.platform, raw))
    }

    pub async fn event_log(&self, input: DeviceEventLogInput) -> Result<Vec<DeviceEventLogEntry>, String> {
        let _guard = self.inner.operation.lock().await;
        let host_id = input.host_id.unwrap_or_else(|| LOCAL_DEVICE_HOST_ID.into());
        let device = self.find_device(&host_id, &input.device_id).await?;
        let port = self.hub_port(&host_id).await.ok_or_else(|| "device hub is not running".to_owned())?;
        if device.platform != DevicePlatform::Ios { return Ok(Vec::new()); }
        let limit = input.limit.clamp(1, 100).to_string();
        let epoch = self.session_epoch_for_device(&host_id, &device.id).await;
        let raw = hub_event_log_payload(port, device.id.as_str(), &limit).await?;
        if epoch != self.session_epoch_for_device(&host_id, &device.id).await {
            return Err("device session changed while reading the event log".into());
        }
        Ok(normalize_event_log(&host_id, &device.id, &epoch, raw))
    }

    pub async fn start_recording(&self, input: DeviceRecordingStartInput) -> Result<DeviceRecordingStatus, String> {
        let _guard = self.inner.operation.lock().await;
        let host_id = input.host_id.unwrap_or_else(|| LOCAL_DEVICE_HOST_ID.into());
        let device = self.find_device(&host_id, &input.device_id).await?;
        if input.format != DeviceRecordingFormat::Mp4 {
            return Err("device recording requires the MP4 format".into());
        }
        let key = (input.thread_id.clone(), host_id.clone(), device.id.clone());
        if self.inner.recordings.lock().await.contains_key(&key) {
            return Err("device recording is already active".into());
        }
        let session = self
            .sessions_for_thread(&input.thread_id)
            .await
            .into_iter()
            .find(|session| session.host_id == host_id && session.device_id == device.id);
        let session_epoch = session.as_ref().map(|session| session.session_epoch.clone());
        let started_at = now_iso();
        let session = session.unwrap_or(DeviceSession { thread_id: input.thread_id.clone(), host_id: host_id.clone(), device_id: device.id.clone(), platform: device.platform, opened_at: started_at.clone(), session_epoch: started_at.clone() });
        let status = DeviceRecordingStatus { thread_id: input.thread_id.clone(), host_id: host_id.clone(), device_id: device.id.clone(), format: input.format, file_name: DEVICE_RECORDING_FILE_NAME.into(), mime_type: DEVICE_RECORDING_MIME_TYPE.into(), active: true, started_at: started_at.clone(), frame_count: 0, byte_count: 0, error: None };
        self.inner.recordings.lock().await.insert(key.clone(), ActiveDeviceRecording { recorder: Mp4Recorder::new(input.format, started_at), error: None, session_epoch });
        let _ = self
            .retain_capture_source(&session, false)
            .await;
        let _ = self.inner.events.send(DeviceEvent::Recording(status.clone()));
        Ok(status)
    }

    pub async fn stop_recording(&self, input: DeviceRecordingStopInput) -> Result<DeviceRecording, String> {
        let _guard = self.inner.operation.lock().await;
        let host_id = input.host_id.unwrap_or_else(|| LOCAL_DEVICE_HOST_ID.into());
        let key = (input.thread_id.clone(), host_id.clone(), input.device_id.clone());
        let recording = self.inner.recordings.lock().await.remove(&key).ok_or_else(|| "device recording is not active".to_owned())?;
        let source_key = (host_id.clone(), input.device_id.clone());
        let result = finish_recording(input.thread_id, host_id, input.device_id, recording);
        self.release_capture_source(&source_key).await;
        let _ = self.inner.events.send(DeviceEvent::Recording(result.status.clone()));
        let _ = self.inner.events.send(DeviceEvent::RecordingComplete(result.clone()));
        Ok(result)
    }

    async fn hub_port(&self, host_id: &str) -> Option<u16> {
        if host_id == LOCAL_DEVICE_HOST_ID {
            let mut hub = self.inner.hub.lock().await;
            let port = hub.as_mut().and_then(|current| match current.child.try_wait() {
                Ok(None) => Some(current.port),
                Ok(Some(_)) | Err(_) => None,
            });
            if let Some(port) = port {
                return Some(port);
            }
            let stale = hub.take();
            drop(hub);
            if let Some(mut stale) = stale {
                let _ = stale.child.wait().await;
            }
            return None;
        }
        let mut hubs = self.inner.remote_hubs.lock().await;
        let port = hubs.get_mut(host_id).and_then(|current| match current.tunnel.try_wait() {
            Ok(None) => Some(current.local_port),
            Ok(Some(_)) | Err(_) => None,
        });
        if let Some(port) = port {
            return Some(port);
        }
        let stale = hubs.remove(host_id);
        drop(hubs);
        if let Some(mut stale) = stale {
            let _ = stale.tunnel.wait().await;
        }
        None
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

/// Reattach to the pinned remote agent recorded by the SSH lifecycle.  The
/// forwarding process belongs to this Host instance, so the persisted remote
/// endpoint is reused only after a fresh local health probe succeeds.
async fn persisted_remote_agent(
    host: &dyn DeviceHostRunner,
    state_dir: &Path,
) -> Option<(u16, String, ForwardedPort)> {
    let _record = tokio::fs::read(state_dir.join("agent.json"))
        .await
        .ok()
        .and_then(|bytes| serde_json::from_slice::<PersistedAgentState>(&bytes).ok())
        .filter(|record| record.version == AGENT_VERSION && !record.entry_path.is_empty())?;
    let daemon = tokio::fs::read(state_dir.join("daemon.json"))
        .await
        .ok()
        .and_then(|bytes| serde_json::from_slice::<AgentDaemonState>(&bytes).ok())
        .filter(|daemon| daemon.http_port != 0 && !daemon.token.is_empty())?;
    let mut tunnel = host.forward(daemon.http_port).await.ok().flatten()?;
    if loopback_http_ok(tunnel.local_port, "/health").await {
        return Some((tunnel.local_port, daemon.token, tunnel));
    }
    let _ = tunnel.child.kill().await;
    let _ = tunnel.child.wait().await;
    None
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
                if let Err(error) = write_private_json(
                    &agent_file,
                    &PersistedAgentState {
                        entry_path: entry_string.clone(),
                        version: AGENT_VERSION.into(),
                    },
                )
                .await
                {
                    let _ = child.kill().await;
                    let _ = child.wait().await;
                    let _ = tokio::fs::remove_file(&daemon_file).await;
                    return Err(error);
                }
                tokio::spawn(async move { let _ = child.wait().await; });
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
    let agent_installed = agent_versions.iter().any(|version| version == AGENT_VERSION);
    let hub_running = std::fs::read(root.join("hub.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<PersistedHubState>(&bytes).ok())
        .filter(|state| state.version == HUB_VERSION && process_matches_entry(state.pid, &state.entry_path))
        .map(|state| state.version);
    let agent_running = std::fs::read(root.join("agent-device").join("hosts").join(LOCAL_DEVICE_HOST_ID).join("agent.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<PersistedAgentState>(&bytes).ok())
        .filter(|state| state.version == AGENT_VERSION)
        .filter(|state| {
            std::fs::read(root.join("agent-device").join("hosts").join(LOCAL_DEVICE_HOST_ID).join("daemon.json"))
                .ok()
                .and_then(|bytes| serde_json::from_slice::<AgentDaemonState>(&bytes).ok())
                .is_some_and(|daemon| process_for_port_is_alive(daemon.http_port, &state.entry_path))
        })
        .map(|state| state.version);
    (
        hub_installed,
        agent_installed,
        DeviceToolVersions {
            hub: DeviceToolVersion {
                required_version: HUB_VERSION.into(),
                installed_versions: hub_versions,
                running_version: hub_running,
            },
            agent: DeviceToolVersion {
                required_version: AGENT_VERSION.into(),
                installed_versions: agent_versions,
                running_version: agent_running,
            },
        },
    )
}

fn process_matches_entry(pid: u32, entry: &str) -> bool {
    let Ok(output) = std::process::Command::new("ps").args(["-p", &pid.to_string(), "-o", "command="]).output() else { return false; };
    output.status.success() && String::from_utf8_lossy(&output.stdout).contains(entry)
}

fn process_for_port_is_alive(_port: u16, entry: &str) -> bool {
    // agent-device records the entry path separately from its daemon endpoint;
    // process scanning is intentionally conservative and never treats a stale
    // endpoint file as a running version.
    let Ok(output) = std::process::Command::new("ps").args(["-ax", "-o", "command="]).output() else { return false; };
    output.status.success() && String::from_utf8_lossy(&output.stdout).contains(entry)
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

fn hub_device_component(device_id: &str) -> String {
    url::form_urlencoded::byte_serialize(device_id.as_bytes()).collect()
}

#[cfg(test)]
async fn hub_avcc_frame(
    port: u16,
    device_id: &str,
    panel_id: Option<u8>,
) -> Result<Vec<TransportFrame>, String> {
    let device = hub_device_component(device_id);
    let path = match panel_id {
        Some(panel @ (1 | 3)) => format!("/vendor/serve-sim/helper/{device}/panel/{panel}/stream.avcc"),
        _ => format!("/vendor/serve-sim/helper/{device}/stream.avcc"),
    };
    let response = reqwest::Client::new()
        .get(format!("http://127.0.0.1:{port}{path}"))
        .timeout(std::time::Duration::from_secs(3))
        .send()
        .await
        .map_err(|error| format!("device AVCC stream failed: {error}"))?;
    if !response.status().is_success() {
        return Err(format!("device AVCC stream returned {}", response.status()));
    }
    let mut demuxer = AvccDemuxer::default();
    let mut stream = response.bytes_stream();
    let mut result = Vec::new();
    loop {
        let chunk = match tokio::time::timeout(Duration::from_secs(1), stream.next()).await {
            Ok(Some(Ok(chunk))) => chunk,
            Ok(Some(Err(error))) => {
                return if result.is_empty() {
                    Err(format!("device AVCC stream body failed: {error}"))
                } else {
                    Ok(result)
                };
            }
            Ok(None) => break,
            Err(_) => {
                return if result.is_empty() {
                    Err("device AVCC stream did not deliver a frame within 1 second".into())
                } else {
                    Ok(result)
                };
            }
        };
        for frame in demuxer.push(&chunk)? {
            let (encoding, keyframe) = match frame.kind {
                AvccChunkKind::Description => (DeviceFrameEncoding::AvccDescription, true),
                AvccChunkKind::Keyframe => (DeviceFrameEncoding::H264, true),
                AvccChunkKind::Delta => (DeviceFrameEncoding::H264, false),
                AvccChunkKind::Seed => (DeviceFrameEncoding::Jpeg, true),
            };
            result.push(TransportFrame {
                payload: frame.payload,
                encoding,
                keyframe,
                timestamp_us: None,
                screen_id: panel_id,
            });
            if result.len() >= 4 {
                return Ok(result);
            }
        }
        if result.iter().any(|frame| frame.encoding == DeviceFrameEncoding::H264) {
            return Ok(result);
        }
    }
    if result.is_empty() { Err("device AVCC stream ended without a complete frame".into()) } else { Ok(result) }
}

async fn sleep_until_cancelled(cancel: &CancellationToken, duration: Duration) -> bool {
    tokio::select! {
        _ = cancel.cancelled() => false,
        _ = tokio::time::sleep(duration) => true,
    }
}

fn session_matches_generation(generation: &BTreeMap<String, String>, session: &DeviceSession) -> bool {
    generation.get(session.thread_id.as_str()) == Some(&session.session_epoch)
}

async fn persistent_avcc_reader(
    port: u16,
    device_id: String,
    panel_id: Option<u8>,
    epoch: String,
    width: u32,
    height: u32,
    sender: mpsc::Sender<SourceMessage>,
    cancel: CancellationToken,
) {
    let device = hub_device_component(&device_id);
    let path = match panel_id {
        Some(panel @ (1 | 3)) => format!("/vendor/serve-sim/helper/{device}/panel/{panel}/stream.avcc"),
        _ => format!("/vendor/serve-sim/helper/{device}/stream.avcc"),
    };
    let client = reqwest::Client::new();
    loop {
        if cancel.is_cancelled() {
            return;
        }
        let response = match client.get(format!("http://127.0.0.1:{port}{path}")).send().await {
            Ok(response) if response.status().is_success() => response,
            Ok(_) | Err(_) => {
                let _ = sender.try_send(SourceMessage::TransportEnded { epoch: epoch.clone() });
                continue;
            }
        };
        let mut stream = response.bytes_stream();
        let mut demuxer = AvccDemuxer::default();
        let mut ended = false;
        while !ended {
            let chunk = tokio::select! {
                _ = cancel.cancelled() => return,
                chunk = stream.next() => chunk,
            };
            let Some(chunk) = chunk else { break };
            let chunk = match chunk {
                Ok(chunk) => chunk,
                Err(_) => { let _ = sender.try_send(SourceMessage::TransportEnded { epoch: epoch.clone() }); ended = true; continue; }
            };
            let frames = match demuxer.push(&chunk) {
                Ok(frames) => frames,
                Err(_) => { let _ = sender.try_send(SourceMessage::TransportEnded { epoch: epoch.clone() }); ended = true; continue; }
            };
            for frame in frames {
                let (encoding, keyframe) = match frame.kind {
                    AvccChunkKind::Description => (DeviceFrameEncoding::AvccDescription, true),
                    AvccChunkKind::Keyframe => (DeviceFrameEncoding::H264, true),
                    AvccChunkKind::Delta => (DeviceFrameEncoding::H264, false),
                    AvccChunkKind::Seed => (DeviceFrameEncoding::Jpeg, true),
                };
                let message = SourceMessage::Frame {
                    epoch: epoch.clone(),
                    frame: TransportFrame { payload: frame.payload, encoding, keyframe, timestamp_us: None, screen_id: panel_id },
                    width,
                    height,
                };
                match sender.try_send(message) {
                    Ok(()) | Err(mpsc::error::TrySendError::Full(_)) => {}
                    Err(mpsc::error::TrySendError::Closed(_)) => return,
                }
            }
        }
        if !cancel.is_cancelled() {
            let _ = sender.try_send(SourceMessage::TransportEnded { epoch: epoch.clone() });
        }
        if cancel.is_cancelled() { return; }
        if !sleep_until_cancelled(&cancel, Duration::from_millis(100)).await { return; }
    }
}

async fn persistent_mjpeg_reader(
    port: u16,
    device_id: String,
    epoch: String,
    width: u32,
    height: u32,
    sender: mpsc::Sender<SourceMessage>,
    cancel: CancellationToken,
) {
    let device = hub_device_component(&device_id);
    let client = reqwest::Client::new();
    loop {
        if cancel.is_cancelled() { return; }
        let response = match client.get(format!("http://127.0.0.1:{port}/vendor/serve-sim/helper/{device}/stream.mjpeg")).send().await {
            Ok(response) if response.status().is_success() => response,
            Ok(_) | Err(_) => {
                if !sleep_until_cancelled(&cancel, Duration::from_millis(250)).await { return; }
                continue;
            }
        };
        let mut stream = response.bytes_stream();
        let mut pending = Vec::new();
        loop {
            let chunk = tokio::select! {
                _ = cancel.cancelled() => return,
                chunk = stream.next() => chunk,
            };
            let Some(chunk) = chunk else { break };
            let chunk = match chunk { Ok(chunk) => chunk, Err(_) => break };
            if pending.len().saturating_add(chunk.len()) > MAX_STREAM_CHUNK { break; }
            pending.extend_from_slice(&chunk);
            while let Some((start, end)) = jpeg_bounds(&pending) {
                let payload = pending[start..end].to_vec();
                pending.drain(..end);
                let message = SourceMessage::Frame {
                    epoch: epoch.clone(),
                    frame: TransportFrame { payload, encoding: DeviceFrameEncoding::Mjpeg, keyframe: true, timestamp_us: None, screen_id: None },
                    width,
                    height,
                };
                match sender.try_send(message) {
                    Ok(()) | Err(mpsc::error::TrySendError::Full(_)) => {}
                    Err(mpsc::error::TrySendError::Closed(_)) => return,
                }
            }
        }
        if !sleep_until_cancelled(&cancel, Duration::from_millis(100)).await { return; }
    }
}

async fn persistent_semu_reader(
    port: u16,
    device_id: String,
    epoch: String,
    width: u32,
    height: u32,
    sender: mpsc::Sender<SourceMessage>,
    cancel: CancellationToken,
) {
    let device = hub_device_component(&device_id);
    let url = format!("ws://127.0.0.1:{port}/vendor/serve-emu/ws?device={device}&frame-meta=1");
    loop {
        if cancel.is_cancelled() { return; }
        let connection = tokio::select! {
            _ = cancel.cancelled() => return,
            result = tokio::time::timeout(Duration::from_secs(10), async_tungstenite::tokio::connect_async(&url)) => result,
        };
        let (mut socket, _) = match connection {
            Ok(Ok(connection)) => connection,
            Ok(Err(_)) | Err(_) => {
                let _ = sender.try_send(SourceMessage::TransportEnded { epoch: epoch.clone() });
                if !sleep_until_cancelled(&cancel, Duration::from_millis(250)).await { return; }
                continue;
            }
        };
        loop {
            let message = tokio::select! {
                _ = cancel.cancelled() => return,
                message = socket.next() => message,
            };
            let Some(message) = message else { break };
            let message = match message { Ok(message) => message, Err(_) => break };
            let async_tungstenite::tungstenite::Message::Binary(bytes) = message else { continue };
            if bytes.len() > MAX_STREAM_CHUNK { break; }
            let frame = parse_semu_packet(&bytes);
            let message = SourceMessage::Frame { epoch: epoch.clone(), frame, width, height };
            match sender.try_send(message) {
                Ok(()) | Err(mpsc::error::TrySendError::Full(_)) => {}
                Err(mpsc::error::TrySendError::Closed(_)) => return,
            }
        }
        if !cancel.is_cancelled() {
            let _ = sender.try_send(SourceMessage::TransportEnded { epoch: epoch.clone() });
        }
        if !sleep_until_cancelled(&cancel, Duration::from_millis(100)).await { return; }
    }
}

async fn screen_config_reader(
    port: u16,
    platform: DevicePlatform,
    device_id: String,
    epoch: String,
    sender: mpsc::Sender<SourceMessage>,
    cancel: CancellationToken,
) {
    loop {
        if cancel.is_cancelled() { return; }
        if let Ok(screen) = hub_screen_config(port, platform, &device_id).await {
            match sender.try_send(SourceMessage::Screen { epoch: epoch.clone(), screen }) {
                Ok(()) | Err(mpsc::error::TrySendError::Full(_)) => {}
                Err(mpsc::error::TrySendError::Closed(_)) => return,
            }
        }
        if !sleep_until_cancelled(&cancel, Duration::from_secs(1)).await { return; }
    }
}

async fn foreground_reader(
    port: u16,
    device_id: String,
    epoch: String,
    sender: mpsc::Sender<SourceMessage>,
    cancel: CancellationToken,
) {
    loop {
        if cancel.is_cancelled() { return; }
        if let Ok(raw) = hub_sse_first_json(port, "/vendor/serve-sim/appstate", &[("device", device_id.as_str())], "foreground").await {
            let app = raw.get("bundleId").and_then(serde_json::Value::as_str)
                .filter(|id| !id.is_empty())
                .map(|id| DeviceForegroundApp { id: id.into(), name: None, version: None });
            match sender.try_send(SourceMessage::Foreground { epoch: epoch.clone(), app }) {
                Ok(()) | Err(mpsc::error::TrySendError::Full(_)) => {}
                Err(mpsc::error::TrySendError::Closed(_)) => return,
            }
        }
        if !sleep_until_cancelled(&cancel, Duration::from_secs(1)).await { return; }
    }
}

#[cfg(test)]
async fn hub_mjpeg_frame(port: u16, device_id: &str) -> Result<TransportFrame, String> {
    let device = hub_device_component(device_id);
    let response = reqwest::Client::new()
        .get(format!("http://127.0.0.1:{port}/vendor/serve-sim/helper/{device}/stream.mjpeg"))
        .timeout(std::time::Duration::from_secs(3))
        .send()
        .await
        .map_err(|error| format!("device MJPEG stream failed: {error}"))?;
    if !response.status().is_success() { return Err(format!("device MJPEG stream returned {}", response.status())); }
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| format!("device MJPEG stream body failed: {error}"))?;
        if bytes.len().saturating_add(chunk.len()) > MAX_STREAM_CHUNK { return Err("device MJPEG frame exceeded the Host limit".into()); }
        bytes.extend_from_slice(&chunk);
        if let Some((start, end)) = jpeg_bounds(&bytes) {
            return Ok(TransportFrame { payload: bytes[start..end].to_vec(), encoding: DeviceFrameEncoding::Mjpeg, keyframe: true, timestamp_us: None, screen_id: None });
        }
    }
    Err("device MJPEG stream ended without a complete frame".into())
}

#[cfg(test)]
async fn hub_android_frame(port: u16, device_id: &str) -> Result<TransportFrame, String> {
    use futures_util::SinkExt;
    let device = hub_device_component(device_id);
    let url = format!("ws://127.0.0.1:{port}/vendor/serve-emu/ws?device={device}&frame-meta=1");
    let (mut socket, _) = tokio::time::timeout(
        Duration::from_secs(10),
        async_tungstenite::tokio::connect_async(url),
    )
    .await
    .map_err(|_| "device SEMU stream connection timed out".to_owned())?
    .map_err(|error| format!("device SEMU stream failed: {error}"))?;
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err("device SEMU stream did not deliver a frame within 3 seconds".into());
        }
        let message = tokio::time::timeout(remaining, socket.next())
            .await
            .map_err(|_| "device SEMU stream did not deliver a frame within 3 seconds".to_owned())?;
        let Some(message) = message else { break };
        let message = message.map_err(|error| format!("device SEMU frame failed: {error}"))?;
        if let async_tungstenite::tungstenite::Message::Binary(bytes) = message {
            if bytes.len() > MAX_STREAM_CHUNK { return Err("device SEMU frame exceeded the Host limit".into()); }
            let frame = parse_semu_packet(&bytes);
            let _ = socket.close(None).await;
            return Ok(frame);
        }
    }
    Err("device SEMU stream closed without a frame".into())
}

async fn hub_screen_config(port: u16, platform: DevicePlatform, device_id: &str) -> Result<DeviceScreenConfig, String> {
    let device = hub_device_component(device_id);
    let (vendor, path) = match platform {
        DevicePlatform::Ios => ("serve-sim", format!("/helper/{device}/config")),
        DevicePlatform::Android => ("serve-emu", "/api/stream-settings".into()),
    };
    let response = reqwest::Client::new()
        .get(format!("http://127.0.0.1:{port}/vendor/{vendor}{path}"))
        .query(&[("device", device_id)])
        .timeout(std::time::Duration::from_secs(10))
        .send().await.map_err(|error| format!("device screen config failed: {error}"))?;
    if !response.status().is_success() { return Err(format!("device screen config returned {}", response.status())); }
    let raw = response.json::<serde_json::Value>().await.map_err(|error| format!("device screen config was invalid: {error}"))?;
    let source = raw.get("screen").unwrap_or(&raw);
    let number = |key: &str, default| source.get(key).and_then(serde_json::Value::as_u64).unwrap_or(default) as u32;
    let orientation = match source.get("orientation").and_then(serde_json::Value::as_str).unwrap_or("portrait") {
        "portrait_upside_down" => DeviceOrientation::PortraitUpsideDown,
        "landscape_left" => DeviceOrientation::LandscapeLeft,
        "landscape_right" => DeviceOrientation::LandscapeRight,
        _ => DeviceOrientation::Portrait,
    };
    Ok(DeviceScreenConfig {
        thread_id: None,
        session_epoch: String::new(),
        host_id: None,
        device_id: None,
        width: number("width", 1).max(1), height: number("height", 1).max(1), orientation,
        screen_id: source.get("screenId").and_then(serde_json::Value::as_u64).and_then(|value| u8::try_from(value).ok()),
        supports_hinge_angle: source.get("supportsHingeAngle").and_then(serde_json::Value::as_bool).unwrap_or(false),
        supports_physical_orientation: source.get("supportsPhysicalOrientation").and_then(serde_json::Value::as_bool).unwrap_or(false),
        hinge_angle: source.get("hingeAngle").and_then(serde_json::Value::as_f64).map(|value| value as f32),
        hinge_pose: source.get("hingePose").and_then(serde_json::Value::as_str).map(str::to_owned),
        table_mode: source.get("tableMode").and_then(serde_json::Value::as_bool).unwrap_or(false),
        table_mode_available: source.get("tableModeAvailable").and_then(serde_json::Value::as_bool).unwrap_or(false),
    })
}

async fn hub_json_get(port: u16, path: &str, query: &[(&str, &str)]) -> Result<serde_json::Value, String> {
    let response = reqwest::Client::new()
        .get(format!("http://127.0.0.1:{port}{path}"))
        .query(query)
        .timeout(std::time::Duration::from_secs(10))
        .send().await.map_err(|error| format!("device hub request failed: {error}"))?;
    if !response.status().is_success() { return Err(format!("device hub request returned {}", response.status())); }
    response.json().await.map_err(|error| format!("device hub returned invalid JSON: {error}"))
}

async fn hub_sse_first_json(
    port: u16,
    path: &str,
    query: &[(&str, &str)],
    label: &str,
) -> Result<serde_json::Value, String> {
    let response = reqwest::Client::new()
        .get(format!("http://127.0.0.1:{port}{path}"))
        .query(query)
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .map_err(|error| format!("device {label} stream failed: {error}"))?;
    if !response.status().is_success() {
        return Err(format!("device {label} stream returned {}", response.status()));
    }
    let mut stream = response.bytes_stream();
    let mut pending = String::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| format!("device {label} stream body failed: {error}"))?;
        if pending.len().saturating_add(chunk.len()) > MAX_STREAM_CHUNK {
            return Err(format!("device {label} stream exceeded the Host limit"));
        }
        pending.push_str(&String::from_utf8_lossy(&chunk));
        while let Some(end) = pending.find('\n') {
            let line = pending[..end].trim_end_matches('\r').to_owned();
            pending.drain(..=end);
            if let Some(data) = line.strip_prefix("data:") {
                return serde_json::from_str(data.trim()).map_err(|error| format!("device {label} event was invalid: {error}"));
            }
        }
    }
    if let Some(data) = pending.trim().strip_prefix("data:") {
        return serde_json::from_str(data.trim()).map_err(|error| format!("device {label} event was invalid: {error}"));
    }
    Err(format!("device {label} stream ended without an event"))
}

async fn hub_event_log_payload(port: u16, device_id: &str, limit: &str) -> Result<serde_json::Value, String> {
    hub_sse_first_json(
        port,
        "/vendor/serve-sim/api/event-log/events",
        &[("device", device_id), ("limit", limit)],
        "event log",
    )
    .await
}

fn ios_hid_usage(code: &str) -> Option<u8> {
    if let Some(letter) = code.strip_prefix("Key").and_then(|value| value.as_bytes().first()).copied().filter(|value| value.is_ascii_uppercase()) { return Some(0x04 + letter - b'A'); }
    if let Some(digit) = code.strip_prefix("Digit").and_then(|value| value.as_bytes().first()).copied() { return if digit == b'0' { Some(0x27) } else if (b'1'..=b'9').contains(&digit) { Some(0x1e + digit - b'1') } else { None }; }
    Some(match code {
        "Enter" => 0x28,
        "Escape" => 0x29,
        "Backspace" => 0x2a,
        "Tab" => 0x2b,
        "Space" => 0x2c,
        "Minus" => 0x2d,
        "Equal" => 0x2e,
        "BracketLeft" => 0x2f,
        "BracketRight" => 0x30,
        "Backslash" => 0x31,
        "Semicolon" => 0x33,
        "Quote" => 0x34,
        "Backquote" => 0x35,
        "Comma" => 0x36,
        "Period" => 0x37,
        "Slash" => 0x38,
        "Delete" => 0x4c,
        "ArrowRight" => 0x4f,
        "ArrowLeft" => 0x50,
        "ArrowDown" => 0x51,
        "ArrowUp" => 0x52,
        "ControlLeft" => 0xe0,
        "ShiftLeft" => 0xe1,
        "AltLeft" => 0xe2,
        "MetaLeft" => 0xe3,
        "ControlRight" => 0xe4,
        "ShiftRight" => 0xe5,
        "AltRight" => 0xe6,
        "MetaRight" => 0xe7,
        _ => return None,
    })
}

fn android_keycode(key: &str) -> Option<u16> {
    Some(match key { "ArrowUp" => 19, "ArrowDown" => 20, "ArrowLeft" => 21, "ArrowRight" => 22, "Tab" => 61, "Enter" => 66, "Backspace" => 67, "Delete" => 112, "Home" => 122, "End" => 123, "PageUp" => 92, "PageDown" => 93, _ => return None })
}

fn button_wire(button: DeviceHardwareButton, ios: bool) -> &'static str {
    match button {
        DeviceHardwareButton::Home => "home",
        DeviceHardwareButton::Back => "back",
        DeviceHardwareButton::Recents => "recents",
        DeviceHardwareButton::Power => if ios { "lock" } else { "power" },
        DeviceHardwareButton::AppSwitcher => "app_switcher",
    }
}

async fn hub_input(
    port: u16,
    platform: DevicePlatform,
    device_id: &str,
    input: &DeviceInputKind,
    request_id: u64,
) -> Result<(), String> {
    if platform == DevicePlatform::Android
        && matches!(input, DeviceInputKind::Key { down: false, .. })
    {
        return Ok(());
    }
    if platform == DevicePlatform::Android {
        if let DeviceInputKind::Fold { command } = input {
            let result = hub_action(
                port,
                &format!("/vendor/serve-emu/api/fold?device={}", hub_device_component(device_id)),
                serde_json::json!({"posture": fold_posture_wire(*command)}),
            )
            .await?;
            if !result.ok { return Err(result.error.unwrap_or_else(|| "Android fold command failed".into())); }
            return Ok(());
        }
        if matches!(input, DeviceInputKind::Duo { .. }) {
            return Err("Duo controls require an iOS hinge stream".into());
        }
    }
    let device = hub_device_component(device_id);
    let path = match platform { DevicePlatform::Ios => format!("/vendor/serve-sim/helper/ws?device={device}"), DevicePlatform::Android => format!("/vendor/serve-emu/ws?device={device}&frame-meta=1") };
    let url = format!("ws://127.0.0.1:{port}{path}");
    let (mut socket, _) = tokio::time::timeout(
        Duration::from_secs(10),
        async_tungstenite::tokio::connect_async(url),
    )
    .await
    .map_err(|_| "device input stream connection timed out".to_owned())?
    .map_err(|error| format!("device input stream failed: {error}"))?;
    let message = match (platform, input) {
        (DevicePlatform::Ios, DeviceInputKind::Touch { phase, x, y }) => {
            let phase = match phase { DeviceTouchPhase::Begin => "begin", DeviceTouchPhase::Move => "move", DeviceTouchPhase::End => "end" };
            async_tungstenite::tungstenite::Message::binary([vec![0x03], serde_json::to_vec(&serde_json::json!({"type": phase, "x": x, "y": y})).map_err(|error| error.to_string())?].concat())
        }
        (DevicePlatform::Ios, DeviceInputKind::Key { code, down, .. }) => {
            let usage = ios_hid_usage(code).ok_or_else(|| format!("unsupported iOS keyboard code {code}"))?;
            async_tungstenite::tungstenite::Message::binary([vec![0x06], serde_json::to_vec(&serde_json::json!({"type": if *down { "down" } else { "up" }, "usage": usage})).map_err(|error| error.to_string())?].concat())
        }
        (_, DeviceInputKind::Touch { phase, x, y }) => {
            let action = match phase { DeviceTouchPhase::Begin => "down", DeviceTouchPhase::Move => "move", DeviceTouchPhase::End => "up" };
            async_tungstenite::tungstenite::Message::Text(serde_json::json!({"type":"touch", "action": action, "x": x, "y": y}).to_string().into())
        }
        (DevicePlatform::Android, DeviceInputKind::Key { key, meta, ctrl, .. }) => {
            if key == "Escape" { async_tungstenite::tungstenite::Message::Text(serde_json::json!({"type":"back"}).to_string().into()) }
            else if let Some(keycode) = android_keycode(key) { async_tungstenite::tungstenite::Message::Text(serde_json::json!({"type":"key", "keycode": keycode}).to_string().into()) }
            else if key.encode_utf16().count() == 1 && !meta && !ctrl { async_tungstenite::tungstenite::Message::Text(serde_json::json!({"type":"text", "text": key}).to_string().into()) }
            else { return Err(format!("unsupported Android keyboard key {key}")); }
        }
        (DevicePlatform::Ios, DeviceInputKind::HardwareButton(button)) => async_tungstenite::tungstenite::Message::binary([vec![0x04], serde_json::to_vec(&serde_json::json!({"button": button_wire(*button, true)})).map_err(|error| error.to_string())?].concat()),
        (_, DeviceInputKind::HardwareButton(button)) => async_tungstenite::tungstenite::Message::Text(serde_json::json!({"type": button_wire(*button, false)}).to_string().into()),
        (DevicePlatform::Ios, DeviceInputKind::Rotate) => async_tungstenite::tungstenite::Message::binary([vec![0x07], serde_json::to_vec(&serde_json::json!({"orientation": "rotate"})).map_err(|error| error.to_string())?].concat()),
        (DevicePlatform::Ios, DeviceInputKind::SetOrientation(orientation)) => async_tungstenite::tungstenite::Message::binary([vec![0x07], serde_json::to_vec(&serde_json::json!({"orientation": orientation_wire(*orientation)})).map_err(|error| error.to_string())?].concat()),
        (_, DeviceInputKind::Rotate) => async_tungstenite::tungstenite::Message::Text(serde_json::json!({"type":"rotate"}).to_string().into()),
        (_, DeviceInputKind::SetOrientation(orientation)) => async_tungstenite::tungstenite::Message::Text(serde_json::json!({"type":"orientation", "orientation": orientation_wire(*orientation)}).to_string().into()),
        (DevicePlatform::Ios, DeviceInputKind::Fold { command }) => {
            let pose = match command {
                DeviceFoldPosture::Closed => DeviceDuoPose::Closed,
                DeviceFoldPosture::Opened => DeviceDuoPose::Open,
            };
            let command = DeviceDuoCommand::Pose { value: pose };
            async_tungstenite::tungstenite::Message::binary(
                [
                    vec![0x10],
                    serde_json::to_vec(&serde_json::json!({"requestId": request_id, "command": duo_command_wire(&command)}))
                        .map_err(|error| error.to_string())?,
                ]
                .concat(),
            )
        }
        (DevicePlatform::Ios, DeviceInputKind::Duo { command: DeviceDuoCommand::Orientation { value } }) => {
            async_tungstenite::tungstenite::Message::binary(
                [
                    vec![0x07],
                    serde_json::to_vec(&serde_json::json!({"orientation": orientation_wire(*value)}))
                        .map_err(|error| error.to_string())?,
                ]
                .concat(),
            )
        }
        (DevicePlatform::Ios, DeviceInputKind::Duo { command }) => {
            command.validate()?;
            async_tungstenite::tungstenite::Message::binary(
                [
                    vec![0x10],
                    serde_json::to_vec(&serde_json::json!({"requestId": request_id, "command": duo_command_wire(command)}))
                        .map_err(|error| error.to_string())?,
                ]
                .concat(),
            )
        }
        (DevicePlatform::Android, DeviceInputKind::Fold { .. } | DeviceInputKind::Duo { .. }) => {
            return Err("Android fold controls must use the typed fold route".into());
        }
    };
    use futures_util::SinkExt;
    tokio::time::timeout(Duration::from_secs(5), socket.send(message))
        .await
        .map_err(|_| "device input send timed out".to_owned())?
        .map_err(|error| format!("device input failed: {error}"))?;
    if platform == DevicePlatform::Ios && matches!(input, DeviceInputKind::Fold { .. } | DeviceInputKind::Duo { .. }) {
        let expected_orientation = match input {
            DeviceInputKind::Duo { command: DeviceDuoCommand::Orientation { value } } => Some(*value),
            _ => None,
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err("device control acknowledgement timed out".into());
            }
            let message = tokio::time::timeout(remaining, socket.next())
                .await
                .map_err(|_| "device control acknowledgement timed out".to_owned())?
                .ok_or_else(|| "device control stream closed before acknowledgement".to_owned())?
                .map_err(|error| format!("device control acknowledgement failed: {error}"))?;
            let async_tungstenite::tungstenite::Message::Binary(bytes) = message else { continue };
            if let Some(expected) = expected_orientation
                && bytes.first() == Some(&0x82)
            {
                let payload = serde_json::from_slice::<serde_json::Value>(&bytes[1..])
                    .map_err(|error| format!("device orientation readback was invalid: {error}"))?;
                if payload.get("orientation").and_then(serde_json::Value::as_str) == Some(orientation_wire(expected)) {
                    break;
                }
                continue;
            }
            if bytes.first() != Some(&0x90) {
                continue;
            }
            let reply: serde_json::Value = serde_json::from_slice(&bytes[1..]).map_err(|error| format!("device control reply was invalid: {error}"))?;
            if reply.get("requestId").and_then(serde_json::Value::as_u64) != Some(request_id) {
                continue;
            }
            if reply.get("ok").and_then(serde_json::Value::as_bool) != Some(true) {
                return Err(reply.get("error").and_then(serde_json::Value::as_str).unwrap_or("device control failed").into());
            }
            break;
        }
    }
    let _ = socket.close(None).await;
    Ok(())
}

fn json_number(value: Option<&serde_json::Value>, fallback: f32) -> f32 {
    value.and_then(serde_json::Value::as_f64).filter(|value| value.is_finite()).map(|value| value as f32).unwrap_or(fallback)
}

fn normalize_accessibility(host_id: &str, device_id: &str, session_epoch: &str, platform: DevicePlatform, raw: serde_json::Value) -> DeviceAccessibilityTree {
    let mut elements = Vec::new();
    let mut errors = Vec::new();
    match platform {
        DevicePlatform::Ios => {
            let screen_width = raw.as_array().and_then(|nodes| nodes.first()).and_then(|node| node.get("frame")).map(|frame| json_number(frame.get("width"), 1.0)).unwrap_or(1.0).max(1.0);
            let screen_height = raw.as_array().and_then(|nodes| nodes.first()).and_then(|node| node.get("frame")).map(|frame| json_number(frame.get("height"), 1.0)).unwrap_or(1.0).max(1.0);
            fn visit(node: &serde_json::Value, path: &str, width: f32, height: f32, elements: &mut Vec<agent_protocol::device::DeviceAccessibilityElement>) {
                if elements.len() >= 500 { return; }
                let Some(frame) = node.get("frame") else { return; };
                let node_width = json_number(frame.get("width"), 0.0);
                let node_height = json_number(frame.get("height"), 0.0);
                let covers_screen = (node_width - width).abs() < 0.5 && (node_height - height).abs() < 0.5;
                if !covers_screen && node_width > 0.0 && node_height > 0.0 {
                    elements.push(agent_protocol::device::DeviceAccessibilityElement { id: node.get("AXUniqueId").and_then(serde_json::Value::as_str).unwrap_or(path).into(), label: node.get("AXLabel").and_then(serde_json::Value::as_str).unwrap_or_default().into(), role: node.get("type").and_then(serde_json::Value::as_str).unwrap_or_default().into(), x: json_number(frame.get("x"), 0.0) / width, y: json_number(frame.get("y"), 0.0) / height, width: node_width / width, height: node_height / height });
                }
                if let Some(children) = node.get("children").and_then(serde_json::Value::as_array) { for (index, child) in children.iter().enumerate() { visit(child, &format!("{path}.{index}"), width, height, elements); } }
            }
            if let Some(nodes) = raw.as_array() { for (index, node) in nodes.iter().enumerate() { visit(node, &index.to_string(), screen_width, screen_height, &mut elements); } } else { errors.push("unexpected iOS accessibility payload".into()); }
        }
        DevicePlatform::Android => {
            let Some(nodes) = raw.get("nodes").and_then(serde_json::Value::as_array) else { errors.push(raw.get("error").and_then(serde_json::Value::as_str).unwrap_or("unexpected Android accessibility payload").into()); return DeviceAccessibilityTree { host_id: host_id.into(), device_id: device_id.into(), session_epoch: session_epoch.into(), elements, errors, read_at: now_iso() }; };
            let (screen_width, screen_height) = nodes.first().and_then(|node| node.get("bounds")).map(|bounds| (json_number(bounds.get("right"), 1.0).max(1.0), json_number(bounds.get("bottom"), 1.0).max(1.0))).unwrap_or((1.0, 1.0));
            for node in nodes.iter().skip(1) {
                let Some(bounds) = node.get("bounds") else { continue; };
                let left = json_number(bounds.get("left"), 0.0); let top = json_number(bounds.get("top"), 0.0); let right = json_number(bounds.get("right"), left); let bottom = json_number(bounds.get("bottom"), top);
                let width = (right - left) / screen_width; let height = (bottom - top) / screen_height;
                let label = node.get("text").and_then(serde_json::Value::as_str).filter(|value| !value.is_empty()).or_else(|| node.get("contentDescription").and_then(serde_json::Value::as_str)).unwrap_or_default();
                if width >= 0.95 && height >= 0.9 || (label.is_empty() && node.get("clickable").and_then(serde_json::Value::as_bool) != Some(true)) { continue; }
                let role = node.get("className").and_then(serde_json::Value::as_str).and_then(|value| value.rsplit('.').next()).unwrap_or_default();
                elements.push(agent_protocol::device::DeviceAccessibilityElement { id: node.get("id").map(ToString::to_string).unwrap_or_default(), label: label.into(), role: role.into(), x: left / screen_width, y: top / screen_height, width, height });
                if elements.len() >= 500 { break; }
            }
        }
    }
    DeviceAccessibilityTree { host_id: host_id.into(), device_id: device_id.into(), session_epoch: session_epoch.into(), elements, errors, read_at: now_iso() }
}

fn normalize_event_log(host_id: &str, device_id: &str, session_epoch: &str, raw: serde_json::Value) -> Vec<DeviceEventLogEntry> {
    let entries = raw.get("events").and_then(serde_json::Value::as_array).or_else(|| raw.as_array()).cloned().or_else(|| raw.get("event").map(|event| vec![event.clone()])).unwrap_or_default();
    entries.into_iter().filter_map(|entry| {
        let entry = entry.get("event").unwrap_or(&entry);
        Some(DeviceEventLogEntry { host_id: host_id.into(), device_id: device_id.into(), session_epoch: session_epoch.into(), id: entry.get("id")?.as_u64()?, timestamp: entry.get("timestamp").and_then(serde_json::Value::as_str).unwrap_or_default().into(), kind: entry.get("kind").and_then(serde_json::Value::as_str).unwrap_or_default().into(), summary: entry.get("summary").and_then(serde_json::Value::as_str).or_else(|| entry.get("msg").and_then(serde_json::Value::as_str)).unwrap_or_default().into() })
    }).collect()
}

fn event_log_error(host_id: &str, device_id: &str, session_epoch: &str, summary: impl Into<String>) -> DeviceEvent {
    DeviceEvent::EventLog(DeviceEventLogEntry {
        host_id: host_id.into(),
        device_id: device_id.into(),
        session_epoch: session_epoch.into(),
        id: 0,
        timestamp: now_iso(),
        kind: "error".into(),
        summary: summary.into(),
    })
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
                String::from("simctl"),
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
    use async_trait::async_trait;
    use std::sync::{Arc as StdArc, Mutex as StdMutex};
    use futures_util::{SinkExt, StreamExt};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::sync::oneshot;

    fn recording_description() -> Vec<u8> {
        let sps = [0x67, 0x42, 0x00, 0x1f, 0x95, 0xa8, 0x14, 0x01, 0x6e, 0x40];
        let pps = [0x68, 0xce, 0x3c, 0x80];
        let mut description = vec![1, sps[1], sps[2], sps[3], 0xff, 0xe1];
        description.extend_from_slice(&(sps.len() as u16).to_be_bytes());
        description.extend_from_slice(&sps);
        description.push(1);
        description.extend_from_slice(&(pps.len() as u16).to_be_bytes());
        description.extend_from_slice(&pps);
        description
    }

    fn recording_keyframe() -> Vec<u8> { vec![0, 0, 0, 1, 0x65, 0x88, 0x84, 0] }

    struct FixtureHost {
        id: String,
        commands: StdArc<StdMutex<Vec<(String, HostOutput)>>>,
    }

    impl FixtureHost {
        fn new(commands: Vec<(&str, HostOutput)>) -> Self {
            Self {
                id: "fixture".into(),
                commands: StdArc::new(StdMutex::new(
                    commands
                        .into_iter()
                        .map(|(command, output)| (command.into(), output))
                        .collect(),
                )),
            }
        }
    }

    #[async_trait]
    impl DeviceHostRunner for FixtureHost {
        fn id(&self) -> &str { &self.id }
        fn kind(&self) -> DeviceHostKind { DeviceHostKind::Local }
        fn label(&self) -> &str { "Fixture host" }
        fn helper_path(&self, _helper: DeviceHelper) -> Option<String> { None }

        async fn run(&self, command: &str, _args: &[String], _stdin: Option<&[u8]>) -> Result<HostOutput, String> {
            let mut commands = self.commands.lock().unwrap();
            if let Some(index) = commands.iter().position(|(expected, _)| expected == command) {
                Ok(commands.remove(index).1)
            } else {
                Err(format!("fixture command {command} was not queued"))
            }
        }

        async fn start(&self, _command: &str, _args: &[String]) -> Result<(), String> { Ok(()) }
        async fn forward(&self, _remote_port: u16) -> Result<Option<ForwardedPort>, String> { Ok(None) }
        async fn lifecycle(&self, _mode: &str) -> Result<Option<HostOutput>, String> { Ok(None) }
    }

    async fn fake_http_server(
        expected_path: &'static str,
        body: Vec<u8>,
        split_at: Option<usize>,
        keep_open: bool,
    ) -> (u16, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buffer = [0_u8; 1024];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let size = socket.read(&mut buffer).await.unwrap();
                assert!(size > 0, "fake device hub request ended before headers");
                request.extend_from_slice(&buffer[..size]);
            }
            let request = String::from_utf8_lossy(&request);
            assert!(request.starts_with(&format!("GET {expected_path} HTTP/1.1")), "unexpected request: {request}");
            if keep_open {
                let headers = "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nTransfer-Encoding: chunked\r\nConnection: keep-alive\r\n\r\n";
                socket.write_all(headers.as_bytes()).await.unwrap();
                if let Some(split_at) = split_at.filter(|split_at| *split_at < body.len()) {
                    let size = format!("{:X}\r\n", split_at);
                    socket.write_all(size.as_bytes()).await.unwrap();
                    socket.write_all(&body[..split_at]).await.unwrap();
                    socket.write_all(b"\r\n").await.unwrap();
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                    let remainder = body.len() - split_at;
                    socket.write_all(format!("{:X}\r\n", remainder).as_bytes()).await.unwrap();
                    socket.write_all(&body[split_at..]).await.unwrap();
                    socket.write_all(b"\r\n").await.unwrap();
                } else {
                    socket.write_all(format!("{:X}\r\n", body.len()).as_bytes()).await.unwrap();
                    socket.write_all(&body).await.unwrap();
                    socket.write_all(b"\r\n").await.unwrap();
                }
            } else {
                let headers = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                socket.write_all(headers.as_bytes()).await.unwrap();
                if let Some(split_at) = split_at.filter(|split_at| *split_at < body.len()) {
                    socket.write_all(&body[..split_at]).await.unwrap();
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                    socket.write_all(&body[split_at..]).await.unwrap();
                } else {
                    socket.write_all(&body).await.unwrap();
                }
            }
            if keep_open {
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            }
        });
        (port, task)
    }

    async fn fake_websocket_server() -> (
        u16,
        oneshot::Receiver<async_tungstenite::tungstenite::Message>,
        tokio::task::JoinHandle<()>,
    ) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (sender, receiver) = oneshot::channel();
        let task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (mut socket, _) = async_tungstenite::tokio::accept_async(stream).await.unwrap();
            if let Some(Ok(message)) = socket.next().await {
                let acknowledgement = match &message {
                    async_tungstenite::tungstenite::Message::Binary(bytes) if bytes.first() == Some(&0x10) => {
                        serde_json::from_slice::<serde_json::Value>(&bytes[1..])
                            .ok()
                            .and_then(|payload| payload.get("requestId").and_then(serde_json::Value::as_u64))
                            .map(|request_id| [vec![0x90], serde_json::to_vec(&serde_json::json!({"requestId": request_id, "ok": true})).unwrap()].concat())
                    }
                    async_tungstenite::tungstenite::Message::Binary(bytes) if bytes.first() == Some(&0x07) => {
                        serde_json::from_slice::<serde_json::Value>(&bytes[1..])
                            .ok()
                            .and_then(|payload| payload.get("orientation").and_then(serde_json::Value::as_str).map(|orientation| {
                                [
                                    vec![0x82],
                                    serde_json::to_vec(&serde_json::json!({"width": 100, "height": 200, "orientation": orientation})).unwrap(),
                                ]
                                .concat()
                            }))
                    }
                    _ => None,
                };
                let _ = sender.send(message);
                if let Some(acknowledgement) = acknowledgement {
                    socket
                        .send(async_tungstenite::tungstenite::Message::binary(acknowledgement))
                        .await
                        .unwrap();
                }
            }
        });
        (port, receiver, task)
    }

    async fn fake_frame_websocket_server(
        frame: Vec<u8>,
    ) -> (u16, oneshot::Receiver<()>, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (closed_sender, closed_receiver) = oneshot::channel();
        let task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (mut socket, _) = async_tungstenite::tokio::accept_async(stream).await.unwrap();
            socket
                .send(async_tungstenite::tungstenite::Message::binary(frame))
                .await
                .unwrap();
            while let Some(Ok(message)) = socket.next().await {
                if matches!(message, async_tungstenite::tungstenite::Message::Close(_)) {
                    let _ = closed_sender.send(());
                    return;
                }
            }
        });
        (port, closed_receiver, task)
    }

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
                    String::from("simctl"),
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
                String::from("simctl"),
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
                String::from("simctl"),
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

    #[tokio::test]
    async fn active_device_session_blocks_handoff_probe_without_starting_a_host() {
        let directory = tempfile::tempdir().unwrap();
        let service = DeviceService::new(directory.path().to_owned());
        assert!(!service.has_active_tasks());

        service.inner.state.write().await.sessions.push(DeviceSession {
            thread_id: ThreadId::new("thread").unwrap(),
            host_id: LOCAL_DEVICE_HOST_ID.into(),
            device_id: "simulator".into(),
            platform: DevicePlatform::Ios,
            opened_at: "now".into(),
        });

        assert!(service.has_active_tasks());
    }

    #[test]
    fn keyboard_codes_are_mapped_to_reference_hid_and_android_values() {
        assert_eq!(ios_hid_usage("KeyA"), Some(0x04));
        assert_eq!(ios_hid_usage("Digit0"), Some(0x27));
        assert_eq!(ios_hid_usage("Unknown"), None);
        assert_eq!(android_keycode("ArrowLeft"), Some(21));
        assert_eq!(android_keycode("Unknown"), None);
        assert_eq!(button_wire(DeviceHardwareButton::Power, true), "lock");
        assert_eq!(button_wire(DeviceHardwareButton::AppSwitcher, true), "app_switcher");
    }

    #[test]
    fn duo_controls_preserve_each_reference_wire_fact() {
        let cases = [
            (
                DeviceDuoCommand::Angle { value: 42.5 },
                serde_json::json!({"control": "angle", "value": 42.5}),
            ),
            (
                DeviceDuoCommand::Pose { value: DeviceDuoPose::Laptop },
                serde_json::json!({"control": "pose", "value": "laptop"}),
            ),
            (
                DeviceDuoCommand::Table { value: false },
                serde_json::json!({"control": "table", "value": false}),
            ),
            (
                DeviceDuoCommand::Physical { value: DeviceDuoPhysical::Facedown },
                serde_json::json!({"control": "physical", "value": "facedown"}),
            ),
            (
                DeviceDuoCommand::Orientation { value: DeviceOrientation::LandscapeRight },
                serde_json::json!({"control": "orientation", "value": "landscape_right"}),
            ),
        ];
        for (command, expected) in cases {
            assert_eq!(duo_command_wire(&command), expected);
            assert!(command.validate().is_ok());
        }
        assert!(DeviceDuoCommand::Angle { value: -1.0 }.validate().is_err());
        assert!(DeviceDuoCommand::Angle { value: 181.0 }.validate().is_err());
        assert!(DeviceDuoCommand::Angle { value: f32::NAN }.validate().is_err());
    }

    #[test]
    fn accessibility_normalization_skips_full_window_containers() {
        let tree = normalize_accessibility("local", "sim", "epoch", DevicePlatform::Android, serde_json::json!({
            "nodes": [
                {"bounds": {"left": 0, "top": 0, "right": 100, "bottom": 200}},
                {"id": 1, "bounds": {"left": 10, "top": 20, "right": 90, "bottom": 50}, "text": "Continue", "className": "android.widget.Button"},
                {"id": 2, "bounds": {"left": 0, "top": 0, "right": 100, "bottom": 200}, "text": ""}
            ]
        }));
        assert_eq!(tree.elements.len(), 1);
        assert_eq!(tree.elements[0].label, "Continue");
        assert_eq!(tree.elements[0].role, "Button");
    }

    #[test]
    fn ios_accessibility_normalization_flattens_nested_frames_and_caps_the_root() {
        let tree = normalize_accessibility("local", "sim", "epoch", DevicePlatform::Ios, serde_json::json!([
            {
                "frame": {"x": 0, "y": 0, "width": 100, "height": 200},
                "type": "Application",
                "children": [{
                    "AXUniqueId": "continue",
                    "AXLabel": "Continue",
                    "type": "Button",
                    "frame": {"x": 10, "y": 20, "width": 80, "height": 40}
                }]
            }
        ]));
        assert_eq!(tree.elements.len(), 1);
        assert_eq!(tree.elements[0].id, "continue");
        assert_eq!(tree.elements[0].label, "Continue");
        assert!((tree.elements[0].x - 0.1).abs() < 0.001);
        assert!((tree.elements[0].height - 0.2).abs() < 0.001);
    }

    #[test]
    fn orientation_and_touch_transforms_match_the_reference_rotation_order() {
        assert_eq!(next_orientation(DeviceOrientation::Portrait), DeviceOrientation::LandscapeLeft);
        assert_eq!(next_orientation(DeviceOrientation::LandscapeLeft), DeviceOrientation::PortraitUpsideDown);
        assert_eq!(orientation_wire(DeviceOrientation::LandscapeRight), "landscape_right");
        let screen = DeviceScreenConfig {
            thread_id: None,
            session_epoch: "epoch".into(),
            host_id: None,
            device_id: None,
            width: 400,
            height: 800,
            orientation: DeviceOrientation::LandscapeLeft,
            screen_id: None,
            supports_hinge_angle: false,
            supports_physical_orientation: false,
            hinge_angle: None,
            hinge_pose: None,
            table_mode: false,
            table_mode_available: false,
        };
        assert_eq!(rotate_touch(Some(&screen), 0.25, 0.75), (0.75, 0.75));
    }

    #[tokio::test]
    async fn avcc_transport_reads_a_fragmented_fake_stream_and_returns_without_waiting_for_close() {
        let body = [4_u32.to_be_bytes().as_slice(), &[2, 1, 2, 3]].concat();
        let (port, server) = fake_http_server(
            "/vendor/serve-sim/helper/sim/stream.avcc",
            body,
            Some(3),
            true,
        )
        .await;
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            hub_avcc_frame(port, "sim", None),
        )
        .await
        .expect("AVCC reader waited for the helper stream to close")
        .unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].payload, [1, 2, 3]);
        assert_eq!(result[0].encoding, DeviceFrameEncoding::H264);
        server.abort();
    }

    #[tokio::test]
    async fn persistent_avcc_reader_keeps_one_connection_and_emits_every_access_unit() {
        let mut body = Vec::new();
        for (tag, payload) in [(1, vec![1, 2]), (2, vec![3, 4]), (3, vec![5, 6])] {
            body.extend_from_slice(&(payload.len() as u32 + 1).to_be_bytes());
            body.push(tag);
            body.extend_from_slice(&payload);
        }
        let (port, server) = fake_http_server(
            "/vendor/serve-sim/helper/sim/stream.avcc",
            body,
            Some(5),
            true,
        )
        .await;
        let cancel = CancellationToken::new();
        let (sender, mut receiver) = tokio::sync::mpsc::channel(8);
        let task = tokio::spawn(persistent_avcc_reader(
            port,
            "sim".into(),
            None,
            "epoch".into(),
            1280,
            720,
            sender,
            cancel.clone(),
        ));
        let mut frames = Vec::new();
        for _ in 0..3 {
            let Some(SourceMessage::Frame { frame, .. }) = tokio::time::timeout(Duration::from_secs(1), receiver.recv()).await.unwrap() else { panic!("persistent reader ended before all frames"); };
            frames.push(frame);
        }
        assert_eq!(frames.len(), 3);
        assert_eq!(frames[0].encoding, DeviceFrameEncoding::AvccDescription);
        assert_eq!(frames[1].encoding, DeviceFrameEncoding::H264);
        assert!(!frames[2].keyframe);
        cancel.cancel();
        task.abort();
        let _ = task.await;
        server.abort();
    }

    #[test]
    fn generation_recipient_rejects_a_reopened_thread_without_starving_another_thread() {
        let first = DeviceSession { thread_id: ThreadId::new("first").unwrap(), host_id: "local".into(), device_id: "sim".into(), platform: DevicePlatform::Ios, opened_at: "open-1".into(), session_epoch: "epoch-1".into() };
        let second = DeviceSession { thread_id: ThreadId::new("second").unwrap(), host_id: "local".into(), device_id: "sim".into(), platform: DevicePlatform::Ios, opened_at: "open-2".into(), session_epoch: "epoch-2".into() };
        let generation = BTreeMap::from([
            (first.thread_id.to_string(), first.session_epoch.clone()),
            (second.thread_id.to_string(), second.session_epoch.clone()),
        ]);
        assert!(session_matches_generation(&generation, &first));
        assert!(session_matches_generation(&generation, &second));
        let reopened = DeviceSession { session_epoch: "epoch-3".into(), ..first.clone() };
        assert!(!session_matches_generation(&generation, &reopened));
        assert!(session_matches_generation(&generation, &second));
    }

    #[tokio::test]
    async fn source_fanout_never_relabels_a_queued_frame_for_a_reopened_thread() {
        let directory = tempfile::tempdir().unwrap();
        let service = DeviceService::new(directory.path().to_path_buf());
        let first = DeviceSession { thread_id: ThreadId::new("first-fanout").unwrap(), host_id: "local".into(), device_id: "sim".into(), platform: DevicePlatform::Ios, opened_at: "open-1".into(), session_epoch: "epoch-1".into() };
        let second = DeviceSession { thread_id: ThreadId::new("second-fanout").unwrap(), host_id: "local".into(), device_id: "sim".into(), platform: DevicePlatform::Ios, opened_at: "open-2".into(), session_epoch: "epoch-2".into() };
        *service.inner.state.write().await = DeviceServiceState {
            sessions: vec![first.clone(), second.clone()],
            ..DeviceServiceState::default()
        };
        let generation = BTreeMap::from([
            (first.thread_id.to_string(), first.session_epoch.clone()),
            (second.thread_id.to_string(), second.session_epoch.clone()),
        ]);
        let device = DeviceSummary {
            host_id: "local".into(),
            id: "sim".into(),
            platform: DevicePlatform::Ios,
            name: "Fixture".into(),
            version: "iOS".into(),
            booted: true,
            physical: false,
        };
        let mut events = service.subscribe();
        let frame = TransportFrame {
            payload: vec![0, 0, 0, 1, 0x65],
            encoding: DeviceFrameEncoding::H264,
            keyframe: true,
            timestamp_us: Some(1),
            screen_id: None,
        };
        service
            .publish_source_frame(
                &("local".into(), "sim".into()),
                &first,
                &device,
                &generation,
                frame.clone(),
                1280,
                720,
                false,
            )
            .await;
        let mut epochs = vec![];
        for _ in 0..2 {
            let DeviceEvent::Video(video) = events.recv().await.unwrap() else { panic!("source fanout emitted a non-video event"); };
            epochs.push(video.session_epoch);
        }
        epochs.sort();
        assert_eq!(epochs, vec!["epoch-1", "epoch-2"]);

        service.inner.state.write().await.sessions[0].session_epoch = "epoch-3".into();
        service
            .publish_source_frame(
                &("local".into(), "sim".into()),
                &first,
                &device,
                &generation,
                frame,
                1280,
                720,
                false,
            )
            .await;
        let DeviceEvent::Video(video) = events.recv().await.unwrap() else { panic!("source fanout emitted a non-video event"); };
        assert_eq!(video.thread_id, second.thread_id);
        assert_eq!(video.session_epoch, "epoch-2");
        assert!(matches!(events.try_recv(), Err(tokio::sync::broadcast::error::TryRecvError::Empty)));
    }

    #[tokio::test]
    async fn source_generation_does_not_append_old_frames_to_a_reopened_recording() {
        let directory = tempfile::tempdir().unwrap();
        let service = DeviceService::new(directory.path().to_path_buf());
        let session = DeviceSession { thread_id: ThreadId::new("recording-generation").unwrap(), host_id: "local".into(), device_id: "sim".into(), platform: DevicePlatform::Ios, opened_at: "open".into(), session_epoch: "epoch-new".into() };
        service.inner.recordings.lock().await.insert(
            (session.thread_id.clone(), session.host_id.clone(), session.device_id.clone()),
            ActiveDeviceRecording {
                recorder: Mp4Recorder::new(DeviceRecordingFormat::Mp4, "recording".into()),
                error: None,
                session_epoch: Some(session.session_epoch.clone()),
            },
        );
        let frame = TransportFrame {
            payload: vec![0, 0, 0, 1, 0x65, 0x88],
            encoding: DeviceFrameEncoding::H264,
            keyframe: true,
            timestamp_us: Some(1),
            screen_id: None,
        };
        let old_generation = BTreeMap::from([(session.thread_id.to_string(), "epoch-old".into())]);
        service
            .append_source_recordings(
                &("local".into(), "sim".into()),
                &session,
                &old_generation,
                std::slice::from_ref(&frame),
            )
            .await;
        assert_eq!(service.inner.recordings.lock().await.values().next().unwrap().recorder.frame_count(), 0);
        let current_generation = BTreeMap::from([(session.thread_id.to_string(), session.session_epoch.clone())]);
        service
            .append_source_recordings(
                &("local".into(), "sim".into()),
                &session,
                &current_generation,
                std::slice::from_ref(&frame),
            )
            .await;
        assert_eq!(service.inner.recordings.lock().await.values().next().unwrap().recorder.frame_count(), 1);
    }

    #[tokio::test]
    async fn avcc_transport_does_not_reopen_after_a_jpeg_seed_before_h264() {
        let body = [
            6_u32.to_be_bytes().as_slice(),
            &[4, 0xff, 0xd8, 9, 0xff, 0xd9],
            6_u32.to_be_bytes().as_slice(),
            &[2, 0, 0, 0, 1, 0x65],
        ]
        .concat();
        let (port, server) = fake_http_server(
            "/vendor/serve-sim/helper/sim/stream.avcc",
            body,
            Some(2),
            true,
        )
        .await;
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            hub_avcc_frame(port, "sim", None),
        )
        .await
        .expect("AVCC reader waited for the helper stream to close")
        .unwrap();
        assert_eq!(result.iter().map(|frame| frame.encoding).collect::<Vec<_>>(), [DeviceFrameEncoding::Jpeg, DeviceFrameEncoding::H264]);
        server.abort();
    }

    #[tokio::test]
    async fn mjpeg_transport_extracts_one_fake_frame_and_cancels_the_stream() {
        let body = vec![0, 1, 0xff, 0xd8, 9, 8, 0xff, 0xd9, 7, 6];
        let (port, server) = fake_http_server(
            "/vendor/serve-sim/helper/sim/stream.mjpeg",
            body,
            Some(4),
            true,
        )
        .await;
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            hub_mjpeg_frame(port, "sim"),
        )
        .await
        .expect("MJPEG reader waited for the helper stream to close")
        .unwrap();
        assert_eq!(result.payload, [0xff, 0xd8, 9, 8, 0xff, 0xd9]);
        assert_eq!(result.encoding, DeviceFrameEncoding::Mjpeg);
        server.abort();
    }

    #[tokio::test]
    async fn semu_transport_closes_the_socket_after_transferring_frame_ownership() {
        let mut body = b"SEMU".to_vec();
        body.extend_from_slice(&[1, 1, 0, 0]);
        body.extend_from_slice(&99_u64.to_be_bytes());
        body.extend_from_slice(&[0, 0, 0, 1, 0x65]);
        let (port, closed, server) = fake_frame_websocket_server(body).await;
        let frame = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            hub_android_frame(port, "emulator"),
        )
        .await
        .expect("SEMU reader waited for the helper socket to close")
        .unwrap();
        assert_eq!(frame.encoding, DeviceFrameEncoding::Semu);
        assert_eq!(frame.timestamp_us, Some(99));
        assert!(frame.keyframe);
        tokio::time::timeout(std::time::Duration::from_secs(1), closed)
            .await
            .expect("Host did not finish the helper close handshake")
            .unwrap();
        server.abort();
    }

    #[tokio::test]
    async fn input_transport_preserves_ios_touch_android_keys_and_duo_controls() {
        let (port, receiver, server) = fake_websocket_server().await;
        hub_input(
            port,
            DevicePlatform::Ios,
            "sim",
            &DeviceInputKind::Touch { phase: DeviceTouchPhase::Begin, x: 0.25, y: 0.75 },
            0,
        )
        .await
        .unwrap();
        let message = tokio::time::timeout(std::time::Duration::from_secs(1), receiver)
            .await
            .unwrap()
            .unwrap();
        match message {
            async_tungstenite::tungstenite::Message::Binary(bytes) => {
                assert_eq!(bytes[0], 0x03);
                let payload: serde_json::Value = serde_json::from_slice(&bytes[1..]).unwrap();
                assert_eq!(payload["type"], "begin");
                assert_eq!(payload["x"], 0.25);
            }
            other => panic!("unexpected iOS input frame: {other:?}"),
        }
        server.abort();

        let (port, receiver, server) = fake_websocket_server().await;
        hub_input(
            port,
            DevicePlatform::Android,
            "emu",
            &DeviceInputKind::Key {
                code: "Digit1".into(),
                key: "!".into(),
                down: true,
                meta: false,
                ctrl: false,
            },
            0,
        )
        .await
        .unwrap();
        let message = tokio::time::timeout(std::time::Duration::from_secs(1), receiver)
            .await
            .unwrap()
            .unwrap();
        match message {
            async_tungstenite::tungstenite::Message::Text(text) => {
                let payload: serde_json::Value = serde_json::from_str(&text.to_string()).unwrap();
                assert_eq!(payload, serde_json::json!({"type": "text", "text": "!"}));
            }
            other => panic!("unexpected Android text frame: {other:?}"),
        }
        server.abort();

        let (port, receiver, server) = fake_websocket_server().await;
        hub_input(
            port,
            DevicePlatform::Android,
            "emu",
            &DeviceInputKind::Key {
                code: "KeyE".into(),
                key: "é".into(),
                down: true,
                meta: false,
                ctrl: false,
            },
            0,
        )
        .await
        .unwrap();
        let message = tokio::time::timeout(std::time::Duration::from_secs(1), receiver)
            .await
            .unwrap()
            .unwrap();
        match message {
            async_tungstenite::tungstenite::Message::Text(text) => {
                let payload: serde_json::Value = serde_json::from_str(&text.to_string()).unwrap();
                assert_eq!(payload, serde_json::json!({"type": "text", "text": "é"}));
            }
            other => panic!("unexpected Android Unicode frame: {other:?}"),
        }
        server.abort();

        let (port, _receiver, server) = fake_websocket_server().await;
        let error = hub_input(
            port,
            DevicePlatform::Android,
            "emu",
            &DeviceInputKind::Key {
                code: "KeyC".into(),
                key: "c".into(),
                down: true,
                meta: false,
                ctrl: true,
            },
            0,
        )
        .await
        .unwrap_err();
        assert!(error.contains("unsupported Android keyboard key c"));
        server.abort();

        let (port, receiver, server) = fake_websocket_server().await;
        hub_input(
            port,
            DevicePlatform::Android,
            "emu",
            &DeviceInputKind::Touch { phase: DeviceTouchPhase::End, x: 0.2, y: 0.1 },
            0,
        )
        .await
        .unwrap();
        let message = tokio::time::timeout(std::time::Duration::from_secs(1), receiver)
            .await
            .unwrap()
            .unwrap();
        match message {
            async_tungstenite::tungstenite::Message::Text(text) => {
                let payload: serde_json::Value = serde_json::from_str(&text.to_string()).unwrap();
                assert_eq!(payload, serde_json::json!({"type": "touch", "action": "up", "x": 0.2, "y": 0.1}));
            }
            other => panic!("unexpected Android touch frame: {other:?}"),
        }
        server.abort();

        let (port, receiver, server) = fake_websocket_server().await;
        hub_input(
            port,
            DevicePlatform::Android,
            "emu",
            &DeviceInputKind::Key {
                code: "ArrowLeft".into(),
                key: "ArrowLeft".into(),
                down: true,
                meta: false,
                ctrl: false,
            },
            0,
        )
        .await
        .unwrap();
        let message = tokio::time::timeout(std::time::Duration::from_secs(1), receiver)
            .await
            .unwrap()
            .unwrap();
        match message {
            async_tungstenite::tungstenite::Message::Text(text) => {
                let payload: serde_json::Value = serde_json::from_str(&text.to_string()).unwrap();
                assert_eq!(payload["type"], "key");
                assert_eq!(payload["keycode"], 21);
            }
            other => panic!("unexpected Android input frame: {other:?}"),
        }
        server.abort();

        let (port, receiver, server) = fake_websocket_server().await;
        hub_input(
            port,
            DevicePlatform::Ios,
            "duo",
            &DeviceInputKind::Duo { command: DeviceDuoCommand::Table { value: true } },
            1,
        )
        .await
        .unwrap();
        let message = tokio::time::timeout(std::time::Duration::from_secs(1), receiver)
            .await
            .unwrap()
            .unwrap();
        match message {
            async_tungstenite::tungstenite::Message::Binary(bytes) => {
                assert_eq!(bytes[0], 0x10);
                let payload: serde_json::Value = serde_json::from_slice(&bytes[1..]).unwrap();
                assert_eq!(payload["requestId"], 1);
                assert_eq!(payload["command"]["control"], "table");
                assert_eq!(payload["command"]["value"], true);
            }
            other => panic!("unexpected Duo input frame: {other:?}"),
        }
        server.abort();

        let (port, receiver, server) = fake_websocket_server().await;
        hub_input(
            port,
            DevicePlatform::Ios,
            "duo",
            &DeviceInputKind::Duo {
                command: DeviceDuoCommand::Orientation { value: DeviceOrientation::LandscapeRight },
            },
            2,
        )
        .await
        .unwrap();
        let message = tokio::time::timeout(std::time::Duration::from_secs(1), receiver)
            .await
            .unwrap()
            .unwrap();
        match message {
            async_tungstenite::tungstenite::Message::Binary(bytes) => {
                assert_eq!(bytes[0], 0x07);
                let payload: serde_json::Value = serde_json::from_slice(&bytes[1..]).unwrap();
                assert_eq!(payload, serde_json::json!({"orientation": "landscape_right"}));
            }
            other => panic!("unexpected Duo orientation frame: {other:?}"),
        }
        server.abort();
    }

    #[tokio::test]
    async fn android_fold_uses_the_typed_http_control_route() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buffer = [0_u8; 1024];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let size = socket.read(&mut buffer).await.unwrap();
                assert!(size > 0);
                request.extend_from_slice(&buffer[..size]);
            }
            assert!(String::from_utf8_lossy(&request).starts_with("POST /vendor/serve-emu/api/fold?device=fold HTTP/1.1"));
            let body = br#"{"ok":true}"#;
            let response = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
            socket.write_all(response.as_bytes()).await.unwrap();
            socket.write_all(body).await.unwrap();
        });
        hub_input(port, DevicePlatform::Android, "fold", &DeviceInputKind::Fold { command: DeviceFoldPosture::Opened }, 0).await.unwrap();
        server.await.unwrap();
    }

    #[tokio::test]
    async fn event_log_fixture_accepts_the_reference_sse_route_and_normalizes_entries() {
        let body = br#"data: {"events":[{"id":6,"timestamp":"2026-10-08T00:00:00Z","kind":"seed","summary":"history"},{"id":7,"timestamp":"2026-10-08T00:00:01Z","kind":"touch","summary":"button"}]}

"#
        .to_vec();
        let (port, server) = fake_http_server(
            "/vendor/serve-sim/api/event-log/events?device=sim&limit=20",
            body,
            None,
            false,
        )
        .await;
        let payload = hub_event_log_payload(port, "sim", "20").await.unwrap();
        let entries = normalize_event_log("local", "sim", "epoch", payload);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].id, 6);
        assert_eq!(entries[1].id, 7);
        assert_eq!(entries[1].summary, "button");
        assert_eq!(normalize_event_log("local", "sim", "epoch", serde_json::json!({"event": {"id": 8, "timestamp": "", "kind": "tap", "summary": "live" }}))[0].id, 8);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn foreground_sse_fixture_returns_the_first_update_without_waiting_for_close() {
        let body = br#"data: {"bundleId":"com.example.fixture"}

"#
        .to_vec();
        let (port, server) = fake_http_server(
            "/vendor/serve-sim/appstate?device=sim",
            body,
            None,
            true,
        )
        .await;
        let payload = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            hub_sse_first_json(port, "/vendor/serve-sim/appstate", &[("device", "sim")], "foreground"),
        )
        .await
        .expect("foreground SSE reader waited for the helper stream to close")
        .unwrap();
        assert_eq!(payload["bundleId"], "com.example.fixture");
        server.abort();
    }

    #[tokio::test]
    async fn disabled_consent_does_not_start_a_hub_or_agent_before_enable() {
        let directory = tempfile::tempdir().unwrap();
        let service = DeviceService::new(directory.path().to_path_buf());
        let state = service.list(DeviceListInput::default()).await.unwrap();
        assert_eq!(state.host_status, DeviceHostStatus::Disabled);
        assert!(service.hub_port(LOCAL_DEVICE_HOST_ID).await.is_none());
        let thread_id = ThreadId::new("disabled-device-test").unwrap();
        let error = service
            .agent_device_target(LOCAL_DEVICE_HOST_ID, &thread_id, "sim")
            .await
            .unwrap_err();
        assert!(error.contains("Device support is off"));
    }

    #[tokio::test]
    async fn handoff_activity_comes_from_open_sessions_and_recordings() {
        let directory = tempfile::tempdir().unwrap();
        let service = DeviceService::new(directory.path().to_path_buf());
        assert!(!service.has_active_tasks());
        let thread_id = ThreadId::new("device-handoff").unwrap();
        let session = DeviceSession {
            thread_id: thread_id.clone(),
            host_id: LOCAL_DEVICE_HOST_ID.into(),
            device_id: "sim".into(),
            platform: DevicePlatform::Ios,
            opened_at: "0".into(),
            session_epoch: "0".into(),
        };
        service.inner.state.write().await.sessions.push(session.clone());
        assert!(service.has_active_tasks());
        service.inner.state.write().await.sessions.clear();
        service.inner.recordings.lock().await.insert(
            (thread_id, LOCAL_DEVICE_HOST_ID.into(), session.device_id),
            ActiveDeviceRecording {
                recorder: Mp4Recorder::new(DeviceRecordingFormat::Mp4, "0".into()),
                error: None,
                session_epoch: None,
            },
        );
        assert!(service.has_active_tasks());
    }

    #[tokio::test]
    async fn frame_sequences_are_contiguous_per_device_and_screen() {
        let directory = tempfile::tempdir().unwrap();
        let service = DeviceService::new(directory.path().to_path_buf());
        assert_eq!(service.next_frame_sequence("local", "sim", None).await, 1);
        assert_eq!(service.next_frame_sequence("local", "sim", None).await, 2);
        assert_eq!(service.next_frame_sequence("local", "sim", Some(1)).await, 1);
        assert_eq!(service.next_frame_sequence("local", "sim", Some(3)).await, 1);
        assert_eq!(service.next_frame_sequence("remote", "sim", None).await, 1);
        assert_eq!(service.next_frame_sequence("local", "sim", Some(1)).await, 2);
    }

    #[tokio::test]
    async fn concurrent_close_of_an_absent_session_is_idempotent() {
        let directory = tempfile::tempdir().unwrap();
        let service = DeviceService::new(directory.path().to_path_buf());
        let input = agent_protocol::device::DeviceCloseInput {
            thread_id: ThreadId::new("close-race").unwrap(),
            host_id: Some(LOCAL_DEVICE_HOST_ID.into()),
            device_id: Some("sim".into()),
            shutdown: true,
        };
        let (first, second) = tokio::join!(service.close(input.clone()), service.close(input));
        assert!(first.is_ok());
        assert!(second.is_ok());
        assert!(service.state_async().await.sessions.is_empty());
    }

    #[tokio::test]
    async fn concurrent_close_of_an_open_session_releases_recording_once() {
        let directory = tempfile::tempdir().unwrap();
        let service = DeviceService::new(directory.path().to_path_buf());
        let mut events = service.subscribe();
        let thread_id = ThreadId::new("close-open-race").unwrap();
        let session = DeviceSession {
            thread_id: thread_id.clone(),
            host_id: LOCAL_DEVICE_HOST_ID.into(),
            device_id: "sim".into(),
            platform: DevicePlatform::Ios,
            opened_at: "0".into(),
            session_epoch: "0".into(),
        };
        let mut state = service.state_async().await;
        state.sessions.push(session.clone());
        *service.inner.state.write().await = state;
        service.inner.recordings.lock().await.insert(
            (thread_id.clone(), LOCAL_DEVICE_HOST_ID.into(), "sim".into()),
            ActiveDeviceRecording {
                recorder: Mp4Recorder::new(DeviceRecordingFormat::Mp4, "0".into()),
                error: None,
                session_epoch: Some(session.session_epoch.clone()),
            },
        );
        let input = agent_protocol::device::DeviceCloseInput {
            thread_id,
            host_id: Some(LOCAL_DEVICE_HOST_ID.into()),
            device_id: Some("sim".into()),
            shutdown: false,
        };
        let (first, second) = tokio::join!(service.close(input.clone()), service.close(input));
        assert!(first.is_ok());
        assert!(second.is_ok());
        assert!(service.state_async().await.sessions.is_empty());
        assert!(service.inner.recordings.lock().await.is_empty());
        let mut completed = false;
        for _ in 0..3 {
            if let Ok(DeviceEvent::RecordingComplete(recording)) = events.recv().await {
                completed = !recording.status.active;
                break;
            }
        }
        assert!(completed);
    }

    #[tokio::test]
    async fn capture_source_is_shared_and_preference_survives_viewer_release() {
        let directory = tempfile::tempdir().unwrap();
        let service = DeviceService::new(directory.path().to_path_buf());
        let session = DeviceSession {
            thread_id: ThreadId::new("capture-source").unwrap(),
            host_id: LOCAL_DEVICE_HOST_ID.into(),
            device_id: "sim".into(),
            platform: DevicePlatform::Ios,
            opened_at: "0".into(),
            session_epoch: "0".into(),
        };
        service.inner.state.write().await.sessions.push(session.clone());
        let first = service.retain_capture_source(&session, false).await;
        let second = service.retain_capture_source(&session, false).await;
        assert!(StdArc::ptr_eq(&first, &second));
        assert_eq!(first.load(Ordering::Acquire), 2);
        let _mjpeg_viewer = service.retain_capture_source(&session, true).await;
        assert!(service
            .inner
            .capture_sources
            .lock()
            .await
            .get(&(LOCAL_DEVICE_HOST_ID.into(), "sim".into()))
            .is_some_and(|source| source.prefer_mjpeg.load(Ordering::Acquire)));
        let key = (LOCAL_DEVICE_HOST_ID.into(), "sim".into());
        service.release_capture_source(&key).await;
        assert_eq!(first.load(Ordering::Acquire), 2);
        service.release_capture_source(&key).await;
        assert_eq!(first.load(Ordering::Acquire), 1);
        service.release_capture_source(&key).await;
        assert_eq!(first.load(Ordering::Acquire), 0);
        if let Some(source) = service.inner.capture_sources.lock().await.remove(&key) {
            source.cancel.cancel();
            let mut task = source.task;
            task.abort();
            let _ = task.await;
        }
    }

    #[tokio::test]
    async fn event_log_delivery_requires_the_current_session_epoch() {
        let directory = tempfile::tempdir().unwrap();
        let service = DeviceService::new(directory.path().to_path_buf());
        let session = DeviceSession {
            thread_id: ThreadId::new("event-epoch").unwrap(),
            host_id: LOCAL_DEVICE_HOST_ID.into(),
            device_id: "sim".into(),
            platform: DevicePlatform::Ios,
            opened_at: "first".into(),
            session_epoch: "first".into(),
        };
        service.inner.state.write().await.sessions.push(session);
        assert!(service.session_epoch_is_current(LOCAL_DEVICE_HOST_ID, "sim", "first").await);
        assert!(!service.session_epoch_is_current(LOCAL_DEVICE_HOST_ID, "sim", "second").await);
        service.inner.state.write().await.sessions[0].session_epoch = "second".into();
        assert!(!service.session_epoch_is_current(LOCAL_DEVICE_HOST_ID, "sim", "first").await);
        assert!(service.session_epoch_is_current(LOCAL_DEVICE_HOST_ID, "sim", "second").await);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn fake_process_fixture_captures_success_and_startup_failure() {
        let success = run_process(
            "sh",
            &["-c".into(), "printf fake-device".into()],
            None,
            None,
        )
        .await
        .unwrap();
        assert_eq!(success.code, 0);
        assert_eq!(success.stdout, b"fake-device");

        let failure = run_process(
            "sh",
            &["-c".into(), "printf startup-failed >&2; exit 23".into()],
            None,
            None,
        )
        .await
        .unwrap();
        assert_eq!(failure.code, 23);
        assert_eq!(failure.stderr, b"startup-failed");

        let oversized = run_process(
            "sh",
            &["-c".into(), "head -c 8388609 /dev/zero".into()],
            None,
            None,
        )
        .await
        .unwrap_err();
        assert!(oversized.contains("more than 8388608 bytes"));
    }

    #[tokio::test]
    async fn recording_error_is_returned_when_a_fixture_frame_exceeds_the_bound() {
        let directory = tempfile::tempdir().unwrap();
        let service = DeviceService::new(directory.path().to_path_buf());
        let thread_id = ThreadId::new("recording-error").unwrap();
        let session = DeviceSession {
            thread_id: thread_id.clone(),
            host_id: LOCAL_DEVICE_HOST_ID.into(),
            device_id: "sim".into(),
            platform: DevicePlatform::Ios,
            opened_at: "0".into(),
            session_epoch: "0".into(),
        };
        service.inner.recordings.lock().await.insert(
            (thread_id.clone(), LOCAL_DEVICE_HOST_ID.into(), "sim".into()),
            ActiveDeviceRecording {
                recorder: Mp4Recorder::new(DeviceRecordingFormat::Mp4, "0".into()),
                error: None,
                session_epoch: Some(session.session_epoch.clone()),
            },
        );
        service
            .append_recording(
                &thread_id,
                &session,
                &TransportFrame {
                    payload: vec![0; MAX_STREAM_CHUNK],
                    encoding: DeviceFrameEncoding::H264,
                    keyframe: true,
                    timestamp_us: None,
                    screen_id: None,
                },
            )
            .await;
        let recording = service
            .stop_recording(DeviceRecordingStopInput {
                thread_id,
                host_id: Some(LOCAL_DEVICE_HOST_ID.into()),
                device_id: "sim".into(),
            })
            .await
            .unwrap();
        assert_eq!(recording.status.active, false);
        assert!(recording.status.error.unwrap().contains("byte limit"));
        assert!(recording.bytes.is_empty());
    }

    #[tokio::test]
    async fn recording_status_reports_frames_before_finalization() {
        let directory = tempfile::tempdir().unwrap();
        let service = DeviceService::new(directory.path().to_path_buf());
        let mut events = service.subscribe();
        let thread_id = ThreadId::new("recording-status").unwrap();
        let session = DeviceSession {
            thread_id: thread_id.clone(),
            host_id: LOCAL_DEVICE_HOST_ID.into(),
            device_id: "sim".into(),
            platform: DevicePlatform::Ios,
            opened_at: "0".into(),
            session_epoch: "0".into(),
        };
        service.inner.recordings.lock().await.insert(
            (thread_id.clone(), LOCAL_DEVICE_HOST_ID.into(), "sim".into()),
            ActiveDeviceRecording {
                recorder: Mp4Recorder::new(DeviceRecordingFormat::Mp4, "0".into()),
                error: None,
                session_epoch: Some(session.session_epoch.clone()),
            },
        );
        service
            .append_recording(
                &thread_id,
                &session,
                &TransportFrame {
                    payload: recording_description(),
                    encoding: DeviceFrameEncoding::AvccDescription,
                    keyframe: true,
                    timestamp_us: None,
                    screen_id: None,
                },
            )
            .await;
        service
            .append_recording(
                &thread_id,
                &session,
                &TransportFrame {
                    payload: recording_keyframe(),
                    encoding: DeviceFrameEncoding::H264,
                    keyframe: true,
                    timestamp_us: Some(42),
                    screen_id: None,
                },
            )
            .await;
        let status = tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                if let DeviceEvent::Recording(status) = events.recv().await.unwrap()
                    && status.frame_count == 2
                {
                    break status;
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(status.frame_count, 2);
        assert!(status.byte_count > 0);
        assert_eq!(status.file_name, DEVICE_RECORDING_FILE_NAME);
        assert_eq!(status.mime_type, DEVICE_RECORDING_MIME_TYPE);
        assert!(status.active);
    }

    #[tokio::test]
    async fn shutdown_owned_finalizes_recordings_before_releasing_the_owner() {
        let directory = tempfile::tempdir().unwrap();
        let service = DeviceService::new(directory.path().to_path_buf());
        let mut events = service.subscribe();
        let thread_id = ThreadId::new("recording-shutdown").unwrap();
        let mut recorder = Mp4Recorder::new(DeviceRecordingFormat::Mp4, "0".into());
        recorder
            .push(&TransportFrame {
                payload: recording_description(),
                encoding: DeviceFrameEncoding::AvccDescription,
                keyframe: true,
                timestamp_us: None,
                screen_id: None,
            })
            .unwrap();
        recorder
            .push(&TransportFrame {
                payload: recording_keyframe(),
                encoding: DeviceFrameEncoding::H264,
                keyframe: true,
                timestamp_us: Some(1),
                screen_id: None,
            })
            .unwrap();
        service.inner.recordings.lock().await.insert(
            (thread_id, LOCAL_DEVICE_HOST_ID.into(), "sim".into()),
            ActiveDeviceRecording { recorder, error: None, session_epoch: None },
        );
        service.shutdown_owned().await;
        assert!(service.inner.recordings.lock().await.is_empty());
        let recording = loop {
            if let DeviceEvent::RecordingComplete(recording) = events.recv().await.unwrap() {
                break recording;
            }
        };
        assert!(!recording.status.active);
        assert_eq!(recording.status.frame_count, 2);
        assert!(recording.bytes.windows(4).any(|window| window == b"moov"));
    }

    #[test]
    fn tool_inspection_reports_only_complete_pinned_fixture_versions() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let hub_root = root
            .join("tools")
            .join(HUB_PACKAGE)
            .join(HUB_VERSION)
            .join("node_modules")
            .join(HUB_PACKAGE);
        std::fs::create_dir_all(hub_root.join("dist/server")).unwrap();
        std::fs::write(hub_root.join("dist/server/cli.mjs"), "fake hub").unwrap();
        std::fs::write(
            root.join("tools").join(HUB_PACKAGE).join(HUB_VERSION).join(".install-complete"),
            HUB_VERSION,
        )
        .unwrap();
        let agent_root = root
            .join("tools")
            .join(AGENT_PACKAGE)
            .join(AGENT_VERSION)
            .join("node_modules")
            .join(AGENT_PACKAGE);
        std::fs::create_dir_all(agent_root.join("bin")).unwrap();
        std::fs::write(agent_root.join("bin/agent-device.mjs"), "fake agent").unwrap();
        std::fs::write(
            root.join("tools").join(AGENT_PACKAGE).join(AGENT_VERSION).join(".install-complete"),
            AGENT_VERSION,
        )
        .unwrap();

        let (hub_installed, agent_installed, versions) = inspect_toolchain(root);
        assert!(hub_installed);
        assert!(agent_installed);
        assert_eq!(versions.hub.installed_versions, vec![HUB_VERSION.to_owned()]);
        assert_eq!(versions.agent.installed_versions, vec![AGENT_VERSION.to_owned()]);
        assert_eq!(versions.hub.running_version, None);
        assert_eq!(versions.agent.running_version, None);
    }

    #[tokio::test]
    async fn discovery_uses_fixture_cli_outputs_and_drops_stale_avd_duplicates() {
        let host = Arc::new(FixtureHost::new(vec![
            (
                "xcrun",
                HostOutput {
                    stdout: serde_json::to_vec(&serde_json::json!({
                        "devices": {
                            "com.apple.CoreSimulator.SimRuntime.iOS-18-0": [{
                                "udid": "ios-sim",
                                "name": "iPhone Fixture",
                                "state": "Booted",
                                "isAvailable": true
                            }]
                        }
                    }))
                    .unwrap(),
                    stderr: vec![],
                    code: 0,
                },
            ),
            (
                "adb",
                HostOutput {
                    stdout: b"List of devices attached\nemulator-5554 device model:pixel_8\n".to_vec(),
                    stderr: vec![],
                    code: 0,
                },
            ),
            (
                "adb",
                HostOutput {
                    stdout: b"OK\nPixel_8\n".to_vec(),
                    stderr: vec![],
                    code: 0,
                },
            ),
            (
                "emulator",
                HostOutput {
                    stdout: b"Pixel_8\nPixel_Pro\n".to_vec(),
                    stderr: vec![],
                    code: 0,
                },
            ),
        ]));
        let devices = discover_host(host, None).await.unwrap();
        assert!(devices.iter().any(|device| device.id == "ios-sim" && device.platform == DevicePlatform::Ios));
        assert!(devices.iter().any(|device| device.id == "emulator-5554" && device.booted));
        assert!(devices.iter().any(|device| device.id == "Pixel_Pro" && !device.booted));
        assert!(!devices.iter().any(|device| device.id == "Pixel_8" && !device.booted));
    }
}

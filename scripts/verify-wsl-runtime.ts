// Exercise the distributed ELF payload, not the builder's Nix store binaries.
import assert from "node:assert/strict";
import { execFileSync, type ExecFileSyncOptions, type ExecFileSyncOptionsWithStringEncoding } from "node:child_process";
import { cpSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, readlinkSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

const source = resolve(process.argv[2]);
const scope = mkdtempSync(join(tmpdir(), "bex-wsl-runtime-"));
const tools = join(scope, "tools");
mkdirSync(tools);
// Linux CI has no wsl.exe filesystem bridge. All other bootstrap steps run as shipped.
writeFileSync(join(tools, "wslpath"), '#!/bin/sh\nprintf "%s\\n" "$2"\n', { mode: 0o700 });
const env = { ...process.env, PATH: `${tools}:${process.env.PATH}`,
  XDG_DATA_HOME: join(scope, "data"), XDG_CACHE_HOME: join(scope, "cache"),
  XDG_CONFIG_HOME: join(scope, "config"), CODEX_HOME: join(scope, "codex"),
  CLAUDE_CONFIG_DIR: join(scope, "claude"), WSL_DISTRO_NAME: "Ubuntu" };
const revision = readFileSync(join(source, "revision"), "utf8").trim();
const runtime = join(env.XDG_CACHE_HOME, "bex", "wsl", revision);
const loader = join(runtime, "lib", "ld-linux-x86-64.so.2");
const host = join(runtime, "host-daemon");
const runtimeRoot = join(env.XDG_CACHE_HOME, "bex", "wsl");
const launchFile = join(runtimeRoot, "host-launch.json");
mkdirSync(runtimeRoot, { recursive: true });
const workingDirectory = join(scope, "existing working directory");
mkdirSync(workingDirectory);
const originalLaunch = { executable: "previous-host", working_directory: workingDirectory, arguments: [
  "--name", "Preserved WSL", "--no-relay", "--codex", join(scope, "missing-codex"),
  "--claude", join(scope, "missing-claude"), "--deepseek", join(scope, "missing-deepseek"),
] };
writeFileSync(launchFile, JSON.stringify(originalLaunch), { mode: 0o600 });
const previousSource = join(scope, "previous payload");
cpSync(source, previousSource, { recursive: true });
writeFileSync(join(previousSource, "revision"), "0".repeat(40));
function run(file: string, args: string[], options: ExecFileSyncOptionsWithStringEncoding): string;
function run(file: string, args: string[], options?: ExecFileSyncOptions): Buffer;
function run(file: string, args: string[], options: ExecFileSyncOptions = {}): string | Buffer {
  return execFileSync(file, args, { env, timeout: 60000, ...options });
}
try {
  const firstReply = run("sh", ["crates/agent-local-host/src/wsl-bootstrap.sh", previousSource]);
  const firstAccess = JSON.parse(firstReply.toString("utf8"));
  firstReply.fill(0);
  const previousHost = join(runtimeRoot, "0".repeat(40), "host-daemon");
  const firstStatus = JSON.parse(run(previousHost, ["status"]).toString("utf8"));
  const reply = run("sh", ["crates/agent-local-host/src/wsl-bootstrap.sh", source]);
  const access = JSON.parse(reply.toString("utf8"));
  assert.equal(access.secret.length, 32);
  assert.ok(access.ticket);
  assert.deepEqual(access.secret, firstAccess.secret, "updating must keep the same local client identity");
  reply.fill(0);
  const status = JSON.parse(run(host, ["status"]).toString("utf8"));
  assert.equal(status.name, "Preserved WSL");
  assert.equal(status.nodeId, firstStatus.nodeId, "updating must keep the same Host identity");
  const savedLaunch = JSON.parse(readFileSync(launchFile, "utf8"));
  assert.deepEqual(savedLaunch.arguments, originalLaunch.arguments);
  assert.equal(savedLaunch.working_directory, originalLaunch.working_directory);
  const dependencies = run(loader, ["--list", host], { encoding: "utf8", env: { ...env, LD_LIBRARY_PATH: "" } });
  assert.ok(!dependencies.includes("/nix/store"), dependencies);
  const interpreter = run(loader, ["--library-path", join(runtime, "lib"), join(runtime, "patchelf"), "--print-interpreter", host], { encoding: "utf8" }).trim();
  assert.equal(interpreter, loader);
  console.log("The installed WSL Host upgrades its running image, preserves identity/settings, and uses only bundled libraries.");
} finally {
  const processes = readdirSync("/proc").filter(name => /^\d+$/.test(name)).filter(pid => {
    try { return readlinkSync(`/proc/${pid}/exe`).startsWith(`${runtimeRoot}/`); } catch { return false; }
  });
  for (const pid of processes) {
    try { process.kill(Number(pid), "SIGINT"); } catch (error) { if ((error as NodeJS.ErrnoException).code !== "ESRCH") throw error; }
  }
  for (let attempt = 0; attempt < 100; attempt++) {
    const alive = processes.some(pid => { try { process.kill(Number(pid), 0); return true; } catch { return false; } });
    if (!alive) break;
    if (attempt === 99) throw new Error("The isolated WSL Host did not stop");
    await new Promise(resolve => setTimeout(resolve, 100));
  }
  rmSync(scope, { recursive: true, force: true });
}

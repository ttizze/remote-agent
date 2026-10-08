import crypto from "node:crypto";
import fs from "node:fs";
import fsp from "node:fs/promises";
import path from "node:path";
import { execFile as execFileCallback } from "node:child_process";
import { promisify } from "node:util";

const spawnFile = promisify(execFileCallback);
const VERSION_PATTERN = /^(\d+)\.(\d+)\.(\d+)(?:-([0-9A-Za-z.-]+))?$/;
const CHANNELS = Object.freeze(["nightly", "preview", "stable"]);
const UPDATE_TARGETS = Object.freeze(["host", "desktop"]);
const NATIVE_TARGETS = Object.freeze(["android", "ios"]);
const UPDATE_STATUSES = Object.freeze([
  "disabled",
  "idle",
  "checking",
  "available",
  "downloading",
  "downloaded",
  "installing",
  "up-to-date",
  "error",
]);
const NOTE_GROUP_LIMIT = 6;
const NOTE_ITEM_LIMIT = 8;
const NOTE_ITEM_LENGTH = 220;

function parseVersion(value) {
  const match = VERSION_PATTERN.exec(value);
  if (!match) throw new Error(`invalid semantic version: ${value}`);
  return {
    major: Number(match[1]),
    minor: Number(match[2]),
    patch: Number(match[3]),
    prerelease: match[4] ? match[4].split(".") : [],
  };
}

function compareVersions(left, right) {
  const a = parseVersion(left);
  const b = parseVersion(right);
  for (const key of ["major", "minor", "patch"]) {
    if (a[key] !== b[key]) return a[key] < b[key] ? -1 : 1;
  }
  if (a.prerelease.length === 0 && b.prerelease.length === 0) return 0;
  if (a.prerelease.length === 0) return 1;
  if (b.prerelease.length === 0) return -1;
  for (let index = 0; index < Math.max(a.prerelease.length, b.prerelease.length); index += 1) {
    const leftPart = a.prerelease[index];
    const rightPart = b.prerelease[index];
    if (leftPart === undefined) return -1;
    if (rightPart === undefined) return 1;
    if (leftPart === rightPart) continue;
    const leftNumeric = /^\d+$/.test(leftPart);
    const rightNumeric = /^\d+$/.test(rightPart);
    if (leftNumeric && rightNumeric) return Number(leftPart) < Number(rightPart) ? -1 : 1;
    if (leftNumeric !== rightNumeric) return leftNumeric ? -1 : 1;
    return leftPart < rightPart ? -1 : 1;
  }
  return 0;
}

function assertChannel(channel) {
  if (!CHANNELS.includes(channel)) throw new Error(`unsupported release channel: ${channel}`);
  return channel;
}

function assertTarget(target) {
  if (!UPDATE_TARGETS.includes(target)) throw new Error(`unsupported update target: ${target}`);
  return target;
}

function assertNativeTarget(target) {
  if (!NATIVE_TARGETS.includes(target)) throw new Error(`unsupported native target: ${target}`);
  return target;
}

function truncate(value, length = NOTE_ITEM_LENGTH) {
  const text = String(value).replace(/\s+/gu, " ").trim();
  return text.length > length ? `${text.slice(0, length - 1).trimEnd()}…` : text;
}

/** Normalize bounded release notes for every native client. */
function normalizeReleaseNotes(metadata) {
  const source = metadata.release_notes ?? metadata.releaseNotes ?? metadata.notes;
  if (source === undefined || source === null) return [];
  const groups = Array.isArray(source) ? source : [{ title: "Release notes", items: [source] }];
  return groups.slice(0, NOTE_GROUP_LIMIT).flatMap((group) => {
    if (typeof group === "string") {
      const item = truncate(group);
      return item ? [{ title: "Release notes", items: [item] }] : [];
    }
    if (!group || typeof group !== "object") return [];
    const title = truncate(group.title ?? group.heading ?? "Release notes", 120);
    const values = Array.isArray(group.items)
      ? group.items
      : Array.isArray(group.body)
        ? group.body
        : [group.body ?? group.text ?? ""];
    const items = values.map(truncate).filter(Boolean).slice(0, NOTE_ITEM_LIMIT);
    return items.length ? [{ title: title || "Release notes", items }] : [];
  });
}

function assertHttpsUrl(value, field) {
  let url;
  try {
    url = new URL(value);
  } catch {
    throw new Error(`${field} must be an absolute HTTPS URL`);
  }
  if (url.protocol !== "https:") throw new Error(`${field} must be an absolute HTTPS URL`);
  return url.toString();
}

function validateMetadata(metadata, source = "release metadata") {
  if (
    !metadata ||
    metadata.schema !== 1 ||
    typeof metadata.version !== "string" ||
    typeof metadata.channel !== "string"
  ) {
    throw new Error(`invalid release metadata: ${source}`);
  }
  parseVersion(metadata.version);
  assertChannel(metadata.channel);
  if (metadata.update_url !== undefined && metadata.update_url !== null) {
    assertHttpsUrl(metadata.update_url, "update_url");
  }
  if (metadata.assets !== undefined) {
    if (!Array.isArray(metadata.assets)) throw new Error("release metadata assets must be an array");
    for (const asset of metadata.assets) {
      if (
        !asset ||
        typeof asset.name !== "string" ||
        asset.name.length === 0 ||
        path.basename(asset.name) !== asset.name ||
        typeof asset.sha256 !== "string" ||
        !/^[0-9a-f]{64}$/u.test(asset.sha256) ||
        !Number.isSafeInteger(asset.size) ||
        asset.size < 0
      ) {
        throw new Error("release metadata contains an invalid asset");
      }
    }
  }
  if (metadata.native_updates !== undefined) {
    if (!metadata.native_updates || typeof metadata.native_updates !== "object") {
      throw new Error("release metadata native_updates must be an object");
    }
    for (const target of NATIVE_TARGETS) {
      const update = metadata.native_updates[target];
      if (update === undefined) continue;
      if (!update || typeof update !== "object") throw new Error(`invalid ${target} native update`);
      if (update.url !== undefined && update.url !== null) assertHttpsUrl(update.url, `${target}.url`);
    }
  }
  return metadata;
}

function readMetadata(filePath) {
  return validateMetadata(JSON.parse(fs.readFileSync(filePath, "utf8")), filePath);
}

function platformName(platform = process.platform) {
  if (platform === "darwin") return "macos";
  if (platform === "win32") return "windows";
  if (platform === "linux") return "linux";
  throw new Error(`unsupported host platform: ${platform}`);
}

function architectureName(architecture = process.arch) {
  if (["x64", "x86_64"].includes(architecture)) return "x86_64";
  if (["arm64", "aarch64"].includes(architecture)) return "arm64";
  throw new Error(`unsupported host architecture: ${architecture}`);
}

function assetName({ target, platform = process.platform, architecture = process.arch }) {
  assertTarget(target);
  const platformValue = platformName(platform);
  const architectureValue = architectureName(architecture);
  if (target === "host") return `host-${platformValue}-${architectureValue}.tar.gz`;
  if (platformValue === "macos") return "desktop-macos-arm64.zip";
  return `desktop-${platformValue}-${architectureValue}.tar.gz`;
}

function findAsset(metadata, name) {
  const asset = metadata.assets?.find((candidate) => candidate.name === name);
  if (!asset) return null;
  return { name: asset.name, sha256: asset.sha256, size: asset.size };
}

function assetUrl(metadata, asset) {
  if (!metadata.update_url || !asset) return null;
  return new URL(asset.name, metadata.update_url).toString();
}

function nativeUpdate(metadata, currentVersion, target) {
  assertNativeTarget(target);
  parseVersion(currentVersion);
  const configured = metadata.native_updates?.[target];
  return {
    target,
    channel: metadata.channel,
    current_version: currentVersion,
    latest_version: metadata.version,
    update_available: compareVersions(currentVersion, metadata.version) < 0,
    // Store links are metadata supplied by trusted release configuration. An
    // absent link remains unavailable instead of inventing a published state.
    store_url: configured?.url ?? null,
    release_notes: normalizeReleaseNotes(metadata),
    checked_at: null,
  };
}

function initialUpdateState({ target = "host", currentVersion, channel = "nightly", enabled = true }) {
  assertTarget(target);
  parseVersion(currentVersion);
  assertChannel(channel);
  return {
    enabled,
    status: enabled ? "idle" : "disabled",
    target,
    channel,
    current_version: currentVersion,
    available_version: null,
    downloaded_version: null,
    release_notes: [],
    omitted_release_count: 0,
    download_percent: null,
    checked_at: null,
    message: null,
    error_context: null,
    can_retry: false,
    restart_required: false,
    update_url: null,
    download_url: null,
    artifact_name: null,
  };
}

function updateStateAfterCheck(state, metadata, checkedAt = new Date().toISOString()) {
  validateMetadata(metadata);
  if (metadata.channel !== state.channel) {
    throw new Error(`metadata channel '${metadata.channel}' does not match '${state.channel}'`);
  }
  const available = compareVersions(state.current_version, metadata.version) < 0;
  const name = assetName({ target: state.target });
  const asset = findAsset(metadata, name);
  const downloaded = state.downloaded_version === metadata.version;
  const notes = normalizeReleaseNotes(metadata);
  return {
    ...state,
    status: downloaded ? "downloaded" : available ? "available" : "up-to-date",
    available_version: available || downloaded ? metadata.version : null,
    downloaded_version: downloaded ? metadata.version : null,
    release_notes: downloaded && notes.length === 0 ? state.release_notes : notes,
    omitted_release_count: 0,
    download_percent: downloaded ? 100 : null,
    checked_at: checkedAt,
    message: available && !asset ? `No ${state.target} artifact was published for this platform.` : null,
    error_context: null,
    can_retry: downloaded,
    restart_required: state.restart_required,
    update_url: metadata.update_url ?? null,
    download_url: assetUrl(metadata, asset),
    artifact_name: asset?.name ?? null,
  };
}

function transitionUpdateState(state, event) {
  if (!UPDATE_STATUSES.includes(state.status)) throw new Error(`invalid update status: ${state.status}`);
  switch (event.type) {
    case "check-start":
      return {
        ...state,
        status: "checking",
        checked_at: event.checked_at ?? state.checked_at,
        message: null,
        error_context: null,
        download_percent: state.downloaded_version ? 100 : null,
        can_retry: false,
      };
    case "check-success":
      return updateStateAfterCheck(state, event.metadata, event.checked_at);
    case "check-failure":
      return state.downloaded_version
        ? { ...state, status: "downloaded", checked_at: event.checked_at, can_retry: true, message: null, error_context: null, download_percent: 100 }
        : { ...state, status: "error", checked_at: event.checked_at, can_retry: true, message: event.message, error_context: "check", download_percent: null };
    case "download-start":
      if (!state.available_version) throw new Error("cannot download without an available update");
      return { ...state, status: "downloading", download_percent: 0, message: null, error_context: null, can_retry: false };
    case "download-progress":
      if (!Number.isFinite(event.percent) || event.percent < 0 || event.percent > 100) throw new Error("download progress must be between 0 and 100");
      return { ...state, status: "downloading", download_percent: Math.round(event.percent), message: null, error_context: null };
    case "download-success":
      return { ...state, status: "downloaded", available_version: event.version, downloaded_version: event.version, download_percent: 100, message: null, error_context: null, can_retry: true };
    case "download-failure":
      return { ...state, status: state.available_version ? "available" : "error", message: event.message, error_context: "download", can_retry: Boolean(state.available_version), download_percent: null };
    case "install-start":
      if (state.downloaded_version === null) throw new Error("cannot install without a downloaded update");
      return { ...state, status: "installing", message: null, error_context: null, can_retry: false };
    case "install-success":
      return { ...state, status: "downloaded", restart_required: true, message: null, error_context: null, can_retry: true };
    case "install-failure":
      return { ...state, status: "downloaded", message: event.message, error_context: "install", can_retry: true };
    case "clear":
      return initialUpdateState({ target: state.target, currentVersion: state.current_version, channel: state.channel, enabled: state.enabled });
    default:
      throw new Error(`unknown update event: ${event.type}`);
  }
}

/** A view can render this result without knowing how an archive is installed. */
function updateAction(state) {
  if (!state.enabled) return "none";
  switch (state.status) {
    case "idle":
    case "error":
    case "up-to-date":
      return "check";
    case "available":
      return state.download_url ? "download" : "none";
    case "downloaded":
      return "install";
    default:
      return "none";
  }
}

function restartDecision({ serviceInstalled, serviceCurrent, assumeYes = false, interactive = false }) {
  if (!serviceInstalled || serviceCurrent) return { action: "none", restart_required: false, reason: null };
  if (assumeYes) return { action: "restart", restart_required: true, reason: "explicitly-approved" };
  if (interactive) return { action: "ask", restart_required: true, reason: "service-restart-interrupts-work" };
  return { action: "defer", restart_required: true, reason: "non-interactive-service-restart-needs-explicit-approval" };
}

async function fetchMetadata(url, fetchImpl = globalThis.fetch) {
  assertHttpsUrl(url, "metadata URL");
  if (typeof fetchImpl !== "function") throw new Error("fetch is unavailable; pass a fetch implementation");
  const response = await fetchImpl(url, { headers: { accept: "application/json" } });
  if (!response?.ok) throw new Error(`release metadata request failed: HTTP ${response?.status ?? "unknown"}`);
  let metadata;
  try {
    metadata = await response.json();
  } catch (error) {
    throw new Error(`release metadata was not valid JSON: ${error instanceof Error ? error.message : error}`);
  }
  return validateMetadata(metadata, url);
}

function bytesHash(bytes) {
  return crypto.createHash("sha256").update(bytes).digest("hex");
}

function safeChildPath(root, relative) {
  if (path.isAbsolute(relative) || relative.split(/[\\/]/u).includes("..")) {
    throw new Error(`archive entry escapes install directory: ${relative}`);
  }
  const resolvedRoot = path.resolve(root);
  const resolved = path.resolve(resolvedRoot, relative);
  if (resolved !== resolvedRoot && !resolved.startsWith(`${resolvedRoot}${path.sep}`)) {
    throw new Error(`archive entry escapes install directory: ${relative}`);
  }
  return resolved;
}

async function downloadArtifact(metadata, {
  target,
  platform = process.platform,
  architecture = process.arch,
  destination,
  fetchImpl = globalThis.fetch,
  fsImpl = fsp,
} = {}) {
  validateMetadata(metadata);
  assertTarget(target);
  const name = assetName({ target, platform, architecture });
  const asset = findAsset(metadata, name);
  if (!asset) throw new Error(`release has no ${target} artifact for ${platformName(platform)}-${architectureName(architecture)}`);
  const url = assetUrl(metadata, asset);
  if (!url) throw new Error("release metadata does not include an artifact URL");
  if (typeof fetchImpl !== "function") throw new Error("fetch is unavailable; pass a fetch implementation");
  const response = await fetchImpl(url, { headers: { accept: "application/octet-stream" } });
  if (!response?.ok) throw new Error(`artifact request failed: HTTP ${response?.status ?? "unknown"}`);
  const bytes = Buffer.from(await response.arrayBuffer());
  if (bytes.length !== asset.size) throw new Error(`artifact size mismatch for ${asset.name}`);
  const digest = bytesHash(bytes);
  if (digest !== asset.sha256) throw new Error(`artifact checksum mismatch for ${asset.name}`);
  if (!destination) throw new Error("artifact destination is required");
  const destinationPath = path.resolve(destination);
  await fsImpl.mkdir(path.dirname(destinationPath), { recursive: true, mode: 0o700 });
  const temporary = `${destinationPath}.${process.pid}.${Date.now()}.tmp`;
  try {
    await fsImpl.writeFile(temporary, bytes, { mode: 0o600 });
    await fsImpl.rename(temporary, destinationPath);
  } catch (error) {
    await fsImpl.rm(temporary, { force: true }).catch(() => {});
    throw error;
  }
  return { path: destinationPath, version: metadata.version, name: asset.name, size: bytes.length, sha256: digest };
}

async function listArchiveEntries(archivePath, extension, spawnImpl = spawnFile) {
  const command = extension === ".zip" ? "unzip" : "tar";
  const args = extension === ".zip" ? ["-Z1", archivePath] : ["-tzf", archivePath];
  const result = await spawnImpl(command, args, { encoding: "utf8", windowsHide: true });
  return String(result.stdout).split(/\r?\n/u).map((entry) => entry.trim()).filter(Boolean);
}

async function extractArchive(archivePath, destination, { spawnImpl = spawnFile } = {}) {
  const extension = archivePath.endsWith(".zip") ? ".zip" : archivePath.endsWith(".tar.gz") ? ".tar.gz" : null;
  if (!extension) throw new Error(`unsupported update archive: ${archivePath}`);
  const entries = await listArchiveEntries(archivePath, extension, spawnImpl);
  entries.forEach((entry) => safeChildPath(destination, entry));
  await fsp.mkdir(destination, { recursive: true, mode: 0o700 });
  const args = extension === ".zip"
    ? ["-q", archivePath, "-d", destination]
    : ["--extract", "--gzip", "--file", archivePath, "--directory", destination, "--no-same-owner", "--no-same-permissions"];
  await spawnImpl(extension === ".zip" ? "unzip" : "tar", args, { windowsHide: true });
}

/** Install only into a private version directory; the owner decides when to restart. */
async function installArtifact(transaction, { installRoot, extractImpl = extractArchive, fsImpl = fsp } = {}) {
  if (!transaction?.path || !transaction.version) throw new Error("a verified artifact transaction is required");
  if (!installRoot) throw new Error("update install root is required");
  const root = path.resolve(installRoot);
  const versionRoot = safeChildPath(root, transaction.version);
  const temporary = `${versionRoot}.${process.pid}.${Date.now()}.staging`;
  await fsImpl.rm(temporary, { recursive: true, force: true });
  await fsImpl.mkdir(root, { recursive: true, mode: 0o700 });
  try {
    await extractImpl(transaction.path, temporary);
    await fsImpl.rename(temporary, versionRoot);
  } catch (error) {
    await fsImpl.rm(temporary, { recursive: true, force: true }).catch(() => {});
    throw error;
  }
  return { version: transaction.version, install_path: versionRoot, restart_required: true };
}

/** The original small local comparison remains useful to scripts and tests. */
function checkUpdate(metadata, currentVersion, channel = metadata.channel, options = {}) {
  validateMetadata(metadata);
  if (metadata.channel !== channel) {
    throw new Error(`metadata channel '${metadata.channel}' does not match '${channel}'`);
  }
  parseVersion(currentVersion);
  const result = {
    update_available: compareVersions(currentVersion, metadata.version) < 0,
    current_version: currentVersion,
    latest_version: metadata.version,
    channel,
    update_url: metadata.update_url ?? null,
  };
  if (Object.keys(options).length === 0) return result;
  const target = options.target ?? "host";
  const state = updateStateAfterCheck(initialUpdateState({ target, currentVersion, channel }), metadata, options.checkedAt);
  return { ...result, ...state };
}

function checkUpdateState(metadata, {
  target = "host",
  currentVersion,
  channel = metadata.channel,
  checkedAt = new Date().toISOString(),
} = {}) {
  return updateStateAfterCheck(initialUpdateState({ target, currentVersion, channel }), metadata, checkedAt);
}

function argument(name, fallback = undefined) {
  const index = process.argv.indexOf(name);
  return index === -1 ? fallback : process.argv[index + 1];
}

export {
  CHANNELS,
  NATIVE_TARGETS,
  UPDATE_STATUSES,
  UPDATE_TARGETS,
  assetName,
  checkUpdate,
  checkUpdateState,
  compareVersions,
  downloadArtifact,
  extractArchive,
  fetchMetadata,
  initialUpdateState,
  installArtifact,
  nativeUpdate,
  normalizeReleaseNotes,
  parseVersion,
  readMetadata,
  restartDecision,
  transitionUpdateState,
  updateAction,
  updateStateAfterCheck,
  validateMetadata,
};

if (process.argv[1]?.endsWith("release-update-check.mjs")) {
  try {
    const metadataPath = argument("--metadata");
    const metadataUrl = argument("--metadata-url");
    const currentVersion = argument("--current");
    const channel = argument("--channel");
    const target = argument("--target", "host");
    if ((!metadataPath && !metadataUrl) || !currentVersion) {
      throw new Error("--metadata or --metadata-url, and --current are required");
    }
    const metadata = metadataPath ? readMetadata(metadataPath) : await fetchMetadata(metadataUrl);
    const state = checkUpdateState(metadata, { target, currentVersion, channel: channel ?? metadata.channel });
    let result = { ...state, action: updateAction(state) };
    const downloadPath = argument("--download");
    if (downloadPath && result.action === "download") {
      const transaction = await downloadArtifact(metadata, {
        target,
        platform: argument("--platform"),
        architecture: argument("--arch"),
        destination: downloadPath,
      });
      result = { ...transitionUpdateState(state, { type: "download-success", version: transaction.version }), transaction, action: "install" };
      const installRoot = argument("--install-dir");
      if (installRoot) {
        const installed = await installArtifact(transaction, { installRoot });
        result = { ...transitionUpdateState(result, { type: "install-start" }), ...transitionUpdateState(result, { type: "install-success" }), installed, action: "restart" };
      }
    }
    process.stdout.write(`${JSON.stringify(result)}\n`);
  } catch (error) {
    console.error(error instanceof Error ? error.message : error);
    process.exitCode = 1;
  }
}

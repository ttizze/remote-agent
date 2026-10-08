import fs from "node:fs";

const VERSION_PATTERN = /^(\d+)\.(\d+)\.(\d+)(?:-([0-9A-Za-z.-]+))?$/;

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

function readMetadata(path) {
  const metadata = JSON.parse(fs.readFileSync(path, "utf8"));
  if (metadata?.schema !== 1 || typeof metadata.version !== "string" || typeof metadata.channel !== "string") {
    throw new Error(`invalid release metadata: ${path}`);
  }
  parseVersion(metadata.version);
  return metadata;
}

function checkUpdate(metadata, currentVersion, channel = metadata.channel) {
  if (metadata.channel !== channel) {
    throw new Error(`metadata channel '${metadata.channel}' does not match '${channel}'`);
  }
  parseVersion(currentVersion);
  return {
    update_available: compareVersions(currentVersion, metadata.version) < 0,
    current_version: currentVersion,
    latest_version: metadata.version,
    channel,
    update_url: metadata.update_url ?? null,
  };
}

function argument(name, fallback = undefined) {
  const index = process.argv.indexOf(name);
  return index === -1 ? fallback : process.argv[index + 1];
}

export { checkUpdate, compareVersions, parseVersion, readMetadata };

if (process.argv[1]?.endsWith("release-update-check.mjs")) {
  try {
    const metadataPath = argument("--metadata");
    const currentVersion = argument("--current");
    const channel = argument("--channel");
    if (!metadataPath || !currentVersion) throw new Error("--metadata and --current are required");
    const metadata = readMetadata(metadataPath);
    process.stdout.write(`${JSON.stringify(checkUpdate(metadata, currentVersion, channel))}\n`);
  } catch (error) {
    console.error(error instanceof Error ? error.message : error);
    process.exitCode = 1;
  }
}

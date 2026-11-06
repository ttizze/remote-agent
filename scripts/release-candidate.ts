import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { appendFileSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';
import { iosDistribution, requestFromEnvironment, type AppleBuildPlan } from './apple-release.ts';
import { submitProduction } from './app-store-release.ts';

type AssetVerification = { size: number; sha256: string };
type ReleaseManifest = AppleBuildPlan & { assets: Record<string, AssetVerification> };
type ReleaseSummary = { tagName: string; isDraft?: boolean; isPrerelease?: boolean };

const commonPackages = [
  'bex-macOS-arm64.zip', 'bex-windows-x86_64-setup.exe',
  'bex-windows-x86_64-release.zip', 'bex-android.apk', 'bex-linux-x86_64.tar.gz',
];
export const packages = (channel: string) => [...commonPackages, ...(channel === 'nightly' ? ['bex-iOS-preview.ipa'] : [])];
const checksum = (bytes: Buffer) => createHash('sha256').update(bytes).digest('hex');

export function releaseNumber(tag: string | undefined, channel: string) {
  const match = new RegExp(`^${channel}-([1-9][0-9]*)$`).exec(tag ?? '');
  if (!match || !Number.isSafeInteger(Number(match[1]))) throw new Error(`Invalid ${channel} release tag.`);
  return match[1];
}

export function latestNightly(releases: readonly ReleaseSummary[]) {
  const latest = releases.filter(item => !item.isDraft && item.isPrerelease && /^nightly-[1-9][0-9]*$/.test(item.tagName))
    .sort((a, b) => Number(releaseNumber(b.tagName, 'nightly')) - Number(releaseNumber(a.tagName, 'nightly')))[0];
  if (!latest) throw new Error('Publish a verified Nightly before creating a stable release.');
  return latest.tagName;
}

export function validateRelease(manifest: ReleaseManifest, tag: string, sha: string) {
  const channel = tag.startsWith('nightly-') ? 'nightly' : 'stable';
  const number = releaseNumber(tag, channel === 'nightly' ? 'nightly' : 'prd');
  if (manifest.channel !== channel || manifest.tag !== tag || manifest.version !== number
    || manifest.sha !== sha || !/^[a-f0-9]{40}$/.test(sha)) throw new Error('The release tag, source revision, channel and build number must agree.');
  if (manifest.bundleId !== iosDistribution(channel).bundleId || !/^\d+\.\d+\.\d+$/.test(manifest.versionName)) throw new Error('Invalid iPhone release identity.');
  if (channel === 'stable') {
    releaseNumber(manifest.sourceTag, 'nightly');
    if (manifest.appId !== '6816015343' || !manifest.buildId) throw new Error('Stable must identify its rebuilt BEX App Store binary.');
  } else if (manifest.appId || manifest.buildId || manifest.sourceTag) throw new Error('Nightly distributes the separate internally signed Preview app.');
  const expected = packages(channel).flatMap(name => [name, `${name}.sha256`]).sort();
  if (JSON.stringify(Object.keys(manifest.assets).sort()) !== JSON.stringify(expected)) throw new Error('The release must contain every platform package and checksum.');
  for (const asset of Object.values(manifest.assets)) {
    if (!/^[a-f0-9]{64}$/.test(asset.sha256) || !Number.isSafeInteger(asset.size) || asset.size <= 0) throw new Error('Invalid release asset verification data.');
  }
}

export function verifyPackages(directory: string, assets: Record<string, AssetVerification>) {
  for (const [name, expected] of Object.entries(assets)) {
    const bytes = readFileSync(join(directory, name));
    if (bytes.length !== expected.size || checksum(bytes) !== expected.sha256) throw new Error(`Release checksum mismatch: ${name}`);
  }
  for (const name of Object.keys(assets).filter(name => !name.endsWith('.sha256'))) {
    const sidecar = readFileSync(join(directory, `${name}.sha256`), 'utf8').trim();
    if (!new RegExp(`^${assets[name].sha256}\\s+\\*?${name.replaceAll('.', '\\.')}\\s*$`).test(sidecar)) throw new Error(`Invalid checksum file: ${name}`);
  }
}

const run = (command: string, args: string[]) => execFileSync(command, args, { encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] }).trim();
const repository = () => {
  const value = process.env.GITHUB_REPOSITORY;
  if (!/^[\w.-]+\/[\w.-]+$/.test(value || '')) throw new Error('GITHUB_REPOSITORY is required.');
  return value!;
};
const gh = (...args: string[]) => run('gh', args);
const releases = (): ReleaseSummary[] => JSON.parse(gh('release', 'list', '--repo', repository(), '--limit', '1000', '--json', 'tagName,isDraft,isPrerelease'));
const revision = (tag: string) => run('git', ['rev-parse', '--verify', `refs/tags/${tag}^{commit}`]);

async function inDirectory<T>(action: (directory: string) => T | Promise<T>) {
  const directory = mkdtempSync(join(tmpdir(), 'bex-release-'));
  try { return await action(directory); }
  finally { rmSync(directory, { recursive: true, force: true }); }
}

function readManifest(directory: string, tag: string) {
  const manifest = JSON.parse(readFileSync(join(directory, 'release.json'), 'utf8')) as ReleaseManifest;
  validateRelease(manifest, tag, revision(tag));
  return manifest;
}

async function withManifest<T>(tag: string, action: (manifest: ReleaseManifest, directory: string) => T | Promise<T>) {
  return inDirectory(async directory => {
    gh('release', 'download', tag, '--repo', repository(), '--pattern', 'release.json', '--dir', directory);
    return action(readManifest(directory, tag), directory);
  });
}

async function resolve() {
  const tag = latestNightly(releases());
  await withManifest(tag, manifest => {
    run('git', ['merge-base', '--is-ancestor', manifest.sha, 'origin/main']);
    if (process.env.GITHUB_OUTPUT) appendFileSync(process.env.GITHUB_OUTPUT, `sha=${manifest.sha}\nsource-tag=${tag}\n`);
    console.log(`Stable will rebuild ${tag} at ${manifest.sha}.`);
  });
}

function publish(tag: string, channel: string) {
  const newer = channel === 'stable' && releases().some(item => !item.isDraft && !item.isPrerelease && /^prd-[1-9][0-9]*$/.test(item.tagName)
    && Number(releaseNumber(item.tagName, 'prd')) > Number(releaseNumber(tag, 'prd')));
  gh('release', 'edit', tag, '--repo', repository(), '--draft=false', `--prerelease=${channel === 'nightly'}`, `--latest=${channel === 'stable' && !newer}`);
}

async function seal(tag: string) {
  // The REST tag endpoint only returns published releases; view also resolves staged drafts.
  const isDraft = JSON.parse(gh('release', 'view', tag, '--repo', repository(), '--json', 'isDraft')).isDraft;
  await inDirectory(async directory => {
    gh('release', 'download', tag, '--repo', repository(), '--dir', directory);
    const previous = isDraft ? null : readFileSync(join(directory, 'release.json'), 'utf8');
    const plan = JSON.parse(readFileSync(join(directory, 'apple-build.json'), 'utf8')) as AppleBuildPlan;
    const manifest: ReleaseManifest = { ...plan, assets: Object.fromEntries(packages(plan.channel).flatMap(name => [name, `${name}.sha256`]).map(name => {
      const bytes = readFileSync(join(directory, name));
      return [name, { size: bytes.length, sha256: checksum(bytes) }];
    })) };
    validateRelease(manifest, tag, revision(tag));
    verifyPackages(directory, manifest.assets);
    if (manifest.channel === 'stable') await withManifest(manifest.sourceTag!, source => {
      if (source.sha !== manifest.sha) throw new Error('Stable must rebuild the selected Nightly source revision.');
    });
    const path = join(directory, 'release.json');
    writeFileSync(path, JSON.stringify(manifest, null, 2) + '\n');
    if (!isDraft) {
      if (readFileSync(path, 'utf8') !== previous) throw new Error('Published releases cannot be changed.');
      return;
    }
    gh('release', 'upload', tag, path, '--repo', repository(), '--clobber');
    if (manifest.channel === 'nightly') publish(tag, 'nightly');
    else {
      const status = await submitProduction(requestFromEnvironment(), manifest.appId!, manifest.buildId!, manifest.versionName, manifest.locale, manifest.notes);
      if (status.published) publish(tag, 'stable');
      console.log(`${tag}: ${status.state}`);
    }
  });
}

async function sync() {
  const request = requestFromEnvironment();
  const failures = [];
  for (const item of releases().filter(item => item.isDraft && /^prd-[1-9][0-9]*$/.test(item.tagName))) {
    try {
      await withManifest(item.tagName, async (manifest, directory) => {
        const status = await submitProduction(request, manifest.appId!, manifest.buildId!, manifest.versionName, manifest.locale, manifest.notes);
        if (status.published) {
          gh('release', 'download', item.tagName, '--repo', repository(), '--skip-existing', '--dir', directory);
          verifyPackages(directory, manifest.assets);
          publish(item.tagName, 'stable');
        }
        console.log(`${item.tagName}: ${status.state}`);
      });
    } catch (error) { failures.push((error as Error).message); }
  }
  if (failures.length) throw new Error(failures.join('\n'));
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const [command, tag] = process.argv.slice(2);
  const task = command === 'resolve' ? resolve() : command === 'seal' ? seal(tag!) : command === 'sync' ? sync()
    : Promise.reject(new Error('Use resolve, seal nightly-N/prd-N, or sync.'));
  task.catch(error => { console.error((error as Error).message); process.exitCode = 1; });
}

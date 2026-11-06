import { appendFileSync, existsSync, readFileSync, readdirSync, statSync, writeFileSync } from 'node:fs';
import { createPrivateKey, sign } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { pathToFileURL } from 'node:url';
import { join } from 'node:path';

export type AscRequest = <T>(path: string, options?: { method?: string; data?: unknown }) => Promise<T>;
export type AscResource<A> = { id: string; attributes: A };
export type AppleBuild = AscResource<{ version: string; processingState: string; expired: boolean }>;
export type StoreVersion = AscResource<{ versionString: string; appStoreState: string; platform: string }>;
export type AppleBuildPlan = { channel: 'nightly' | 'stable'; sourceTag?: string; bundleId: string; appId?: string;
  version: string; versionName: string; tag: string; sha: string; locale: string; notes: string;
  deliveryId?: string; buildId?: string };

export function nextBuildNumber(sourceVersions: readonly (string | undefined)[], appleVersions: readonly string[], releaseTags: readonly string[]) {
  const source = new Set(sourceVersions);
  if (source.size !== 1 || !sourceVersions.length) throw new Error('Apple source build numbers must agree.');
  const versions = [...source, ...appleVersions, ...releaseTags.flatMap(tag => /^(?:nightly|prd)-(\d+)$/.exec(tag)?.slice(1) ?? [])];
  if (versions.some(version => !/^[1-9]\d*$/.test(version ?? ''))) throw new Error('Apple build numbers must be positive integers.');
  const next = Math.max(...versions.map(Number)) + 1;
  if (!Number.isSafeInteger(next) || next > 9999) throw new Error('The Apple build number needs a new numbering range.');
  return String(next);
}

// Apple versions already approved for distribution keep their existing build.
// All subsequent candidates use the next patch until that version is released.
export function nextMarketingVersion(source: string | undefined, approvedVersions: readonly string[]) {
  const parts = (value: string | undefined) => {
    if (!/^\d+\.\d+(?:\.\d+)?$/.test(value ?? '')) throw new Error('Invalid marketing version.');
    return value!.split('.').map(Number).concat([0]).slice(0, 3);
  };
  const compare = (a: number[], b: number[]) => a[0] - b[0] || a[1] - b[1] || a[2] - b[2];
  let next = parts(source);
  for (const version of approvedVersions) {
    const released = parts(version);
    if (compare(next, released) <= 0) next = [released[0], released[1], released[2] + 1];
  }
  return next.join('.');
}

export function iosDistribution(channel: string | undefined) {
  if (channel === 'nightly') return { bundleId: 'dev.remoteagent.mobile.ios.preview', profileType: 'IOS_APP_ADHOC', method: 'release-testing', product: 'BEX Preview' };
  if (channel === 'stable') return { bundleId: 'dev.remoteagent.mobile.ios', profileType: 'IOS_APP_STORE', method: 'app-store-connect', product: 'Bex' };
  throw new Error('Release channel must be nightly or stable.');
}

const required = (name: string) => { const value = process.env[name]; if (!value) throw new Error(`${name} is required.`); return value; };

export function requestFromEnvironment() {
  return ascClient({ keyId: required('AGENT_ASC_KEY_ID'), issuer: required('AGENT_ASC_ISSUER_ID'),
    privateKey: readFileSync(required('AGENT_ASC_PRIVATE_KEY_FILE')) });
}

export function parseBuildStatus(output: string) {
  const status = (key: string) => new RegExp(`^\\s*=?\\s*${key}:\\s*(\\S+)`, 'mi').exec(output)?.[1];
  if (status('BUILD-STATUS') !== 'VALID' || status('IMPORT-STATUS') !== 'VALID' || status('IS-ON-APP-STORE-CONNECT') !== 'true') {
    throw new Error('Apple has not confirmed a VALID imported build on App Store Connect.');
  }
}

export function deliveryId(logs: readonly string[]) {
  const ids = new Set(logs.flatMap(log => [...log.matchAll(/delivery[-_ ]?(?:uuid|id)["']?\s*[:=]\s*["']?([a-f0-9]{8}(?:-[a-f0-9]{4}){3}-[a-f0-9]{12})/gi)].map(match => match[1].toLowerCase())));
  if (ids.size !== 1) throw new Error('The Xcode upload must have exactly one delivery receipt.');
  return [...ids][0];
}

export function ascClient({ keyId, issuer, privateKey }: { keyId: string; issuer: string; privateKey: string | Buffer }, fetcher = fetch): AscRequest {
  const key = createPrivateKey(privateKey);
  return async <T>(path: string, { method = 'GET', data }: { method?: string; data?: unknown } = {}): Promise<T> => {
    if (!path.startsWith('/v1/') || path.includes('://')) throw new Error('Invalid App Store Connect API path.');
    const encode = (value: unknown) => Buffer.from(JSON.stringify(value)).toString('base64url');
    const now = Math.floor(Date.now() / 1000);
    const payload = `${encode({ alg: 'ES256', kid: keyId, typ: 'JWT' })}.${encode({ iss: issuer, iat: now, exp: now + 600, aud: 'appstoreconnect-v1' })}`;
    const signature = sign('sha256', Buffer.from(payload), { key, dsaEncoding: 'ieee-p1363' }).toString('base64url');
    const response = await fetcher(`https://api.appstoreconnect.apple.com${path}`, {
      method, headers: { Authorization: `Bearer ${payload}.${signature}`, 'Content-Type': 'application/json' },
      ...(data === undefined ? {} : { body: JSON.stringify(data) }), signal: AbortSignal.timeout(60_000),
    });
    if (!response.ok) throw Object.assign(new Error(`App Store Connect ${method} ${path.split('?')[0]} failed (HTTP ${response.status}).`), { status: response.status });
    return (response.status === 204 ? null : await response.json()) as T;
  };
}

export async function listAll<T>(request: AscRequest, path: string): Promise<T[]> {
  const result: T[] = [];
  while (path) {
    const page: { data: T[]; links?: { next?: string | null } } = await request(path);
    result.push(...page.data);
    const next = page.links?.next;
    if (!next) break;
    const url = new URL(next);
    if (url.origin !== 'https://api.appstoreconnect.apple.com') throw new Error('Unexpected App Store Connect pagination origin.');
    path = url.pathname + url.search;
  }
  return result;
}

export async function appForBundleId(request: AscRequest, bundleId: string) {
  // ASC's bundle filter also returns apps whose identifiers have this prefix.
  const apps = await listAll<AscResource<{ bundleId: string; primaryLocale: string }>>(request, `/v1/apps?filter[bundleId]=${encodeURIComponent(bundleId)}`);
  const app = apps.find(app => app.attributes.bundleId === bundleId);
  if (!app) throw new Error(`App ${bundleId} is missing from App Store Connect.`);
  return app;
}

export async function distributeBuild(request: AscRequest, appId: string, buildId: string, groupName: string, notes: string, locale: string) {
  const groups = await listAll<AscResource<{ name: string; isInternalGroup: boolean; hasAccessToAllBuilds: boolean }>>(request, `/v1/betaGroups?filter[app]=${appId}`);
  const group = groups.find(group => group.attributes.name === groupName);
  if (!group) throw new Error(`TestFlight group ${groupName} is missing.`);
  const detail = await request<{ data: AscResource<{ internalBuildState?: string; externalBuildState?: string; autoNotifyEnabled: boolean }> }>(`/v1/builds/${buildId}/buildBetaDetail`);
  const state = detail.data.attributes[group.attributes.isInternalGroup ? 'internalBuildState' : 'externalBuildState'];
  if (!state || ['BETA_REJECTED', 'EXPIRED', 'PROCESSING_EXCEPTION', 'MISSING_EXPORT_COMPLIANCE'].includes(state)) throw new Error(`TestFlight distribution is blocked (${state ?? 'unknown state'}).`);
  if (!group.attributes.isInternalGroup && !detail.data.attributes.autoNotifyEnabled) {
    await request(`/v1/buildBetaDetails/${detail.data.id}`, { method: 'PATCH', data: {
      data: { type: 'buildBetaDetails', id: detail.data.id, attributes: { autoNotifyEnabled: true } },
    } });
  }
  const localized = (await request<{ data: AscResource<{ locale: string; whatsNew: string }>[] }>(`/v1/builds/${buildId}/betaBuildLocalizations`)).data;
  const existing = localized.find(info => info.attributes.locale === locale);
  if (existing) {
    if (existing.attributes.whatsNew !== notes) await request(`/v1/betaBuildLocalizations/${existing.id}`, { method: 'PATCH', data: { data: { type: 'betaBuildLocalizations', id: existing.id, attributes: { whatsNew: notes } } } });
  } else {
    await request('/v1/betaBuildLocalizations', { method: 'POST', data: { data: { type: 'betaBuildLocalizations', attributes: { locale, whatsNew: notes }, relationships: { build: { data: { type: 'builds', id: buildId } } } } } });
  }
  if (!group.attributes.hasAccessToAllBuilds) {
    await request(`/v1/betaGroups/${group.id}/relationships/builds`, { method: 'POST', data: { data: [{ type: 'builds', id: buildId }] } });
  }
  const submit = !group.attributes.isInternalGroup && state === 'READY_FOR_BETA_SUBMISSION';
  if (submit) {
    try {
      await request('/v1/betaAppReviewSubmissions', { method: 'POST', data: { data: { type: 'betaAppReviewSubmissions', relationships: { build: { data: { type: 'builds', id: buildId } } } } } });
    } catch (error) {
      // Apple limits concurrent beta reviews and submissions per day. App
      // Review can proceed independently while TestFlight submission waits.
      if ((error as Error & { status?: number }).status === 409 || (error as Error & { status?: number }).status === 429) return 'Waiting for TestFlight review capacity';
      throw error;
    }
  }
  return submit ? 'Submitted for TestFlight beta review' : state;
}

async function main(command: string | undefined) {
  const planPath = 'target/apple-release/plan.json';
  if (command === 'record-delivery') {
    const directory = process.argv[3];
    const marker = process.argv[4];
    if (!directory || !marker) throw new Error('record-delivery requires the native temporary directory and upload start marker.');
    const since = statSync(marker).mtimeMs;
    const plan = JSON.parse(readFileSync(planPath, 'utf8')) as AppleBuildPlan;
    // Inspect only Xcode-owned bundles, without entering protected system folders.
    const paths = readdirSync(directory, { withFileTypes: true })
      .filter(entry => entry.isDirectory() && entry.name.endsWith('.xcdistributionlogs'))
      .map(entry => join(directory, entry.name, 'ContentDelivery.log'))
      .filter(path => existsSync(path) && statSync(path).mtimeMs > since);
    plan.deliveryId = deliveryId(paths.map(path => readFileSync(path, 'utf8')));
    writeFileSync(planPath, JSON.stringify(plan) + '\n');
    console.log('Xcode delivery receipt recorded.');
    return;
  }
  const keyId = required('AGENT_ASC_KEY_ID');
  const issuer = required('AGENT_ASC_ISSUER_ID');
  const keyPath = required('AGENT_ASC_PRIVATE_KEY_FILE');
  const request = requestFromEnvironment();
  if (command === 'plan') {
    const channel = required('AGENT_RELEASE_CHANNEL');
    const distribution = iosDistribution(channel);
    const app = await appForBundleId(request, 'dev.remoteagent.mobile.ios');
    const builds = await listAll<AppleBuild>(request, `/v1/builds?filter[app]=${app.id}&limit=200`);
    const source = readFileSync('apps/mobile/iosApp/Bex.xcodeproj/project.pbxproj', 'utf8');
    const versions = [...source.matchAll(/CURRENT_PROJECT_VERSION = (\d+);/g)].map(match => match[1]);
    const mac = readFileSync('apps/desktop/macos/Info.plist', 'utf8');
    const macVersion = /<key>CFBundleVersion<\/key>\s*<string>(\d+)<\/string>/.exec(mac)?.[1];
    const releases = JSON.parse(readFileSync('target/apple-release/releases.json', 'utf8')) as { tagName: string }[];
    const version = nextBuildNumber([...versions, macVersion], builds.map(build => build.attributes.version), releases.map(release => release.tagName));
    const storeVersions = await listAll<StoreVersion>(request, `/v1/apps/${app.id}/appStoreVersions?limit=200`);
    if (channel === 'stable' && storeVersions.some(version => version.attributes.platform === 'IOS'
      && version.attributes.appStoreState === 'PENDING_DEVELOPER_RELEASE')) {
      throw new Error('Publish the already approved App Store version before building the next stable release.');
    }
    const sourceVersion = /MARKETING_VERSION = ([\d.]+);/.exec(source)?.[1];
    const approvedVersions = storeVersions.filter(version => version.attributes.platform === 'IOS'
      && ['PENDING_DEVELOPER_RELEASE', 'READY_FOR_DISTRIBUTION', 'READY_FOR_SALE'].includes(version.attributes.appStoreState))
      .map(version => version.attributes.versionString);
    const versionName = nextMarketingVersion(sourceVersion, approvedVersions);
    const sourceTag = channel === 'stable' ? required('AGENT_SOURCE_TAG') : undefined;
    if (sourceTag && !/^nightly-[1-9]\d*$/.test(sourceTag)) throw new Error('Stable must rebuild a selected Nightly.');
    const plan = { channel, sourceTag, bundleId: distribution.bundleId, ...(channel === 'stable' ? { appId: app.id } : {}),
      version, versionName, tag: `${channel === 'nightly' ? 'nightly' : 'prd'}-${version}`,
      sha: required('AGENT_RELEASE_SHA'), locale: app.attributes.primaryLocale,
      notes: execFileSync('git', ['log', '-1', '--format=%s'], { encoding: 'utf8' }).trim() };
    writeFileSync(planPath, JSON.stringify(plan) + '\n');
    appendFileSync(required('GITHUB_ENV'), `AGENT_BUILD_NUMBER=${version}\nAGENT_RELEASE_TAG=${plan.tag}\nAGENT_MARKETING_VERSION=${versionName}\n`);
    console.log(`Apple release build ${version} planned for ${plan.sha}.`);
  } else if (command === 'verify') {
    const plan = JSON.parse(readFileSync(planPath, 'utf8')) as AppleBuildPlan;
    if (plan.channel === 'nightly') {
      const ipa = 'target/apple-release/bex-iOS-preview.ipa';
      if (!statSync(ipa).size) throw new Error('The signed iPhone Preview package is missing.');
      writeFileSync('target/apple-release/apple-build.json', JSON.stringify(plan) + '\n');
      appendFileSync(required('GITHUB_STEP_SUMMARY'), `iPhone **BEX Preview ${plan.version}**: signed internal-distribution IPA for registered devices.\n\nSource: \`${plan.sha}\`.\n`);
      return;
    }
    let output;
    try {
      if (!plan.deliveryId) throw new Error('Missing delivery receipt.');
      output = execFileSync('xcrun', ['altool', '--build-status', '--delivery-id', plan.deliveryId,
        '--wait', '--apiKey', keyId, '--apiIssuer', issuer, '--p8-file-path', keyPath],
      { encoding: 'utf8', timeout: 30 * 60_000, stdio: ['ignore', 'pipe', 'pipe'] });
    } catch { throw new Error('Apple build processing did not complete successfully.'); }
    parseBuildStatus(output);
    const builds = (await request<{ data: AppleBuild[] }>(`/v1/builds?filter[app]=${plan.appId}&filter[version]=${plan.version}`)).data;
    const build = builds.find(build => build.attributes.version === plan.version && build.attributes.processingState === 'VALID');
    if (!build) throw new Error('The processed BEX build could not be verified.');
    plan.buildId = build.id;
    writeFileSync('target/apple-release/apple-build.json', JSON.stringify(plan) + '\n');
    const distribution = await distributeBuild(request, plan.appId!, build.id, process.env.AGENT_TESTFLIGHT_GROUP || 'BEX External', plan.notes, plan.locale);
    console.log(`TestFlight ${plan.version}: ${distribution}.`);
    appendFileSync(required('GITHUB_STEP_SUMMARY'), `TestFlight build **${plan.version}**: ${distribution}.\n\nSource: \`${plan.sha}\`.\n`);
  } else throw new Error('Use apple-release.ts plan, record-delivery or verify.');
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  main(process.argv[2]).catch(error => { console.error((error as Error).message); process.exitCode = 1; });
}

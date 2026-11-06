import test from 'node:test';
import assert from 'node:assert/strict';
import { generateKeyPairSync, verify } from 'node:crypto';
import { chmodSync, mkdirSync, mkdtempSync, readFileSync, rmSync, utimesSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { appForBundleId, ascClient, deliveryId, distributeBuild, iosDistribution, listAll, nextBuildNumber, nextMarketingVersion, parseBuildStatus } from './apple-release.ts';

test('release waits for pending checks and requires both workflows to pass for the exact revision', (t) => {
  const workflow = readFileSync(new URL('../.github/workflows/nightly-release.yml', import.meta.url), 'utf8');
  const block = workflow.match(/      - name: Require both workflows[\s\S]*?        run: \|\n([\s\S]*?)(?=^  release:)/m);
  assert.ok(block);
  const script = block[1].split('\n').map(line => line.replace(/^          /, '')).join('\n');
  const root = mkdtempSync(join(tmpdir(), 'release-checks-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const gh = join(root, 'gh');
  writeFileSync(gh, `#!/usr/bin/env bash
set -euo pipefail
[[ $BEX_TEST_QUERY_FAILURE == 0 ]] || exit 1
if [[ $2 == watch ]]; then
  [[ "$*" == *--exit-status* ]] || exit 3
  printf '%s\\n' "$3" >> "$BEX_TEST_WATCH_LOG"
  [[ $3 != "$BEX_TEST_FAILED_RUN" ]]
  exit
fi
while [[ $# -gt 0 ]]; do
  case "$1" in
    --workflow) workflow=$2; shift ;;
    --commit) [[ $2 == "$AGENT_RELEASE_SHA" ]] || exit 2; shift ;;
  esac
  shift
done
case "$workflow" in
  portable.yml) printf '%s\\n' "$BEX_TEST_PORTABLE_RUN" ;;
  native.yml) printf '%s\\n' "$BEX_TEST_NATIVE_RUN" ;;
  *) exit 3 ;;
esac
`);
  chmodSync(gh, 0o755);
  const log = join(root, 'watched');
  for (const [portable, native, failed, queryFailure, watched, status] of [
    ['portable', 'native', '', false, 'portable\nnative\n', 0],
    ['portable', 'native', 'portable', false, 'portable\n', 1],
    ['portable', 'native', 'native', false, 'portable\nnative\n', 1],
    ['', 'native', '', false, '', 1], ['portable', '', '', false, 'portable\n', 1],
    ['portable', 'native', '', true, '', 1],
  ]) {
    writeFileSync(log, '');
    const result = spawnSync('bash', ['-euo', 'pipefail', '-c', script], {
      encoding: 'utf8',
      env: { ...process.env, PATH: `${root}:${process.env.PATH}`,
        AGENT_RELEASE_SHA: 'revision', GITHUB_REPOSITORY: 'fixture/repository', BEX_TEST_WATCH_LOG: log,
        BEX_TEST_PORTABLE_RUN: portable, BEX_TEST_NATIVE_RUN: native,
        BEX_TEST_FAILED_RUN: failed, BEX_TEST_QUERY_FAILURE: queryFailure ? '1' : '0' },
    });
    assert.ifError(result.error);
    assert.equal(result.status, status, result.stderr);
    assert.equal(readFileSync(log, 'utf8'), watched);
  }
});

test('production planning selects the exact bundle when ASC returns a prefixed bundle first', async () => {
  const bundleId = 'dev.remoteagent.mobile.ios';
  const prefixed = { id: 'prefixed-app', attributes: { bundleId: `${bundleId}.dev` } };
  const production = { id: 'production-app', attributes: { bundleId } };
  for (const data of [[prefixed, production], [production, prefixed]]) {
    assert.equal(await appForBundleId(async () => ({ data }), bundleId), production);
  }
  await assert.rejects(appForBundleId(async () => ({ data: [prefixed] }), bundleId), /missing/);
});

test('build numbers advance past source, uploaded Apple builds and published releases', () => {
  assert.equal(nextBuildNumber(['76', '76'], ['75', '77'], ['android-preview-1000', 'prd-81']), '82');
  assert.equal(nextBuildNumber(['76'], [], []), '77');
  assert.equal(nextBuildNumber(['76'], [], ['nightly-144', 'prd-143']), '145');
  assert.throws(() => nextBuildNumber(['75', '76'], [], []));
  assert.throws(() => nextBuildNumber([], [], []));
  assert.throws(() => nextBuildNumber(['76'], ['77.1'], []));
  assert.throws(() => nextBuildNumber(['9999'], [], []));
});

test('Nightly uses an internally distributed Preview app and stable uses the production store app', () => {
  assert.deepEqual(iosDistribution('nightly'), { bundleId: 'dev.remoteagent.mobile.ios.preview', profileType: 'IOS_APP_ADHOC', method: 'release-testing', product: 'BEX Preview' });
  assert.deepEqual(iosDistribution('stable'), { bundleId: 'dev.remoteagent.mobile.ios', profileType: 'IOS_APP_STORE', method: 'app-store-connect', product: 'Bex' });
  assert.throws(() => iosDistribution('development'), /channel/);
});

test('processing succeeds only when all three Apple delivery checks are valid', () => {
  const fields = 'BUILD-STATUS: VALID\nIMPORT-STATUS: VALID\nIS-ON-APP-STORE-CONNECT: true\n';
  // altool's actual text report frames each field with an '=' prefix.
  for (const valid of [fields, `=======================\n${fields.replace(/^/gm, '=    ')}\n=======================\n`]) {
    parseBuildStatus(valid);
    for (const invalid of [valid.replace('BUILD-STATUS: VALID', 'BUILD-STATUS: PROCESSING'), valid.replace('IMPORT-STATUS: VALID', 'IMPORT-STATUS: INVALID'), valid.replace('true', 'false')]) {
      assert.throws(() => parseBuildStatus(invalid));
    }
    for (const key of ['BUILD-STATUS', 'IMPORT-STATUS', 'IS-ON-APP-STORE-CONNECT']) {
      assert.throws(() => parseBuildStatus(valid.split('\n').filter(line => !line.includes(key)).join('\n')));
    }
  }
  assert.throws(() => parseBuildStatus('Upload succeeded'));
});

test('ASC requests use a valid ES256 signature and never follow pagination to another origin', async () => {
  const { privateKey, publicKey } = generateKeyPairSync('ec', { namedCurve: 'prime256v1' });
  let calls = 0;
  const request = ascClient({ keyId: 'fixture', issuer: 'fixture-issuer', privateKey: privateKey.export({ type: 'pkcs8', format: 'pem' }) }, async (url, options) => {
    calls++;
    assert.equal(new URL(url).origin, 'https://api.appstoreconnect.apple.com');
    const token = options.headers.Authorization.slice(7);
    const [header, body, signature] = token.split('.');
    assert.equal(JSON.parse(Buffer.from(header, 'base64url')).kid, 'fixture');
    assert.equal(JSON.parse(Buffer.from(body, 'base64url')).aud, 'appstoreconnect-v1');
    assert.ok(verify('sha256', Buffer.from(`${header}.${body}`), { key: publicKey, dsaEncoding: 'ieee-p1363' }, Buffer.from(signature, 'base64url')));
    return { ok: true, status: 200, json: async () => ({ data: [], links: { next: 'https://example.test/v1/builds' } }) };
  });
  await assert.rejects(listAll(request, '/v1/builds'), /origin/);
  assert.equal(calls, 1);
  await assert.rejects(request('https://example.test/v1/apps'), /path/);
  assert.equal(calls, 1);
});

test('TestFlight assigns the existing group and submits only builds needing beta review', async () => {
  for (const state of ['READY_FOR_BETA_SUBMISSION', 'IN_BETA_TESTING']) {
    const mutations = [];
    const request = async (path, options = {}) => {
      if (options.method) { mutations.push({ path, data: options.data }); return null; }
      if (path.startsWith('/v1/betaGroups?')) return { data: [{ id: 'external', attributes: { name: 'BEX External', isInternalGroup: false } }] };
      if (path.endsWith('/betaBuildLocalizations')) return { data: [] };
      return { data: { id: 'beta-detail', attributes: { externalBuildState: state, autoNotifyEnabled: false } } };
    };
    await distributeBuild(request, 'app', 'build', 'BEX External', 'Release changes', 'ja');
    assert.ok(mutations.some(call => call.path === '/v1/betaGroups/external/relationships/builds' && call.data.data[0].id === 'build'));
    assert.ok(mutations.some(call => call.path === '/v1/buildBetaDetails/beta-detail' && call.data.data.attributes.autoNotifyEnabled));
    assert.equal(mutations.some(call => call.path === '/v1/betaAppReviewSubmissions'), state === 'READY_FOR_BETA_SUBMISSION');
  }
});

test('missing groups and rejected builds cannot be reported as distributed', async () => {
  await assert.rejects(distributeBuild(async () => ({ data: [] }), 'app', 'build', 'missing', 'changes', 'ja'), /missing/);
  const request = async (path, options = {}) => {
    if (options.method === 'POST') return null;
    if (path.startsWith('/v1/betaGroups?')) return { data: [{ id: 'group', attributes: { name: 'external', isInternalGroup: false } }] };
    return { data: { id: 'detail', attributes: { externalBuildState: 'BETA_REJECTED' } } };
  };
  await assert.rejects(distributeBuild(request, 'app', 'build', 'external', 'changes', 'ja'), /blocked/);
});

test('internal groups with automatic access do not need manual build assignment', async () => {
  for (const automatic of [false, true]) {
    const mutations = [];
    const request = async (path, options = {}) => {
      if (options.method) { mutations.push(path); return null; }
      if (path.startsWith('/v1/betaGroups?')) return { data: [{ id: 'internal', attributes: {
        name: 'BEX Internal', isInternalGroup: true, hasAccessToAllBuilds: automatic,
      } }] };
      if (path.endsWith('/betaBuildLocalizations')) return { data: [] };
      return { data: { attributes: { internalBuildState: 'IN_BETA_TESTING' } } };
    };
    assert.equal(await distributeBuild(request, 'app', 'build', 'BEX Internal', 'Changes', 'ja'), 'IN_BETA_TESTING');
    assert.ok(mutations.includes('/v1/betaBuildLocalizations'));
    assert.equal(mutations.includes('/v1/betaGroups/internal/relationships/builds'), !automatic);
    assert.ok(!mutations.includes('/v1/betaAppReviewSubmissions'));
  }
});


test('Xcode receipts must identify one unambiguous delivery', () => {
  const a = '12345678-1234-1234-1234-123456789abc';
  const b = '12345678-1234-1234-1234-123456789abd';
  assert.equal(deliveryId([JSON.stringify({ deliveryUUID: a }), `Delivery ID: ${a}`]), a);
  assert.throws(() => deliveryId(['Upload succeeded']));
  assert.throws(() => deliveryId([`delivery-id: ${a}`, `deliveryUUID = "${b}"`]));
});

test('receipt recording reads recent Xcode bundles and preserves the plan on ambiguity', () => {
  const root = mkdtempSync(join(tmpdir(), 'agent-receipt-'));
  try {
    const native = join(root, 'native temporary directory');
    const output = join(root, 'target/apple-release');
    mkdirSync(native);
    mkdirSync(output, { recursive: true });
    const marker = join(output, 'upload-start');
    writeFileSync(marker, '');
    const cutoff = new Date('2026-01-02T00:00:00Z');
    utimesSync(marker, cutoff, cutoff);
    const a = '12345678-1234-1234-1234-123456789abc';
    const b = '12345678-1234-1234-1234-123456789abd';
    const log = (name, id, date) => {
      const folder = join(native, name);
      mkdirSync(folder, { recursive: true });
      const path = join(folder, 'ContentDelivery.log');
      writeFileSync(path, JSON.stringify({ deliveryUUID: id }));
      utimesSync(path, date, date);
    };
    const recent = new Date('2026-01-03T00:00:00Z');
    log('Bex-current.xcdistributionlogs', a, recent);
    log('Bex-old.xcdistributionlogs', b, new Date('2026-01-01T00:00:00Z'));
    log('Bex-at-cutoff.xcdistributionlogs', b, cutoff);
    log('unrelated', b, recent);
    log('unrelated/TemporaryItems', b, recent);
    mkdirSync(join(native, 'Bex-export.xcdistributionlogs'));
    const planPath = join(output, 'plan.json');
    const plan = { appId: 'fixture-app', version: '78', sha: 'fixture-source' };
    writeFileSync(planPath, JSON.stringify(plan));
    const run = () => spawnSync(process.execPath, [fileURLToPath(new URL('./apple-release.ts', import.meta.url)),
      'record-delivery', native, marker], { cwd: root, encoding: 'utf8' });
    const result = run();
    assert.equal(result.status, 0, result.stderr);
    assert.deepEqual(JSON.parse(readFileSync(planPath)), { ...plan, deliveryId: a });
    log('Bex-other.xcdistributionlogs', b, recent);
    assert.notEqual(run().status, 0);
    assert.deepEqual(JSON.parse(readFileSync(planPath)), { ...plan, deliveryId: a });
    rmSync(join(native, 'Bex-other.xcdistributionlogs'), { recursive: true });
    assert.equal(run().status, 0);
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test('future candidates advance past approved versions without replacing their builds', () => {
  assert.equal(nextMarketingVersion('1.0', []), '1.0.0');
  assert.equal(nextMarketingVersion('1.0', ['1.0']), '1.0.1');
  assert.equal(nextMarketingVersion('1.0', ['1.0']), '1.0.1');
  assert.equal(nextMarketingVersion('2.0', ['1.9.9']), '2.0.0');
  assert.throws(() => nextMarketingVersion('invalid', []));
});

test('planning separates Preview and stable identities and preserves an approved initial App Store version', t => {
  const root = mkdtempSync(join(tmpdir(), 'release-plan-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  assert.equal(spawnSync('git', ['init', '-q', root]).status, 0);
  assert.equal(spawnSync('git', ['-C', root, '-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.invalid',
    '-c', 'commit.gpgsign=false', 'commit', '--allow-empty', '-qm', 'Fixture release']).status, 0);
  const files = {
    'apps/mobile/iosApp/Bex.xcodeproj/project.pbxproj': 'CURRENT_PROJECT_VERSION = 76; MARKETING_VERSION = 1.0;',
    'apps/desktop/macos/Info.plist': '<key>CFBundleVersion</key><string>76</string>',
    'target/apple-release/releases.json': JSON.stringify([{ tagName: 'nightly-81' }]),
  };
  for (const [name, contents] of Object.entries(files)) {
    mkdirSync(join(root, name, '..'), { recursive: true });
    writeFileSync(join(root, name), contents);
  }
  const stub = join(root, 'apple.mjs');
  writeFileSync(stub, `globalThis.fetch = async url => {
    const path = new URL(url).pathname;
    const data = path === '/v1/apps' ? [{ id: '6816015343', attributes: { bundleId: 'dev.remoteagent.mobile.ios', primaryLocale: 'ja' } }]
      : path === '/v1/builds' ? [{ attributes: { version: '77' } }]
      : [{ attributes: { platform: 'IOS', versionString: '1.0', appStoreState: process.env.APPLE_PLAN_STATE } }];
    return { ok: true, status: 200, json: async () => ({ data }) };
  };`);
  const { privateKey } = generateKeyPairSync('ec', { namedCurve: 'prime256v1' });
  const key = join(root, 'fixture.p8');
  writeFileSync(key, privateKey.export({ type: 'pkcs8', format: 'pem' }), { mode: 0o600 });
  const script = fileURLToPath(new URL('./apple-release.ts', import.meta.url));
  const planPath = join(root, 'target/apple-release/plan.json');
  for (const [channel, state, status] of [['nightly', 'PENDING_DEVELOPER_RELEASE', 0], ['stable', 'PENDING_DEVELOPER_RELEASE', 1], ['stable', 'READY_FOR_DISTRIBUTION', 0]]) {
    const before = channel === 'stable' ? readFileSync(planPath, 'utf8') : null;
    const result = spawnSync(process.execPath, ['--import', stub, script, 'plan'], { cwd: root, encoding: 'utf8', env: {
      ...process.env, AGENT_RELEASE_CHANNEL: channel, APPLE_PLAN_STATE: state, AGENT_SOURCE_TAG: 'nightly-81',
      AGENT_RELEASE_SHA: 'a'.repeat(40), AGENT_ASC_KEY_ID: 'fixture', AGENT_ASC_ISSUER_ID: 'fixture', AGENT_ASC_PRIVATE_KEY_FILE: key,
      GITHUB_ENV: join(root, 'environment'),
    } });
    assert.equal(result.status, status, result.stderr);
    if (status) {
      assert.match(result.stderr, /already approved/);
      assert.equal(readFileSync(planPath, 'utf8'), before);
      continue;
    }
    const plan = JSON.parse(readFileSync(planPath));
    assert.equal(plan.tag, channel === 'nightly' ? 'nightly-82' : 'prd-82');
    assert.equal(plan.versionName, '1.0.1');
    assert.equal(plan.bundleId, channel === 'nightly' ? 'dev.remoteagent.mobile.ios.preview' : 'dev.remoteagent.mobile.ios');
    assert.equal(plan.sourceTag, channel === 'nightly' ? undefined : 'nightly-81');
    assert.equal(plan.appId, channel === 'nightly' ? undefined : '6816015343');
  }
});

test('Apple beta-review capacity waits remain retryable and never claim distribution', async () => {
  for (const status of [409, 429]) {
    let attempts = 0;
    const request = async (path, options = {}) => {
      if (path === '/v1/betaAppReviewSubmissions') { attempts++; if (attempts === 1) throw Object.assign(new Error('Capacity'), { status }); return {}; }
      if (options.method) return {};
      if (path.startsWith('/v1/betaGroups?')) return { data: [{ id: 'group', attributes: { name: 'external', isInternalGroup: false, hasAccessToAllBuilds: true } }] };
      if (path.endsWith('/betaBuildLocalizations')) return { data: [] };
      return { data: { id: 'detail', attributes: { externalBuildState: 'READY_FOR_BETA_SUBMISSION', autoNotifyEnabled: true } } };
    };
    assert.equal(await distributeBuild(request, 'app', 'build', 'external', 'Changes', 'ja'), 'Waiting for TestFlight review capacity');
    assert.equal(await distributeBuild(request, 'app', 'build', 'external', 'Changes', 'ja'), 'Submitted for TestFlight beta review');
  }
});

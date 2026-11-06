import test from 'node:test';
import assert from 'node:assert/strict';
import { createHash, generateKeyPairSync } from 'node:crypto';
import { chmodSync, mkdirSync, mkdtempSync, readFileSync, writeFileSync, rmSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { latestNightly, packages, releaseNumber, validateRelease, verifyPackages } from './release-candidate.ts';

test('production tags identify one numbered candidate without accepting branches or shell syntax', () => {
  assert.equal(releaseNumber('prd-145', 'prd'), '145');
  for (const value of ['nightly-145', 'prd-0', 'prd-01', 'prd-145/other', 'prd-$(echo)', 'prd-999999999999999999']) assert.throws(() => releaseNumber(value, 'prd'));
});

test('stable selects the latest Nightly source, seals rebuilt packages and finishes approval without downgrading stable', t => {
  const root = mkdtempSync(join(tmpdir(), 'release-promotion-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const bin = join(root, 'bin');
  const source = join(root, 'nightly-145');
  mkdirSync(bin);
  mkdirSync(source);
  mkdirSync(join(root, 'prd-146'));
  const assets = {};
  for (const name of packages('nightly')) {
    const bytes = Buffer.from(`immutable ${name}`);
    const hash = createHash('sha256').update(bytes).digest('hex');
    for (const [file, content] of [[name, bytes], [`${name}.sha256`, Buffer.from(`${hash}  ${name}\n`)]]) {
      writeFileSync(join(source, file), content);
      assets[file] = { size: content.length, sha256: createHash('sha256').update(content).digest('hex') };
    }
  }
  const manifest = { channel: 'nightly', bundleId: 'dev.remoteagent.mobile.ios.preview', tag: 'nightly-145', version: '145', versionName: '1.0.1', sha: 'a'.repeat(40), notes: 'Changes', locale: 'ja', assets };
  writeFileSync(join(source, 'release.json'), JSON.stringify(manifest));
  const appleBuild = { ...manifest };
  delete appleBuild.assets;
  writeFileSync(join(source, 'apple-build.json'), JSON.stringify(appleBuild));
  const rebuilt = { ...appleBuild, channel: 'stable', bundleId: 'dev.remoteagent.mobile.ios', tag: 'prd-146', version: '146', sourceTag: 'nightly-145', appId: '6816015343', buildId: 'candidate' };
  for (const name of packages('stable')) {
    const bytes = Buffer.from(`rebuilt ${name}`);
    writeFileSync(join(root, 'prd-146', name), bytes);
    writeFileSync(join(root, 'prd-146', `${name}.sha256`), createHash('sha256').update(bytes).digest('hex') + `  ${name}\n`);
  }
  writeFileSync(join(root, 'prd-146', 'apple-build.json'), JSON.stringify(rebuilt));
  const state = join(root, 'state.json');
  writeFileSync(state, JSON.stringify({ apple: 'WAITING_FOR_REVIEW', releases: [{ tagName: 'nightly-145', isDraft: true, isPrerelease: true }, { tagName: 'prd-146', isDraft: true, isPrerelease: false }] }));
  writeFileSync(join(bin, 'gh'), `#!${process.execPath}
const fs = require('node:fs'), path = require('node:path');
const root = process.env.RELEASE_FIXTURE, args = process.argv.slice(2);
const statePath = path.join(root, 'state.json'), state = JSON.parse(fs.readFileSync(statePath));
fs.appendFileSync(path.join(root, 'commands'), JSON.stringify(args) + '\\n');
if (args[1] === 'view') {
 const release = state.releases.find(item => item.tagName === args[2]);
 console.log(JSON.stringify({ isDraft: release.isDraft }));
}
else if (args[1] === 'list') console.log(JSON.stringify(state.releases));
else if (args[1] === 'download') {
 const files = args.includes('--pattern') ? [args[args.indexOf('--pattern') + 1]] : fs.readdirSync(path.join(root, args[2]));
 for (const file of files) fs.copyFileSync(path.join(root, args[2], file), path.join(args[args.indexOf('--dir') + 1], file));
} else if (args[1] === 'upload') {
 fs.copyFileSync(args[3], path.join(root, args[2], path.basename(args[3])));
} else if (args[1] === 'edit') state.releases.find(item => item.tagName === args[2]).isDraft = false;
else throw new Error('Unexpected gh command');
fs.writeFileSync(statePath, JSON.stringify(state));
`);
  writeFileSync(join(bin, 'git'), `#!${process.execPath}
const fs = require('node:fs');
const state = JSON.parse(fs.readFileSync(process.env.RELEASE_FIXTURE + '/state.json'));
console.log(process.argv.at(-1).includes('prd-146') ? (state.stableSha || 'a'.repeat(40)) : 'a'.repeat(40));
`);
  for (const file of ['gh', 'git']) chmodSync(join(bin, file), 0o755);
  const stub = join(root, 'fetch.mjs');
  writeFileSync(stub, `
import { readFileSync } from 'node:fs';
globalThis.fetch = async (url, options) => {
 if (options.method !== 'GET') throw new Error('An ongoing or approved review must not be mutated');
 const state = JSON.parse(readFileSync(process.env.RELEASE_FIXTURE + '/state.json'));
 const pathname = new URL(url).pathname;
 let data;
 if (pathname.endsWith('/appStoreVersions')) data = [{ id: 'version', attributes: { versionString: '1.0.1', appStoreState: state.apple } }];
 else if (pathname.endsWith('/build')) data = { id: 'candidate' };
 else throw new Error('Unexpected Apple GET ' + pathname);
 return { ok: true, status: 200, json: async () => ({ data }) };
};
`);
  const { privateKey } = generateKeyPairSync('ec', { namedCurve: 'prime256v1' });
  const key = join(root, 'fixture.p8');
  writeFileSync(key, privateKey.export({ type: 'pkcs8', format: 'pem' }), { mode: 0o600 });
  const run = (command, tag, expectedStatus = 0) => {
    const args = [command, tag].filter(value => value !== undefined);
    const result = spawnSync(process.execPath, ['--import', stub, fileURLToPath(new URL('./release-candidate.ts', import.meta.url)), ...args], {
      encoding: 'utf8', env: { ...process.env, PATH: `${bin}:${process.env.PATH}`, RELEASE_FIXTURE: root,
        GITHUB_OUTPUT: join(root, 'outputs'), GITHUB_REPOSITORY: 'fixture/repo', AGENT_ASC_KEY_ID: 'fixture', AGENT_ASC_ISSUER_ID: 'fixture', AGENT_ASC_PRIVATE_KEY_FILE: key },
    });
    assert.equal(result.status, expectedStatus, result.stderr);
  };
  run('seal', 'nightly-145');
  run('seal', 'nightly-145');
  run('resolve');
  assert.match(readFileSync(join(root, 'outputs'), 'utf8'), /source-tag=nightly-145/);
  const incorrect = { ...rebuilt, sha: 'b'.repeat(40) };
  writeFileSync(join(root, 'prd-146/apple-build.json'), JSON.stringify(incorrect));
  writeFileSync(state, JSON.stringify({ ...JSON.parse(readFileSync(state)), stableSha: incorrect.sha }));
  run('seal', 'prd-146', 1);
  assert.ok(!readFileSync(join(root, 'commands'), 'utf8').includes('["release","upload","prd-146"'));
  writeFileSync(join(root, 'prd-146/apple-build.json'), JSON.stringify(rebuilt));
  writeFileSync(state, JSON.stringify({ ...JSON.parse(readFileSync(state)), stableSha: rebuilt.sha }));
  run('seal', 'prd-146');
  run('seal', 'prd-146');
  let current = JSON.parse(readFileSync(state));
  assert.equal(current.releases.find(item => item.tagName === 'prd-146').isDraft, true);
  const stable = JSON.parse(readFileSync(join(root, 'prd-146/release.json')));
  assert.equal(stable.sourceTag, 'nightly-145');
  assert.equal(stable.sha, manifest.sha);
  assert.notDeepEqual(readFileSync(join(root, 'prd-146', 'bex-macOS-arm64.zip')), readFileSync(join(source, 'bex-macOS-arm64.zip')));
  current.apple = 'READY_FOR_DISTRIBUTION';
  current.releases.push({ tagName: 'prd-147', isDraft: false, isPrerelease: false });
  writeFileSync(state, JSON.stringify(current));
  run('sync');
  current = JSON.parse(readFileSync(state));
  assert.equal(current.releases.find(item => item.tagName === 'prd-146').isDraft, false);
  const commands = readFileSync(join(root, 'commands'), 'utf8').trim().split('\n').map(JSON.parse);
  assert.equal(commands.filter(args => args[1] === 'create').length, 0);
  assert.ok(commands.find(args => args[1] === 'edit' && args[2] === 'prd-146').includes('--latest=false'));

});

test('release verification binds every package to its source, build and iPhone variant', t => {
  const directory = mkdtempSync(join(tmpdir(), 'candidate-verification-'));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  const assets = {};
  for (const name of packages('nightly')) {
    const bytes = Buffer.from(`signed candidate ${name}`);
    const digest = createHash('sha256').update(bytes).digest('hex');
    const sidecar = Buffer.from(`${digest}  ${name}\n`);
    for (const [file, content] of [[name, bytes], [`${name}.sha256`, sidecar]]) {
      writeFileSync(join(directory, file), content);
      assets[file] = { size: content.length, sha256: createHash('sha256').update(content).digest('hex') };
    }
  }
  const manifest = { channel: 'nightly', bundleId: 'dev.remoteagent.mobile.ios.preview', tag: 'nightly-145', version: '145', versionName: '1.0.1', sha: 'a'.repeat(40), assets };
  validateRelease(manifest, 'nightly-145', 'a'.repeat(40));
  verifyPackages(directory, manifest.assets);
  for (const patch of [{ sha: 'b'.repeat(40) }, { version: '146' }, { appId: '6820657007' }, { assets: {} }]) {
    assert.throws(() => validateRelease({ ...manifest, ...patch }, 'nightly-145', 'a'.repeat(40)));
  }
  writeFileSync(join(directory, packages('nightly')[0]), 'different signed application');
  assert.throws(() => verifyPackages(directory, manifest.assets), /checksum mismatch/);
});


test('latest Nightly ignores drafts, stable releases and malformed tags', () => {
  const item = (tagName, isDraft = false, isPrerelease = true) => ({ tagName, isDraft, isPrerelease });
  assert.equal(latestNightly([item('nightly-9'), item('nightly-10'), item('nightly-11', true), item('prd-100', false, false), item('nightly-01')]), 'nightly-10');
  assert.throws(() => latestNightly([item('nightly-11', true), item('prd-100', false, false)]), /Publish a verified Nightly/);
});

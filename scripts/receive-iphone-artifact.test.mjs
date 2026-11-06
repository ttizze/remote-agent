import test from 'node:test';
import assert from 'node:assert/strict';
import { chmodSync, existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { basename, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawn } from 'node:child_process';
import { setTimeout as delay } from 'node:timers/promises';
import { once } from 'node:events';

function fixture(t, mode, kind = 'iphone-test-build') {
  const root = mkdtempSync(join(tmpdir(), 'iphone-artifact-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const bin = join(root, 'bin');
  mkdirSync(bin);
  const log = join(root, 'commands');
  const output = join(root, kind === 'iphone-test-build' ? 'ios-e2e.tar.gz' : 'ios-runner.tar.gz');
  const gh = join(bin, 'gh');
  writeFileSync(gh, `#!/usr/bin/env node
const fs = require('node:fs');
const path = require('node:path');
const args = process.argv.slice(2);
const mode = process.env.ARTIFACT_TEST_MODE;
fs.appendFileSync(process.env.ARTIFACT_TEST_LOG, JSON.stringify(args) + '\\n');
if (args[0] === 'api' && mode === 'unauthorized') {
  console.error('HTTP 403: Resource not accessible by integration');
  process.exit(17);
}
if (args[0] === 'api' && ['transient', 'network-outage'].includes(mode)) {
  const calls = fs.readFileSync(process.env.ARTIFACT_TEST_LOG, 'utf8').trim().split('\\n').map(JSON.parse);
  if (mode === 'network-outage' || calls.filter(call => call[1] === args[1]).length === 1) {
    console.error('net/http: TLS handshake timeout');
    process.exit(17);
  }
}
if (args[0] === 'api') {
  if (args[1].includes('/artifacts?')) {
    const reused = ['rerun-consumer', 'rerun-copied-producer'].includes(mode);
    let artifacts = [{name: process.env.ARTIFACT_TEST_KIND + (reused ? '-1' : '-2'), expired: false,
      created_at: reused ? '2026-01-01T01:00:00Z' : '2026-01-02T01:00:00Z'}];
    if (['missing', 'waiting', 'failed'].includes(mode)) artifacts = [];
    if (mode === 'stale') {
      artifacts[0].name = process.env.ARTIFACT_TEST_KIND + '-1';
      artifacts[0].created_at = '2026-01-01T01:00:00Z';
    }
    if (mode === 'future') artifacts[0].name = process.env.ARTIFACT_TEST_KIND + '-3';
    if (mode === 'expired') artifacts[0].expired = true;
    if (mode === 'duplicate') artifacts.push(artifacts[0]);
    const pages = [{artifacts}];
    if (mode === 'paginated') pages.unshift({artifacts: [{name: 'unrelated', expired: false, created_at: '2026-01-02T01:00:00Z'}]});
    console.log(JSON.stringify(pages));
  } else {
    const running = ['ready', 'waiting', 'cancel-download'].includes(mode);
    const reused = ['rerun-consumer', 'rerun-copied-producer'].includes(mode);
    console.log(JSON.stringify({jobs: [{name: 'iPhone build', run_attempt: mode === 'rerun-consumer' ? 1 : 2,
      started_at: ['invalid-start', 'queued'].includes(mode) ? undefined : reused ? '2026-01-01T00:00:00Z' : '2026-01-02T00:00:00Z',
      status: mode === 'queued' ? 'queued' : running ? 'in_progress' : 'completed', conclusion: mode === 'failed' ? 'failure' : 'success'}]}));
  }
} else {
  const dir = args[args.indexOf('--dir') + 1];
  fs.mkdirSync(dir, {recursive: true});
  fs.writeFileSync(path.join(dir, mode === 'wrong-file' ? 'unexpected' : process.env.ARTIFACT_TEST_FILE), 'current build');
  if (mode === 'download-error') {
    console.error('net/http: TLS handshake timeout');
    process.exit(18);
  }
  if (mode === 'cancel-download') setInterval(() => {}, 1000);
}
`);
  chmodSync(gh, 0o755);
  function start(overrides = {}, timeout = '10') {
    const child = spawn(process.execPath, [fileURLToPath(new URL('./receive-iphone-artifact.ts', import.meta.url)), kind, output, timeout], {
      env: {
        ...process.env, PATH: `${bin}:${process.env.PATH}`, GITHUB_REPOSITORY: 'fixture/repo',
        GITHUB_RUN_ID: '123', GITHUB_RUN_ATTEMPT: '2',
        ARTIFACT_TEST_MODE: mode, ARTIFACT_TEST_LOG: log, ARTIFACT_TEST_KIND: kind,
        ARTIFACT_TEST_FILE: basename(output), ...overrides,
      },
    });
    let stdout = '', stderr = '';
    child.stdout.on('data', chunk => { stdout += chunk; });
    child.stderr.on('data', chunk => { stderr += chunk; });
    const completed = once(child, 'close').then(([status, signal]) => ({status, signal, stdout, stderr}));
    t.after(() => { if (child.exitCode === null && child.signalCode === null) child.kill('SIGKILL'); });
    return {child, completed};
  }
  return {
    root, log, output, start,
    commands: () => existsSync(log) ? readFileSync(log, 'utf8').trim().split('\n').map(JSON.parse) : [],
  };
}

test('runner and app artifacts can arrive before the producer finishes, from this exact run attempt', async t => {
  for (const kind of ['iphone-test-runner', 'iphone-test-build']) {
    await t.test(kind, async t => {
      const f = fixture(t, 'ready', kind);
      const result = await f.start().completed;
      assert.equal(result.status, 0, result.stderr);
      assert.equal(readFileSync(f.output, 'utf8'), 'current build');
      const commands = f.commands();
      assert.deepEqual(commands.find(args => args[1].includes('/artifacts?')),
        ['api', 'repos/fixture/repo/actions/runs/123/artifacts?per_page=100', '--paginate', '--slurp']);
      assert.ok(commands.some(args => args[1] === 'repos/fixture/repo/actions/runs/123/jobs?filter=latest&per_page=100'));
      assert.deepEqual(commands.find(args => args[0] === 'run').slice(0, 8),
        ['run', 'download', '123', '--repo', 'fixture/repo', '--name', `${kind}-2`, '--dir']);
      assert.ok(!readdirSync(f.root).some(name => name.startsWith('.iphone-artifact-')));
    });
  }
});

test('consumer reruns keep the actual producer even when GitHub copies it into the new attempt', async t => {
  for (const mode of ['rerun-consumer', 'rerun-copied-producer']) {
    await t.test(mode, async t => {
      const f = fixture(t, mode);
      const result = await f.start().completed;
      assert.equal(result.status, 0, result.stderr);
      assert.equal(readFileSync(f.output, 'utf8'), 'current build');
      assert.equal(f.commands().find(args => args[0] === 'run')[6], 'iphone-test-build-1');
    });
  }
});

test('artifact lookup reads all pages and ignores unrelated artifacts', async t => {
  const f = fixture(t, 'paginated');
  const result = await f.start().completed;
  assert.equal(result.status, 0, result.stderr);
  assert.equal(readFileSync(f.output, 'utf8'), 'current build');
});

test('transient jobs and artifact metadata connection failures recover within one reception', async t => {
  const f = fixture(t, 'transient');
  const result = await f.start({}, '20').completed;
  assert.equal(result.status, 0, result.stderr);
  assert.equal(readFileSync(f.output, 'utf8'), 'current build');
  assert.equal(f.commands().filter(args => args[0] === 'api').length, 4);
  assert.equal(f.commands().filter(args => args[0] === 'run').length, 1);
});

test('metadata authentication failures are reported without retrying', async t => {
  const f = fixture(t, 'unauthorized');
  const result = await f.start().completed;
  assert.equal(result.status, 1, result.stderr);
  assert.match(result.stderr, /HTTP 403/);
  assert.equal(f.commands().length, 1);
  assert.ok(!existsSync(f.output));
});

test('failed producers and missing, expired, duplicate, old or future artifacts never start a download', async t => {
  for (const mode of ['failed', 'missing', 'expired', 'duplicate', 'stale', 'future']) {
    await t.test(mode, async t => {
      const f = fixture(t, mode);
      const result = await f.start().completed;
      assert.equal(result.status, 1, result.stderr);
      assert.ok(!existsSync(f.output));
      assert.ok(!f.commands().some(args => args[0] === 'run'));
    });
  }
});

test('failed or malformed downloads retain the previous output and remove partial files', async t => {
  for (const mode of ['download-error', 'wrong-file']) {
    await t.test(mode, async t => {
      const f = fixture(t, mode);
      writeFileSync(f.output, 'previous build');
      const result = await f.start().completed;
      assert.equal(result.status, 1, result.stderr);
      assert.equal(readFileSync(f.output, 'utf8'), 'previous build');
      assert.equal(f.commands().filter(args => args[0] === 'run').length, 1);
      assert.ok(!readdirSync(f.root).some(name => name.startsWith('.iphone-artifact-')));
    });
  }
});

test('waiting has a deadline and does not publish an output', async t => {
  const f = fixture(t, 'waiting');
  const result = await f.start({}, '1').completed;
  assert.equal(result.status, 1, result.stderr);
  assert.match(result.stderr, /Timed out waiting/);
  assert.ok(!existsSync(f.output));
});

test('queued producers wait for an execution, while a started producer must identify its start time', async t => {
  for (const mode of ['queued', 'invalid-start']) {
    await t.test(mode, async t => {
      const f = fixture(t, mode);
      const result = await f.start({}, '1').completed;
      assert.equal(result.status, 1, result.stderr);
      assert.match(result.stderr, mode === 'queued' ? /Timed out waiting/ : /did not identify its start time/);
      assert.ok(f.commands().every(args => args[1].includes('/jobs?')));
      assert.ok(!existsSync(f.output));
    });
  }
});

test('a network outage cannot extend the reception deadline', async t => {
  const f = fixture(t, 'network-outage');
  const result = await f.start({}, '1').completed;
  assert.equal(result.status, 1, result.stderr);
  assert.match(result.stderr, /Timed out waiting/);
  assert.equal(f.commands().length, 1);
  assert.ok(!existsSync(f.output));
});

test('cancelling a download stops the child and removes partial files', async t => {
  const f = fixture(t, 'cancel-download');
  const {child, completed} = f.start();
  const deadline = Date.now() + 5000;
  while (!f.commands().some(args => args[0] === 'run')) {
    assert.ok(Date.now() < deadline, 'download did not start');
    await delay(10);
  }
  child.kill('SIGTERM');
  const result = await completed;
  assert.equal(result.status, 1, result.stderr);
  assert.match(result.stderr, /interrupted/);
  assert.ok(!existsSync(f.output));
  assert.ok(!readdirSync(f.root).some(name => name.startsWith('.iphone-artifact-')));
});

test('invalid run identities are rejected before accessing GitHub', async t => {
  for (const overrides of [{GITHUB_RUN_ATTEMPT: '0'}, {GITHUB_RUN_ID: 'bad'}, {GITHUB_REPOSITORY: '../other'}]) {
    await t.test(JSON.stringify(overrides), async t => {
      const f = fixture(t, 'ready');
      const result = await f.start(overrides).completed;
      assert.equal(result.status, 1);
      assert.deepEqual(f.commands(), []);
    });
  }
});

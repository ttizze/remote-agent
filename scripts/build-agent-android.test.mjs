import test from 'node:test';
import assert from 'node:assert/strict';
import { chmodSync, copyFileSync, existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from 'node:fs';
import { availableParallelism, tmpdir } from 'node:os';
import { join } from 'node:path';
import { spawn } from 'node:child_process';
import { setTimeout as delay } from 'node:timers/promises';

async function until(check) {
  const deadline = Date.now() + 10000;
  while (!check()) {
    assert.ok(Date.now() < deadline, 'Android build did not reach the expected stage');
    await delay(10);
  }
}

function fixture(t, jobs, mode = 'parallel') {
  const root = mkdtempSync(join(tmpdir(), 'android-build-'));
  const bin = join(root, 'bin');
  mkdirSync(bin);
  mkdirSync(join(root, 'scripts'));
  copyFileSync('scripts/build-agent-android.ts', join(root, 'scripts/build-agent-android.ts'));
  mkdirSync(join(root, 'native output'));
  writeFileSync(join(root, 'native output/retired.so'), 'previous build');
  writeFileSync(join(root, 'events'), '');
  const cargo = join(bin, 'cargo');
  writeFileSync(cargo, `#!${process.execPath}
const fs = require('node:fs');
const path = require('node:path');
const cp = require('node:child_process');
const root = process.env.ANDROID_BUILD_ROOT;
const mode = process.env.ANDROID_BUILD_MODE;
const args = process.argv.slice(2);
const log = entry => fs.appendFileSync(path.join(root, 'events'), JSON.stringify(entry) + '\\n');
const wait = async name => { while (!fs.existsSync(path.join(root, name))) await new Promise(resolve => setTimeout(resolve, 10)); };
(async () => {
  const abi = args[args.indexOf('--target') + 1];
  fs.writeFileSync(path.join(root, abi + '.pid'), String(process.pid));
  log({ stage: 'start', abi, jobs: Number(process.env.CARGO_BUILD_JOBS), args });
  if (mode === 'fail-' + abi) {
    await wait((abi === 'arm64-v8a' ? 'x86_64' : 'arm64-v8a') + '.descendant');
    process.exit(37);
  }
  if (mode !== 'sequential') {
    const descendant = cp.spawn(process.execPath, ['-e', 'setInterval(() => {}, 1000)'], { stdio: 'ignore' });
    fs.writeFileSync(path.join(root, abi + '.descendant'), String(descendant.pid));
    const exited = new Promise(resolve => descendant.once('close', resolve));
    for (const [signal, status] of [['SIGINT', 130], ['SIGTERM', 143]]) {
      process.on(signal, async () => { await exited; process.exit(status); });
    }
    await wait(abi + '.release');
    descendant.kill();
    await exited;
  }
  const output = path.join(args[args.indexOf('--output-dir') + 1], abi);
  fs.mkdirSync(output, { recursive: true });
  if (mode !== 'missing-' + abi) fs.writeFileSync(path.join(output, 'libagent_ffi.so'), abi);
  fs.writeFileSync(path.join(output, 'libunused.so'), 'unused dependency');
  log({ stage: 'exit', abi });
})().catch(error => { console.error(error); process.exit(70); });
`);
  chmodSync(cargo, 0o755);
  const bootstrap = [];
  if (mode === 'publish-failure') {
    const loader = join(root, 'publish-failure.mjs');
    writeFileSync(loader, `import fs from 'node:fs';
import { syncBuiltinESMExports } from 'node:module';
import { basename } from 'node:path';
const rename = fs.renameSync;
fs.renameSync = (source, destination) => {
  if (basename(source) === 'next') throw Object.assign(new Error('fixture publication failed'), { code: 'EIO' });
  return rename(source, destination);
};
syncBuiltinESMExports();
`);
    bootstrap.push('--import', loader);
  }
  const child = spawn(process.execPath, [...bootstrap, 'scripts/build-agent-android.ts', join(root, 'native output'), 'arm64-v8a', 'x86_64'], {
    cwd: root, detached: true,
    env: { ...process.env, PATH: `${bin}:${process.env.PATH}`, CARGO_BUILD_JOBS: jobs, ANDROID_BUILD_ROOT: root, ANDROID_BUILD_MODE: mode },
  });
  let stderr = '';
  child.stdout.resume();
  child.stderr.setEncoding('utf8').on('data', data => { stderr += data; });
  const done = new Promise((resolve, reject) => {
    child.once('error', reject);
    child.once('close', (code, signal) => resolve({ code, signal, stderr }));
  });
  t.after(async () => {
    if (child.exitCode === null && child.signalCode === null) child.kill('SIGKILL');
    for (const abi of ['arm64-v8a', 'x86_64']) {
      const parent = join(root, abi + '.pid');
      if (existsSync(parent)) {
        try { process.kill(-Number(readFileSync(parent)), 'SIGKILL'); } catch (error) { if (!['ESRCH', 'EPERM'].includes(error.code)) throw error; }
      }
      const pidFile = join(root, abi + '.descendant');
      if (existsSync(pidFile)) {
        try { process.kill(Number(readFileSync(pidFile)), 'SIGKILL'); } catch (error) { if (error.code !== 'ESRCH') throw error; }
      }
    }
    await done;
    rmSync(root, { recursive: true, force: true });
  });
  return {
    async finish() {
      let result;
      done.then(value => { result = value; });
      await until(() => result !== undefined);
      assert.deepEqual(readdirSync(root).filter(name => name.startsWith('.agent-android.')), [], 'temporary native libraries survived build completion');
      return result;
    },
    exists: name => existsSync(join(root, name)),
    events: () => readFileSync(join(root, 'events'), 'utf8').trim().split('\n').filter(Boolean).map(line => JSON.parse(line)),
    release: abi => writeFileSync(join(root, abi + '.release'), ''),
    output: abi => readFileSync(join(root, 'native output', abi, 'libagent_ffi.so'), 'utf8'),
    cancel: signal => child.kill(signal),
    assertDescendantExited(abi) {
      assert.throws(() => process.kill(Number(readFileSync(join(root, abi + '.descendant'))), 0), { code: 'ESRCH' });
    },
  };
}

test('ABI builds overlap within the CPU budget and publish both JNI libraries', { timeout: 20000 }, async t => {
  const build = fixture(t, '5');
  await until(() => ['arm64-v8a', 'x86_64'].every(abi => build.exists(abi + '.descendant')));
  const starts = build.events().filter(event => event.stage === 'start');
  assert.deepEqual(starts.map(event => event.abi).sort(), ['arm64-v8a', 'x86_64']);
  assert.deepEqual(starts.map(event => event.jobs).sort(), [2, 3]);
  assert.ok(starts.every(event => event.args.includes('--locked') && event.args.includes('--release')));
  assert.ok(!build.events().some(event => event.stage === 'exit'));
  for (const { abi } of starts) build.release(abi);
  const result = await build.finish();
  assert.equal(result.code, 0, result.stderr);
  assert.ok(!build.exists('native output/retired.so'));
  for (const { abi } of starts) {
    assert.equal(build.output(abi), abi);
    assert.ok(!build.exists('native output/' + abi + '/libunused.so'));
    build.assertDescendantExited(abi);
  }
});

test('a single CPU slot builds sequentially, including negative budgets that leave no spare CPUs', { timeout: 30000 }, async t => {
  for (const jobs of ['1', '-99999']) {
    await t.test(jobs, async t => {
      const build = fixture(t, jobs, 'sequential');
      const result = await build.finish();
      assert.equal(result.code, 0, result.stderr);
      assert.deepEqual(build.events().filter(event => event.abi).map(event => [event.stage, event.abi]), [
        ['start', 'arm64-v8a'], ['exit', 'arm64-v8a'], ['start', 'x86_64'], ['exit', 'x86_64'],
      ]);
      assert.ok(build.events().filter(event => event.stage === 'start').every(event => event.jobs === 1));
    });
  }
});

test('the default budget uses the CPUs available to this process', { timeout: 20000 }, async t => {
  const build = fixture(t, undefined, 'sequential');
  const result = await build.finish();
  assert.equal(result.code, 0, result.stderr);
  const slots = build.events().filter(event => event.stage === 'start').map(event => event.jobs);
  assert.equal(slots.length, 2);
  assert.ok(slots.every(value => value > 0));
  if (availableParallelism() > 1) assert.equal(slots.reduce((a, b) => a + b), availableParallelism());
  else assert.deepEqual(slots, [1, 1]);
});

test('either ABI failure stops the other compiler and preserves the failure status', { timeout: 30000 }, async t => {
  for (const failed of ['arm64-v8a', 'x86_64']) {
    await t.test(failed, async t => {
      const build = fixture(t, '4', 'fail-' + failed);
      const result = await build.finish();
      assert.equal(result.code, 37, result.stderr);
      assert.ok(build.exists('native output/retired.so'));
      build.assertDescendantExited(failed === 'arm64-v8a' ? 'x86_64' : 'arm64-v8a');
      assert.ok(!build.events().some(event => event.stage === 'exit'));
    });
  }
});

test('cancellation waits for both compiler process groups to exit', { timeout: 30000 }, async t => {
  for (const [signal, status] of [['SIGINT', 130], ['SIGTERM', 143]]) {
    await t.test(signal, async t => {
      const build = fixture(t, '4');
      await until(() => ['arm64-v8a', 'x86_64'].every(abi => build.exists(abi + '.descendant')));
      build.cancel(signal);
      const result = await build.finish();
      assert.equal(result.code, status, result.stderr);
      assert.ok(build.exists('native output/retired.so'));
      for (const abi of ['arm64-v8a', 'x86_64']) build.assertDescendantExited(abi);
    });
  }
});

test('a missing FFI library preserves the complete previous output', { timeout: 20000 }, async t => {
  const build = fixture(t, '4', 'missing-x86_64');
  await until(() => ['arm64-v8a', 'x86_64'].every(abi => build.exists(abi + '.descendant')));
  for (const abi of ['arm64-v8a', 'x86_64']) build.release(abi);
  const result = await build.finish();
  assert.notEqual(result.code, 0);
  assert.match(result.stderr, /libagent_ffi\.so/);
  assert.ok(build.exists('native output/retired.so'));
  assert.ok(!build.exists('native output/arm64-v8a/libagent_ffi.so'));
});

test('publication failure restores the previous output after moving it aside', { timeout: 20000 }, async t => {
  const build = fixture(t, '4', 'publish-failure');
  await until(() => ['arm64-v8a', 'x86_64'].every(abi => build.exists(abi + '.descendant')));
  for (const abi of ['arm64-v8a', 'x86_64']) build.release(abi);
  const result = await build.finish();
  assert.notEqual(result.code, 0);
  assert.match(result.stderr, /fixture publication failed/);
  assert.ok(build.exists('native output/retired.so'));
  assert.ok(!build.exists('native output/arm64-v8a/libagent_ffi.so'));
});

test('invalid CPU budgets fail before starting Cargo', { timeout: 20000 }, async t => {
  for (const jobs of ['0', '1.5', 'invalid']) {
    await t.test(jobs, async t => {
      const build = fixture(t, jobs);
      const result = await build.finish();
      assert.notEqual(result.code, 0);
      assert.match(result.stderr, /CARGO_BUILD_JOBS must be a nonzero integer/);
      assert.deepEqual(build.events(), []);
    });
  }
});

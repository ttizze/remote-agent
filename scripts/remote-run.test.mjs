import test from 'node:test';
import assert from 'node:assert/strict';
import { chmodSync, copyFileSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, symlinkSync, utimesSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { execFileSync, spawn } from 'node:child_process';
import { setTimeout } from 'node:timers/promises';

async function until(predicate) {
  // Bound fixture startup under parallel builds; explicit gates assert ordering.
  const deadline = Date.now() + 30_000;
  while (!predicate()) {
    assert.ok(Date.now() < deadline, 'remote command did not reach the expected stage');
    await setTimeout(10);
  }
}

function fixture(t) {
  const root = mkdtempSync(join(tmpdir(), 'remote-run-'));
  const work = join(root, 'work');
  const bin = join(root, 'bin');
  const log = join(root, 'events');
  mkdirSync(work);
  mkdirSync(bin);
  const git = execFileSync('sh', ['-c', 'command -v git'], { encoding: 'utf8' }).trim();
  execFileSync(git, ['init', '--quiet'], { cwd: work });
  writeFileSync(join(work, '.gitignore'), 'target/\n');
  execFileSync(git, ['add', '.gitignore'], { cwd: work });
  mkdirSync(join(work, 'scripts'));
  copyFileSync('scripts/remote-run.sh', join(work, 'scripts/remote-run.sh'));
  writeFileSync(log, '');
  const command = join(bin, 'command');
  writeFileSync(command, `#!${process.execPath}
const fs = require('node:fs');
const path = require('node:path');
const cp = require('node:child_process');
const name = path.basename(process.argv[1]);
const args = process.argv.slice(2);
const root = process.env.REMOTE_FIXTURE_ROOT;
const task = process.env.REMOTE_FIXTURE_TASK;
const record = stage => fs.appendFileSync(path.join(root, 'events'), task + ':' + stage + '\\n');
async function gate(stage) {
  record(stage);
  if (process.env.REMOTE_FIXTURE_HOLD) {
    while (!fs.existsSync(path.join(root, task + '.' + stage))) await new Promise(resolve => setTimeout(resolve, 10));
  }
}
async function main() {
  if (name === 'git') {
    if (args[0] === 'push') {
      if (!args.some(argument => argument.startsWith('builder:'))) throw new Error('wrong push destination');
      fs.writeFileSync(path.join(root, task + '.snapshot'), args.find(argument => argument.includes(':refs/snapshots/')).split(':')[0]);
      record('push'); return;
    }
    const result = cp.spawnSync(process.env.REMOTE_FIXTURE_GIT, args, { stdio: 'inherit' });
    process.exitCode = result.status ?? 1;
    return;
  }
  if (!args.includes('builder')) throw new Error('wrong SSH destination');
  if (args.includes('-O')) return;
  if (args.includes('-fN')) {
    const control = args.find(argument => argument.startsWith('ControlPath='))?.slice('ControlPath='.length);
    // Reserve room for the temporary suffix added while OpenSSH binds the socket.
    if (!control || Buffer.byteLength(control) > 80) throw new Error('SSH control path is too long');
    record('connect'); return;
  }
  const script = args.at(-1);
  if (script.startsWith('bash -c ')) {
    fs.writeFileSync(path.join(root, task + '.command'), script);
    await gate('run');
    process.exitCode = Number(process.env.REMOTE_FIXTURE_STATUS || 0);
  } else if (script.startsWith('cat ')) {
    await gate('download');
    const output = path.join(root, 'outputs', task);
    fs.mkdirSync(path.join(output, 'target'), { recursive: true });
    fs.writeFileSync(path.join(output, 'target', task), task);
    const result = cp.spawnSync('tar', ['-cf', '-', '-C', output, 'target'], { stdio: ['ignore', 'inherit', 'inherit'] });
    process.exitCode = result.status ?? 1;
  }
}
main().catch(error => { console.error(error); process.exitCode = 1; });
`);
  chmodSync(command, 0o755);
  for (const name of ['git', 'ssh']) symlinkSync(command, join(bin, name));
  const children = [];
  t.after(async () => {
    for (const { child } of children) {
      if (child.exitCode === null && child.signalCode === null) {
        try { process.kill(-child.pid, 'SIGKILL'); } catch (error) { if (error.code !== 'ESRCH') throw error; }
      }
    }
    await Promise.all(children.map(task => task.done));
    rmSync(root, { recursive: true, force: true });
  });
  return {
    work,
    events: () => readFileSync(log, 'utf8').trim().split('\n').filter(Boolean),
    command: task => readFileSync(join(root, task + '.command'), 'utf8'),
    release: (task, stage) => writeFileSync(join(root, `${task}.${stage}`), ''),
    start(name, overrides = {}, options = []) {
      const child = spawn('bash', ['scripts/remote-run.sh', ...options, 'builder', 'true'], {
        cwd: work,
        detached: true,
        env: { ...process.env, PATH: `${bin}:${process.env.PATH}`, REMOTE_FIXTURE_ROOT: root, REMOTE_FIXTURE_TASK: name, REMOTE_FIXTURE_GIT: git, ...overrides },
      });
      let stderr = '';
      child.stderr.setEncoding('utf8').on('data', data => { stderr += data; });
      child.stdout.resume();
      const task = { child, stderr: () => stderr, done: new Promise(resolve => child.on('close', code => resolve(code))) };
      children.push(task);
      return task;
    },
  };
}

test('one worktree owns the snapshot and returned outputs until the download completes', async (t) => {
  const build = fixture(t);
  const first = build.start('first', { REMOTE_FIXTURE_HOLD: '1' });
  await until(() => build.events().includes('first:run'));
  const second = build.start('second');
  await until(() => second.stderr().includes('Waiting for another remote run'));
  assert.ok(!build.events().includes('second:connect'));
  build.release('first', 'run');
  await until(() => build.events().includes('first:download'));
  assert.ok(!build.events().includes('second:push'));
  build.release('first', 'download');
  assert.equal(await first.done, 0, first.stderr());
  assert.equal(await second.done, 0, second.stderr());
  assert.ok(build.events().indexOf('first:download') < build.events().indexOf('second:connect'));
  for (const name of ['first', 'second']) assert.equal(readFileSync(join(build.work, 'target', name), 'utf8'), name);
});

// Runs the script the client sends, on a builder whose tools are the given stubs.
async function builderRun(t, tools, edit = script => script, stdin = 'pipe') {
  const build = fixture(t);
  const task = build.start('remote');
  assert.equal(await task.done, 0, task.stderr());
  const home = mkdtempSync(join(tmpdir(), 'remote-run-builder-'));
  t.after(() => rmSync(home, { recursive: true, force: true }));
  mkdirSync(join(home, '.cache/agent-remote-builds'), { recursive: true });
  const bin = join(home, 'bin');
  mkdirSync(bin);
  for (const [tool, body] of Object.entries({ nix: '', ...tools })) {
    writeFileSync(join(bin, tool), `#!/usr/bin/env bash\n${body}`);
    chmodSync(join(bin, tool), 0o755);
  }
  const locker = join(home, 'locker');
  const client = spawn('bash', ['-c', edit(build.command('remote'))], {
    cwd: home,
    env: { ...process.env, PATH: `${bin}:${process.env.PATH}`, FIXTURE_LOCKER: locker },
    stdio: [stdin, 'pipe', 'pipe'],
  });
  client.stdout.resume();
  client.stderr.resume();
  t.after(() => client.kill('SIGKILL'));
  return { client, locker, done: new Promise(resolve => client.on('close', code => resolve(code))) };
}

test('a failed checkout ends the session while the client is still connected', async t => {
  // Node closes a child's own stdin when it exits; SSH keeps it open until output ends.
  const heartbeats = spawn('sleep', ['60'], { stdio: ['ignore', 'pipe', 'ignore'] });
  t.after(() => heartbeats.kill('SIGKILL'));
  const { done } = await builderRun(t, { flock: 'exit 0\n', git: 'exit 1\n' }, undefined, heartbeats.stdout);
  assert.equal(await done, 1);
});

for (const [name, leave, edit] of [
  ['closes its connection', client => client.stdin.end(), undefined],
  ['stops sending heartbeats', () => {}, script => script.replace('read -r -t 120', 'read -r -t 1')],
]) {
  test(`a run queued for the worktree lock stops when its client ${name}`, { timeout: 30_000 }, async t => {
    const { client, locker, done } = await builderRun(
      t,
      { flock: '[[ $1 == -n ]] && exit 1\necho $$ > "$FIXTURE_LOCKER"\nexec sleep 60\n' },
      edit,
    );
    await until(() => existsSync(locker) && readFileSync(locker, 'utf8').trim());
    leave(client);
    assert.equal(await done, 1);
    const pid = Number(readFileSync(locker, 'utf8'));
    await until(() => {
      try { process.kill(pid, 0); return false; } catch { return true; }
    });
  });
}

test('snapshots include same-size edits with racy index timestamps without modifying staging', async t => {
  const build = fixture(t);
  const git = args => execFileSync('git', args, { cwd: build.work, encoding: 'utf8' });
  git(['config', 'core.trustctime', 'false']);
  const path = join(build.work, 'tracked.txt');
  const index = join(build.work, '.git/index');
  const timestamp = new Date('2023-11-14T22:13:20Z');
  writeFileSync(path, 'local commit\n');
  utimesSync(path, timestamp, timestamp);
  git(['add', 'tracked.txt']);
  utimesSync(index, timestamp, timestamp);
  const staged = readFileSync(index);
  writeFileSync(path, 'pending edit\n');
  utimesSync(path, timestamp, timestamp);
  const task = build.start('racy');
  assert.equal(await task.done, 0, task.stderr());
  const snapshot = readFileSync(join(build.work, '../racy.snapshot'), 'utf8');
  assert.equal(git(['show', `${snapshot}:tracked.txt`]), 'pending edit\n');
  assert.deepEqual(readFileSync(index), staged);
});

test('remote failures return their outputs and release the worktree for another command', async (t) => {
  const build = fixture(t);
  const failed = build.start('failed', { REMOTE_FIXTURE_STATUS: '19' });
  assert.equal(await failed.done, 19, failed.stderr());
  assert.ok(existsSync(join(build.work, 'target/failed')));
  const next = build.start('next');
  assert.equal(await next.done, 0, next.stderr());
  assert.ok(existsSync(join(build.work, 'target/next')));
});

test('native verification selects the remote environment and preserves failures', async (t) => {
  const build = fixture(t);
  const task = build.start('native', { REMOTE_FIXTURE_STATUS: '23' }, ['--native']);
  assert.equal(await task.done, 23, task.stderr());
  assert.match(build.command('native'), /'--native' 'true'/);
  assert.ok(existsSync(join(build.work, 'target/native')));
});

test('nested temporary directories cannot overflow the SSH control socket', async (t) => {
  const build = fixture(t);
  const longTemporary = join(build.work, '..', 'nested-' + 'a'.repeat(150));
  mkdirSync(longTemporary);
  const task = build.start('nested', { TMPDIR: longTemporary });
  assert.equal(await task.done, 0, task.stderr());
  assert.ok(existsSync(join(build.work, 'target/nested')));
});

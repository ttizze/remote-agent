import test from 'node:test';
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { mkdtempSync, mkdirSync, readFileSync, writeFileSync, chmodSync, existsSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

const script = fileURLToPath(new URL('./finish-ci-rust-cache.ts', import.meta.url));

async function fixture(t, failCommand = '') {
  const root = mkdtempSync(join(tmpdir(), 'finish compiler cache '));
  const worker = spawn(process.execPath, ['-e', `
    process.on('SIGTERM', () => setTimeout(() => {
      require('node:fs').writeFileSync(process.env.FINISHED, 'closed'); process.exit(0);
    }, 250));
    setInterval(() => {}, 1000); console.log('ready');
  `], {env: {...process.env, FINISHED: join(root, 'finished')}, stdio: ['ignore', 'pipe', 'inherit']});
  const exited = once(worker, 'exit');
  t.after(async () => {
    if (worker.exitCode === null && worker.signalCode === null) worker.kill('SIGKILL');
    await exited;
    rmSync(root, {recursive: true, force: true});
  });
  await once(worker.stdout, 'data');
  mkdirSync(join(root, 'run'));
  writeFileSync(join(root, 'run/daemon.state.json'), JSON.stringify({pid: worker.pid}));
  writeFileSync(join(root, 'kache'), `#!/usr/bin/env bash
set -euo pipefail
printf '%s\\n' "$*" >> "$COMMAND_LOG"
[[ "$*" != "$FAIL_COMMAND" ]] || exit 23
if [[ "$*" == 'gc --json' ]]; then
  if [[ ! -f "$COMMAND_LOG.gc" ]]; then
    touch "$COMMAND_LOG.gc"
    echo '{"skipped":true,"disk":{"store_bytes":0,"store_limit_bytes":1}}'
  elif [[ ! -f "$COMMAND_LOG.gc-budget" ]]; then
    touch "$COMMAND_LOG.gc-budget"
    echo '{"skipped":false,"disk":{"store_bytes":2,"store_limit_bytes":1}}'
  else
    echo '{"skipped":false,"disk":{"store_bytes":0,"store_limit_bytes":1}}'
  fi
fi
if [[ "$*" == 'daemon stop' ]]; then kill -TERM "$TEST_DAEMON_PID"; fi
`);
  chmodSync(join(root, 'kache'), 0o755);
  return {
    root,
    async run() {
      const child = spawn(process.execPath, [script], {stdio: 'ignore', env: {
        ...process.env, PATH: `${root}:${process.env.PATH}`, KACHE_RUNTIME_DIR: join(root, 'run'),
        COMMAND_LOG: join(root, 'commands'), FAIL_COMMAND: failCommand, TEST_DAEMON_PID: String(worker.pid),
      }});
      const [status] = await once(child, 'exit');
      return {status, commands: readFileSync(join(root, 'commands'), 'utf8').trim().split('\n')};
    },
  };
}

test('a bounded snapshot waits for a completed sweep and daemon exit', async t => {
  const f = await fixture(t);
  const result = await f.run();
  assert.equal(result.status, 0);
  assert.deepEqual(result.commands, ['daemon start', 'stats --full', 'gc --json', 'gc --json', 'gc --json', 'daemon stop']);
  assert.ok(existsSync(join(f.root, 'finished')));
});

test('a failed sweep still stops and waits for the daemon', async t => {
  const f = await fixture(t, 'gc --json');
  const result = await f.run();
  assert.notEqual(result.status, 0);
  assert.deepEqual(result.commands, ['daemon start', 'stats --full', 'gc --json', 'daemon stop']);
  assert.ok(existsSync(join(f.root, 'finished')));
});

test('an invalid process record prevents publishing and still requests shutdown', async t => {
  const f = await fixture(t);
  writeFileSync(join(f.root, 'run/daemon.state.json'), JSON.stringify({pid: 0}));
  const result = await f.run();
  assert.notEqual(result.status, 0);
  assert.deepEqual(result.commands, ['daemon start', 'daemon stop']);
});

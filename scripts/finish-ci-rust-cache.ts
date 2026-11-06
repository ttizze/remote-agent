import { spawnSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { setTimeout } from 'node:timers/promises';

const timeoutMs = 10_000;

function kache(args: string[]) {
  const result = spawnSync('kache', args, {encoding: 'utf8', stdio: ['inherit', 'pipe', 'inherit']});
  if (result.stdout) process.stdout.write(result.stdout);
  if (result.error) throw result.error;
  if (result.status !== 0) throw new Error(`kache ${args.join(' ')} failed (${result.status})`);
  return result.stdout;
}

async function sweep() {
  // Kache protects artifacts accessed in the preceding 120 seconds.
  const deadline = Date.now() + 120_000 + timeoutMs;
  for (;;) {
    const {skipped, disk: {store_bytes: bytes, store_limit_bytes: limit}} = JSON.parse(kache(['gc', '--json']));
    if (typeof skipped !== 'boolean' || !Number.isSafeInteger(bytes) || !Number.isSafeInteger(limit)) {
      throw new Error('Compiler cache sweep did not report completion and capacity');
    }
    if (!skipped && bytes <= limit) return;
    if (Date.now() >= deadline) throw new Error('Compiler cache sweep has not reached its capacity limit');
    await setTimeout(skipped ? 100 : 1_000);
  }
}

async function waitForExit(pid: number) {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    try {
      process.kill(pid, 0);
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code === 'ESRCH') return;
      throw error;
    }
    if (Date.now() >= deadline) throw new Error(`Compiler cache daemon ${pid} is still running`);
    await setTimeout(100);
  }
}

if (!process.env.KACHE_RUNTIME_DIR) throw new Error('KACHE_RUNTIME_DIR is required');
kache(['daemon', 'start']);
let pid: number | undefined;
try {
  const state = JSON.parse(readFileSync(join(process.env.KACHE_RUNTIME_DIR, 'daemon.state.json'), 'utf8')) as { pid: number };
  if (!Number.isSafeInteger(state.pid) || state.pid <= 1) throw new Error('Invalid compiler cache daemon PID');
  pid = state.pid;
  // Report every compilation in this job; its runtime directory holds no older events.
  kache(['stats', '--full']);
  await sweep();
} finally {
  kache(['daemon', 'stop']);
  // Ownership release can precede process exit and final SQLite file cleanup.
  if (pid !== undefined) await waitForExit(pid);
}

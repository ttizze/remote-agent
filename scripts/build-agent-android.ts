import { spawn, type ChildProcess } from 'node:child_process';
import { existsSync, mkdirSync, mkdtempSync, renameSync, rmSync } from 'node:fs';
import { availableParallelism } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const [destination, ...abis] = process.argv.slice(2);
if (!destination || abis.length === 0) throw new Error('usage: node scripts/build-agent-android.ts output-directory abi...');
const configured = process.env.CARGO_BUILD_JOBS;
const requested = configured === undefined ? availableParallelism() : Number(configured);
if ((configured !== undefined && !/^-?\d+$/.test(configured)) || !Number.isSafeInteger(requested) || requested === 0) {
  throw new Error('CARGO_BUILD_JOBS must be a nonzero integer');
}
const jobs = Math.max(1, requested < 0 ? availableParallelism() + requested : requested);
const cwd = fileURLToPath(new URL('..', import.meta.url));
const output = resolve(destination);
mkdirSync(dirname(output), { recursive: true });
const work = mkdtempSync(join(dirname(output), '.agent-android.'));
const generated = join(work, 'cargo');
const next = join(work, 'next');
const previous = join(work, 'previous');
const children = new Set<ChildProcess>();
function stop(signal: NodeJS.Signals, status: number) {
  if (process.exitCode) return;
  process.exitCode = status;
  for (const child of children) {
    if (child.exitCode !== null || child.signalCode !== null) continue;
    try { process.kill(-child.pid!, signal); } catch (error) { if ((error as NodeJS.ErrnoException).code !== 'ESRCH') throw error; }
  }
}
for (const [signal, status] of [['SIGINT', 130], ['SIGTERM', 143]] as const) {
  process.on(signal, () => stop(signal, status));
}

function build(abi: string, slots: number) {
  if (process.exitCode) return Promise.reject(new Error('Android build cancelled'));
  const child = spawn('cargo', [
    'ndk', '--target', abi, '--output-dir', generated,
    'build', '--locked', '--package', 'agent-ffi', '--release',
  ], {
    cwd, detached: true, stdio: ['ignore', 'inherit', 'inherit'],
    env: { ...process.env, CARGO_BUILD_JOBS: String(slots) },
  });
  if (child.pid) children.add(child);
  return new Promise<void>((resolve, reject) => {
    child.once('error', reject);
    child.once('close', (code, signal) => {
      children.delete(child);
      if (code === 0) resolve();
      else reject(Object.assign(new Error(`${abi} build failed (${signal || code})`), { status: code || 1 }));
    });
  });
}

let pending: Promise<void>[] = [];
try {
  for (let offset = 0; offset < abis.length; offset += jobs) {
    const batch = abis.slice(offset, offset + jobs);
    pending = batch.map((abi, index) => build(abi, Math.floor(jobs / batch.length) + (index < jobs % batch.length ? 1 : 0)));
    await Promise.all(pending);
  }
  // Dependencies are linked into agent-ffi. cargo-ndk also copies unused
  // dependency cdylibs; keep those and retired build hashes out of the APK.
  for (const abi of abis) {
    mkdirSync(join(next, abi), { recursive: true });
    renameSync(join(generated, abi, 'libagent_ffi.so'), join(next, abi, 'libagent_ffi.so'));
  }
  if (existsSync(output)) renameSync(output, previous);
  renameSync(next, output);
} catch (error) {
  console.error((error as Error).message);
  stop('SIGTERM', (error as Error & { status?: number }).status || 1);
  await Promise.allSettled(pending);
} finally {
  if (existsSync(previous) && !existsSync(output)) renameSync(previous, output);
  rmSync(work, { recursive: true, force: true });
}

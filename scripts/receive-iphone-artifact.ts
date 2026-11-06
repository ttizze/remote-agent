import { execFile } from 'node:child_process';
import { mkdir, mkdtemp, readdir, rename, rm } from 'node:fs/promises';
import { basename, dirname, join, resolve } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import { promisify } from 'node:util';

// A producer publishes the runner before building the app. Both artifacts belong
// to its exact build attempt. Rerunning only a consumer keeps the last producer;
// rerunning the producer must never consume that producer's older artifacts.
const [kind, output, timeout = '3600'] = process.argv.slice(2);
const repository = process.env.GITHUB_REPOSITORY!;
const run = process.env.GITHUB_RUN_ID!;
const attempt = process.env.GITHUB_RUN_ATTEMPT!;
if (!['iphone-test-runner', 'iphone-test-build'].includes(kind ?? '') || !output ||
    !/^[\w-]+\/[\w.-]+$/.test(repository ?? '') ||
    ![run, attempt, timeout].every(value => /^[1-9][0-9]*$/.test(value ?? '')) ||
    Number(timeout) > 2_147_483) {
  throw new Error('Expected an iPhone artifact, output path, positive timeout and GitHub run identity');
}

const destination = resolve(output);
const execute = promisify(execFile);
const controller = new AbortController();
const interrupt = () => controller.abort(new Error('Artifact reception interrupted'));
const deadline = setTimeout(() => controller.abort(new Error(`Timed out waiting for ${kind}`)), Number(timeout) * 1000);
process.once('SIGINT', interrupt);
process.once('SIGTERM', interrupt);

async function gh(args: string[]) {
  for (;;) {
    const operation = execute('gh', args, {
      signal: controller.signal,
      maxBuffer: 4 * 1024 * 1024,
    });
    try {
      return (await operation).stdout;
    } catch (error) {
      // Metadata reads are safe to repeat after transport failures. Keep
      // authentication errors and downloads with partial files fail-fast.
      if (args[0] !== 'api' || controller.signal.aborted ||
          !/TLS handshake timeout|i\/o timeout|connection reset by peer|unexpected EOF/.test((error as Error & { stderr?: string }).stderr ?? '')) {
        throw error;
      }
      console.error('Temporary GitHub API connection failure; retrying in 5 seconds');
    } finally {
      const child = operation.child;
      if (child.pid && child.exitCode === null && child.signalCode === null) {
        await new Promise(resolve => child.once('close', resolve));
      }
    }
    await delay(5000, undefined, { signal: controller.signal });
  }
}

let pending;
try {
  console.log(`Waiting for ${kind} from run ${run}`);
  let artifactName!: string;
  for (;;) {
    const jobs = JSON.parse(await gh(['api', `repos/${repository}/actions/runs/${run}/jobs?filter=latest&per_page=100`])) as { jobs?: { name: string; status: string; conclusion?: string; run_attempt: number }[] };
    if (!Array.isArray(jobs.jobs)) throw new Error('GitHub did not return jobs');
    const producer = jobs.jobs.find(job => job.name === 'iPhone build');
    if (producer?.status === 'completed' && producer.conclusion !== 'success') {
      throw new Error(`iPhone build ended with ${producer.conclusion}`);
    }
    if (producer) {
      if (!Number.isSafeInteger(producer.run_attempt) || producer.run_attempt < 1 || producer.run_attempt > Number(attempt)) {
        throw new Error('iPhone build did not identify its run attempt');
      }
      artifactName = `${kind}-${producer.run_attempt}`;
      const artifacts = JSON.parse(await gh(['api', `repos/${repository}/actions/runs/${run}/artifacts?name=${artifactName}`])) as { artifacts?: { name: string; expired: boolean }[] };
      if (!Array.isArray(artifacts.artifacts)) throw new Error('GitHub did not return artifacts');
      const matches = artifacts.artifacts.filter(artifact => artifact.name === artifactName && !artifact.expired);
      if (matches.length > 1) throw new Error(`More than one ${artifactName} was published`);
      if (matches.length === 1) break;
      if (producer.status === 'completed') throw new Error(`iPhone build did not publish ${artifactName}`);
    }
    // Three consumers share the repository's GitHub API quota while building.
    await delay(30_000, undefined, { signal: controller.signal });
  }
  await mkdir(dirname(destination), { recursive: true });
  pending = await mkdtemp(join(dirname(destination), '.iphone-artifact-'));
  await gh(['run', 'download', run, '--repo', repository, '--name', artifactName, '--dir', pending]);
  const files = await readdir(pending);
  if (files.length !== 1 || files[0] !== basename(destination)) {
    throw new Error(`${artifactName} did not contain only ${basename(destination)}`);
  }
  await rename(join(pending, files[0]), destination);
  console.log(`Received ${artifactName}: ${destination}`);
} catch (error) {
  console.error(controller.signal.reason?.message ?? (error as Error).message);
  process.exitCode = 1;
} finally {
  clearTimeout(deadline);
  process.removeListener('SIGINT', interrupt);
  process.removeListener('SIGTERM', interrupt);
  if (pending) await rm(pending, { recursive: true, force: true });
}

// Explicit Mac debugging: render the same fixture conversations with each adapter.
// UI journeys run sequentially so their windows never steal each other's input.
import { spawn, execFileSync } from 'node:child_process';
import { mkdir, readFile, readdir, writeFile } from 'node:fs/promises';
import { dirname, join, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

export const MODES = ['hybrid', 'native', 'nsh', 'standard'];
export const DEFAULT_SCENARIOS = [
  'long-history', 'history-position', 'history-live-scroll', 'long-markdown',
  'markdown-document', 'markdown-table', 'images', 'selection-bar',
  'reading-order', 'scroll-history',
];

export function cpuSeconds(value) {
  return value.split(':').reduce((seconds, part) => seconds * 60 + Number(part), 0);
}

export function escapeHtml(value) {
  return String(value).replace(/[&<>"']/g, character => ({
    '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;',
  })[character]);
}

function fileLink(from, path) {
  return relative(from, path).split('/').map(encodeURIComponent).join('/');
}

function sample(app, metrics) {
  const processes = execFileSync('/bin/ps', ['-axo', 'pid=,rss=,time=,comm='], { encoding: 'utf8' });
  for (const line of processes.split('\n')) {
    const match = line.match(/^\s*(\d+)\s+(\d+)\s+([\d:.]+)\s+(.+?)\s*$/);
    if (!match || match[4] !== app) continue;
    metrics.peakRssMiB = Math.max(metrics.peakRssMiB, Number(match[2]) / 1024);
    metrics.cpuByPid[match[1]] = cpuSeconds(match[3]);
    metrics.samples++;
  }
}

export function observedResult(outcome) {
  return {
    ...outcome?.result,
    passed: outcome?.result?.passed === true && outcome.error == null,
    runnerError: outcome ? outcome.error : 'Scenario was not run',
    seconds: outcome?.seconds,
  };
}

async function runMode(root, app, mode, scenarios) {
  console.log(`\nConversation renderer: ${mode}`);
  const child = spawn('scripts/dev-env.sh', ['--native', 'cargo', 'xtask', 'nsh-acceptance', ...scenarios], {
    cwd: root, env: { ...process.env, BEX_CONVERSATION_RENDERER: mode }, stdio: ['ignore', 'pipe', 'pipe'],
  });
  let output = '';
  let current;
  const metrics = {};
  let incomplete = '';
  child.stdout.on('data', chunk => {
    output += chunk;
    process.stdout.write(chunk);
    incomplete += chunk;
    const lines = incomplete.split('\n');
    incomplete = lines.pop();
    for (const line of lines) {
      const running = line.match(/^(.+): running$/);
      if (running) {
        current = running[1];
        metrics[current] = { peakRssMiB: 0, cpuByPid: {}, samples: 0 };
      }
    }
  });
  child.stderr.on('data', chunk => { output += chunk; process.stderr.write(chunk); });
  const timer = setInterval(() => {
    if (current) sample(app, metrics[current]);
  }, 500);
  let exitCode;
  try {
    exitCode = await new Promise((done, reject) => { child.once('error', reject); child.once('close', done); });
  } finally {
    clearInterval(timer);
  }
  const report = output.match(/^report: (.+)$/m)?.[1];
  if (!report) return { mode, exitCode, error: output.slice(-5000), cases: {} };
  const directory = dirname(resolve(root, report));
  const summary = JSON.parse(await readFile(join(directory, 'summary.json'), 'utf8'));
  const outcomes = new Map(summary.outcomes.map(outcome => [outcome.name, outcome]));
  const cases = {};
  for (const name of summary.scenarios) {
    const measured = metrics[name];
    let files;
    try { files = await readdir(join(directory, name)); }
    catch (error) { if (error.code !== 'ENOENT') throw error; files = []; }
    cases[name] = {
      ...observedResult(outcomes.get(name)), directory: join(directory, name),
      screenshots: files.filter(file => file.endsWith('.png')).sort(),
      metrics: measured ? { peakRssMiB: measured.peakRssMiB,
        appCpuSeconds: Object.values(measured.cpuByPid).reduce((sum, value) => sum + value, 0), samples: measured.samples } : null,
    };
  }
  return { mode, exitCode, directory, report: await readFile(report, 'utf8'), cases };
}

export function renderReport(results, directory, revision) {
  const names = [...new Set(results.flatMap(result => Object.keys(result.cases)))].sort();
  const namesOfModes = results.map(result => result.mode);
  const sections = names.map(name => {
    const screenshots = [...new Set(results.flatMap(result => result.cases[name]?.screenshots ?? []))].sort();
    return `<section><h2>${escapeHtml(name)}</h2><div class="grid">${results.map(result => {
      const test = result.cases[name];
      const failure = test?.steps?.find(step => step.status === 'failed');
      return `<article><h3>${escapeHtml(result.mode)}</h3><p class="${test?.passed ? 'pass' : 'fail'}">${test?.passed ? 'PASS' : 'FAIL / incomplete'}</p>
        <p>${test?.metrics ? `Peak RSS: ${test.metrics.peakRssMiB.toFixed(1)} MiB · App CPU: ${test.metrics.appCpuSeconds.toFixed(2)} s` : 'No process measurement'}</p>
        ${test?.runnerError ? `<p>${escapeHtml(test.runnerError)}</p>` : ''}
        ${failure ? `<p>${escapeHtml(failure.source)}<br>${escapeHtml(failure.message)}</p>` : ''}</article>`;
    }).join('')}</div>${screenshots.map(file => `<h4>${escapeHtml(file)}</h4><div class="grid">${results.map(result => {
      const test = result.cases[name];
      return `<figure>${test?.screenshots.includes(file) ? `<a href="${fileLink(directory, join(test.directory, file))}"><img loading="lazy" src="${fileLink(directory, join(test.directory, file))}" alt="${escapeHtml(result.mode)} ${escapeHtml(name)} ${escapeHtml(file)}"></a>` : '<p>No capture at this step</p>'}<figcaption>${escapeHtml(result.mode)}</figcaption></figure>`;
    }).join('')}</div>`).join('')}</section>`;
  }).join('');
  return `<!doctype html><html lang="ja"><meta charset="utf-8"><meta name="viewport" content="width=device-width"><title>Conversation renderer comparison</title>
    <style>body{font:15px system-ui;margin:24px;background:#111;color:#ddd}a{color:#8bc6ff}.grid{display:grid;grid-template-columns:repeat(${namesOfModes.length},minmax(0,1fr));gap:16px}article,figure{margin:0;background:#1b1b1b;padding:12px;border-radius:8px;overflow-wrap:anywhere}img{width:100%;height:auto}.pass{color:#7ed79b}.fail{color:#ff9a91}section{margin:40px 0}h4{margin-bottom:8px}pre{white-space:pre-wrap}summary{cursor:pointer}</style>
    <h1>Conversation renderer comparison</h1><p>Revision ${escapeHtml(revision)} · ${escapeHtml(new Date().toISOString())}</p>
    <p>Same fixture conversations, fresh isolated Host and app for each journey. Native is an AppKit conversation prototype; hybrid is the current renderer; nsh uses NSH text and lists.</p>
    <p>RSS is sampled every 500 ms for the app process only; CPU includes app startup and the whole journey. These are diagnostic observations, not frame-time benchmarks. Failed and shorter journeys are not performance-equivalent. Native prototype gaps (such as rich media and table controls) are separate from scrolling defects.</p>
    ${sections}<h2>Runner reports</h2>${results.map(result => `<details><summary>${escapeHtml(result.mode)} · exit ${result.exitCode}</summary><pre>${escapeHtml(result.report ?? result.error)}</pre></details>`).join('')}</html>`;
}

async function main(scenarios) {
  // A focused rerun can select a subset with BEX_COMPARISON_RENDERERS=native.
  const modes = process.env.BEX_COMPARISON_RENDERERS?.split(',') ?? MODES;
  if (modes.some(mode => !MODES.includes(mode)) || new Set(modes).size !== modes.length) {
    throw new Error(`BEX_COMPARISON_RENDERERS must select from ${MODES.join(',')}`);
  }
  const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
  const target = JSON.parse(execFileSync('cargo', ['metadata', '--no-deps', '--format-version', '1'], { cwd: root, encoding: 'utf8' })).target_directory;
  const app = join(target, 'debug/bex.app/Contents/MacOS/bex');
  const directory = join(target, 'conversation-comparison', `run-${Date.now()}`);
  await mkdir(directory, { recursive: true });
  const results = [];
  for (const mode of modes) {
    results.push(await runMode(root, app, mode, scenarios.length ? scenarios : DEFAULT_SCENARIOS));
    await writeFile(join(directory, 'results.json'), JSON.stringify(results, null, 2));
  }
  const revision = execFileSync('git', ['rev-parse', 'HEAD'], { cwd: root, encoding: 'utf8' }).trim();
  const report = join(directory, 'index.html');
  await writeFile(report, renderReport(results, directory, `${revision} (worktree)`));
  console.log(`\nComparison: ${report}`);
  if (results.some(result => result.exitCode !== 0)) process.exitCode = 1;
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main(process.argv.slice(2)).catch(error => { console.error(error); process.exitCode = 1; });
}

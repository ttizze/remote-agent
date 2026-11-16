// T3 Code's side of `cargo xtask nsh-parity`: renders the conversation states the
// plan names in the T3 Code the user has installed (`t3App`): its own server and
// web client, run apart from the user's data, in headless Chromium, and writes a
// screenshot and DOM metrics for each. A T3 checkout (`t3Root`) only lends
// Playwright and the Codex protocol's schema.
//
// T3's server drives Codex through `codex app-server`; here that is Bex's own
// Codex fixture (`crates/host-fixture`), the program the NSH side's fixture Host
// runs, so both clients show the same turns, items and text. The fixture
// implements the subset of the protocol Bex reads; T3 decodes every payload
// against the full generated schema, so this script relays the fixture through
// itself (`codex-shim`) and fills only the keys that schema requires and the
// fixture omits, with schema defaults. Content is never changed.
//
// Why not mount components with props: the conversation screen is ChatView and
// everything it renders (header, timeline, minimap, composer, panels), driven by
// stores, a router and the server's live events. Running the whole client
// against the same fixture renders every row and state as T3 users see it,
// without rebuilding T3's state by hand.
//
// Usage: node scripts/nsh-parity-t3.ts <plan.json> <output-directory>
//        node scripts/nsh-parity-t3.ts codex-shim <fixture> <traffic.log> <args...>
import { spawn, execFileSync, type ChildProcess } from 'node:child_process';
import { appendFileSync, copyFileSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { tmpdir } from 'node:os';
import { basename, dirname, join } from 'node:path';
import { createInterface } from 'node:readline';
import { fileURLToPath, pathToFileURL } from 'node:url';

type Step =
  | { send: string } | { type: string } | { key: string }
  | { click: string } | { clickText: string } | { hover: string }
  | { waitText: string; timeoutMs?: number } | { waitFor: string; timeoutMs?: number }
  | { waitMs: number } | { release: true };

export type Plan = {
  t3App: string;
  t3Root: string;
  codexFixture: string;
  viewport: { width: number; height: number; scale: number };
  states: { name: string; steps: Step[]; elements: Record<string, string> }[];
};

// The few Playwright calls this script makes.
type Locator = {
  click(options?: object): Promise<void>;
  hover(): Promise<void>;
  waitFor(options: object): Promise<void>;
  first(): Locator;
  getByRole(role: string, options?: object): Locator;
};
type Page = {
  goto(url: string): Promise<unknown>;
  locator(selector: string): Locator;
  getByText(text: string, options?: object): Locator;
  getByRole(role: string, options?: object): Locator;
  keyboard: { type(text: string): Promise<void>; press(key: string): Promise<void> };
  mouse: { move(x: number, y: number): Promise<void> };
  screenshot(options: { path: string }): Promise<unknown>;
  evaluate<T>(expression: string): Promise<T>;
  waitForTimeout(ms: number): Promise<void>;
  on(event: 'console', listener: (message: { type(): string; text(): string }) => void): void;
};
type Context = {
  addInitScript(script: string): Promise<void>;
  newPage(): Promise<Page>;
};
type Browser = { newContext(options: object): Promise<Context>; close(): Promise<void> };

/** T3's client settings: onboarding is finished, as for a returning user. */
const CLIENT_SETTINGS = `localStorage.setItem('t3code:client-settings:v1', ${JSON.stringify(JSON.stringify({ onboardingCompletedAt: '2026-01-01T00:00:00.000Z' }))})`;
const STEP_TIMEOUT_MS = 20_000;

/** Measures the elements each selector matches, in the page. */
const MEASURE = `(selectors) => {
  const out = {};
  for (const [id, selector] of Object.entries(selectors)) {
    out[id] = [...document.querySelectorAll(selector)].flatMap((element) => {
      const rect = element.getBoundingClientRect();
      if (rect.width <= 0 || rect.height <= 0) return [];
      const style = getComputedStyle(element);
      if (style.visibility === 'hidden') return [];
      // Hover-only controls are transparent, often through an ancestor.
      for (let node = element; node; node = node.parentElement) {
        if (Number(getComputedStyle(node).opacity) === 0) return [];
      }
      const animations = element.getAnimations({ subtree: true })
        .filter((animation) => animation.playState === 'running' && animation.effect?.getComputedTiming().iterations === Infinity)
        .map((animation) => animation.animationName || animation.constructor.name);
      return [{
        frame: [rect.x, rect.y, rect.width, rect.height],
        text: (element.innerText ?? '').trim().slice(0, 200),
        css: {
          fontSize: style.fontSize, fontWeight: style.fontWeight, lineHeight: style.lineHeight,
          fontFamily: style.fontFamily.split(',')[0], color: style.color,
          backgroundColor: style.backgroundColor, borderRadius: style.borderTopLeftRadius,
          padding: [style.paddingTop, style.paddingRight, style.paddingBottom, style.paddingLeft].join(' '),
        },
        animations,
      }];
    });
  }
  return out;
}`;

/** The page's elements with their frames, data attributes, labels and own
 * text, indented by depth: what map selectors are written from. */
const OUTLINE = `(() => {
  const lines = [];
  const walk = (element, depth) => {
    const style = getComputedStyle(element);
    if (style.display === 'none' || style.visibility === 'hidden' || Number(style.opacity) === 0) return;
    const rect = element.getBoundingClientRect();
    if (rect.width === 0 && rect.height === 0 && element.children.length === 0) return;
    if (['svg', 'path', 'circle', 'line', 'rect', 'polyline', 'g'].includes(element.tagName.toLowerCase())) return;
    const attributes = [...element.attributes]
      .filter((attribute) => attribute.name.startsWith('data-') || ['aria-label', 'role'].includes(attribute.name))
      .map((attribute) => attribute.name + '=' + attribute.value.slice(0, 40)).join(' ');
    const text = [...element.childNodes].filter((node) => node.nodeType === 3).map((node) => node.textContent.trim()).join(' ').slice(0, 60);
    const animations = element.getAnimations().map((animation) => animation.animationName).filter(Boolean).join(',');
    lines.push('  '.repeat(depth) + element.tagName.toLowerCase() + ' [' + [rect.x, rect.y, rect.width, rect.height].map(Math.round).join(',') + '] '
      + attributes + (text ? ' ' + JSON.stringify(text) : '') + (animations ? ' animations=' + animations : '')
      + ' .' + (element.getAttribute('class') ?? '').slice(0, 100));
    for (const child of element.children) walk(child, depth + 1);
  };
  walk(document.body, 0);
  return lines.join('\\n');
})()`;

if (process.argv[2] === 'codex-shim') {
  await codexShim(process.argv[3]!, process.argv[4]!, process.argv.slice(5));
} else if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const [planPath, output] = process.argv.slice(2);
  if (!planPath || !output) throw new Error('usage: node scripts/nsh-parity-t3.ts <plan.json> <output-directory>');
  await render(JSON.parse(readFileSync(planPath, 'utf8')) as Plan, output);
}

async function render(plan: Plan, output: string) {
  const { chromium } = (await import(pathToFileURL(playwright(plan.t3Root)).href)) as { chromium: { launch(options: object): Promise<Browser> } };
  const browser = await chromium.launch({ headless: true });
  const summary: Record<string, string | null> = {};
  try {
    for (const state of plan.states) {
      const directory = join(output, state.name);
      mkdirSync(directory, { recursive: true });
      try {
        await renderState(plan, state, browser, directory);
        summary[state.name] = null;
        console.log(`t3 ${state.name}: captured`);
      } catch (error) {
        summary[state.name] = String(error instanceof Error ? error.message : error).split('\n')[0]!;
        console.log(`t3 ${state.name}: FAILED ${summary[state.name]}`);
      }
    }
  } finally {
    await browser.close();
  }
  writeFileSync(join(output, 't3-summary.json'), JSON.stringify(summary, null, 2));
}

/** Playwright as T3's own dependencies pin it (its Chromium is installed with them). */
function playwright(t3Root: string) {
  const store = join(t3Root, 'node_modules/.pnpm');
  const name = readdirSync(store).find(entry => entry.startsWith('playwright-core@'));
  if (!name) throw new Error(`no playwright-core in ${store}; install T3's dependencies`);
  return join(store, name, 'node_modules/playwright-core/index.mjs');
}

/** A T3 server with its own home, data and project, whose Codex is the fixture. */
function environment(plan: Plan, log: string) {
  const root = mkdtempSync(join(tmpdir(), 'bex-parity-t3-'));
  const home = join(root, 'home');
  const base = join(root, 'base');
  const codex = join(root, 'codex');
  const project = join(root, 'project');
  for (const directory of [home, join(base, 'userdata'), codex, join(project, 'nested')]) mkdirSync(directory, { recursive: true });
  // As `host_fixture::fixture::Config::install` and `agent-ui-fixture` prepare it.
  copyFileSync(plan.codexFixture, join(codex, 'agent-codex-fixture'));
  execFileSync('chmod', ['+x', join(codex, 'agent-codex-fixture')]);
  writeFileSync(join(codex, 'fixture-config.json'), JSON.stringify({ deferred_thread_metadata: false, initialize_gate: null, trace: true, expected_cwd: null, stream_delay_ms: 2000 }));
  writeFileSync(join(codex, 'account-fixture.json'), JSON.stringify({ type: 'chatgpt', email: 'desktop@example.invalid', planType: 'plus', accountId: 'desktop' }));
  writeFileSync(join(project, 'hello.txt'), '日本語のファイル\n編集を確認します。\n');
  writeFileSync(join(project, 'nested/child.txt'), 'Nested directory navigation fixture.\n');
  execFileSync('git', ['init', '--quiet'], { cwd: project });
  writeFileSync(join(root, 'codex-shim'), `#!/bin/sh\nexec "${process.execPath}" "${fileURLToPath(import.meta.url)}" codex-shim "${join(codex, 'agent-codex-fixture')}" "${log}" "${plan.t3Root}" "$@"\n`, { mode: 0o755 });
  writeFileSync(join(base, 'userdata/settings.json'), JSON.stringify({
    providerInstances: {
      codex: { driver: 'codex', enabled: true, config: { setupMode: 'existing', binaryPath: join(root, 'codex-shim'), homePath: codex } },
      ...Object.fromEntries(['claudeAgent', 'cursor', 'grok', 'pi', 'opencode', 'antigravity'].map(driver => [driver, { driver, enabled: false }])),
    },
  }));
  return { root, home, base, codex, project };
}

async function renderState(plan: Plan, state: Plan['states'][number], browser: Browser, directory: string) {
  const env = environment(plan, join(directory, 't3-codex.log'));
  const port = 30_000 + Math.floor(Math.random() * 20_000);
  const serverLog = join(directory, 't3-server.log');
  // The user's own Codex, Claude and settings stay out of reach: a home and PATH of its own.
  // The app's own runtime and server, as it starts them itself; it serves its web client.
  const server = spawn(join(plan.t3App, 'Contents/MacOS', basename(plan.t3App, '.app')), [join(plan.t3App, 'Contents/Resources/app.asar/apps/server/dist/bin.mjs'), 'serve', '--host', '127.0.0.1', '--port', String(port), '--base-dir', env.base, '--no-browser', env.project], {
    cwd: env.root,
    env: { PATH: `/usr/bin:/bin:/usr/sbin:/sbin:${dirname(process.execPath)}`, HOME: env.home, NO_COLOR: '1', TMPDIR: env.root, ELECTRON_RUN_AS_NODE: '1' },
    stdio: ['ignore', 'pipe', 'pipe'],
    detached: true,
  });
  let printed = '';
  const append = (chunk: Buffer) => { printed += chunk; appendFileSync(serverLog, chunk); };
  server.stdout!.on('data', append);
  server.stderr!.on('data', append);
  try {
    const token = await until(() => /Token: (\S+)/.exec(printed)?.[1], 60_000, 'the T3 server printed no pairing token');
    const context = await browser.newContext({
      viewport: { width: plan.viewport.width, height: plan.viewport.height },
      deviceScaleFactor: plan.viewport.scale,
      colorScheme: 'dark',
      reducedMotion: 'no-preference',
    });
    await context.addInitScript(CLIENT_SETTINGS);
    const page = await context.newPage();
    page.on('console', message => { if (message.type() === 'error') appendFileSync(serverLog, `[console] ${message.text()}\n`); });
    await page.goto(`http://127.0.0.1:${port}/pair#token=${encodeURIComponent(token)}`);
    try {
      await addProject(page, env.project);
      for (const step of state.steps) {
        await take(page, step, env.codex);
        // The NSH driver presses through accessibility, without a pointer: park it.
        if ('click' in step || 'clickText' in step || 'send' in step) await page.mouse.move(plan.viewport.width - 2, plan.viewport.height - 2);
      }
    } catch (error) {
      // What the page showed instead, for fixing the steps.
      await page.screenshot({ path: join(directory, 't3-failure.png') });
      writeFileSync(join(directory, 't3-outline.txt'), await page.evaluate<string>(OUTLINE));
      throw error;
    }
    // Let transitions settle; endless animations (shimmer, spinners) keep running.
    await page.waitForTimeout(600);
    await page.screenshot({ path: join(directory, 't3.png') });
    writeFileSync(join(directory, 't3-outline.txt'), await page.evaluate<string>(OUTLINE));
    const elements = await page.evaluate(`(${MEASURE})(${JSON.stringify(state.elements)})`);
    writeFileSync(join(directory, 't3.json'), JSON.stringify({ ...plan.viewport, elements }, null, 2));
  } finally {
    try { process.kill(-server.pid!, 'SIGTERM'); } catch { /* already gone */ }
    await exited(server);
    // The state's logs and captures are in `directory`; the environment is scratch.
    rmSync(env.root, { recursive: true, force: true });
  }
}

async function addProject(page: Page, project: string) {
  await page.getByRole('main').getByRole('button', { name: 'Add project' }).click();
  await page.getByRole('option', { name: /Local folder/ }).click();
  const path = page.getByRole('combobox', { name: /Enter path/ });
  await path.click();
  await page.keyboard.press('Meta+A');
  await page.keyboard.type(project);
  await page.getByRole('button', { name: /^Add/ }).click();
  await page.getByRole('textbox', { name: 'Message' }).waitFor({ state: 'visible', timeout: STEP_TIMEOUT_MS });
  // The provider probe finishes after the project opens; sending needs it.
  await page.getByText('Enable a provider', { exact: false }).waitFor({ state: 'hidden', timeout: 60_000 });
  // Its notice over the conversation until then is the probe's, not the state's.
  await page.getByText('has not been checked', { exact: false }).waitFor({ state: 'hidden', timeout: 60_000 });
}

async function take(page: Page, step: Step, codexHome: string) {
  const timeout = 'timeoutMs' in step && step.timeoutMs ? step.timeoutMs : STEP_TIMEOUT_MS;
  if ('send' in step) {
    await page.getByRole('textbox', { name: 'Message' }).click();
    await page.keyboard.type(step.send);
    await page.keyboard.press('Enter');
  } else if ('type' in step) {
    await page.getByRole('textbox', { name: 'Message' }).click();
    await page.keyboard.type(step.type);
  } else if ('key' in step) {
    await page.keyboard.press(step.key);
  } else if ('click' in step) {
    await page.locator(step.click).first().click({ timeout });
  } else if ('clickText' in step) {
    await page.getByText(step.clickText, { exact: false }).first().click({ timeout });
  } else if ('hover' in step) {
    await page.locator(step.hover).first().hover();
  } else if ('waitText' in step) {
    await page.getByText(step.waitText, { exact: false }).first().waitFor({ state: 'visible', timeout });
  } else if ('waitFor' in step) {
    await page.locator(step.waitFor).first().waitFor({ state: 'visible', timeout });
  } else if ('waitMs' in step) {
    await page.waitForTimeout(step.waitMs);
  } else if ('release' in step) {
    // `wait_for_release` in the fixture's scenarios looks for this file.
    writeFileSync(join(codexHome, 'release-inputs'), '');
  }
}

async function until<T>(read: () => T | undefined, ms: number, failure: string): Promise<T> {
  const deadline = Date.now() + ms;
  for (;;) {
    const value = read();
    if (value !== undefined) return value;
    if (Date.now() > deadline) throw new Error(failure);
    await new Promise(resolve => setTimeout(resolve, 200));
  }
}

function exited(child: ChildProcess) {
  return child.exitCode !== null ? Promise.resolve() : new Promise<void>(resolve => {
    const timer = setTimeout(() => { try { process.kill(-child.pid!, 'SIGKILL'); } catch { /* gone */ } resolve(); }, 10_000);
    child.once('exit', () => { clearTimeout(timer); resolve(); });
  });
}

// ── The Codex relay ──────────────────────────────────────────────────────────

type Codec = unknown;
type Meta = Record<'SERVER_REQUEST_PARAMS' | 'SERVER_NOTIFICATION_PARAMS' | 'CLIENT_REQUEST_RESPONSES', Record<string, Codec>>;
type SchemaModule = { decodeUnknownSync(schema: Codec): (value: unknown, options: object) => unknown };
type Shape = { literals?: unknown[]; literal?: unknown; members?: Shape[]; fields?: Record<string, Shape>; ast?: { _tag?: string; context?: { isOptional?: boolean }; thunk?: () => Shape } };

async function codexShim(fixture: string, log: string, rest: string[]) {
  const [t3Root, ...args] = rest;
  const generated = join(t3Root!, 'packages/effect-codex-app-server/src/_generated');
  const meta = (await import(pathToFileURL(join(generated, 'meta.gen.ts')).href)) as Meta;
  const named = (await import(pathToFileURL(join(generated, 'schema.gen.ts')).href)) as Record<string, Shape>;
  const schema = (await import(pathToFileURL(createRequire(join(generated, 'meta.gen.ts')).resolve('effect/Schema')).href)) as SchemaModule;
  const child = spawn(fixture, args, { stdio: ['pipe', 'pipe', 'inherit'] });
  const methods = new Map<unknown, string>();
  const record = (direction: string, line: string) => appendFileSync(log, `${direction} ${line}\n`);
  createInterface({ input: process.stdin }).on('line', line => {
    record('>', line);
    try {
      const message = JSON.parse(line) as { id?: unknown; method?: string };
      if (message.id !== undefined && message.method) methods.set(message.id, message.method);
    } catch { /* not JSON: relayed as is */ }
    child.stdin.write(`${line}\n`);
  });
  createInterface({ input: child.stdout }).on('line', line => {
    let out = line;
    try {
      const message = JSON.parse(line) as { id?: unknown; method?: string; params?: unknown; result?: unknown };
      const note = (text: string) => record('!', text.replaceAll('\n', ' '));
      const fill = (codec: Codec | undefined, value: unknown) => codec ? conform(schema, named, codec, value, note) : value;
      if (message.method && message.id !== undefined) message.params = fill(meta.SERVER_REQUEST_PARAMS[message.method], message.params);
      else if (message.method) message.params = fill(meta.SERVER_NOTIFICATION_PARAMS[message.method], message.params);
      else if (message.result !== undefined) message.result = fill(meta.CLIENT_REQUEST_RESPONSES[methods.get(message.id) ?? ''], message.result);
      out = JSON.stringify(message);
    } catch (error) { record('!', String(error)); }
    record('<', out);
    process.stdout.write(`${out}\n`);
  });
  process.stdin.on('end', () => child.stdin.end());
  child.on('exit', code => process.exit(code ?? 0));
}

/** The issues of a decoding failure: each message line followed by `at [path]`. */
export function issues(message: string) {
  const lines = message.split('\n');
  const found: { text: string; path: (string | number)[] }[] = [];
  for (let index = 0; index < lines.length; index++) {
    const at = /^\s*at (\[.*\])$/.exec(lines[index + 1] ?? '');
    if (!at) continue;
    found.push({ text: lines[index]!.trim(), path: JSON.parse(`[${at[1]!.slice(1, -1).replaceAll('][', ',')}]`) as (string | number)[] });
    index++;
  }
  return found;
}

/** The simplest value a schema accepts: its first literal or member, its required fields. */
function defaultFor(shape: Shape | undefined, depth = 0): unknown {
  if (!shape || depth > 8) return undefined;
  if (Array.isArray(shape.literals)) return shape.literals[0];
  if ('literal' in shape) return shape.literal;
  if (Array.isArray(shape.members)) {
    const members = shape.members.filter(member => member.ast?._tag !== 'Null');
    return defaultFor(members[0] ?? shape.members[0], depth + 1);
  }
  if (shape.fields) {
    return Object.fromEntries(Object.entries(shape.fields)
      .filter(([, field]) => !field.ast?.context?.isOptional)
      .map(([key, field]) => [key, defaultFor(field, depth + 1)]));
  }
  const tag = shape.ast?._tag;
  if (tag === 'Suspend') return defaultFor(shape.ast?.thunk?.(), depth + 1) ?? {};
  return ({ String: '', Number: 0, Boolean: false, Arrays: [], Objects: {}, Null: null } as Record<string, unknown>)[tag ?? ''];
}

/** What to put where a decoding issue says a value is missing or of the wrong kind. */
export function replacement(text: string, current: unknown, named: Record<string, Shape>): unknown {
  if (text === 'Missing key') return '';
  const expected = /^Expected (.*?)(?:, got .*)?$/s.exec(text)?.[1];
  if (expected === undefined) return undefined;
  if (/^V2[A-Za-z_]+$/.test(expected)) return defaultFor(named[expected]);
  if (expected.startsWith('"')) return JSON.parse(/^"(?:[^"\\]|\\.)*"/.exec(expected)![0]) as unknown;
  if (/^(number|integer)/.test(expected)) return 0;
  if (expected.startsWith('boolean')) return false;
  if (/^(ReadonlyArray|Array|\[)|array/i.test(expected)) return [];
  if (expected.startsWith('string')) return current === '' ? 'x' : '';
  if (expected.startsWith('{') || expected.startsWith('object')) return {};
  if (/(^| )null/.test(expected)) return null;
  return undefined;
}

function conform(schema: SchemaModule, named: Record<string, Shape>, codec: Codec, value: unknown, note: (text: string) => void) {
  const decode = schema.decodeUnknownSync(codec);
  const attempts = new Map<string, number>();
  for (let round = 0; round < 400; round++) {
    try {
      decode(value, { errors: 'all' });
      return value;
    } catch (error) {
      let changed = false;
      for (const { text, path } of issues(String((error as Error).message))) {
        const key = JSON.stringify(path) + text;
        const parent = path.slice(0, -1).reduce<unknown>((node, part) => (node as Record<string | number, unknown> | undefined)?.[part], value);
        const last = path[path.length - 1];
        if (last === undefined || typeof parent !== 'object' || parent === null || (attempts.get(key) ?? 0) > 2) continue;
        const next = replacement(text, (parent as Record<string | number, unknown>)[last], named);
        if (next === undefined) continue;
        attempts.set(key, (attempts.get(key) ?? 0) + 1);
        (parent as Record<string | number, unknown>)[last] = next;
        changed = true;
      }
      if (!changed) {
        note(`unconformed: ${String((error as Error).message).slice(0, 600)}`);
        return value;
      }
    }
  }
  return value;
}

import test from 'node:test';
import assert from 'node:assert/strict';
import { chmodSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { spawnSync } from 'node:child_process';

const workflow = readFileSync(new URL('../.github/workflows/native.yml', import.meta.url), 'utf8');
// Exercise the actual workflow policy; keep its rules beside the job conditions.
const block = workflow.match(/      - id: areas[\s\S]*?        run: \|\n([\s\S]*?)(?=^  repository:)/m);
assert.ok(block, 'Native clients must define its changes selector');
const selector = block[1].split('\n').map(line => line.replace(/^          /, '')).join('\n');
const platforms = ['linux', 'windows', 'android', 'iphone', 'mac'];

function select(t, paths, { event = 'push', base = 'last-verified-main', queryFailure = false, fetchFailure = false, diffFailure = false } = {}) {
  const root = mkdtempSync(join(tmpdir(), 'native-ci-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const bin = join(root, 'bin');
  const output = join(root, 'output');
  const log = join(root, 'commands');
  mkdirSync(bin);
  writeFileSync(log, '');
  const commands = {
    git: [
      'printf "git %s\\n" "$*" >> "$NATIVE_CI_LOG"',
      'case "$1" in',
      '  rev-parse) echo pull-request-base ;;',
      '  fetch) exit "$NATIVE_CI_FETCH_FAILURE" ;;',
      '  diff)',
      '    if [[ $NATIVE_CI_DIFF_FAILURE != 0 ]]; then exit "$NATIVE_CI_DIFF_FAILURE"; fi',
      '    printf "%s\\n" "$NATIVE_CI_PATHS"',
      '    ;;',
      '  *) exit 1 ;;',
      'esac',
    ].join('\n'),
    gh: 'printf "gh %s\\n" "$*" >> "$NATIVE_CI_LOG"\nif [[ $NATIVE_CI_QUERY_FAILURE != 0 ]]; then exit "$NATIVE_CI_QUERY_FAILURE"; fi\nprintf "%s\\n" "$NATIVE_CI_BASE"',
  };
  for (const [name, script] of Object.entries(commands)) {
    const path = join(bin, name);
    writeFileSync(path, `#!/usr/bin/env bash\nset -eu\n${script}\n`);
    chmodSync(path, 0o755);
  }
  const result = spawnSync('bash', ['-euo', 'pipefail', '-c', selector], {
    encoding: 'utf8',
    env: {
      ...process.env, PATH: `${bin}:${process.env.PATH}`,
      GITHUB_EVENT_NAME: event, GITHUB_OUTPUT: output, GITHUB_REPOSITORY: 'fixture/repository',
      GITHUB_BEFORE: 'unverified-previous-push',
      NATIVE_CI_LOG: log, NATIVE_CI_PATHS: paths.join('\n'), NATIVE_CI_BASE: base,
      NATIVE_CI_QUERY_FAILURE: queryFailure ? '1' : '0',
      NATIVE_CI_FETCH_FAILURE: fetchFailure ? '1' : '0', NATIVE_CI_DIFF_FAILURE: diffFailure ? '1' : '0',
    },
  });
  assert.ifError(result.error);
  assert.equal(result.status, 0, result.stderr);
  const selected = Object.fromEntries(readFileSync(output, 'utf8').trim().split('\n').map(line => {
    const [name, value] = line.split('=');
    assert.ok(value === 'true' || value === 'false', line);
    return [name, value === 'true'];
  }));
  assert.deepEqual(Object.keys(selected).sort(), [...platforms].sort());
  return { selected: platforms.filter(platform => selected[platform]), commands: readFileSync(log, 'utf8') };
}

test('only affected native clients run while repository-only changes stay lightweight', async (t) => {
  const cases = [
    [[], []],
    [['README.md', 'docs/development.md'], []],
    [['cccc.toml', 'AGENTS.md', 'lp/download.test.cjs'], []],
    [['scripts/native-ci.test.mjs', 'tools/windows-fxc/update.test.ps1'], []],
    [['crates/agent-protocol/src/browser_input.test.cjs', 'crates/host-daemon/src/browser/password-fill.test.cjs', 'apps/desktop/src/browser/native/snapshot.test.cjs'], []],
    [['apps/mobile/src/main/kotlin/Conversation.kt'], ['android']],
    [['apps/mobile/build.gradle.kts', 'gradle.properties', 'scripts/android-e2e.sh', 'scripts/build-agent-android.mjs'], ['android']],
    [['apps/mobile/iosApp/Bex/ConversationView.swift'], ['iphone']],
    [['apps/mobile/iosApp/UITests/HistoryUITests.swift'], ['iphone']],
    [['.swiftformat', '.swiftlint.yml', 'scripts/build-agent-ios.sh'], ['iphone']],
    [['apps/mobile/iosApp/Bex/ConversationScrollObserver.swift'], ['iphone', 'mac']],
    [['apps/mobile/iosApp/Bex/ConversationTextLayout.swift'], ['iphone', 'mac']],
    [['apps/mobile/iosApp/Bex/ConversationTextLayout.swift', 'apps/mobile/iosApp/Bex/RenamedTextLayout.swift'], ['iphone', 'mac']],
    [['apps/mobile/iosApp/UITests/Fixtures/scroll-tests.swift', 'apps/mobile/iosApp/UITests/Fixtures/text-view-tests.swift'], ['iphone', 'mac']],
    [['scripts/test-ios-scroll.sh'], ['mac']],
    [['apps/desktop/src/app.rs'], ['linux', 'windows', 'mac']],
    [['apps/desktop/native-focus/src/lib.rs'], ['linux', 'windows', 'mac']],
    [['tools/windows-fxc/prepare.ps1', 'tools/windows-fxc/verify.ps1'], ['windows']],
    [['scripts/package-windows.ps1', 'scripts/windows-installer.iss'], ['windows']],
    [['apps/mobile/src/main/kotlin/Conversation.kt', 'apps/mobile/iosApp/Bex/ConversationView.swift'], ['android', 'iphone']],
  ];
  for (const [paths, expected] of cases) {
    await t.test(paths.join(', ') || 'no changes', (t) => {
      assert.deepEqual(select(t, paths).selected, expected);
    });
  }
});

test('shared Rust, build contracts, privacy and unknown paths verify every platform', async (t) => {
  for (const path of [
    'crates/agent-core/src/state.rs', 'crates/host-daemon/src/lib.rs', 'crates/xtask/src/main.rs',
    'Cargo.lock', 'flake.nix', '.github/workflows/native.yml', 'docs/PRIVACY.md', 'new-client/source',
  ]) {
    await t.test(path, (t) => assert.deepEqual(select(t, [path]).selected, platforms));
  }
});

test('pushes include unverified earlier changes rather than only the latest push', (t) => {
  const result = select(t, ['README.md', 'crates/agent-protocol/src/lib.rs']);
  assert.deepEqual(result.selected, platforms);
  assert.match(result.commands, /gh run list .*--status success/);
  assert.match(result.commands, /git diff --no-renames --name-only last-verified-main HEAD/);
  assert.ok(!result.commands.includes('unverified-previous-push'));
});

test('pull requests compare against the checked-out merge base without consulting main CI', (t) => {
  const result = select(t, ['apps/mobile/src/Conversation.kt'], { event: 'pull_request' });
  assert.deepEqual(result.selected, ['android']);
  assert.match(result.commands, /git rev-parse HEAD\^1/);
  assert.match(result.commands, /git diff --no-renames --name-only pull-request-base HEAD/);
  assert.ok(!result.commands.includes('gh '));
});

test('manual runs and unavailable comparison data conservatively verify everything', async (t) => {
  for (const options of [
    { event: 'workflow_dispatch' }, { event: 'release' }, { base: '' }, { queryFailure: true }, { fetchFailure: true }, { diffFailure: true },
  ]) {
    await t.test(JSON.stringify(options), (t) => assert.deepEqual(select(t, ['README.md'], options).selected, platforms));
  }
});

test('Android acceptance counts include every test declared by its selected classes', () => {
  const script = readFileSync(new URL('./android-e2e.sh', import.meta.url), 'utf8');
  const groups = [...script.matchAll(/-e class ([\w.,]+)[\s\S]*?\| tee "\$log\.(\w+)\.log"[\s\S]*?grep -qx 'OK \((\d+) tests?\)'/g)];
  assert.equal(groups.length, 4);
  for (const [, classes, label, expected] of groups) {
    const count = classes.split(',').reduce((count, name) => {
      const source = readFileSync(new URL(`../apps/mobile/src/androidTest/kotlin/${name.replaceAll('.', '/')}.kt`, import.meta.url), 'utf8');
      return count + [...source.matchAll(/^\s*@Test\b/gm)].length;
    }, 0);
    assert.equal(Number(expected), count, `${label} acceptance count is out of date`);
  }
});

test('iPhone acceptance lists select existing Simulator tests', () => {
  const directory = new URL('../apps/mobile/iosApp/UITests/', import.meta.url);
  const declarations = new Set(readdirSync(directory)
    .filter(name => name.endsWith('.swift'))
    .flatMap(name => [...readFileSync(new URL(name, directory), 'utf8')
      .matchAll(/^\s*func\s+(testSimulator\w+)\s*\(/gm)].map(match => match[1])));
  assert.ok(declarations.size > 0, 'Simulator test declarations must be discovered');
  for (const path of ['../justfile', './ios-e2e.sh']) {
    const source = readFileSync(new URL(path, import.meta.url), 'utf8');
    const selected = new Set(source.match(/\btestSimulator\w+\b/g));
    assert.ok(selected.size > 0, `${path} must select Simulator tests`);
    for (const name of selected) {
      assert.ok(declarations.has(name), `${path} selects missing Simulator test: ${name}`);
    }
  }
});

test('focused iPhone runs reach acceptance even though repository checks are skipped', () => {
  // A job without `if` is skipped whenever any upstream job was skipped.
  const job = workflow.match(/^ {2}iphone-tests:\n((?: {4}.*\n|\n)+)/m)?.[1];
  assert.ok(job, 'iPhone acceptance job must exist');
  assert.match(job, /^ {4}if: \$\{\{ !cancelled\(\) && needs\.iphone\.result == 'success' \}\}$/m);
});

import test from 'node:test';
import assert from 'node:assert/strict';
import { chmodSync, copyFileSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, symlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { spawnSync } from 'node:child_process';

test('native acceptance selectors name implemented UI tests', () => {
  const root = 'apps/mobile/iosApp/UITests';
  const implemented = new Set(readdirSync(root).filter(name => name.endsWith('.swift')).flatMap(name =>
    [...readFileSync(join(root, name), 'utf8').matchAll(/^\s*func (testSimulator\w+)\(/gm)].map(match => match[1])));
  for (const path of ['justfile', 'scripts/ios-e2e.sh']) {
    const selected = [...readFileSync(path, 'utf8').matchAll(/\btestSimulator\w+/g)].map(match => match[0]);
    assert.ok(selected.length > 0, path);
    for (const name of selected) assert.ok(implemented.has(name), `${path}: missing ${name}`);
  }
});

function fixture(t) {
  const root = mkdtempSync(join(tmpdir(), 'native-validation-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const bin = join(root, 'bin');
  const log = join(root, 'commands.log');
  mkdirSync(bin);
  mkdirSync(join(root, 'scripts'));
  for (const script of ['quality.sh', 'ios-e2e.sh']) copyFileSync(`scripts/${script}`, join(root, 'scripts', script));
  const remote = join(root, 'scripts/remote-run.sh');
  writeFileSync(remote, '#!/bin/bash\nprintf \'remote-run %s\\n\' "$*" >> "$NATIVE_VALIDATION_LOG"\nif [[ $1 == --native ]]; then shift; fi\nshift\nexec "$@"\n');
  chmodSync(remote, 0o755);
  writeFileSync(log, '');
  const command = join(bin, 'command');
  writeFileSync(command, `#!/bin/bash
name=\${0##*/}
if [[ $name == uname ]]; then
  if [[ $1 == -s ]]; then echo Darwin; else echo arm64; fi
  exit 0
fi
printf '%s %s\\n' "$name" "$*" >> "$NATIVE_VALIDATION_LOG"
if [[ -n \${NATIVE_VALIDATION_FAIL:-} && "$name $*" == "$NATIVE_VALIDATION_FAIL"* ]]; then exit 17; fi
`);
  chmodSync(command, 0o755);
  for (const name of ['swiftformat', 'swiftlint', 'actionlint', 'nix', 'cargo', 'just', 'uname']) {
    symlinkSync(command, join(bin, name));
  }
  const target = join(root, 'target');
  mkdirSync(join(target, 'debug'), { recursive: true });
  mkdirSync(join(target, 'qa'), { recursive: true });
  symlinkSync(command, join(target, 'debug', 'xtask'));
  symlinkSync(command, join(target, 'qa', 'xtask'));
  return {
    run(script, args, failure = '', overrides = {}) {
      const result = spawnSync('/bin/bash', [join(root, script), ...args], {
        cwd: root,
        encoding: 'utf8',
        env: { ...process.env, PATH: `${bin}:${process.env.PATH}`, NATIVE_VALIDATION_LOG: log, NATIVE_VALIDATION_FAIL: failure, AGENT_REMOTE_BUILDER: '', ...overrides },
      });
      assert.ifError(result.error);
      return { ...result, commands: readFileSync(log, 'utf8').trim().split('\n').filter(Boolean) };
    },
  };
}

test('Swift formatting and lint failures stop validation', async (t) => {
  for (const failure of ['swiftformat', 'swiftlint']) {
    await t.test(failure, (t) => {
      const result = fixture(t).run('scripts/quality.sh', ['swift'], failure);
      assert.equal(result.status, 17, result.stderr);
      assert.equal(result.commands.length, failure === 'swiftformat' ? 1 : 2);
      assert.ok(result.commands.every(command => command.startsWith('swift')));
    });
  }
});

test('Rust formatting and contract failures stop validation', async (t) => {
  for (const [failure, lastCommand] of [
    ['cargo fmt', 'cargo fmt --all --check'],
    ['cargo clippy', 'cargo clippy --locked --workspace --all-targets -- --no-deps -D warnings'],
    ['just unit-tests', null],
  ]) {
    await t.test(failure, (t) => {
      const result = fixture(t).run('scripts/quality.sh', ['rust'], failure);
      assert.equal(result.status, 17, result.stderr);
      assert.ok(!result.commands.includes('just macos-e2e'));
      if (lastCommand) assert.equal(result.commands.at(-1), lastCommand);
      if (failure === 'cargo fmt') assert.ok(!result.commands.some(command => command.startsWith('nix ')));
      if (failure === 'just unit-tests') assert.ok(result.commands.at(-1).startsWith('just unit-tests --test errors'));
    });
  }
});

test('Rust quality validates contracts without native builds, acceptance or cleanup', (t) => {
  const result = fixture(t).run('scripts/quality.sh', ['rust']);
  assert.equal(result.status, 0, result.stderr);
  assert.ok(result.commands.includes('cargo clippy --locked --workspace --all-targets -- --no-deps -D warnings'));
  assert.ok(!result.commands.some(command => command.startsWith('remote-run ')));
  assert.ok(result.commands.at(-1).startsWith('just unit-tests '));
  assert.ok(!result.commands.some(command => /macos-e2e|conversation-ui/.test(command)));
});

test('Swift quality checks sources without compiling or starting a Simulator', (t) => {
  const result = fixture(t).run('scripts/quality.sh', ['swift']);
  assert.equal(result.status, 0, result.stderr);
  assert.deepEqual(result.commands, [
    'swiftformat --lint apps/mobile/iosApp/Bex apps/mobile/iosApp/UITests apps/mobile/iosApp/TaskActivity',
    'swiftlint lint --strict',
  ]);
});

test('Apple quality runs Swift checks locally and delegates Rust verification as one remote command', async (t) => {
  for (const failure of ['', 'just quality rust']) {
    await t.test(failure || 'success', (t) => {
      const result = fixture(t).run('scripts/quality.sh', [], failure, { AGENT_REMOTE_BUILDER: 'validation-builder' });
      assert.equal(result.status, failure ? 17 : 0, result.stderr);
      assert.deepEqual(result.commands.slice(2), [
        'remote-run --native validation-builder just quality rust',
        'just quality rust',
      ]);
      assert.ok(result.commands.slice(0, 2).every(command => command.startsWith('swift')));
    });
  }
});

test('quality rejects acceptance options before doing work', (t) => {
  const result = fixture(t).run('scripts/quality.sh', ['swift', '--build-only']);
  assert.equal(result.status, 2, result.stderr);
  assert.deepEqual(result.commands, []);
});

test('prebuilt acceptance executes the shipped runner without invoking Cargo', (t) => {
  const result = fixture(t).run('scripts/ios-e2e.sh', ['--prebuilt', '--without-codex', 'testSimulatorExample']);
  assert.equal(result.status, 0, result.stderr);
  assert.deepEqual(result.commands, ['xtask ios-e2e --prebuilt --without-codex testSimulatorExample']);
});

test('overlapped acceptance executes the early runner without invoking Cargo', (t) => {
  const result = fixture(t).run('scripts/ios-e2e.sh', ['--await-build', 'testSimulatorExample']);
  assert.equal(result.status, 0, result.stderr);
  assert.deepEqual(result.commands, ['xtask ios-e2e --await-build testSimulatorExample']);
});

test('build-only mode forwards the requested tests without starting acceptance', (t) => {
  const result = fixture(t).run('scripts/ios-e2e.sh', ['--build-only', 'testSimulatorExample']);
  assert.equal(result.status, 0, result.stderr);
  assert.deepEqual(result.commands, ['cargo xtask ios-e2e --build-only testSimulatorExample']);
});

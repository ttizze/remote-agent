import test from 'node:test';
import assert from 'node:assert/strict';
import { chmodSync, copyFileSync, existsSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { spawnSync } from 'node:child_process';

function put(root, path, content) {
  const destination = join(root, path);
  mkdirSync(join(destination, '..'), { recursive: true });
  writeFileSync(destination, content);
  return destination;
}

function fixture(t, builder = '') {
  const root = realpathSync(mkdtempSync(join(tmpdir(), 'dev-env-')));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const git = (...args) => {
    const result = spawnSync('git', args, { cwd: root, encoding: 'utf8' });
    assert.equal(result.status, 0, result.stderr);
  };
  git('init', '--quiet');
  if (builder) git('config', 'bex.remoteBuilder', builder);
  for (const path of [
    'flake.nix', 'flake.lock', 'tools/github-release-binary.nix', 'tools/cccc/package.nix',
    'tools/cargo/package.nix', 'tools/node-runtime/package.nix',
  ]) put(root, path, '');
  copyFileSync('justfile', join(root, 'justfile'));
  put(root, 'scripts/remote-run.sh', '#!/usr/bin/env bash\nprintf "%s\\n" "$@"\nexit "${DEV_ENV_REMOTE_STATUS:-0}"\n');
  chmodSync(join(root, 'scripts/remote-run.sh'), 0o755);
  copyFileSync('scripts/dev-env.sh', join(root, 'scripts/dev-env.sh'));
  const nixLog = join(root, 'nix.log');
  const nix = put(root, 'bin/nix', `#!/usr/bin/env bash
printf '%s\\n' "$*" >> "$DEV_ENV_NIX_LOG"
for argument; do
  case $argument in .|.\\#native) printf 'export DEV_ENV_SELECTED_SHELL=%q\\n' "$argument" ;; esac
done
printf 'export NIX_BUILD_TOP=$(mktemp -d)\\n'
`);
  chmodSync(nix, 0o755);
  const cargo = put(root, 'bin/cargo', '#!/usr/bin/env bash\necho local-cargo\n');
  chmodSync(cargo, 0o755);
  const archive = 'registry/cache/index.crates.io-fixture/archived-1.0.0.crate';
  put(root, `seed/${archive}`, 'archive');
  return {
    root, archive, nixLog, sharedHome: join(root, 'seed'),
    home: join(root, 'target/cargo-home'),
    run(args = ['true'], overrides = {}) {
      const result = spawnSync('bash', ['scripts/dev-env.sh', ...args], {
        cwd: root, encoding: 'utf8',
        env: { ...process.env, PATH: `${join(root, 'bin')}:${process.env.PATH}`, CARGO_HOME: join(root, 'seed'), DEV_ENV_NIX_LOG: nixLog, ...overrides },
      });
      assert.ifError(result.error);
      return result;
    },
  };
}

test('local commands reuse Cargo-owned dependency storage without copying it into the worktree', (t) => {
  const build = fixture(t);
  const result = build.run(['bash', '-c', 'printf "%s" "$CARGO_HOME"']);
  assert.equal(result.status, 0, result.stderr);
  assert.equal(result.stdout, build.sharedHome);
  assert.equal(readFileSync(join(build.sharedHome, build.archive), 'utf8'), 'archive');
  assert.ok(!existsSync(build.home));
});

test('the build volume owns shared Cargo storage unless CARGO_HOME is explicit', t => {
  const build = fixture(t);
  const volume = join(build.root, 'build volume');
  mkdirSync(volume);
  spawnSync('git', ['config', 'bex.buildRoot', volume], { cwd: build.root });
  const command = ['bash', '-c', 'printf "%s" "$CARGO_HOME"'];
  const result = build.run(command, { CARGO_HOME: '' });
  assert.equal(result.status, 0, result.stderr);
  assert.equal(result.stdout, join(volume, '.cargo'));
  assert.equal(build.run(command).stdout, build.sharedHome);
});

test('an unavailable build volume fails before selecting tools or creating outputs', t => {
  const build = fixture(t);
  spawnSync('git', ['config', 'bex.buildRoot', join(build.root, 'unavailable')], { cwd: build.root });
  const result = build.run();
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /Build storage is unavailable/);
  assert.ok(!existsSync(build.nixLog));
  assert.ok(!existsSync(join(build.root, 'target')));
});

test('test launchers supply public runtime resources without changing ordinary Cargo commands', t => {
  const build = fixture(t);
  put(build.root, 'bin/cargo', '#!/usr/bin/env bash\nprintf "%s" "${AGENT_RUNTIME_DIRECTORY-}"\n');
  for (const args of [['cargo', 'test'], ['cargo', 'nextest', 'run'], ['env', '-u', 'UNUSED', 'cargo', 'test']]) {
    const result = build.run(args, { AGENT_RUNTIME_DIRECTORY: '' });
    assert.equal(result.status, 0, result.stderr);
    assert.equal(result.stdout, join(build.root, 'target/debug'));
    assert.equal(build.run(args, { AGENT_RUNTIME_DIRECTORY: build.root }).stdout, build.root);
  }
  for (const command of ['build', 'run']) {
    const result = build.run(['cargo', command], { AGENT_RUNTIME_DIRECTORY: '' });
    assert.equal(result.status, 0, result.stderr);
    assert.equal(result.stdout, '');
  }
});

test('portable builds and verification reach the builder and preserve failures', async (t) => {
  for (const args of [
    ['cargo', 'check', '--locked'],
    ['cargo', 'build', '--locked', '-p', 'agent-core'],
    ['cargo', 'rustc', '--lib'],
    ['cargo', 'run', '-p', 'agent-cli', '--', '--target=aarch64-apple-ios'],
    ['env', '-u', 'CARGO_TARGET_DIR', 'CARGO_INCREMENTAL=0', 'cargo', 'test'],
    ['just', 'check'],
    ['just', 'unit-tests'],
  ]) {
    await t.test(args.join(' '), (t) => {
      const build = fixture(t, 'test-builder');
      const result = build.run(args, { DEV_ENV_REMOTE_STATUS: '23' });
      assert.equal(result.status, 23, result.stderr);
      assert.deepEqual(result.stdout.trim().split('\n'), ['--native', 'test-builder', ...args]);
      assert.ok(!existsSync(build.home));
    });
  }
});

test('native and mobile environments are cached independently and reused', (t) => {
  const build = fixture(t);
  for (const options of [[], ['--native'], [], ['--native']]) {
    const result = build.run([...options, 'bash', '-c', 'printf "%s" "$DEV_ENV_SELECTED_SHELL"']);
    assert.equal(result.status, 0, result.stderr);
    assert.equal(result.stdout, options.length ? '.#native' : '.');
  }
  assert.equal(readFileSync(build.nixLog, 'utf8').trim().split('\n').length, 2);
});

test('explicit Apple builds and checks run locally', async (t) => {
  for (const command of ['check', 'build', 'rustc', 'run']) {
    for (const target of [['--target', 'aarch64-apple-darwin'], ['--target=aarch64-apple-ios']]) {
      await t.test([command, ...target].join(' '), (t) => {
        const build = fixture(t, 'test-builder');
        const result = build.run(['cargo', command, ...target]);
        assert.equal(result.status, 0, result.stderr);
        assert.equal(result.stdout.trim(), 'local-cargo');
        assert.ok(!existsSync(build.home));
      });
    }
  }
});

import test from 'node:test';
import assert from 'node:assert/strict';
import { chmodSync, copyFileSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, statSync, symlinkSync, utimesSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { spawnSync } from 'node:child_process';

function fixture(t) {
  const root = mkdtempSync(join(tmpdir(), 'agent-bindings-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const bin = join(root, 'bin');
  const target = join(root, 'target');
  const log = join(root, 'commands.jsonl');
  mkdirSync(bin);
  mkdirSync(join(root, 'scripts'));
  for (const script of ['build-agent-bindings.sh', 'build-agent-ios.sh']) {
    copyFileSync(`scripts/${script}`, join(root, 'scripts', script));
  }
  mkdirSync(join(root, 'apps/nsh'), { recursive: true });
  writeFileSync(join(root, 'apps/nsh/build-native.sh'), 'exec native-build "$@"\n');
  mkdirSync(join(root, 'crates/agent-core'), { recursive: true });
  copyFileSync('crates/agent-core/uniffi.toml', join(root, 'crates/agent-core/uniffi.toml'));
  const command = join(bin, 'command');
  writeFileSync(command, `#!${process.execPath}
const fs = require('node:fs');
const path = require('node:path');
const name = path.basename(process.argv[1]);
const args = process.argv.slice(2);
const target = process.env.BINDINGS_TARGET;
fs.appendFileSync(process.env.BINDINGS_LOG, JSON.stringify({ name, args }) + '\\n');
const write = (file, content = '') => { fs.mkdirSync(path.dirname(file), { recursive: true }); fs.writeFileSync(file, content); };
if (name === 'native-build' && process.env.BINDINGS_NATIVE_FAIL) process.exit(22);
if (name === 'cargo') {
  if (args[0] === 'metadata') console.log(JSON.stringify({ target_directory: target }));
  if (args[0] === 'build' || args[0] === 'rustc') {
    if (args.includes('--target')) write(path.join(target, args[args.indexOf('--target') + 1], 'release/libagent_ffi.a'));
    else write(path.join(target, 'debug/libagent_core.rlib'));
  }
  if (args[0] === 'run') {
    if (process.env.BINDINGS_FAIL) process.exit(19);
    const [library, config, output] = args.slice(args.indexOf('--') + 1);
    if (!fs.existsSync(library)) process.exit(20);
    if (!fs.existsSync(config)) process.exit(21);
    for (const file of ['AgentCore.swift', 'AgentCoreFFI.h', 'AgentCoreFFI.modulemap', 'dev/remoteagent/core/agent_core.kt']) {
      write(path.join(output, file), process.env.BINDINGS_SCHEMA || 'schema');
    }
  }
}
if (name === 'xcrun') {
  if (args.includes('--version')) console.log('Swift fixture');
  else if (args.includes('--show-sdk-build-version')) console.log('SDK fixture');
  else if (args.includes('--show-sdk-path')) console.log('/SDK');
  else {
    write(args[args.indexOf('-o') + 1]);
    const module = args[args.indexOf('-emit-module-path') + 1];
    write(module); write(module.replace(/swiftmodule$/, 'swiftdoc'));
  }
}
`);
  chmodSync(command, 0o755);
  for (const name of ['cargo', 'xcrun', 'native-build']) symlinkSync(command, join(bin, name));
  return {
    root, target,
    run(script = 'scripts/build-agent-ios.sh', args = ['device'], overrides = {}) {
      writeFileSync(log, '');
      const result = spawnSync('bash', [script, ...args], {
        cwd: root, encoding: 'utf8',
        env: { ...process.env, PATH: `${bin}:${process.env.PATH}`, BINDINGS_TARGET: target, BINDINGS_LOG: log, ...overrides },
      });
      assert.ifError(result.error);
      const commands = readFileSync(log, 'utf8').trim().split('\n').filter(Boolean).map(line => JSON.parse(line));
      return { ...result, commands };
    },
  };
}

test('iOS uses its target archive and reuses Swift outputs across invocation paths', async (t) => {
  for (const [platform, triple] of [['device', 'aarch64-apple-ios'], ['simulator', 'aarch64-apple-ios-sim']]) {
    await t.test(platform, (t) => {
      const build = fixture(t);
      const output = join(build.target, triple, 'release');
      const first = build.run('scripts/build-agent-ios.sh', [platform]);
      assert.equal(first.status, 0, first.stderr);
      assert.deepEqual(first.commands.find(command => command.name === 'native-build').args,
        [platform === 'device' ? 'ios' : 'ios-simulator', 'release']);
      const builds = first.commands.filter(command => command.name === 'cargo' && ['build', 'rustc'].includes(command.args[0]));
      assert.equal(builds.length, 1);
      assert.ok(builds[0].args.includes(triple));
      assert.equal(builds[0].args[builds[0].args.indexOf('--crate-type') + 1], 'staticlib');
      const generation = first.commands.find(command => command.name === 'cargo' && command.args[0] === 'run');
      assert.equal(generation.args[generation.args.indexOf('--') + 1], join(output, 'libagent_ffi.a'));
      const source = join(build.target, 'agent-bindings/AgentCore.swift');
      utimesSync(source, 100, 100);
      const repeated = build.run(join(build.root, 'scripts/build-agent-ios.sh'), [platform]);
      assert.equal(repeated.status, 0, repeated.stderr);
      assert.equal(statSync(source).mtimeMs, 100000);
      assert.ok(!repeated.commands.some(command => command.name === 'xcrun' && command.args.includes('-emit-library')));
      const retired = join(build.target, 'agent-bindings/retired.swift');
      writeFileSync(retired, 'retired');
      const changed = build.run('scripts/build-agent-ios.sh', [platform], { BINDINGS_SCHEMA: 'changed schema' });
      assert.equal(changed.status, 0, changed.stderr);
      assert.ok(changed.commands.some(command => command.name === 'xcrun' && command.args.includes('-emit-library')));
      assert.ok(!existsSync(retired));
    });
  }
});

test('a failed native framework build stops before replacing the Rust archive or bindings', (t) => {
  const build = fixture(t);
  const result = build.run('scripts/build-agent-ios.sh', ['device'], { BINDINGS_NATIVE_FAIL: '1' });
  assert.equal(result.status, 22);
  assert.deepEqual(result.commands.map(command => command.name), ['native-build']);
  assert.ok(!existsSync(join(build.target, 'aarch64-apple-ios/release/libagent_ffi.a')));
});

test('standalone generation reads Core metadata without an FFI link and retains bindings after failure', (t) => {
  const build = fixture(t);
  const first = build.run('scripts/build-agent-bindings.sh', []);
  assert.equal(first.status, 0, first.stderr);
  const libraryBuild = first.commands.find(command => command.name === 'cargo' && command.args[0] === 'build');
  assert.deepEqual(libraryBuild.args, ['build', '--locked', '-p', 'agent-core', '--features', 'bindings', '--lib']);
  const generation = first.commands.find(command => command.name === 'cargo' && command.args[0] === 'run');
  assert.equal(generation.args[generation.args.indexOf('--') + 1], join(build.target, 'debug/libagent_core.rlib'));
  const failed = build.run('scripts/build-agent-bindings.sh', [], { BINDINGS_FAIL: '1' });
  assert.equal(failed.status, 19, failed.stderr);
  assert.equal(readFileSync(join(build.target, 'agent-bindings/AgentCore.swift'), 'utf8'), 'schema');
});

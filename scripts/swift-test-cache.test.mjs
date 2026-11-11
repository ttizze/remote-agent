import test from 'node:test';
import assert from 'node:assert/strict';
import { chmodSync, copyFileSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { spawnSync } from 'node:child_process';

function fixture(t) {
  const root = mkdtempSync(join(tmpdir(), 'swift-test-cache-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const bin = join(root, 'bin');
  const target = join(root, 'target');
  const log = join(root, 'commands.log');
  mkdirSync(bin);
  mkdirSync(join(root, 'scripts'));
  copyFileSync('scripts/test-ios-scroll.sh', join(root, 'scripts/test-ios-scroll.sh'));
  for (const path of [
    'apps/mobile/iosApp/Bex/NavigationAppearance.swift',
    'apps/mobile/iosApp/UITests/Fixtures/navigation-tests.swift',
    'apps/nsh/apple/Tests/Fixtures/message-layout-tests.swift',
    'apps/nsh/apple/Sources/BexNshNative/MessageAttributes.swift',
    'apps/nsh/apple/Sources/BexNshNative/MessageText.swift',
  ]) {
    const destination = join(root, path);
    mkdirSync(join(destination, '..'), { recursive: true });
    copyFileSync(path, destination);
  }
  const commands = {
    cargo: 'printf \'{"target_directory":"%s"}\\n\' "$SWIFT_FIXTURE_TARGET"',
    uname: 'echo "${SWIFT_FIXTURE_ARCH:-arm64}"',
    codesign: 'echo sign >> "$SWIFT_FIXTURE_LOG"\nexit "${SWIFT_FIXTURE_SIGN_STATUS:-0}"',
    xcrun: [
      'case "$*" in',
      '  "--sdk macosx --show-sdk-path") echo "$SWIFT_FIXTURE_SDK" ;;',
      '  "--sdk macosx --show-sdk-build-version") echo "${SWIFT_FIXTURE_SDK_BUILD:-26A}" ;;',
      '  "swiftc --version") echo "${SWIFT_FIXTURE_COMPILER:-Swift 6}" ;;',
      '  *)',
      '    echo compile >> "$SWIFT_FIXTURE_LOG"',
      '    if [[ ${SWIFT_FIXTURE_COMPILE_STATUS:-0} != 0 ]]; then exit "$SWIFT_FIXTURE_COMPILE_STATUS"; fi',
      '    while [[ $1 != -o ]]; do shift; done',
      '    printf \'#!/bin/bash\\necho run >> "$SWIFT_FIXTURE_LOG"\\nexit "${SWIFT_FIXTURE_TEST_STATUS:-0}"\\n\' > "$2"',
      '    chmod +x "$2"',
      '    ;;',
      'esac',
    ].join('\n'),
  };
  for (const [name, script] of Object.entries(commands)) {
    const path = join(bin, name);
    writeFileSync(path, `#!/bin/bash\n${script}\n`);
    chmodSync(path, 0o755);
  }
  return {
    root, target,
    run(overrides = {}) {
      writeFileSync(log, '');
      const result = spawnSync('/bin/bash', ['scripts/test-ios-scroll.sh'], {
        cwd: root,
        encoding: 'utf8',
        env: { ...process.env, PATH: `${bin}:${process.env.PATH}`, SWIFT_FIXTURE_TARGET: target, SWIFT_FIXTURE_LOG: log, SWIFT_FIXTURE_SDK: join(root, 'SDK'), ...overrides },
      });
      assert.ifError(result.error);
      return { status: result.status, stderr: result.stderr, commands: readFileSync(log, 'utf8').trim().split('\n').filter(Boolean) };
    },
  };
}

function successfulRun(build, compileCount, overrides) {
  const result = build.run(overrides);
  assert.equal(result.status, 0, result.stderr);
  assert.equal(result.commands.filter(command => command === 'compile').length, compileCount);
  assert.equal(result.commands.filter(command => command === 'run').length, 2);
}

test('unchanged Swift tests reuse compilation and always execute all fixtures', (t) => {
  const build = fixture(t);
  const directory = join(build.target, 'qa/ios-scroll/builds/navigation-tests');
  successfulRun(build, 2);
  successfulRun(build, 0);
  writeFileSync(join(build.root, 'apps/mobile/iosApp/Bex/NavigationAppearance.swift'), '// changed source\n');
  successfulRun(build, 1);
  const beforeFixtureChange = new Set(readdirSync(directory));
  writeFileSync(join(build.root, 'apps/mobile/iosApp/UITests/Fixtures/navigation-tests.swift'), '// changed test\n');
  successfulRun(build, 1);
  const currentBuild = readdirSync(directory).find(name => !beforeFixtureChange.has(name));
  writeFileSync(join(build.root, 'apps/nsh/apple/Sources/BexNshNative/MessageText.swift'), '// changed NSH layout\n');
  successfulRun(build, 1);
  for (const overrides of [
    { SWIFT_FIXTURE_COMPILER: 'Swift 7' },
    { SWIFT_FIXTURE_SDK_BUILD: '26B' },
    { SWIFT_FIXTURE_SDK: join(build.root, 'Other SDK') },
    { SWIFT_FIXTURE_ARCH: 'x86_64' },
  ]) successfulRun(build, 2, overrides);
  const app = join(directory, currentBuild, 'Contents');
  assert.match(readFileSync(join(app, 'Info.plist'), 'utf8'), /<key>CFBundleIdentifier<\/key><string>app\.bex\.tests\.navigation-tests<\/string>/);
  chmodSync(join(app, 'MacOS/navigation-tests'), 0o644);
  successfulRun(build, 1);
  successfulRun(build, 0);
  rmSync(join(app, 'Info.plist'));
  successfulRun(build, 1);
});

test('failed compilations are retried and cached test failures still fail verification', (t) => {
  const build = fixture(t);
  const compilation = build.run({ SWIFT_FIXTURE_COMPILE_STATUS: '1' });
  assert.equal(compilation.status, 1);
  assert.deepEqual(compilation.commands, ['compile']);
  const signing = build.run({ SWIFT_FIXTURE_SIGN_STATUS: '1' });
  assert.equal(signing.status, 1);
  assert.deepEqual(signing.commands, ['compile', 'sign']);
  successfulRun(build, 2);
  const failure = build.run({ SWIFT_FIXTURE_TEST_STATUS: '7' });
  assert.equal(failure.status, 7);
  assert.deepEqual(failure.commands, ['run']);
  successfulRun(build, 0);
});

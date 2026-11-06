import test from 'node:test';
import assert from 'node:assert/strict';
import { sourceDirectories, compilerEnvironment } from './rust-build-inputs.ts';

test('normal and build dependencies include transitive path patches through registry and git crates', () => {
  const packages = [
    ['app', null], ['registry', 'registry+index'], ['git', 'git+revision'], ['patch', null], ['build', null], ['dev', null],
  ].map(([name, source]) => ({id: name, name, source, manifest_path: `/source with spaces/${name}/Cargo.toml`}));
  const dependency = (pkg, kind) => ({pkg, dep_kinds: [{kind}]});
  const nodes = [
    {id: 'app', deps: [dependency('registry', null), dependency('build', 'build'), dependency('dev', 'dev')]},
    {id: 'registry', deps: [dependency('git', null)]},
    {id: 'git', deps: [dependency('patch', null)]},
    {id: 'patch', deps: [dependency('app', null)]},
    {id: 'build', deps: []}, {id: 'dev', deps: []},
  ];
  const before = structuredClone({packages, nodes});
  assert.deepEqual(sourceDirectories(packages, nodes, 'app'), [
    '/source with spaces/app', '/source with spaces/build', '/source with spaces/patch',
  ]);
  assert.deepEqual({packages, nodes}, before);
});

test('mixed dependency kinds remain production inputs and malformed graphs fail closed', () => {
  const packages = ['app', 'dep'].map(name => ({id: name, name, source: null, manifest_path: `/${name}/Cargo.toml`}));
  const nodes = [{id: 'app', deps: [{pkg: 'dep', dep_kinds: [{kind: 'dev'}, {kind: null}]}]}, {id: 'dep', deps: []}];
  assert.deepEqual(sourceDirectories(packages, nodes, 'app'), ['/app', '/dep']);
  assert.throws(() => sourceDirectories(packages, nodes, 'missing'), /Missing workspace package/);
  assert.throws(() => sourceDirectories(packages, nodes.slice(0, 1), 'app'), /Incomplete Cargo/);
  assert.throws(() => sourceDirectories(packages, [{id: 'app', deps: [{pkg: 'dep'}]}], 'app'), /Missing Cargo dependency kinds/);
});

test('compiler flags affect the key while credentials and output locations do not become cache inputs', () => {
  const environment = {
    RUSTFLAGS: '--cfg example', CARGO_PROFILE_RELEASE_LTO: 'thin', CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER: 'linker',
    CARGO_NDK_PLATFORM: '26', CARGO_INCREMENTAL: '0', HOST_CFLAGS: '-DHOST_BUILD',
    CC_x86_64_unknown_linux_gnu: 'host-cc', CFLAGS_aarch64_linux_android: '-O2',
    CARGO_TARGET_DIR: '/outputs', GH_TOKEN: 'fixture-value', ARTIFACT_TOKEN: 'fixture-value',
  };
  assert.deepEqual(compilerEnvironment(environment, 'x86_64-unknown-linux-gnu'), {
    CARGO_NDK_PLATFORM: '26', CARGO_INCREMENTAL: '0', HOST_CFLAGS: '-DHOST_BUILD',
    CARGO_PROFILE_RELEASE_LTO: 'thin', CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER: 'linker',
    CC_x86_64_unknown_linux_gnu: 'host-cc', CFLAGS_aarch64_linux_android: '-O2', RUSTFLAGS: '--cfg example',
  });
});

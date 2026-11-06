import test from 'node:test';
import assert from 'node:assert/strict';
import { replacedBuildCaches, replacedDependencyCaches } from './prune-build-cache.ts';

const prefix = 'bex-gradle-v1-Linux-X64-toolchain-';
const current = prefix + 'a'.repeat(40);
const previous = prefix + 'b'.repeat(40);
const entries = [
  {id: 1, key: current, ref: 'refs/heads/main'},
  {id: 2, key: previous, ref: 'refs/heads/main'},
  {id: 3, key: previous, ref: 'refs/pull/10/merge'},
  {id: 4, key: 'unrelated-' + 'b'.repeat(40), ref: 'refs/heads/main'},
  {id: 5, key: 'bex-gradle-v1-Linux-X64-other-' + 'b'.repeat(40), ref: 'refs/heads/main'},
];

test('only replaced snapshots from the same main toolchain are retired', () => {
  const before = structuredClone(entries);
  assert.deepEqual(replacedBuildCaches(entries, current), [2]);
  assert.deepEqual(entries, before);
});

test('an unconfirmed upload preserves every existing snapshot', () => {
  assert.deepEqual(replacedBuildCaches(entries.filter(entry => entry.id !== 1), current), []);
  assert.deepEqual(replacedBuildCaches(entries, prefix + 'c'.repeat(40)), []);
  assert.throws(() => replacedBuildCaches(entries, 'other-cache'), /Invalid build snapshot key/);
});

test('iPhone snapshots retire only older builds for their Xcode version', () => {
  const key = 'ios-swift-xcode-26.6-' + 'a'.repeat(64);
  const snapshots = [
    {id: 1, key, ref: 'refs/heads/main'},
    {id: 2, key: 'ios-swift-xcode-26.6-' + 'b'.repeat(64), ref: 'refs/heads/main'},
    {id: 3, key: 'ios-swift-xcode-26.5-' + 'b'.repeat(64), ref: 'refs/heads/main'},
    {id: 4, key: 'ios-swift-xcode-26.6-' + 'b'.repeat(64), ref: 'refs/pull/10/merge'},
    ...entries,
  ];
  assert.deepEqual(replacedBuildCaches(snapshots, key), [2]);
  assert.deepEqual(replacedBuildCaches(snapshots.filter(entry => entry.id !== 1), key), []);
});

test('iPhone release snapshots retire only older releases for their Xcode version', () => {
  const key = 'ios-release-xcode-26.6-' + 'a'.repeat(40);
  const snapshots = [
    {id: 1, key, ref: 'refs/heads/main'},
    {id: 2, key: 'ios-release-xcode-26.6-' + 'b'.repeat(40), ref: 'refs/heads/main'},
    {id: 3, key: 'ios-release-xcode-26.5-' + 'b'.repeat(40), ref: 'refs/heads/main'},
    {id: 4, key: 'ios-swift-xcode-26.6-' + 'b'.repeat(64), ref: 'refs/heads/main'},
    ...entries,
  ];
  assert.deepEqual(replacedBuildCaches(snapshots, key), [2]);
  assert.deepEqual(replacedBuildCaches(snapshots.filter(entry => entry.id !== 1), key), []);
});

test('repository compiler snapshots preserve other jobs, toolchains and refs', () => {
  const prefix = 'local-rust-macOS-ARM64-xcode-26.6-' + 'a'.repeat(64) + '-iphone-';
  const current = prefix + 'b'.repeat(40);
  const previous = prefix + 'c'.repeat(40);
  const snapshots = [
    {id: 10, key: current, ref: 'refs/heads/main'},
    {id: 11, key: previous, ref: 'refs/heads/main'},
    {id: 12, key: previous.replace('-iphone-', '-mac-'), ref: 'refs/heads/main'},
    {id: 13, key: previous.replace('26.6', '26.5'), ref: 'refs/heads/main'},
    {id: 14, key: previous.replace('a'.repeat(64), 'd'.repeat(64)), ref: 'refs/heads/main'},
    {id: 15, key: previous, ref: 'refs/pull/10/merge'},
    ...entries,
  ];
  assert.deepEqual(replacedBuildCaches(snapshots, current), [11]);
  assert.deepEqual(replacedBuildCaches(snapshots.filter(entry => entry.id !== 10), current), []);
});

test('dependency caches keep the newest lockfile per job and environment on main', () => {
  const iphone = 'v0-rust-macos-swift-xcode-26.6-' + 'a'.repeat(64) + '-iphone-Darwin-arm64-86122005-';
  const windows = 'v0-rust-windows-Windows_NT-x64-2db6d1a2-';
  const caches = [
    {id: 1, key: iphone + '062f81dc', ref: 'refs/heads/main', createdAt: '2026-11-04T01:00:00Z'},
    {id: 2, key: iphone + '1e8cce06', ref: 'refs/heads/main', createdAt: '2026-11-04T03:00:00Z'},
    {id: 3, key: iphone + '9c0ffee0', ref: 'refs/heads/main', createdAt: '2026-11-03T03:00:00Z'},
    {id: 4, key: iphone.replace('86122005', 'deadbeef') + '062f81dc', ref: 'refs/heads/main', createdAt: '2026-11-01T00:00:00Z'},
    {id: 5, key: iphone + '00000000', ref: 'refs/pull/10/merge', createdAt: '2026-11-05T00:00:00Z'},
    {id: 6, key: windows + '1e8cce06', ref: 'refs/heads/main', createdAt: '2026-11-04T03:00:00Z'},
    {id: 7, key: windows + '062f81dc', ref: 'refs/heads/main', createdAt: '2026-11-04T01:00:00Z'},
    {id: 8, key: 'ios-swift-xcode-26.6-' + 'b'.repeat(64), ref: 'refs/heads/main', createdAt: '2026-10-01T00:00:00Z'},
  ];
  const before = structuredClone(caches);
  assert.deepEqual(replacedDependencyCaches(caches).sort(), [1, 3, 7]);
  assert.deepEqual(caches, before);
  assert.deepEqual(replacedDependencyCaches([]), []);
});

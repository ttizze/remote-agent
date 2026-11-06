import assert from 'node:assert/strict';
import test from 'node:test';
import { mergeDecision } from './dependabot-merge.ts';

const head = 'da896023d1b4ca723efa29279f5c969cc61e32d4';
const metadata = entries => [
  'Bumps dependencies.',
  '',
  '---',
  'updated-dependencies:',
  ...entries.flatMap(({ name, version, type }) => [
    `- dependency-name: ${name.startsWith('@') ? `"${name}"` : name}`,
    ...(version === undefined ? [] : [`  dependency-version: ${version}`]),
    '  dependency-type: direct:production',
    ...(type ? [`  update-type: version-update:semver-${type}`] : []),
  ]),
  '...',
  '',
  'Signed-off-by: dependabot[bot] <support@github.com>',
].join('\n');
const pr = (entries, overrides = {}) => ({
  state: 'OPEN',
  isDraft: false,
  baseRefName: 'main',
  author: { is_bot: true, login: 'app/dependabot' },
  headRefOid: head,
  title: 'Bump dependencies',
  body: '',
  commits: [{ authors: [{ login: 'dependabot[bot]' }], messageBody: metadata(entries) }],
  ...overrides,
});
const decide = (entries, overrides) => mergeDecision(pr(entries, overrides), head);

test('patch releases and minor releases of stable versions merge', () => {
  assert.equal(decide([{ name: 'libc', version: '0.2.191', type: 'patch' }]).merge, true);
  assert.equal(decide([
    { name: 'uuid', version: '1.29.0', type: 'minor' },
    { name: 'plist', version: '1.10.3', type: 'patch' },
    // Build metadata does not make a release unstable.
    { name: 'toml', version: '1.1.6+spec-1.1.0', type: 'patch' },
  ]).merge, true);
});

test('any update that may change the API waits for a person', () => {
  for (const entry of [
    { name: 'actions/cache', version: '6.1.0', type: 'major' },
    { name: 'sha2', version: '0.12.0', type: 'minor' },
    { name: 'early', version: '0.0.4', type: 'patch' },
    { name: 'material3', version: '1.13.0-alpha02', type: 'minor' },
    { name: 'nixpkgs', version: 'e158d9ed9b51c98974c5e66e1ba1c9e0255fecaa' },
    { name: 'missing' },
  ]) {
    assert.equal(decide([entry]).merge, false, entry.name);
  }
  // One incompatible update in a group holds back the whole pull request.
  assert.equal(decide([
    { name: 'libc', version: '0.2.191', type: 'patch' },
    { name: 'sha2', version: '0.12.0', type: 'minor' },
  ]).merge, false);
});

test('security updates without an update type are judged from the versions in the text', () => {
  const single = {
    title: 'Bump ip-address from 10.7.3 to 10.7.5 in /crates/host-daemon/src/providers/claude/sdk',
    body: 'Bumps [ip-address](https://github.com/beaugunderson/ip-address) from 10.7.3 to 10.7.5.',
  };
  assert.equal(decide([{ name: 'ip-address', version: '10.7.5' }], single).merge, true);
  const together = {
    title: 'Bump zod and @remotion/cli in /lp/promo-video',
    body: 'Bumps [zod](https://github.com/colinhacks/zod) to 4.6.7 and updates ancestor dependency [@remotion/cli](https://github.com/remotion-dev/remotion).\n\n'
      + 'Updates `zod` from 4.5.4 to 4.6.7\n<details>notes</details>\n\nUpdates `@remotion/cli` from 4.0.539 to 4.0.540\n',
  };
  assert.equal(decide([{ name: 'zod', version: '4.6.7' }, { name: '@remotion/cli', version: '4.0.540' }], together).merge, true);
  assert.equal(decide([{ name: 'zod', version: '5.0.0' }], { body: 'Updates `zod` from 4.6.7 to 5.0.0' }).merge, false);
  assert.equal(decide([{ name: 'zod', version: '4.6.6' }], { body: 'Updates `zod` from 4.6.7 to 4.6.6' }).merge, false);
  // Without the previous version the change cannot be classified.
  assert.equal(decide([{ name: 'zod', version: '4.6.7' }], { body: 'Bumps zod to 4.6.7.' }).merge, false);
});

test('only the passing head of an open Dependabot pull request into main merges', () => {
  const entries = [{ name: 'libc', version: '0.2.191', type: 'patch' }];
  assert.equal(mergeDecision(pr(entries), 'f'.repeat(40)).merge, false);
  for (const overrides of [
    { state: 'MERGED' },
    { isDraft: true },
    { baseRefName: 'release' },
    { author: { is_bot: false, login: 'someone' } },
    { commits: [] },
    { commits: [
      { authors: [{ login: 'dependabot[bot]' }], messageBody: metadata(entries) },
      { authors: [{ login: 'someone' }], messageBody: 'Adjust the lockfile' },
    ] },
  ]) {
    assert.equal(decide(entries, overrides).merge, false, JSON.stringify(overrides));
  }
});

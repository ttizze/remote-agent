import test from 'node:test';
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { macSigningIdentity } from './apple-signing.ts';

const developerId = 'A'.repeat(40);
const renewedId = 'B'.repeat(40);
const developmentId = 'C'.repeat(40);
const foreignId = 'D'.repeat(40);
const identity = (hash, authority, team = 'K65K9J8686') => `  1) ${hash} "${authority}: Fixture (${team})"`;
const developer = identity(developerId, 'Developer ID Application');
const development = identity(developmentId, 'Apple Development');
const foreign = identity(foreignId, 'Developer ID Application', 'FOREIGN123');

test('development and distribution select the BEX Developer ID even with other installed certificates', () => {
  const installed = [development, foreign, developer].join('\n');
  assert.equal(macSigningIdentity(installed), developerId);
  assert.equal(macSigningIdentity(installed, developerId), developerId);
  assert.equal(macSigningIdentity(`${developer}\n${developer}`), developerId);
});

test('Mac signing never falls back to development, another team or ad hoc signing', () => {
  for (const installed of ['', development, foreign, `${development}\n${foreign}`]) {
    assert.throws(() => macSigningIdentity(installed), /BEX Developer ID Application/);
  }
  const installed = [developer, development, foreign].join('\n');
  for (const requested of [developmentId, foreignId, '-', 'Apple Development', 'E'.repeat(40)]) {
    assert.throws(() => macSigningIdentity(installed, requested), /BEX Developer ID Application/);
  }
});

test('certificate renewal keeps the same team and authority and requires an explicit choice when ambiguous', () => {
  const renewed = identity(renewedId, 'Developer ID Application');
  assert.equal(macSigningIdentity(renewed), renewedId);
  assert.throws(() => macSigningIdentity(`${developer}\n${renewed}`), /multiple/);
  assert.equal(macSigningIdentity(`${developer}\n${renewed}`, renewedId), renewedId);
});

test('the build entry point resolves an identity without CI credentials and rejects a development override', () => {
  const script = fileURLToPath(new URL('./apple-signing.ts', import.meta.url));
  for (const [requested, status] of [['', 0], [developerId, 0], [developmentId, 1]]) {
    const result = spawnSync(process.execPath, [script, 'identity', requested], {
      input: `${development}\n${developer}`, encoding: 'utf8', env: {},
    });
    assert.ifError(result.error);
    assert.equal(result.status, status, result.stderr);
    if (status === 0) assert.equal(result.stdout.trim(), developerId);
    else assert.match(result.stderr, /BEX Developer ID Application/);
  }
});

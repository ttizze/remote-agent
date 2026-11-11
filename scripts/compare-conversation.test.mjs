import test from 'node:test';
import assert from 'node:assert/strict';
import { cpuSeconds, observedResult, renderReport } from './compare-conversation.mjs';

test('runner panics override completed steps and missing scenarios stay visible', () => {
  const panic = observedResult({ result: { passed: true, steps: [] }, error: 'engine <panic>', seconds: 1 });
  assert.equal(panic.passed, false);
  const missing = observedResult(undefined);
  assert.equal(missing.passed, false);
  const html = renderReport([{ mode: 'nsh', exitCode: 1, cases: { panic, missing } }], '/tmp/report', 'revision');
  assert.match(html, /engine &lt;panic&gt;/);
  assert.match(html, /Scenario was not run/);
});

test('process CPU time is comparable in minutes or hours', () => {
  assert.equal(cpuSeconds('0:01.25'), 1.25);
  assert.equal(cpuSeconds('2:03.50'), 123.5);
  assert.equal(cpuSeconds('1:02:03.50'), 3723.5);
});

test('comparison reports preserve failures and escape fixture labels and runner output', () => {
  const results = [{ mode: 'native', exitCode: 1, report: '<script>bad()</script>', cases: {
    'long<&>': { passed: false, steps: [{ status: 'failed', source: 'expect "A&B"', message: '<missing>' }], screenshots: [], metrics: null },
  } }];
  const html = renderReport(results, '/tmp/report', 'revision');
  assert.match(html, /FAIL \/ incomplete/);
  assert.match(html, /long&lt;&amp;&gt;/);
  assert.match(html, /&lt;missing&gt;/);
  assert.doesNotMatch(html, /<script>/);
});

import test from 'node:test';
import assert from 'node:assert/strict';
import { productionStatus, submitProduction } from './app-store-release.ts';

function fixture({ state = 'PREPARE_FOR_SUBMISSION', selected = 'candidate', valid = true, submission = null, failOnce = false } = {}) {
  let version = state === 'NOT_CREATED' ? null : { id: 'version', attributes: { versionString: '1.0.1', appStoreState: state } };
  let linked = selected;
  let items = [];
  let interrupted = failOnce;
  const changes = [];
  const request = async (path, options = {}) => {
    if (!options.method) {
      if (path === '/v1/builds/candidate') return { data: { id: 'candidate', attributes: { processingState: valid ? 'VALID' : 'PROCESSING', expired: false } } };
      if (path.endsWith('/preReleaseVersion')) return { data: { attributes: { version: '1.0.1' } } };
      if (path.endsWith('/app')) return { data: { id: '6816015343' } };
      if (path.includes('/appStoreVersions?')) return { data: version ? [version] : [] };
      if (path.endsWith('/build')) return { data: linked ? { id: linked } : null };
      if (path.endsWith('/appStoreVersionLocalizations')) return { data: [{ id: 'ja' }, { id: 'en' }] };
      if (path.startsWith('/v1/reviewSubmissions?')) return { data: submission ? [submission] : [] };
      if (path.includes('/items?')) return { data: items };
      throw new Error(`Unexpected GET ${path}`);
    }
    changes.push({ path, ...options });
    const data = options.data.data;
    if (path === '/v1/appStoreVersions') {
      version = { id: 'version', attributes: { ...data.attributes, appStoreState: 'PREPARE_FOR_SUBMISSION' } };
      linked = data.relationships.build.data.id;
      return { data: version };
    }
    if (path === '/v1/reviewSubmissions') {
      submission = { id: 'submission', attributes: { state: 'READY_FOR_REVIEW' } };
      return { data: submission };
    }
    if (path === '/v1/reviewSubmissionItems') {
      if (interrupted) { interrupted = false; throw new Error('Temporary API failure'); }
      items.push({ relationships: data.relationships });
    }
    if (path === '/v1/reviewSubmissions/submission') version.attributes.appStoreState = 'WAITING_FOR_REVIEW';
    return { data: version };
  };
  const submit = () => submitProduction(request, '6816015343', 'candidate', '1.0.1', 'ja', 'Release notes');
  return { request, submit, changes, finish: () => { version.attributes.appStoreState = 'READY_FOR_DISTRIBUTION'; } };
}

test('a stable rebuild submits its own App Store binary with release after approval', async () => {
  const api = fixture({ state: 'NOT_CREATED', selected: null });
  assert.equal((await api.submit()).state, 'WAITING_FOR_REVIEW');
  const creation = api.changes.find(change => change.path === '/v1/appStoreVersions').data.data;
  assert.equal(creation.attributes.releaseType, 'AFTER_APPROVAL');
  assert.equal(creation.relationships.build.data.id, 'candidate');
  assert.equal(api.changes.filter(change => change.path.startsWith('/v1/appStoreVersionLocalizations/')).length, 2);
  const count = api.changes.length;
  assert.equal((await api.submit()).published, false);
  assert.equal(api.changes.length, count, 'retrying an in-flight review must not modify it');
  api.finish();
  assert.equal((await productionStatus(api.request, '6816015343', '1.0.1', 'candidate')).published, true);
});

test('an interrupted submission reuses the empty review instead of leaving duplicate submissions', async () => {
  const api = fixture({ failOnce: true });
  await assert.rejects(api.submit(), /Temporary API failure/);
  assert.equal((await api.submit()).state, 'WAITING_FOR_REVIEW');
  assert.equal(api.changes.filter(change => change.path === '/v1/reviewSubmissions').length, 1);
});

test('approved builds and ongoing reviews can never be replaced by a different candidate', async () => {
  for (const state of ['PENDING_DEVELOPER_RELEASE', 'WAITING_FOR_REVIEW', 'READY_FOR_DISTRIBUTION']) {
    const api = fixture({ state, selected: 'approved-115' });
    await assert.rejects(api.submit(), /different build/);
    assert.deepEqual(api.changes, []);
  }
});

test('invalid and rejected builds fail before further release side effects', async () => {
  for (const options of [{ valid: false }, { state: 'REJECTED' }]) {
    const api = fixture(options);
    await assert.rejects(api.submit());
    assert.deepEqual(api.changes, []);
  }
});

test('manual approval releases only the build selected by the production tag', async () => {
  const api = fixture({ state: 'PENDING_DEVELOPER_RELEASE' });
  assert.equal((await api.submit()).state, 'PROCESSING_FOR_DISTRIBUTION');
  assert.deepEqual(api.changes.map(change => change.path), ['/v1/appStoreVersionReleaseRequests']);
});

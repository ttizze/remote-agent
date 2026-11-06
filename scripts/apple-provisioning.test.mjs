import assert from 'node:assert/strict';
import test from 'node:test';
import { distributionProfiles } from './apple-provisioning.ts';

const ids = ['dev.remoteagent.mobile.ios', 'dev.remoteagent.mobile.ios.TaskActivity'];
const serial = 'ABC123';
const expiry = '2030-01-01T00:00:00Z';
const now = Date.parse('2026-01-01T00:00:00Z');
const profile = (identifier, index) => ({ id: `profile-${index}`, attributes: {
  name: `BEX CI ${serial} ${identifier}`, profileType: 'IOS_APP_STORE', profileState: 'ACTIVE',
  expirationDate: expiry, uuid: `00000000-0000-0000-0000-00000000000${index}`, profileContent: 'cHJvZmlsZQ==',
}, relationships: { bundleId: { data: { id: `bundle-${index}` } }, certificates: { data: [{ id: 'certificate' }] } } });

function portal(existing = new Map(), bundleIds = ids, profileType = 'IOS_APP_STORE', deviceIds = []) {
  const calls = [];
  let failWidget = false;
  const request = async (path, options = {}) => {
    calls.push({ path, ...options });
    const url = new URL(path, 'https://api.appstoreconnect.apple.com');
    if (path.startsWith('/v1/certificates')) return { data: [{ id: 'certificate', attributes: { serialNumber: serial, certificateType: 'DISTRIBUTION', expirationDate: expiry } }] };
    if (path.startsWith('/v1/devices')) return { data: deviceIds.map(id => ({ id })) };
    if (path.startsWith('/v1/bundleIds')) {
      const identifier = url.searchParams.get('filter[identifier]');
      // Apple filtering can also return longer identifiers sharing the prefix.
      return { data: [{ id: 'unrelated', attributes: { identifier: identifier + '.other' } },
        { id: `bundle-${bundleIds.indexOf(identifier)}`, attributes: { identifier } }] };
    }
    if (options.method === 'POST') {
      const data = options.data.data;
      const index = Number(data.relationships.bundleId.data.id.slice(-1));
      assert.equal(data.relationships.certificates.data[0].id, 'certificate');
      assert.equal(data.attributes.profileType, profileType);
      assert.ok(data.attributes.name.endsWith(bundleIds[index]));
      if (index === 1 && failWidget) throw new Error('Apple temporarily unavailable');
      const created = profile(bundleIds[index], index);
      created.attributes = { ...created.attributes, ...data.attributes };
      if (deviceIds.length) created.relationships.devices = data.relationships.devices;
      existing.set(created.attributes.name, created);
      return { data: created };
    }
    return { data: existing.has(url.searchParams.get('filter[name]')) ? [existing.get(url.searchParams.get('filter[name]'))] : [] };
  };
  return { request, calls, failWidget: () => { failWidget = true; }, recover: () => { failWidget = false; } };
}

test('creates exact app and extension profiles using the installed distribution certificate', async () => {
  const api = portal();
  const result = await distributionProfiles(api.request, serial, ids[0], 'IOS_APP_STORE', now);
  assert.equal(result.prefix, `BEX CI ${serial}`);
  assert.deepEqual(Object.keys(result.profiles), ids);
  assert.equal(api.calls.filter(call => call.method === 'POST').length, 2);
});

test('Preview profiles include registered devices and refresh without revoking installed builds', async () => {
  const preview = ['dev.remoteagent.mobile.ios.preview', 'dev.remoteagent.mobile.ios.preview.TaskActivity'];
  const existing = new Map();
  const devices = ['iphone-1'];
  const api = portal(existing, preview, 'IOS_APP_ADHOC', devices);
  const first = await distributionProfiles(api.request, serial, preview[0], 'IOS_APP_ADHOC', now);
  assert.deepEqual(Object.keys(first.profiles), preview);
  const creations = () => api.calls.filter(call => call.method === 'POST');
  assert.equal(creations().length, 2);
  for (const creation of creations()) assert.deepEqual(creation.data.data.relationships.devices.data, [{ type: 'devices', id: 'iphone-1' }]);
  api.calls.length = 0;
  await distributionProfiles(api.request, serial, preview[0], 'IOS_APP_ADHOC', now);
  assert.equal(creations().length, 0);
  devices.push('iphone-2');
  const next = await distributionProfiles(api.request, serial, preview[0], 'IOS_APP_ADHOC', now);
  assert.notEqual(next.prefix, first.prefix);
  assert.equal(creations().length, 2);
  assert.equal(existing.size, 4);
  assert.ok(api.calls.every(call => !call.method || call.method === 'POST'));
});

test('Preview requires registered devices and cannot request App Store profiles', async () => {
  const preview = 'dev.remoteagent.mobile.ios.preview';
  const api = portal();
  await assert.rejects(distributionProfiles(api.request, serial, preview, 'IOS_APP_ADHOC', now), /Register an iPhone/);
  await assert.rejects(distributionProfiles(api.request, serial, preview, 'IOS_APP_STORE', now), /Invalid BEX/);
  assert.ok(api.calls.every(call => !call.method));
});

test('reuses active profiles without modifying Apple resources', async () => {
  const api = portal(new Map(ids.map((id, index) => [profile(id, index).attributes.name, profile(id, index)])));
  await distributionProfiles(api.request, serial, ids[0], 'IOS_APP_STORE', now);
  assert.ok(api.calls.every(call => !call.method));
});

test('retry after extension failure reuses the already created app profile', async () => {
  const api = portal();
  api.failWidget();
  await assert.rejects(distributionProfiles(api.request, serial, ids[0], 'IOS_APP_STORE', now), /temporarily unavailable/);
  api.recover();
  api.calls.length = 0;
  await distributionProfiles(api.request, serial, ids[0], 'IOS_APP_STORE', now);
  assert.equal(api.calls.filter(call => call.method === 'POST').length, 1);
});

test('does not use profiles for another certificate or bundle', async () => {
  for (const relationship of ['bundleId', 'certificates']) {
    const wrong = profile(ids[0], 0);
    wrong.relationships[relationship].data = relationship === 'bundleId' ? { id: 'other' } : [{ id: 'other' }];
    const api = portal(new Map([[wrong.attributes.name, wrong]]));
    await distributionProfiles(api.request, serial, ids[0], 'IOS_APP_STORE', now);
    assert.equal(api.calls.filter(call => call.method === 'POST').length, 2);
  }
});

test('missing or expired installed certificate stops before profile creation', async () => {
  for (const [value, time] of [['FFF', now], [serial, Date.parse(expiry)]]) {
    const api = portal();
    await assert.rejects(distributionProfiles(api.request, value, ids[0], 'IOS_APP_STORE', time), /certificate is unavailable/);
    assert.equal(api.calls.length, 1);
  }
});

test('rejects invalid profile responses before installing or exporting them', async () => {
  for (const change of [
    { uuid: '../not-a-profile' }, { profileContent: '' }, { expirationDate: 'invalid' },
    { profileState: 'INVALID' }, { name: 'another profile' }, { profileType: 'IOS_APP_DEVELOPMENT' },
  ]) {
    const api = portal();
    const request = async (path, options) => {
      const result = await api.request(path, options);
      if (options?.method === 'POST') Object.assign(result.data.attributes, change);
      return result;
    };
    await assert.rejects(distributionProfiles(request, serial, ids[0], 'IOS_APP_STORE', now), /active distribution profile/);
  }
});

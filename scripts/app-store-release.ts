import { listAll, type AscRequest, type AscResource, type AppleBuild, type StoreVersion } from './apple-release.ts';

const published = new Set(['READY_FOR_DISTRIBUTION', 'READY_FOR_SALE']);
const editable = new Set(['PREPARE_FOR_SUBMISSION', 'READY_FOR_REVIEW']);
const relationship = (type: string, id: string) => ({ data: { type, id } });

export async function productionStatus(request: AscRequest, appId: string, versionName: string, buildId: string) {
  const versions = await listAll<StoreVersion>(request, `/v1/apps/${appId}/appStoreVersions?filter[platform]=IOS&limit=200`);
  const version = versions.find(version => version.attributes.versionString === versionName);
  if (!version) return { versionId: null, state: 'NOT_CREATED', published: false };
  const build = (await request<{ data: AppleBuild | null }>(`/v1/appStoreVersions/${version.id}/build`)).data;
  if (build && build.id !== buildId) throw new Error(`App Store ${versionName} already selects a different build; preserve its review and release it first.`);
  const state = version.attributes.appStoreState;
  if (!build && !editable.has(state)) throw new Error('The App Store version has no selected candidate build.');
  return { versionId: version.id, state, published: published.has(state) && build?.id === buildId };
}

async function updateReleaseNotes(request: AscRequest, versionId: string, locale: string, notes: string) {
  const localizations = await listAll<AscResource<{ locale: string }>>(request, `/v1/appStoreVersions/${versionId}/appStoreVersionLocalizations`);
  if (!localizations.length) {
    await request('/v1/appStoreVersionLocalizations', { method: 'POST', data: { data: {
      type: 'appStoreVersionLocalizations', attributes: { locale, whatsNew: notes },
      relationships: { appStoreVersion: relationship('appStoreVersions', versionId) },
    } } });
  }
  for (const localization of localizations) {
    await request(`/v1/appStoreVersionLocalizations/${localization.id}`, { method: 'PATCH', data: { data: {
      type: 'appStoreVersionLocalizations', id: localization.id, attributes: { whatsNew: notes },
    } } });
  }
}

async function reviewSubmission(request: AscRequest, appId: string, versionId: string) {
  const submissions = await listAll<AscResource<{ state: string }>>(request, `/v1/reviewSubmissions?filter[app]=${appId}&filter[platform]=IOS&limit=200`);
  let empty;
  for (const submission of submissions.filter(item => item.attributes.state === 'READY_FOR_REVIEW')) {
    const items = await listAll<{ relationships?: { appStoreVersion?: { data?: { id: string } } } }>(request, `/v1/reviewSubmissions/${submission.id}/items?include=appStoreVersion`);
    if (items.some(item => item.relationships?.appStoreVersion?.data?.id === versionId)) return submission;
    if (!items.length) empty = submission;
  }
  const submission = empty || (await request<{ data: AscResource<{ state: string }> }>('/v1/reviewSubmissions', { method: 'POST', data: { data: {
    type: 'reviewSubmissions', attributes: { platform: 'IOS' }, relationships: { app: relationship('apps', appId) },
  } } })).data;
  await request('/v1/reviewSubmissionItems', { method: 'POST', data: { data: {
    type: 'reviewSubmissionItems', relationships: {
      reviewSubmission: relationship('reviewSubmissions', submission.id),
      appStoreVersion: relationship('appStoreVersions', versionId),
    },
  } } });
  return submission;
}

// A production release authorizes this rebuild of the selected Nightly source, including automatic
// release after approval. Never cancel an existing review or replace its build.
export async function submitProduction(request: AscRequest, appId: string, buildId: string, versionName: string, locale: string, notes: string) {
  const status = await productionStatus(request, appId, versionName, buildId);
  if (status.state === 'PENDING_DEVELOPER_RELEASE') {
    await request('/v1/appStoreVersionReleaseRequests', { method: 'POST', data: { data: {
      type: 'appStoreVersionReleaseRequests', relationships: { appStoreVersion: relationship('appStoreVersions', status.versionId!) },
    } } });
    return { ...status, state: 'PROCESSING_FOR_DISTRIBUTION' };
  }
  if (status.versionId && !editable.has(status.state)) {
    if (['REJECTED', 'METADATA_REJECTED', 'DEVELOPER_REJECTED', 'INVALID_BINARY'].includes(status.state)) throw new Error(`App Store release needs attention (${status.state}).`);
    return status;
  }
  const build = (await request<{ data: AppleBuild }>(`/v1/builds/${buildId}`)).data;
  if (build.attributes.processingState !== 'VALID' || build.attributes.expired) throw new Error('The production build is invalid or expired.');
  const app = (await request<{ data: { id: string } }>(`/v1/builds/${buildId}/app`)).data;
  const preRelease = (await request<{ data: AscResource<{ version: string }> }>(`/v1/builds/${buildId}/preReleaseVersion`)).data;
  if (app.id !== appId || preRelease.attributes.version !== versionName) throw new Error('The Apple build does not match this candidate.');
  let versionId = status.versionId;
  if (!versionId) {
    versionId = (await request<{ data: { id: string } }>('/v1/appStoreVersions', { method: 'POST', data: { data: {
      type: 'appStoreVersions', attributes: { platform: 'IOS', versionString: versionName, releaseType: 'AFTER_APPROVAL' },
      relationships: { app: relationship('apps', appId), build: relationship('builds', buildId) },
    } } })).data.id;
  } else {
    await request(`/v1/appStoreVersions/${versionId}`, { method: 'PATCH', data: { data: {
      type: 'appStoreVersions', id: versionId, attributes: { releaseType: 'AFTER_APPROVAL' },
      relationships: { build: relationship('builds', buildId) },
    } } });
  }
  await updateReleaseNotes(request, versionId, locale, notes);
  const submission = await reviewSubmission(request, appId, versionId);
  await request(`/v1/reviewSubmissions/${submission.id}`, { method: 'PATCH', data: { data: {
    type: 'reviewSubmissions', id: submission.id, attributes: { submitted: true },
  } } });
  return { versionId, state: 'WAITING_FOR_REVIEW', published: false };
}

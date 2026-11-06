import { execFileSync } from 'node:child_process';
import { createHash, X509Certificate } from 'node:crypto';
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';
import { ascClient, iosDistribution, listAll, type AscRequest, type AscResource } from './apple-release.ts';

type ProfileAttributes = { uuid: string; profileContent: string; name: string; profileType: string; profileState: string; expirationDate: string };
type Profile = AscResource<ProfileAttributes> & { relationships: { bundleId: { data?: { id: string } }; certificates: { data?: { id: string }[] }; devices?: { data?: { id: string }[] } } };

// Reuse profiles for the installed certificate; certificate renewal gets new names.
export async function distributionProfiles(request: AscRequest, serial: string, bundleId: string, profileType: string, now = Date.now()) {
  if (!/^[A-F0-9]+$/.test(serial)) throw new Error('Invalid distribution certificate serial.');
  if (!['dev.remoteagent.mobile.ios', 'dev.remoteagent.mobile.ios.preview'].includes(bundleId)
    || profileType !== (bundleId.endsWith('.preview') ? 'IOS_APP_ADHOC' : 'IOS_APP_STORE')) throw new Error('Invalid BEX distribution profile configuration.');
  const certificates = await listAll<AscResource<{ serialNumber: string; certificateType: string; expirationDate: string }>>(request, '/v1/certificates?limit=200');
  const certificate = certificates.find(item => item.attributes.serialNumber.toUpperCase() === serial
    && ['DISTRIBUTION', 'IOS_DISTRIBUTION'].includes(item.attributes.certificateType)
    && Date.parse(item.attributes.expirationDate) > now);
  if (!certificate) throw new Error('The installed distribution certificate is unavailable on App Store Connect.');
  const devices = profileType === 'IOS_APP_ADHOC'
    ? (await listAll<{ id: string }>(request, '/v1/devices?filter[platform]=IOS&filter[status]=ENABLED&limit=200')).map(device => device.id).sort() : [];
  if (profileType === 'IOS_APP_ADHOC' && !devices.length) throw new Error('Register an iPhone in the Apple Developer account before building Preview.');
  // A changed device list gets a new profile without revoking installed builds.
  const suffix = devices.length ? ` ${createHash('sha256').update(devices.join(',')).digest('hex').slice(0, 12)}` : '';
  const prefix = `BEX CI ${serial}${suffix}`;
  const profiles: Record<string, ProfileAttributes> = {};
  for (const identifier of [bundleId, `${bundleId}.TaskActivity`]) {
    const bundles = await listAll<AscResource<{ identifier: string }>>(request, `/v1/bundleIds?filter[identifier]=${identifier}&limit=200`);
    const bundle = bundles.find(item => item.attributes.identifier === identifier);
    if (!bundle) throw new Error(`Registered bundle ${identifier} is missing.`);
    const name = `${prefix} ${identifier}`;
    const candidates = await listAll<Profile>(request, `/v1/profiles?filter[name]=${encodeURIComponent(name)}&include=bundleId,certificates,devices&limit=200`);
    let profile: Profile | undefined = candidates.find(item => item.attributes.name === name
      && item.attributes.profileType === profileType && item.attributes.profileState === 'ACTIVE'
      && Date.parse(item.attributes.expirationDate) > now
      && item.relationships.bundleId.data?.id === bundle.id
      && item.relationships.certificates.data?.some(cert => cert.id === certificate.id)
      && (!devices.length || JSON.stringify(item.relationships.devices?.data?.map(device => device.id).sort()) === JSON.stringify(devices)));
    if (!profile) {
      // Do not revoke or replace profiles used by other developers or builds.
      profile = (await request<{ data: Profile }>('/v1/profiles', { method: 'POST', data: { data: {
        type: 'profiles', attributes: { name, profileType },
        relationships: {
          bundleId: { data: { type: 'bundleIds', id: bundle.id } },
          certificates: { data: [{ type: 'certificates', id: certificate.id }] },
          ...(devices.length ? { devices: { data: devices.map(id => ({ type: 'devices', id })) } } : {}),
        },
      } } })).data;
    }
    if (!/^[A-F0-9]{8}(?:-[A-F0-9]{4}){3}-[A-F0-9]{12}$/i.test(profile.attributes.uuid) || !profile.attributes.profileContent
      || profile.attributes.name !== name || profile.attributes.profileType !== profileType
      || profile.attributes.profileState !== 'ACTIVE' || !(Date.parse(profile.attributes.expirationDate) > now)) {
      throw new Error(`Apple did not return an active distribution profile for ${identifier}.`);
    }
    profiles[identifier] = profile.attributes;
  }
  return { prefix, profiles };
}

async function main() {
  let pem;
  try {
    pem = execFileSync('/usr/bin/security', ['find-certificate', '-a', '-p', process.env.AGENT_CODE_SIGN_KEYCHAIN!], { encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] });
  } catch { throw new Error('Could not read the dedicated signing certificates.'); }
  const certificates = [...pem.matchAll(/-----BEGIN CERTIFICATE-----[\s\S]+?-----END CERTIFICATE-----/g)]
    .map(match => new X509Certificate(match[0])).filter(cert => /^CN=Apple Distribution:/m.test(cert.subject) && /^OU=K65K9J8686$/m.test(cert.subject));
  if (certificates.length !== 1) throw new Error('The BEX distribution certificate is missing or ambiguous.');
  const request = ascClient({ keyId: process.env.AGENT_ASC_KEY_ID!, issuer: process.env.AGENT_ASC_ISSUER_ID!,
    privateKey: readFileSync(process.env.AGENT_ASC_PRIVATE_KEY_FILE!) });
  const distribution = iosDistribution(process.env.AGENT_RELEASE_CHANNEL);
  const result = await distributionProfiles(request, certificates[0].serialNumber, distribution.bundleId, distribution.profileType);
  const directory = join(process.env.HOME!, 'Library/Developer/Xcode/UserData/Provisioning Profiles');
  mkdirSync(directory, { recursive: true, mode: 0o700 });
  const mapping: Record<string, string> = {};
  for (const [identifier, profile] of Object.entries(result.profiles)) {
    writeFileSync(join(directory, `${profile.uuid}.mobileprovision`), Buffer.from(profile.profileContent, 'base64'), { mode: 0o600 });
    mapping[identifier] = profile.name;
  }
  writeFileSync('target/apple-release/provisioning.json', JSON.stringify({ method: distribution.method, prefix: result.prefix, profiles: mapping }) + '\n');
  console.log(`Installed ${distribution.profileType} profiles for ${distribution.product} and its activity extension.`);
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  main().catch(error => { console.error(error.message); process.exitCode = 1; });
}

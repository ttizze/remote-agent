import { execFileSync } from 'node:child_process';
import { appendFileSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';

// Keep the macOS privacy identity stable across development and distribution.
export function macSigningIdentity(identities: string, requested = '') {
  const candidates = [...new Set([...identities.matchAll(/([A-F0-9]{40}) "Developer ID Application:[^\n]+\(K65K9J8686\)"/g)].map(match => match[1]))];
  if (requested) {
    if (!candidates.includes(requested)) throw new Error('AGENT_CODE_SIGN_IDENTITY must be the SHA-1 of a BEX Developer ID Application certificate (K65K9J8686).');
    return requested;
  }
  if (candidates.length !== 1) throw new Error('One BEX Developer ID Application certificate (K65K9J8686) is required; set AGENT_CODE_SIGN_IDENTITY to its SHA-1 if multiple are installed.');
  return candidates[0];
}

const security = (...args: string[]) => {
  try { return execFileSync('/usr/bin/security', args, { encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] }).trim(); }
  catch { throw new Error(`Signing keychain operation ${args[0]} failed.`); }
};
function main(operation: string | undefined, requested?: string) {
  if (operation === 'identity') {
    console.log(macSigningIdentity(readFileSync(0, 'utf8'), requested));
    return;
  }
  const root = join(process.env.RUNNER_TEMP!, 'agent-apple-signing');
  const chain = join(root, 'BEX.keychain-db');
  if (operation === 'prepare') {
    for (const name of ['AGENT_SIGNING_KEYCHAIN_BASE64', 'AGENT_SIGNING_KEYCHAIN_PASSWORD', 'AGENT_ASC_PRIVATE_KEY', 'AGENT_ASC_KEY_ID', 'AGENT_ASC_ISSUER_ID']) {
      if (!process.env[name]) throw new Error(`GitHub secret ${name} is required.`);
    }
    mkdirSync(root, { mode: 0o700 });
    const previous = security('list-keychains', '-d', 'user').split('\n').map(line => line.trim().replace(/^"|"$/g, '')).filter(Boolean);
    writeFileSync(join(root, 'previous.json'), JSON.stringify(previous), { mode: 0o600 });
    writeFileSync(chain, Buffer.from(process.env.AGENT_SIGNING_KEYCHAIN_BASE64!, 'base64'), { mode: 0o600 });
    security('unlock-keychain', '-p', process.env.AGENT_SIGNING_KEYCHAIN_PASSWORD!, chain);
    security('set-keychain-settings', '-lut', '21600', chain);
    security('set-key-partition-list', '-S', 'apple-tool:,apple:,codesign:', '-s', '-k', process.env.AGENT_SIGNING_KEYCHAIN_PASSWORD!, chain);
    security('list-keychains', '-d', 'user', '-s', chain, ...previous);
    const identities = security('find-identity', '-v', '-p', 'codesigning', chain);
    const identity = macSigningIdentity(identities);
    if (!identities.includes('Apple Distribution:')) throw new Error('BEX distribution signing identities are missing or ambiguous.');
    const keyPath = join(root, 'AuthKey.p8');
    writeFileSync(keyPath, process.env.AGENT_ASC_PRIVATE_KEY!, { mode: 0o600 });
    appendFileSync(process.env.GITHUB_ENV!, `AGENT_CODE_SIGN_KEYCHAIN=${chain}\nAGENT_CODE_SIGN_IDENTITY=${identity}\nAGENT_ASC_PRIVATE_KEY_FILE=${keyPath}\n`);
  } else if (operation === 'cleanup') {
    let previous;
    try { previous = JSON.parse(readFileSync(join(root, 'previous.json'), 'utf8')); } catch { previous = null; }
    if (previous) security('list-keychains', '-d', 'user', '-s', ...previous);
    rmSync(root, { recursive: true, force: true });
  } else throw new Error('Use apple-signing.ts identity, prepare or cleanup.');
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  try { main(process.argv[2], process.argv[3]); }
  catch (error) { console.error((error as Error).message); process.exitCode = 1; }
}

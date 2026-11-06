import { execFileSync } from 'node:child_process';
import { pathToFileURL } from 'node:url';

type BuildCache = { id: number; key: string; ref: string };
type DependencyCache = BuildCache & { createdAt: string };

const MAIN = 'refs/heads/main';

function cachePrefix(key: string | undefined) {
  const prefix = key?.match(/^(bex-gradle-v1-.+-)[0-9a-f]{40}$/)?.[1] ??
    key?.match(/^(ios-swift-xcode-[0-9]+\.[0-9]+-)[0-9a-f]{64}$/)?.[1] ??
    key?.match(/^(ios-release-xcode-[0-9]+\.[0-9]+-)[0-9a-f]{40}$/)?.[1] ??
    key?.match(/^(local-rust-[A-Za-z0-9]+-[A-Za-z0-9]+-xcode-[0-9]+\.[0-9]+-[0-9a-f]{64}-(?:mac|iphone)-)[0-9a-f]{40}$/)?.[1];
  if (!prefix) throw new Error('Invalid build snapshot key');
  return prefix;
}

export function replacedBuildCaches(entries: readonly BuildCache[], currentKey: string) {
  const prefix = cachePrefix(currentKey);
  const owned = entries.filter(entry => entry.ref === MAIN && entry.key.startsWith(prefix));
  // A successful action step can still report an unsuccessful cache upload.
  if (!owned.some(entry => entry.key === currentKey)) return [];
  return owned.filter(entry => entry.key !== currentKey).map(entry => entry.id);
}

// rust-cache keys end with the environment hash and the lockfile hash. Every
// lockfile change publishes a new cache; only the newest serves later restores.
export function replacedDependencyCaches(entries: readonly DependencyCache[]) {
  const groups = new Map<string, DependencyCache[]>();
  for (const entry of entries) {
    const prefix = entry.key.match(/^(v0-rust-.+-[0-9a-f]{8})-[0-9a-f]{8}$/)?.[1];
    if (!prefix || entry.ref !== MAIN) continue;
    groups.set(prefix, [...(groups.get(prefix) ?? []), entry]);
  }
  return [...groups.values()].flatMap(group =>
    group.toSorted((a, b) => b.createdAt.localeCompare(a.createdAt)).slice(1).map(entry => entry.id));
}

function list(prefix: string): DependencyCache[] {
  return JSON.parse(execFileSync('gh', [
    'cache', 'list', '--key', prefix, '--ref', MAIN, '--limit', '100', '--json', 'id,key,ref,createdAt',
  ], {encoding: 'utf8'}));
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const key = process.argv[2];
  const replaced = key === '--dependencies'
    ? replacedDependencyCaches(list('v0-rust-'))
    : replacedBuildCaches(list(cachePrefix(key)), key!);
  for (const id of replaced) {
    execFileSync('gh', ['cache', 'delete', String(id)], {stdio: 'inherit'});
  }
}

import { readFileSync } from 'node:fs';
import { pathToFileURL } from 'node:url';

type PullRequest = { state: string; isDraft?: boolean; baseRefName: string; author?: { is_bot?: boolean; login: string }; headRefOid: string; title: string; body: string; commits?: { messageBody?: string; authors: { login: string }[] }[] };
type Dependency = { name?: string; version?: string; updateType?: string };
type Version = { major: number; minor: number; patch: number };

// Decides whether a Dependabot pull request may merge after its CI passed on `ciHeadSha`.
// Only updates that keep every existing API merge: patch releases, and minor releases of
// stable (>= 1.0.0) versions. Majors, 0.x minors, 0.0.x releases, pre-releases and
// non-semver versions (such as Nix revisions) wait for a person.
export function mergeDecision(pr: PullRequest, ciHeadSha: string) {
  if (pr.state !== 'OPEN' || pr.isDraft || pr.baseRefName !== 'main') return skip('not an open pull request into main');
  if (!pr.author?.is_bot || pr.author.login !== 'app/dependabot') return skip('not opened by Dependabot');
  if (pr.headRefOid !== ciHeadSha) return skip('CI passed for an older head');
  const commits = pr.commits ?? [];
  if (commits.length === 0 || commits.some(commit => commit.authors.some(author => author.login !== 'dependabot[bot]'))) {
    return skip('the branch has commits from someone other than Dependabot');
  }
  const dependencies = updatedDependencies(commits.at(-1)!.messageBody);
  if (dependencies.length === 0) return skip('no Dependabot update metadata');
  const text = `${pr.title}\n${pr.body}`;
  for (const dependency of dependencies) {
    const problem = incompatibility(dependency, text);
    if (problem) return skip(`${dependency.name}: ${problem}`);
  }
  return { merge: true, reason: `compatible updates: ${dependencies.map(dependency => `${dependency.name} ${dependency.version}`).join(', ')}` };
}

function skip(reason: string) {
  return { merge: false, reason };
}

// Dependabot writes one YAML list between `updated-dependencies:` and `...` in its commit message.
function updatedDependencies(message: string | undefined) {
  const block = /^updated-dependencies:\n([\s\S]*?)^\.\.\.$/m.exec(message ?? '')?.[1] ?? '';
  return block.split(/^- /m).slice(1).map(entry => {
    const value = (key: string) => new RegExp(`^\\s*${key}: (.*)$`, 'm').exec(entry)?.[1].trim().replace(/^"(.*)"$/, '$1');
    return { name: value('dependency-name'), version: value('dependency-version'), updateType: value('update-type') };
  });
}

function incompatibility({ name, version, updateType }: Dependency, text: string) {
  if (!name) return 'the update metadata names no dependency';
  const next = semver(version);
  if (!next) return `version ${version ?? '(missing)'} is not a stable semantic version`;
  const type = updateType?.replace(/^version-update:semver-/, '') ?? changeType(previousVersion(name, version, text), next);
  if (!type) return 'the previous version is unknown';
  if (type === 'patch') return next.major === 0 && next.minor === 0 ? 'a 0.0.x release may change the API' : null;
  if (type === 'minor') return next.major === 0 ? 'a 0.x minor release may change the API' : null;
  return type === 'major' ? 'a major release may change the API' : `a ${type} update needs review`;
}

// Security updates omit `update-type`; their title and body still say "from A to B".
function previousVersion(name: string, version: string | undefined, text: string) {
  const escaped = name.replace(/[.*+?^${}()|[\]\\/]/g, '\\$&');
  const pattern = new RegExp(`(?:\\[${escaped}\\]\\([^)]*\\)|\`${escaped}\`|(?:^|\\s)${escaped})\\s+from\\s+(\\S+)\\s+to\\s+(\\S+)`, 'gm');
  for (const match of text.matchAll(pattern)) {
    const [from, to] = [match[1], match[2]].map(value => value.replace(/[.,)]+$/, ''));
    if (to === version) return from;
  }
  return null;
}

function changeType(from: string | null, to: Version) {
  const previous = semver(from);
  if (!previous) return null;
  const changed = (['major', 'minor', 'patch'] as const).find(part => to[part] !== previous[part]);
  if (!changed) return 'patch';
  return to[changed] < previous[changed] ? 'downgrade' : changed;
}

// Build metadata (`1.1.6+spec-1.1.0`) does not change precedence; pre-release tags do.
function semver(version: string | null | undefined) {
  const match = /^v?(\d+)\.(\d+)\.(\d+)(?:\+[0-9A-Za-z.-]+)?$/.exec(version ?? '');
  return match && { major: Number(match[1]), minor: Number(match[2]), patch: Number(match[3]) };
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  // Usage: gh pr view N --json state,isDraft,baseRefName,author,headRefOid,commits,title,body | node scripts/dependabot-merge.ts HEAD_SHA
  console.log(JSON.stringify(mergeDecision(JSON.parse(readFileSync(0, 'utf8')), process.argv[2]!)));
}

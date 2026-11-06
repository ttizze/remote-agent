import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { existsSync, readFileSync, realpathSync } from 'node:fs';
import { homedir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';

type CargoPackage = { id: string; name: string; source: string | null; manifest_path: string };
type CargoNode = { id: string; deps: { pkg: string; dep_kinds?: { kind: string | null }[] }[] };

export function sourceDirectories(packages: readonly CargoPackage[], nodes: readonly CargoNode[], root: string) {
  const byId = new Map(packages.map(p => [p.id, p]));
  const graph = new Map(nodes.map(n => [n.id, n.deps]));
  const entry = packages.find(p => p.name === root && p.source === null);
  if (!entry) throw new Error(`Missing workspace package: ${root}`);
  const pending = [entry.id], visited = new Set<string>(), directories = new Set<string>();
  while (pending.length) {
    const id = pending.pop()!;
    if (visited.has(id)) continue;
    visited.add(id);
    const pkg = byId.get(id), dependencies = graph.get(id);
    if (!pkg || !dependencies) throw new Error(`Incomplete Cargo dependency graph: ${id}`);
    if (pkg.source === null) directories.add(dirname(pkg.manifest_path));
    for (const dependency of dependencies) {
      if (!dependency.dep_kinds?.length) throw new Error(`Missing Cargo dependency kinds: ${dependency.pkg}`);
      if (dependency.dep_kinds.some(({kind}) => kind !== 'dev')) pending.push(dependency.pkg);
    }
  }
  return [...directories].sort();
}

export function compilerEnvironment(environment: NodeJS.ProcessEnv, host: string) {
  const names = new Set([
    'RUSTC', 'RUSTFLAGS', 'CARGO_ENCODED_RUSTFLAGS', 'RUSTC_WRAPPER', 'RUSTC_WORKSPACE_WRAPPER',
    'CARGO_BUILD_TARGET', 'CARGO_BUILD_RUSTFLAGS', 'CARGO_INCREMENTAL', 'RUSTC_BOOTSTRAP',
    'CC', 'CXX', 'AR', 'AS', 'LD', 'RANLIB',
    'CFLAGS', 'CXXFLAGS', 'CPPFLAGS', 'LDFLAGS', 'LIBCLANG_PATH', 'BINDGEN_EXTRA_CLANG_ARGS',
    'NIX_CFLAGS_COMPILE', 'NIX_LDFLAGS', 'NIX_ENFORCE_PURITY', 'ANDROID_NDK_HOME', 'CARGO_NDK_PLATFORM',
    'AGENT_BUILD_REVISION', 'AGENT_BUILD_NUMBER', 'NODE_OPTIONS',
  ]);
  for (const target of ['HOST', 'TARGET']) {
    for (const suffix of ['CC', 'CXX', 'AR', 'RANLIB', 'CFLAGS', 'CXXFLAGS', 'CPPFLAGS', 'LDFLAGS']) {
      names.add(`${target}_${suffix}`);
    }
  }
  for (const target of [host, 'aarch64-linux-android', 'x86_64-linux-android']) {
    for (const prefix of ['CC', 'CXX', 'AR', 'RANLIB', 'CFLAGS', 'CXXFLAGS', 'CPPFLAGS', 'LDFLAGS']) {
      names.add(`${prefix}_${target}`);
      names.add(`${prefix}_${target.replaceAll('-', '_')}`);
    }
  }
  return Object.fromEntries(Object.entries(environment).filter(([key]) => names.has(key) ||
    /^CARGO_PROFILE_(DEV|RELEASE)(?:_BUILD_OVERRIDE)?_(OPT_LEVEL|DEBUG|SPLIT_DEBUGINFO|STRIP|DEBUG_ASSERTIONS|OVERFLOW_CHECKS|LTO|PANIC|INCREMENTAL|CODEGEN_UNITS|RPATH)$/.test(key) ||
    /^CARGO_TARGET_[A-Z0-9_]+_(LINKER|RUSTFLAGS)$/.test(key)).sort(([a], [b]) => a.localeCompare(b)));
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const roots = process.argv.slice(2);
  if (!roots.length) throw new Error('usage: node scripts/rust-build-inputs.ts package...');
  const metadata = JSON.parse(execFileSync('cargo', ['metadata', '--locked', '--format-version', '1'], {
    encoding: 'utf8', maxBuffer: 32 * 1024 * 1024,
  })) as { packages: CargoPackage[]; resolve: { nodes: CargoNode[] } };
  const rustc = execFileSync('rustc', ['-vV'], {encoding: 'utf8'});
  const host = rustc.match(/^host: (.+)$/m)?.[1];
  if (!host) throw new Error('Missing Rust host triple');
  const configuration = [], visited = new Set();
  const locations = [];
  for (let directory = process.cwd();; directory = dirname(directory)) {
    locations.push(join(directory, '.cargo'));
    if (directory === dirname(directory)) break;
  }
  locations.push(resolve(process.env.CARGO_HOME || join(homedir(), '.cargo')));
  for (const location of locations) {
    for (const name of ['config', 'config.toml']) {
      const path = join(location, name);
      if (!existsSync(path) || visited.has(realpathSync(path))) continue;
      visited.add(realpathSync(path));
      configuration.push(createHash('sha256').update(readFileSync(path)).digest('hex'));
    }
  }
  console.log(JSON.stringify({
    sources: Object.fromEntries(roots.map(root => [root, sourceDirectories(metadata.packages, metadata.resolve.nodes, root)])),
    compiler: {rustc, node: process.version, configuration, environment: compilerEnvironment(process.env, host)},
  }));
}

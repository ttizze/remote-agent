// Build on Linux; distribute the exact same revision with the Windows desktop.
// Include the ELF loader/libc closure so WSL users do not need the Nix store.
import { chmodSync, copyFileSync, existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { basename, join, resolve } from "node:path";
import { execFileSync } from "node:child_process";

if (process.platform !== "linux" || process.arch !== "x64") throw new Error("The WSL runtime requires Linux x64");
const [binaryDirectory, destination] = process.argv.slice(2);
if (!binaryDirectory || !destination) throw new Error("Provide binary directory and destination");
const revision = execFileSync("git", ["rev-parse", "HEAD"], { encoding: "utf8" }).trim();
const output = resolve(destination);
const libraries = join(output, "lib");
mkdirSync(libraries, { recursive: true });
const patchelf = execFileSync("sh", ["-c", "command -v patchelf"], { encoding: "utf8" }).trim();
const executables = [
  [join(binaryDirectory, "host-daemon"), "host-daemon"],
  [join(binaryDirectory, "agent-cli"), "agent-cli"],
  [join(binaryDirectory, "agent-provider-supervisor"), "agent-provider-supervisor"],
  [patchelf, "patchelf"],
];
const dependencies = new Map<string, string>();
function copyWritable(source: string, target: string) {
  if (existsSync(target)) chmodSync(target, 0o755);
  copyFileSync(source, target);
  chmodSync(target, 0o755);
}
const queue = executables.map(([path]) => resolve(path));
while (queue.length) {
  const path = queue.pop()!;
  const listing = execFileSync("ldd", [path], { encoding: "utf8" });
  if (listing.includes("not found")) throw new Error(`Unresolved libraries for ${path}: ${listing}`);
  for (const line of listing.split("\n")) {
    const library = /(?:=>\s+|^\s*)(\/\S+)\s+\(/.exec(line)?.[1];
    if (!library) continue;
    const name = basename(library);
    if (dependencies.has(name)) {
      if (dependencies.get(name) !== library && !readFileSync(library).equals(readFileSync(dependencies.get(name)!))) {
        throw new Error(`Conflicting runtime libraries: ${name}`);
      }
      continue;
    }
    dependencies.set(name, library);
    queue.push(library);
  }
}
for (const [name, path] of dependencies) {
  const target = join(libraries, name);
  copyWritable(path, target);
  if (!name.startsWith("ld-linux")) execFileSync(patchelf, ["--force-rpath", "--set-rpath", "$ORIGIN", target]);
}
for (const [path, name] of executables) {
  const target = join(output, name);
  copyWritable(path, target);
  execFileSync(patchelf, ["--force-rpath", "--set-rpath", "$ORIGIN/lib", target]);
}
execFileSync(process.execPath, ["scripts/install-claude-sdk.ts", output], { stdio: "inherit" });
writeFileSync(join(output, "revision"), `${revision}\n`);

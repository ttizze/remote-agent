import { mkdir, copyFile } from "node:fs/promises";
import { execSync } from "node:child_process";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const destination = process.argv[2];
if (!destination) throw new Error("Provide the Host executable directory");
const root = fileURLToPath(new URL("../", import.meta.url));
const sdk = join(root, "crates/host-daemon/src/providers/claude/sdk");
// Standalone Rust builds also package this runtime, without a prior npm build.
for (const cwd of [root, sdk]) {
  execSync("npm ci --include=dev --ignore-scripts --no-audit --no-fund", { cwd, stdio: "inherit" });
}
const { build } = await import("esbuild");
const output = resolve(destination);
await mkdir(output, { recursive: true });
await build({
  absWorkingDir: sdk,
  entryPoints: ["main.ts"], bundle: true, platform: "node", format: "esm",
  target: "node22", outfile: join(output, "agent-claude-sdk.mjs"), legalComments: "inline",
});
await copyFile(join(sdk, "node_modules/@anthropic-ai/claude-agent-sdk/LICENSE.md"), join(output, "Claude-Agent-SDK-LICENSE.md"));

import { build } from "esbuild";
import { copyFile } from "node:fs/promises";

await build({
  entryPoints: ["main.mjs"], bundle: true, platform: "node", format: "esm",
  target: "node22", outfile: "bridge.bundle.mjs", legalComments: "inline",
});
await copyFile("node_modules/@anthropic-ai/claude-agent-sdk/LICENSE.md", "SDK-LICENSE.md");

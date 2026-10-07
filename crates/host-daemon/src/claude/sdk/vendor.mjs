import { copyFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

const source = new URL("./node_modules/@anthropic-ai/claude-agent-sdk/", import.meta.url);
for (const [input, output] of [["sdk.mjs", "sdk.mjs"], ["LICENSE.md", "SDK-LICENSE.md"]]) {
  copyFileSync(fileURLToPath(new URL(input, source)), fileURLToPath(new URL(output, import.meta.url)));
}

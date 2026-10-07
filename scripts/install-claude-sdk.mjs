import { mkdir, copyFile } from "node:fs/promises";
import { join } from "node:path";

const destination = process.argv[2];
if (!destination) throw new Error("Provide the Host executable directory");
await mkdir(destination, { recursive: true });
await copyFile(new URL("../crates/host-daemon/src/claude/sdk/bridge.bundle.mjs", import.meta.url), join(destination, "bex-claude-sdk.mjs"));
await copyFile(new URL("../crates/host-daemon/src/claude/sdk/SDK-LICENSE.md", import.meta.url), join(destination, "Claude-Agent-SDK-LICENSE.md"));

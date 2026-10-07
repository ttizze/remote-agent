import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";
import { query as realQuery } from "./sdk.mjs";
const fixture = fileURLToPath(new URL("./fake-cli.mjs", import.meta.url));
export function query({ prompt, options }) {
  return realQuery({ prompt, options: { ...options,
    spawnClaudeCodeProcess: (spec) => spawn(process.execPath, [fixture, ...spec.args], { cwd: spec.cwd, env: spec.env, stdio: ["pipe", "pipe", "pipe"] }),
  } });
}

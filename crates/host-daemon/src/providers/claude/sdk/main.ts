import { createInterface } from "node:readline";
import { runBridge } from "./bridge.ts";

const input = createInterface({ input: process.stdin });
const emit = (event: Record<string, unknown>) => process.stdout.write(`${JSON.stringify(event)}\n`);
runBridge(input, emit).catch((error) => {
  emit({ type: "error", message: error.message });
  process.exitCode = 1;
}).finally(() => input.close());

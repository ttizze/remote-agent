import assert from "node:assert/strict";
import { test } from "node:test";
import { runBridge } from "./bridge.mjs";
import { spawn } from "node:child_process";
import { mkdtemp, symlink, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { sessionStateStream } from "./session-state.mjs";

function channel() {
  const values = [];
  let resume;
  let closed = false;
  return {
    push(value) { values.push(value); resume?.(); },
    close() { closed = true; resume?.(); },
    async *[Symbol.asyncIterator]() {
      while (!closed || values.length) {
        if (!values.length) await new Promise((resolve) => { resume = resolve; });
        while (values.length) yield values.shift();
      }
    },
  };
}

test("SDK callbacks wait for Host answers and cancel when the SDK aborts", async () => {
  const input = channel();
  const output = channel();
  let options;
  input.push(JSON.stringify({ type: "initialize", program: "/fixture/claude", cwd: "/fixture", sessionId: "session", resume: true }));
  const events = [];
  const messages = output[Symbol.asyncIterator]();
  const bridge = runBridge(input, (event) => events.push(event), ({ options: configured }) => {
    options = configured;
    return {
      async initializationResult() { return { models: [] }; },
      next: messages.next.bind(messages),
      close() { output.close(); input.close(); },
    };
  });
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(options.resume, "session");
  assert.deepEqual(await options.onUserDialog({ dialogKind: "unknown" }), { behavior: "cancelled" });
  await assert.rejects(options.canUseTool("Bash", { command: "あ".repeat(12_000) }, {
    requestId: "oversized", signal: new AbortController().signal,
  }), /size limit/);
  assert.ok(events.every((event) => event.requestId !== "oversized"));
  const signal = new AbortController();
  const answer = options.canUseTool("Bash", { command: "test" }, { requestId: "request", toolUseID: "tool", signal: signal.signal });
  assert.deepEqual(events.at(-1), { type: "request", requestId: "request", request: { type: "tool", toolName: "Bash", input: { command: "test" }, toolUseId: "tool" } });
  input.push(JSON.stringify({ type: "answer", requestId: "request", response: { behavior: "allow", updatedInput: { command: "test" } } }));
  assert.deepEqual(await answer, { behavior: "allow", updatedInput: { command: "test" } });
  const cancelled = options.onElicitation({ serverName: "server", message: "Name?" }, { requestId: "elicitation", signal: signal.signal });
  signal.abort();
  await assert.rejects(cancelled, /cancelled/);
  assert.deepEqual(events.at(-1), { type: "requestCancelled", requestId: "elicitation" });
  input.push(JSON.stringify({ type: "answer", requestId: "elicitation", response: { action: "accept" } }));
  output.push({ type: "result", result: "still running" });
  await new Promise((resolve) => setImmediate(resolve));
  assert.ok(events.some((event) => event.type === "message" && event.message.result === "still running"));
  input.close();
  await bridge;
});

test("the bundled runtime starts through a symlink", async () => {
  const root = await mkdtemp(join(tmpdir(), "bex-sdk-entry-"));
  try {
    const directory = join(root, "runtime");
    await symlink(fileURLToPath(new URL(".", import.meta.url)), directory, "junction");
    const entry = join(directory, "bridge.bundle.mjs");
    const child = spawn(process.execPath, [entry], { stdio: ["pipe", "pipe", "pipe"] });
    let output = "";
    child.stdout.setEncoding("utf8").on("data", (text) => { output += text; });
    const closed = new Promise((resolve, reject) => {
      child.once("error", reject);
      child.once("close", (code) => resolve(code));
    });
    child.stdin.end(`${JSON.stringify({ type: "invalid" })}\n`);
    assert.equal(await closed, 1);
    assert.deepEqual(JSON.parse(output), { type: "error", message: "Expected SDK initialization" });
  } finally {
    await rm(root, { recursive: true });
  }
});

test("SDK failure exits without waiting for more Host input", async () => {
  const input = channel();
  input.push(JSON.stringify({ type: "initialize", program: "/fixture/claude", cwd: "/fixture" }));
  const bridge = runBridge(input, () => {}, () => ({
    async initializationResult() { return {}; },
    async next() { throw new Error("CLI failed"); },
    close() { input.close(); },
  }));
  await assert.rejects(bridge, /CLI failed/);
});

test("host-only session state remains in order with unmodified SDK messages", async () => {
  const stream = sessionStateStream();
  const messages = [
    { type: "assistant", message: { content: "会話 session_state_changed" } },
    { type: "system", subtype: "session_state_changed", state: "running", sdk_host_only: true },
    { type: "control_response", response: { request_id: "request", subtype: "success" } },
    { type: "result", result: "response" },
    { type: "system", subtype: "session_state_changed", state: "idle", sdk_host_only: true },
  ];
  const bytes = Buffer.from(messages.map((message) => JSON.stringify(message)).join("\n"));
  const read = (async () => {
    let output = "";
    for await (const chunk of stream) output += chunk.toString();
    return output.trimEnd().split("\n").map((line) => JSON.parse(line));
  })();
  for (const byte of bytes) stream.write(Buffer.from([byte]));
  stream.end();
  const actual = await read;
  for (const message of messages) {
    if (message.subtype === "session_state_changed") delete message.sdk_host_only;
  }
  assert.deepEqual(actual, messages);
});

test("persistent SDK input stays open across results and additional messages", async () => {
  const input = channel();
  const output = channel();
  const reader = output[Symbol.asyncIterator]();
  const received = [];
  let consumed;
  input.push(JSON.stringify({ type: "initialize", cwd: "/fixture", sessionId: "session" }));
  const bridge = runBridge(input, () => {}, ({ prompt }) => {
    consumed = (async () => { for await (const message of prompt) received.push(message.uuid); })();
    return {
      async initializationResult() { return {}; },
      next: reader.next.bind(reader),
      close() { output.close(); },
    };
  });
  input.push(JSON.stringify({ type: "message", id: "first", content: "wait" }));
  await new Promise((resolve) => setImmediate(resolve));
  output.push({ type: "result", result: "first result" });
  input.push(JSON.stringify({ type: "message", id: "queued", content: "permission" }));
  await new Promise((resolve) => setImmediate(resolve));
  input.push(JSON.stringify({ type: "message", id: "next", content: "hello" }));
  await new Promise((resolve) => setImmediate(resolve));
  assert.deepEqual(received, ["first", "queued", "next"]);
  input.close();
  await bridge;
  await consumed;
});

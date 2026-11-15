import assert from "node:assert/strict";
import { test } from "node:test";
import { runBridge, probeOptions, thinkingEnabledSetting, thinkingOptions } from "./bridge.ts";
import { spawn, execFileSync } from "node:child_process";
import { mkdtemp, mkdir, symlink, rm, readFile, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { sessionStateStream } from "./session-state.ts";
import { getSessionMessages } from "@anthropic-ai/claude-agent-sdk";

test("native fork captures the remapped SDK head even when the project moved", async () => {
  // The real SDK remaps copied UUIDs. Source user/control IDs cannot identify
  // the baseline inherited by the new native session (T3 QueryRunner.forkSession).
  const home = await mkdtemp(join(tmpdir(), "agent-native-fork-"));
  const previous = process.env.CLAUDE_CONFIG_DIR;
  process.env.CLAUDE_CONFIG_DIR = home;
  try {
    const bytes = await readFile(new URL("../../../../tests/fixtures/claude-2.1.266.jsonl", import.meta.url), "utf8");
    const rows = bytes.trim().split("\n").map(JSON.parse);
    const session = rows.find((row) => row.sessionId).sessionId;
    const cwd = rows.find((row) => row.cwd).cwd;
    const cursor = rows.find((row) => row.type === "assistant" && !row.isSidechain).uuid;
    const directory = join(home, "projects", cwd.replace(/[^a-zA-Z0-9]/g, "-"));
    await mkdir(directory, { recursive: true });
    const original = join(directory, `${session}.jsonl`);
    await writeFile(original, bytes);
    const source = await getSessionMessages(session);
    const boundary = source.findIndex((message) => message.uuid === cursor);
    assert.ok(boundary >= 0);
    for (const [upToMessageId, expected] of [[cursor, source.slice(0, boundary + 1)], [undefined, source]]) {
    for (const workingDirectory of [cwd, join(home, "moved-project")]) {
      const events = [];
      async function* input() {
        yield JSON.stringify({ type: "fork", sessionId: session, cwd: workingDirectory, upToMessageId });
      }
      await runBridge(input(), (event) => events.push(event), () => { throw new Error("fork must not run inference"); });
      assert.equal(events.length, 1);
      const forked = events[0];
      assert.equal(forked.type, "forked");
      assert.notEqual(forked.sessionId, session);
      const copied = await getSessionMessages(forked.sessionId);
      assert.equal(copied.length, expected.length, "copy ends at the selected assistant or includes the whole native thread");
      assert.deepEqual(copied.map((message) => message.message), expected.map((message) => message.message));
      assert.notEqual(copied.at(-1).uuid, cursor, "SDK allocates new transcript UUIDs");
      assert.equal(forked.headMessageId, copied.at(-1).uuid, "save the real inherited head before the first query");
      assert.equal(await readFile(original, "utf8"), bytes, "source history remains intact");
    }
    }
  } finally {
    if (previous === undefined) delete process.env.CLAUDE_CONFIG_DIR;
    else process.env.CLAUDE_CONFIG_DIR = previous;
    await rm(home, { recursive: true, force: true });
  }
});

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

test("native resume forwards the real assistant cursor into query options", async () => {
  for (const [resume, cursor] of [[true, "actual-assistant-uuid"], [false, undefined]]) {
    const input = channel();
    const output = channel();
    const reader = output[Symbol.asyncIterator]();
    let options;
    input.push(JSON.stringify({ type: "initialize", cwd: "/fixture", sessionId: "native-session", resume, resumeSessionAt: cursor }));
    const bridge = runBridge(input, () => {}, (configured) => {
      options = configured.options;
      return { async initializationResult() { return {}; }, next: reader.next.bind(reader), close() { output.close(); } };
    });
    await new Promise((resolve) => setImmediate(resolve));
    input.close();
    await bridge;
    assert.equal(options.resume, resume ? "native-session" : undefined);
    assert.equal(options.sessionId, resume ? undefined : "native-session");
    assert.equal(options.resumeSessionAt, cursor, "a pinned native head reaches the SDK; fresh allocations have no resume position");
  }
});

test("catalog probes cannot run user hooks, tools or automatic IDE connections", async () => {
  for (const probe of [true, false]) {
    const input = channel();
    const output = channel();
    const reader = output[Symbol.asyncIterator]();
    input.push(JSON.stringify({ type: "initialize", cwd: "/fixture", probe }));
    let options;
    const bridge = runBridge(input, () => {}, (configured) => {
      options = configured.options;
      return {
        async initializationResult() { return {}; },
        next: reader.next.bind(reader),
        close() { output.close(); },
      };
    });
    await new Promise((resolve) => setImmediate(resolve));
    input.close();
    await bridge;
    if (probe) {
      assert.equal(options.persistSession, false);
      assert.deepEqual(options.settings, { disableAllHooks: true });
      assert.deepEqual(options.allowedTools, []);
      assert.deepEqual(options.mcpServers, {});
      assert.equal(options.env.ENABLE_CLAUDEAI_MCP_SERVERS, "false");
      assert.equal(options.env.CLAUDE_CODE_AUTO_CONNECT_IDE, "0");
      assert.equal(options.env.CLAUDE_CODE_IDE_SKIP_AUTO_INSTALL, "1");
    } else {
      // The session's thinking follows the user's setting (tested below).
      const thinking = thinkingOptions(thinkingEnabledSetting("/fixture", process.env));
      assert.deepEqual(options.settings, thinking.settings);
      assert.deepEqual(options.thinking, thinking.thinking);
      assert.deepEqual(options.extraArgs, { "replay-user-messages": null, ...thinking.extraArgs });
      assert.equal(options.allowedTools, undefined);
    }
  }
});

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

test("the production build includes its license and starts through a symlink outside the checkout", async () => {
  const root = await mkdtemp(join(tmpdir(), "agent-sdk-entry-"));
  try {
    const builtRuntime = join(root, "built runtime");
    execFileSync(process.execPath, [
      fileURLToPath(new URL("../../../../../../scripts/build-claude-sdk.ts", import.meta.url)), builtRuntime,
    ], { cwd: root, stdio: "pipe", env: { ...process.env, NODE_ENV: "production" } });
    assert.equal(await readFile(join(builtRuntime, "Claude-Agent-SDK-LICENSE.md"), "utf8"),
      await readFile(new URL("./node_modules/@anthropic-ai/claude-agent-sdk/LICENSE.md", import.meta.url), "utf8"));
    const directory = join(root, "runtime");
    await symlink(builtRuntime, directory, "junction");
    const entry = join(directory, "agent-claude-sdk.mjs");
    const child = spawn(process.execPath, [entry], { cwd: root, stdio: ["pipe", "pipe", "pipe"] });
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

test("a steer reaches the running query at once while other input waits its turn", async () => {
  const input = channel();
  const output = channel();
  const reader = output[Symbol.asyncIterator]();
  const received = [];
  let consumed;
  input.push(JSON.stringify({ type: "initialize", cwd: "/fixture", sessionId: "session" }));
  const bridge = runBridge(input, () => {}, ({ prompt }) => {
    consumed = (async () => { for await (const message of prompt) received.push(message); })();
    return {
      async initializationResult() { return {}; },
      next: reader.next.bind(reader),
      close() { output.close(); },
    };
  });
  input.push(JSON.stringify({ type: "message", id: "first", content: "wait" }));
  input.push(JSON.stringify({ type: "steer", id: "steer", content: [{ type: "text", text: "change course" }] }));
  await new Promise((resolve) => setImmediate(resolve));
  assert.deepEqual(received.map(({ uuid, priority }) => [uuid, priority]), [["first", undefined], ["steer", "now"]]);
  assert.deepEqual(received[1].message, { role: "user", content: [{ type: "text", text: "change course" }] });
  input.close();
  await bridge;
  await consumed;
});

test("managed MCP policy retains probe isolation without the incompatible strict flag", () => {
  const managed = probeOptions(true, { PATH: "/fixture/bin" });
  const { strictMcpConfig, ...isolated } = probeOptions(false, { PATH: "/fixture/bin" });
  assert.equal(strictMcpConfig, true);
  assert.equal(managed.strictMcpConfig, undefined);
  assert.deepEqual(isolated, managed);
  assert.equal(managed.env.PATH, "/fixture/bin");
});

test("sessions request thinking summaries as T3 Code does unless the user turned thinking off", () => {
  const summarized = {
    thinking: { type: "adaptive", display: "summarized" },
    settings: { showThinkingSummaries: true },
    extraArgs: { "thinking-display": "summarized" },
  };
  assert.deepEqual(thinkingOptions(undefined), summarized);
  assert.deepEqual(thinkingOptions(true), summarized);
  assert.deepEqual(thinkingOptions(false), {});
});

test("the thinking setting follows user, project and local settings in that order", async () => {
  const root = await mkdtemp(join(tmpdir(), "bridge-thinking-"));
  try {
    const home = join(root, "home");
    const project = join(root, "project");
    await mkdir(home, { recursive: true });
    await mkdir(join(project, ".claude"), { recursive: true });
    const environment = { CLAUDE_CONFIG_DIR: home };
    assert.equal(thinkingEnabledSetting(project, environment), undefined);
    await writeFile(join(home, "settings.json"), JSON.stringify({ alwaysThinkingEnabled: false }));
    assert.equal(thinkingEnabledSetting(project, environment), false);
    await writeFile(join(project, ".claude", "settings.json"), JSON.stringify({ alwaysThinkingEnabled: true }));
    assert.equal(thinkingEnabledSetting(project, environment), true);
    await writeFile(join(project, ".claude", "settings.local.json"), JSON.stringify({ alwaysThinkingEnabled: false }));
    assert.equal(thinkingEnabledSetting(project, environment), false);
    // A file that does not parse, or a value that is not a boolean, sets nothing.
    await writeFile(join(project, ".claude", "settings.local.json"), "{");
    assert.equal(thinkingEnabledSetting(project, environment), true);
    await writeFile(join(project, ".claude", "settings.json"), JSON.stringify({ alwaysThinkingEnabled: "no" }));
    assert.equal(thinkingEnabledSetting(project, environment), false);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

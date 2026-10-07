import assert from "node:assert/strict";
import test from "node:test";
import { PassThrough, Writable } from "node:stream";
import { EventEmitter, once } from "node:events";
import { spawn } from "node:child_process";
import { readFileSync } from "node:fs";
import { createInterface } from "node:readline";
import { fileURLToPath } from "node:url";
import * as sdk from "./sdk.mjs";
import { forkTranscript, runBridge } from "./bridge.mjs";

const sourceSession = "123e4567-e89b-12d3-a456-426614174000";
const targetSession = "123e4567-e89b-12d3-a456-426614174001";
test("SDK forks through a native boundary before any filesystem write", async () => {
  const source = [
    { type: "user", uuid: "u1", parentUuid: null, sessionId: sourceSession, message: { role: "user", content: "hello" } },
    { type: "assistant", uuid: "a1", parentUuid: "u1", sessionId: sourceSession, message: { role: "assistant", content: [{ type: "text", text: "done" }] } },
    { type: "user", uuid: "u2", parentUuid: "a1", sessionId: sourceSession, message: { role: "user", content: "later" } },
  ].map(JSON.stringify).join("\n");
  const transcript = await forkTranscript(sdk, source, sourceSession, targetSession, "a1");
  const entries = transcript.trim().split("\n").map(JSON.parse);
  const messages = entries.filter((entry) => ["user", "assistant"].includes(entry.type));
  assert.equal(messages.length, 2);
  assert.equal(messages[1].parentUuid, messages[0].uuid);
  assert.ok(messages.every((entry) => entry.sessionId === targetSession));
  assert.equal(messages[1].forkedFrom.messageUuid, "a1");
  await assert.rejects(forkTranscript(sdk, source, sourceSession, targetSession, "missing"), /not found/);
});

function harness() {
  const input = new PassThrough();
  const events = new EventEmitter();
  const frames = [];
  const output = new Writable({ write(bytes, _encoding, done) {
    frames.push(JSON.parse(bytes.toString())); events.emit("frame"); done();
  } });
  const completion = runBridge(sdk, fileURLToPath(new URL("./fake-cli.mjs", import.meta.url)), input, output);
  const send = (frame) => input.write(`${JSON.stringify(frame)}\n`);
  async function take(predicate) {
    for (;;) {
      const index = frames.findIndex(predicate);
      if (index >= 0) return frames.splice(index, 1)[0];
      await once(events, "frame", { signal: AbortSignal.timeout(5000) });
    }
  }
  const request = (id, subtype, payload = {}) => send({ type: "control_request", request_id: id, request: { subtype, ...payload } });
  return { input, completion, send, request, take };
}

test("real SDK streams multiple turns and preserves permission response correlation", { timeout: 10000 }, async () => {
  const h = harness();
  try {
    h.request("initialize", "initialize", { options: { installPermissionCallback: true }, appendSystemPrompt: "fixture" });
    assert.equal((await h.take((frame) => frame.response?.request_id === "initialize")).response.subtype, "success");
    for (const id of ["one", "two"]) {
      h.send({ type: "user", uuid: id, session_id: "", parent_tool_use_id: null, message: { role: "user", content: "hello" } });
      const permission = await h.take((frame) => frame.type === "control_request");
      assert.equal(permission.request_id, "permission");
      assert.equal(permission.request.tool_use_id, "tool");
      assert.equal(permission.request.description, "Run the test fixture");
      h.send({ type: "control_response", response: { subtype: "success", request_id: permission.request_id, response: { behavior: "allow", updatedInput: permission.request.input } } });
      assert.equal((await h.take((frame) => frame.type === "assistant")).message.content[0].text, "allow");
      assert.equal((await h.take((frame) => frame.type === "result")).uuid, `result-${id}`);
    }
  } finally { h.input.end(); await h.completion; }
});

test("SDK cancellation closes the pending approval and late responses are ignored", { timeout: 10000 }, async () => {
  const h = harness();
  try {
    h.request("initialize", "initialize", { options: { installPermissionCallback: true } });
    await h.take((frame) => frame.response?.request_id === "initialize");
    h.send({ type: "user", uuid: "one", session_id: "", parent_tool_use_id: null, message: { role: "user", content: "hello" } });
    await h.take((frame) => frame.type === "control_request");
    h.request("interrupt", "interrupt");
    assert.equal((await h.take((frame) => frame.type === "control_cancel_request")).request_id, "permission");
    assert.equal((await h.take((frame) => frame.response?.request_id === "interrupt")).response.subtype, "success");
    h.send({ type: "control_response", response: { subtype: "success", request_id: "permission", response: { behavior: "allow", updatedInput: {} } } });
  } finally { h.input.end(); await h.completion; }
});

test("SDK resume dialogs retain their payload and structured answer", { timeout: 10000 }, async () => {
  const h = harness();
  try {
    h.request("initialize", "initialize");
    await h.take((frame) => frame.response?.request_id === "initialize");
    h.send({ type: "user", uuid: "one", session_id: "", parent_tool_use_id: null, message: { role: "user", content: "dialog" } });
    const dialog = await h.take((frame) => frame.type === "control_request");
    assert.equal(dialog.request.dialog_kind, "resume_return");
    assert.deepEqual(dialog.request.payload, { sessionAgeMinutes: 90, estimatedTokens: 120000 });
    h.send({ type: "control_response", response: { subtype: "success", request_id: dialog.request_id, response: { behavior: "completed", result: "compact" } } });
    assert.equal((await h.take((frame) => frame.type === "assistant")).message.content[0].text, "compact");
  } finally { h.input.end(); await h.completion; }
});

test("SDK process failure closes the worker instead of leaving an unusable session alive", { timeout: 10000 }, async () => {
  const h = harness();
  try {
    h.request("initialize", "initialize");
    await h.take((frame) => frame.response?.request_id === "initialize");
    h.send({ type: "user", uuid: "one", session_id: "", parent_tool_use_id: null, message: { role: "user", content: "crash" } });
    assert.match((await h.take((frame) => frame.type === "sdk_error")).message, /7/);
    await h.completion;
  } finally { h.input.end(); await h.completion; }
});

test("unknown SDK dialogs cancel immediately without blocking the conversation", { timeout: 10000 }, async () => {
  const h = harness();
  try {
    h.request("initialize", "initialize");
    await h.take((frame) => frame.response?.request_id === "initialize");
    h.send({ type: "user", uuid: "one", session_id: "", parent_tool_use_id: null, message: { role: "user", content: "future-dialog" } });
    assert.equal((await h.take((frame) => frame.type === "assistant")).message.content[0].text, "cancelled");
  } finally { h.input.end(); await h.completion; }
});

test("the actual worker exits after SDK failure while Host stdin stays open", { timeout: 10000 }, async () => {
  const worker = readFileSync(new URL("./bridge.mjs", import.meta.url), "utf8");
  const library = fileURLToPath(new URL("./sdk.mjs", import.meta.url));
  const fixture = fileURLToPath(new URL("./fake-cli.mjs", import.meta.url));
  const child = spawn(process.execPath, ["--input-type=module", "--eval", worker, library, fixture], { stdio: ["pipe", "pipe", "pipe"] });
  child.stdin.on("error", () => {});
  const lines = createInterface({ input: child.stdout })[Symbol.asyncIterator]();
  const exit = once(child, "exit", { signal: AbortSignal.timeout(5000) });
  try {
    child.stdin.write(JSON.stringify({ type: "control_request", request_id: "initialize", request: { subtype: "initialize" } }) + "\n");
    const initialized = JSON.parse((await lines.next()).value);
    assert.equal(initialized.response.subtype, "success");
    child.stdin.write(JSON.stringify({ type: "user", uuid: "one", session_id: "", parent_tool_use_id: null, message: { role: "user", content: "crash" } }) + "\n");
    const failure = JSON.parse((await lines.next()).value);
    assert.equal(failure.type, "sdk_error");
    assert.match(failure.message, /7/);
    assert.equal((await exit)[0], 0);
  } finally { child.stdin.end(); child.kill(); }
});

import { query } from "@anthropic-ai/claude-agent-sdk";
import { spawnClaudeCodeProcess } from "./session-state.mjs";

/** The SDK owns Claude's protocol, callbacks and process lifecycle. */
export async function runBridge(lines, emit, startQuery = query) {
  const iterator = lines[Symbol.asyncIterator]();
  const first = await iterator.next();
  if (first.done) return;
  const config = JSON.parse(first.value);
  if (config.type !== "initialize") throw new Error("Expected SDK initialization");
  const requests = new Map();
  const messages = [];
  let wake;
  let closing = false;
  let fail;
  const failed = new Promise((_, reject) => { fail = reject; });
  function reportFailure(error) { if (!closing) fail(error); }
  function request(id, body, signal) {
    return new Promise((resolve, reject) => {
      if (Buffer.byteLength(JSON.stringify(body), "utf8") > 32 * 1024) {
        reject(new Error("Claude request exceeds its size limit"));
        return;
      }
      const cancel = () => {
        requests.delete(id);
        emit({ type: "requestCancelled", requestId: id });
        reject(new Error("Claude request was cancelled"));
      };
      if (signal.aborted) return cancel();
      signal.addEventListener("abort", cancel, { once: true });
      requests.set(id, {
        resolve(value) { signal.removeEventListener("abort", cancel); resolve(value); },
        reject(error) { signal.removeEventListener("abort", cancel); reject(error); },
      });
      emit({ type: "request", requestId: id, request: body });
    });
  }
  const options = {
    cwd: config.cwd,
    pathToClaudeCodeExecutable: config.program,
    ...(config.model ? { model: config.model } : {}),
    ...(config.effort ? { effort: config.effort } : {}),
    ...(config.browser ? { mcpServers: { bex_browser: config.browser } } : {}),
    settingSources: ["user", "project", "local"],
    systemPrompt: { type: "preset", preset: "claude_code" },
    includePartialMessages: true,
    extraArgs: { "replay-user-messages": null },
    spawnClaudeCodeProcess,
    ...(config.sessionId ? config.resume ? { resume: config.sessionId } : { sessionId: config.sessionId } : {}),
    canUseTool: (toolName, input, context) => request(context.requestId, {
      type: "tool", toolName, input, toolUseId: context.toolUseID,
    }, context.signal),
    onElicitation: (body, context) => request(context.requestId, {
      type: "elicitation", serverName: body.serverName, message: body.message,
      mode: body.mode, url: body.url, requestedSchema: body.requestedSchema,
    }, context.signal),
    onUserDialog: async () => ({ behavior: "cancelled" }),
  };
  const q = startQuery({ options, prompt: (async function* () {
    while (!closing) {
      if (!messages.length) await new Promise((resolve) => { wake = resolve; });
      while (!closing && messages.length) yield messages.shift();
    }
  })() });
  const reading = (async () => {
    const initialized = await q.initializationResult();
    emit({ type: "ready", initialized });
    // Query.return() closes the transport before returning the inner iterator.
    for await (const message of { [Symbol.asyncIterator]: () => q }) emit({ type: "message", message });
    if (!closing) throw new Error("Claude SDK exited");
  })();
  reading.catch(reportFailure);
  try {
    const commands = (async () => {
      for (;;) {
        const line = await iterator.next();
        if (line.done) return;
        const command = JSON.parse(line.value);
        switch (command.type) {
          case "message": {
            const message = { type: "user", uuid: command.id, session_id: config.sessionId ?? "",
              message: { role: "user", content: command.content }, parent_tool_use_id: null };
            messages.push(message);
            wake?.();
            break;
          }
          case "answer": {
            const pending = requests.get(command.requestId);
            if (!pending) { emit({ type: "requestCancelled", requestId: command.requestId }); break; }
            requests.delete(command.requestId);
            if (command.error) pending.reject(new Error(command.error));
            else pending.resolve(command.response);
            break;
          }
          case "interrupt":
            try { await q.interrupt(); emit({ type: "interrupted", requestId: command.requestId }); }
            catch (error) { emit({ type: "interrupted", requestId: command.requestId, error: error.message }); }
            break;
          case "usage":
            try { emit({ type: "usage", usage: await q.usage_EXPERIMENTAL_MAY_CHANGE_DO_NOT_RELY_ON_THIS_API_YET({ skipBehaviors: true }) }); }
            catch (error) { emit({ type: "usage", error: error.message }); }
            break;
          default: throw new Error("Unknown Claude SDK operation");
        }
      }
    })();
    await Promise.race([commands, failed]);
  } finally {
    closing = true;
    wake?.();
    for (const pending of requests.values()) pending.reject(new Error("Host disconnected"));
    requests.clear();
    q.close();
    await reading.catch(() => {});
  }
}

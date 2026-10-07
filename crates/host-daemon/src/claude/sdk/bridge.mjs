import { createInterface } from "node:readline";
import { pathToFileURL } from "node:url";

export async function forkTranscript(sdk, source, session, target, through) {
  const entries = source.split("\n").flatMap((line) => {
    try { return [JSON.parse(line)]; } catch { return []; }
  });
  let copied;
  const store = {
    load: async (key) => key.sessionId === session ? entries : null,
    append: async (_key, rows) => { copied = rows; },
  };
  const forked = await sdk.forkSession(session, { sessionStore: store, upToMessageId: through ?? undefined });
  // The Host supplied the deterministic session ID it will reserve before saving.
  // The SDK still owns UUID remapping, compaction chains and fork metadata.
  return copied.map((entry) => JSON.stringify(entry.sessionId === forked.sessionId ? { ...entry, sessionId: target } : entry)).join("\n") + "\n";
}

const supportedDialogKinds = ["resume_return"];

// Only SDK I/O lives here. The Host owns conversation state and permission decisions.
export async function runBridge(sdk, program, input, output) {
  const lines = createInterface({ input, crlfDelay: Infinity });
  let query;
  const pending = new Map();
  const prompts = [];
  let wake;
  let closed = false;
  let writes = Promise.resolve();
  const send = (frame) => {
    writes = writes.then(() => new Promise((resolve, reject) => {
      output.write(`${JSON.stringify(frame)}\n`, (error) => error ? reject(error) : resolve());
    }));
    return writes;
  };
  async function* messages() {
    while (!closed) {
      if (prompts.length) yield prompts.shift();
      else await new Promise((resolve) => { wake = resolve; });
    }
  }
  async function ask(request, signal, nativeId) {
    const id = nativeId;
    if (pending.has(id)) return pending.get(id).promise;
    let resolve;
    const promise = new Promise((done) => { resolve = done; });
    const cancel = () => {
      if (!pending.delete(id)) return;
      resolve(null);
      void send({ type: "control_cancel_request", request_id: id });
    };
    pending.set(id, { resolve, promise, signal, cancel });
    signal.addEventListener("abort", cancel, { once: true });
    if (signal.aborted) cancel();
    else await send({ type: "control_request", request_id: id, request });
    return promise;
  }
  async function request(frame) {
    const body = frame.request;
    let result;
    switch (body.subtype) {
      case "fork_session":
        result = { transcript: await forkTranscript(sdk, body.transcript, body.session, body.target, body.through) };
        break;
      case "initialize": {
        if (query) throw new Error("Claude SDK is already initialized");
        const { installPermissionCallback, ...options } = body.options ?? {};
        query = sdk.query({ prompt: messages(), options: {
          ...options,
          pathToClaudeCodeExecutable: program,
          cwd: process.cwd(),
          systemPrompt: { type: "preset", preset: "claude_code", append: body.appendSystemPrompt ?? "" },
          stderr: (text) => process.stderr.write(text),
          ...(installPermissionCallback ? { canUseTool: async (tool, input, metadata) => {
            const answer = await ask({ subtype: "can_use_tool", tool_name: tool, input,
              tool_use_id: metadata.toolUseID, description: metadata.description,
              permission_suggestions: metadata.suggestions }, metadata.signal, metadata.requestId);
            return answer ?? { behavior: "deny", message: "Permission request cancelled", interrupt: true };
          } } : {}),
          supportedDialogKinds,
          onUserDialog: (dialog, metadata) => supportedDialogKinds.includes(dialog.dialogKind)
            ? ask({ subtype: "request_user_dialog", dialog_kind: dialog.dialogKind, payload: dialog.payload, tool_use_id: dialog.toolUseID }, metadata.signal, metadata.requestId)
            : Promise.resolve({ behavior: "cancelled" }),
        } });
        // Initialization and callbacks are SDK-owned; replaying the pending arrays
        // here would open the same approval twice under different correlation keys.
        const { pending_permission_requests, pending_user_dialog_requests, ...initialized } = await query.initializationResult();
        result = initialized;
        void (async () => {
          try { for await (const message of query) await send(message); }
          catch (error) { if (!closed) await send({ type: "sdk_error", message: error.message }); }
          finally { if (!closed) lines.close(); }
        })();
        break;
      }
      case "interrupt": result = await query.interrupt(); break;
      case "set_permission_mode": await query.setPermissionMode(body.mode); break;
      case "set_model": await query.setModel(body.model); break;
      case "get_usage": result = await query.usage_EXPERIMENTAL_MAY_CHANGE_DO_NOT_RELY_ON_THIS_API_YET({ skipBehaviors: body.skip_behaviors }); break;
      default: throw new Error(`Unsupported Claude SDK operation: ${body.subtype}`);
    }
    await send({ type: "control_response", response: { subtype: "success", request_id: frame.request_id, response: result ?? {} } });
  }
  const tasks = new Set();
  try {
    for await (const line of lines) {
      if (!line.trim()) continue;
      const frame = JSON.parse(line);
      if (frame.type === "user") {
        if (!query) throw new Error("Claude SDK is not initialized");
        prompts.push(frame);
        wake?.();
      } else if (frame.type === "control_response") {
        const response = frame.response;
        const item = pending.get(response.request_id);
        if (item) {
          pending.delete(response.request_id);
          item.signal.removeEventListener("abort", item.cancel);
          item.resolve(response.subtype === "success" ? response.response : null);
        }
      } else if (frame.type === "control_request") {
        const task = request(frame).catch((error) => send({ type: "control_response", response: {
          subtype: "error", request_id: frame.request_id, error: error.message,
        } })).finally(() => tasks.delete(task));
        tasks.add(task);
      } else throw new Error(`Unsupported Host frame: ${frame.type}`);
    }
  } finally {
    closed = true;
    wake?.();
    query?.close();
    for (const item of pending.values()) {
      item.signal.removeEventListener("abort", item.cancel);
      item.resolve(null);
    }
    pending.clear();
    await Promise.allSettled(tasks);
    await writes;
  }
}

if (process.execArgv.includes("--eval")) {
  const sdk = await import(pathToFileURL(process.argv[1]).href);
  await runBridge(sdk, process.argv[2], process.stdin, process.stdout);
}

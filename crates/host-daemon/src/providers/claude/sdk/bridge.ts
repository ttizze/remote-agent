import { forkSession, query } from "@anthropic-ai/claude-agent-sdk";
import { spawnClaudeCodeProcess } from "./session-state.ts";
import { existsSync } from "node:fs";
import type { Options, SDKUserMessage, PermissionResult, ElicitationResult, McpServerConfig } from "@anthropic-ai/claude-agent-sdk";

type Initialize = { type: "initialize"; cwd: string; program: string; model?: string;
  effort?: Options["effort"]; browser?: McpServerConfig; sessionId?: string; resume?: boolean; probe?: boolean };
type Command = { type: "message"; id: SDKUserMessage["uuid"]; content: SDKUserMessage["message"]["content"] }
  | { type: "answer"; requestId: string; error?: string; response: unknown }
  | { type: "interrupt"; requestId: string } | { type: "usage" };

const managedMcpPath = process.platform === "darwin"
  ? "/Library/Application Support/ClaudeCode/managed-mcp.json"
  : process.platform === "win32"
    ? "C:\\Program Files\\ClaudeCode\\managed-mcp.json"
    : "/etc/claude-code/managed-mcp.json";

export function probeOptions(managedMcpConfig: boolean, environment: NodeJS.ProcessEnv): Partial<Options> {
  return {
    persistSession: false,
    settings: { disableAllHooks: true },
    allowedTools: [],
    mcpServers: {},
    // Claude rejects strict MCP configuration alongside managed policy.
    ...(managedMcpConfig ? {} : { strictMcpConfig: true }),
    env: {
      ...environment,
      ENABLE_CLAUDEAI_MCP_SERVERS: "false",
      FORCE_CODE_TERMINAL: undefined,
      CLAUDE_CODE_AUTO_CONNECT_IDE: "0",
      CLAUDE_CODE_IDE_SKIP_AUTO_INSTALL: "1",
    },
  };
}

/** The SDK owns Claude's protocol, callbacks and process lifecycle. */
export async function runBridge(lines: AsyncIterable<string>, emit: (event: Record<string, unknown>) => void, startQuery = query, fork = forkSession) {
  const iterator = lines[Symbol.asyncIterator]();
  const first = await iterator.next();
  if (first.done) return;
  const config = JSON.parse(first.value) as Initialize | { type: "fork"; sessionId: string; cwd: string; upToMessageId?: string };
  if (config.type === "fork") {
    // Forking copies the native transcript; it starts no model request.
    const { sessionId } = await fork(config.sessionId, { dir: config.cwd, upToMessageId: config.upToMessageId });
    emit({ type: "forked", sessionId });
    return;
  }
  if (config.type !== "initialize") throw new Error("Expected SDK initialization");
  const requests = new Map<string, { resolve(value: unknown): void; reject(error: Error): void }>();
  const messages: SDKUserMessage[] = [];
  let wake: (() => void) | undefined;
  let closing = false;
  let fail!: (reason: unknown) => void;
  const failed = new Promise<never>((_, reject) => { fail = reject; });
  function reportFailure(error: unknown) { if (!closing) fail(error); }
  function request<T>(id: string, body: Record<string, unknown>, signal: AbortSignal): Promise<T> {
    return new Promise<T>((resolve, reject) => {
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
        resolve(value) { signal.removeEventListener("abort", cancel); resolve(value as T); },
        reject(error) { signal.removeEventListener("abort", cancel); reject(error); },
      });
      emit({ type: "request", requestId: id, request: body });
    });
  }
  const options: Options = {
    cwd: config.cwd,
    pathToClaudeCodeExecutable: config.program,
    ...(config.model ? { model: config.model } : {}),
    ...(config.effort ? { effort: config.effort } : {}),
    ...(config.browser ? { mcpServers: { agent_browser: config.browser } } : {}),
    settingSources: ["user", "project", "local"],
    systemPrompt: { type: "preset", preset: "claude_code" },
    includePartialMessages: true,
    extraArgs: { "replay-user-messages": null },
    spawnClaudeCodeProcess,
    ...(config.sessionId ? config.resume ? { resume: config.sessionId } : { sessionId: config.sessionId } : {}),
    canUseTool: (toolName, input, context) => request<PermissionResult>(context.requestId, {
      type: "tool", toolName, input, toolUseId: context.toolUseID,
    }, context.signal),
    onElicitation: (body, context) => request<ElicitationResult>(context.requestId, {
      type: "elicitation", serverName: body.serverName, message: body.message,
      mode: body.mode, url: body.url, requestedSchema: body.requestedSchema,
    }, context.signal),
    onUserDialog: async () => ({ behavior: "cancelled" }),
    ...(config.probe ? probeOptions(existsSync(managedMcpPath), process.env) : {}),
  };
  const q = startQuery({ options, prompt: (async function* () {
    while (!closing) {
      if (!messages.length) await new Promise<void>((resolve) => { wake = resolve; });
      while (!closing && messages.length) yield messages.shift()!;
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
        const command = JSON.parse(line.value) as Command;
        switch (command.type) {
          case "message": {
            const message: SDKUserMessage = { type: "user", uuid: command.id, session_id: config.sessionId ?? "",
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
            catch (error) { emit({ type: "interrupted", requestId: command.requestId, error: (error as Error).message }); }
            break;
          case "usage":
            try { emit({ type: "usage", usage: await q.usage_EXPERIMENTAL_MAY_CHANGE_DO_NOT_RELY_ON_THIS_API_YET({ skipBehaviors: true }) }); }
            catch (error) { emit({ type: "usage", error: (error as Error).message }); }
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

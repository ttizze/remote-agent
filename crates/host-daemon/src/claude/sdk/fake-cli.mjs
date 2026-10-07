// A process-level fixture for the real SDK; it makes no model or network calls.
import { createInterface } from "node:readline";
const send = (frame) => process.stdout.write(`${JSON.stringify(frame)}\n`);
let prompt;
for await (const line of createInterface({ input: process.stdin })) {
  const frame = JSON.parse(line);
  if (frame.type === "control_request") {
    if (frame.request.subtype === "interrupt") send({ type: "control_cancel_request", request_id: "permission" });
    send({ type: "control_response", response: { subtype: "success", request_id: frame.request_id, response: { commands: [], models: [], account: {} } } });
  } else if (frame.type === "user") {
    prompt = frame;
    const request = prompt.message.content === "dialog"
      ? { subtype: "request_user_dialog", dialog_kind: "resume_return", payload: { sessionAgeMinutes: 90, estimatedTokens: 120000 } }
      : { subtype: "can_use_tool", tool_name: "Bash", input: { command: "echo test" }, tool_use_id: "tool", description: "Run the test fixture" };
    send({ type: "control_request", request_id: "permission", request });
  } else if (frame.type === "control_response") {
    send({ type: "assistant", uuid: `answer-${prompt.uuid}`, session_id: "session", parent_tool_use_id: null,
      message: { role: "assistant", content: [{ type: "text", text: frame.response.response.result ?? frame.response.response.behavior }] } });
    send({ type: "result", subtype: "success", session_id: "session", uuid: `result-${prompt.uuid}`, is_error: false,
      result: "done", num_turns: 1, duration_ms: 1, duration_api_ms: 1, total_cost_usd: 0, usage: {}, modelUsage: {}, permission_denials: [] });
  }
}

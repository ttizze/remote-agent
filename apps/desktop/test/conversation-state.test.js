import assert from "node:assert/strict";
import test from "node:test";

import { projectConversation, reduceConversationState } from "../dist/conversation-state.js";

test("完了Turnをユーザー本文・折り畳み可能な作業・最終回答へ投影する", () => {
  const result = projectConversation({
    id: "thread-1",
    turns: [{
      id: "turn-1",
      status: "completed",
      durationMs: 40_000,
      items: [
        { id: "user-1", type: "userMessage", text: "起動してみて" },
        { id: "reasoning-1", type: "reasoning", summary: ["起動状態を確認"] },
        { id: "command-1", type: "commandExecution", command: "cargo tauri dev", status: "completed" },
        { id: "commentary-1", type: "agentMessage", phase: "commentary", text: "アプリを起動します。" },
        { id: "final-1", type: "agentMessage", phase: "final_answer", text: "起動しました。" },
      ],
    }],
  }, []);

  assert.deepEqual(result, [{
    id: "turn-1",
    userItems: [{ id: "user-1", type: "userMessage", text: "起動してみて" }],
    activityItems: [
      { id: "reasoning-1", type: "reasoning", summary: ["起動状態を確認"] },
      { id: "command-1", type: "commandExecution", command: "cargo tauri dev", status: "completed" },
      { id: "commentary-1", type: "agentMessage", phase: "commentary", text: "アプリを起動します。" },
    ],
    finalItems: [{ id: "final-1", type: "agentMessage", phase: "final_answer", text: "起動しました。" }],
    pendingRequests: [],
    work: { label: "40s間作業しました", collapsible: true, initiallyExpanded: false },
    showThinking: false,
    error: null,
  }]);
});

test("完了済みの出力なしTurnにThinking placeholderを復元しない", () => {
  const result = projectConversation({
    id: "thread-1",
    turns: [{
      id: "turn-1",
      status: "completed",
      items: [{ id: "user-1", type: "userMessage", text: "追加メッセージ" }],
    }],
  }, []);

  assert.equal(result[0].showThinking, false);
});

test("Live Eventのdeltaを同じCodex Thread snapshotへ反映する", () => {
  let state = { thread: { id: "thread-1", turns: [] }, pendingRequests: [] };
  state = reduceConversationState(state, {
    method: "turn/started",
    params: { threadId: "thread-1", turn: { id: "turn-1", status: "inProgress", items: [] } },
  });
  state = reduceConversationState(state, {
    method: "item/started",
    params: { threadId: "thread-1", turnId: "turn-1", item: { id: "agent-1", type: "agentMessage", phase: "commentary", text: "" } },
  });
  state = reduceConversationState(state, {
    method: "item/agentMessage/delta",
    params: { threadId: "thread-1", turnId: "turn-1", itemId: "agent-1", delta: "確認しています" },
  });
  state = reduceConversationState(state, {
    method: "item/completed",
    params: { threadId: "thread-1", turnId: "turn-1", item: { id: "agent-1", type: "agentMessage", phase: "commentary", text: "確認しています。" } },
  });

  assert.deepEqual(state.thread.turns, [{
    id: "turn-1",
    status: "inProgress",
    items: [{ id: "agent-1", type: "agentMessage", phase: "commentary", text: "確認しています。" }],
  }]);
});

test("Approval Requestを解決通知まで会話状態へ保持する", () => {
  let state = { thread: { id: "thread-1", turns: [] }, pendingRequests: [] };
  state = reduceConversationState(state, {
    id: "request-1",
    method: "item/commandExecution/requestApproval",
    params: { threadId: "thread-1", turnId: "turn-1", command: "npm test" },
  });
  assert.deepEqual(state.pendingRequests, [{
    id: "request-1",
    method: "item/commandExecution/requestApproval",
    params: { threadId: "thread-1", turnId: "turn-1", command: "npm test" },
  }]);

  state = reduceConversationState(state, {
    method: "serverRequest/resolved",
    params: { requestId: "request-1" },
  });
  assert.deepEqual(state.pendingRequests, []);
});

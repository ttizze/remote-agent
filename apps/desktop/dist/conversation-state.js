export function projectConversation(thread, pendingRequests) {
  return (thread?.turns ?? []).map((turn) => projectTurn(turn, pendingRequests));
}

export function reduceConversationState(state, message) {
  const next = cloneConversationState(state);
  const params = message?.params ?? {};
  if (message?.method === "serverRequest/resolved") {
    return resolveServerRequest(next, params);
  }
  if (message?.method && Object.prototype.hasOwnProperty.call(message, "id")) {
    return upsertPendingRequest(next, message, params);
  }
  if (!next.thread || (params.threadId && params.threadId !== next.thread.id)) return next;

  reduceThreadState(next.thread, message.method, params);
  return next;
}

function cloneConversationState(state) {
  return {
    ...state,
    thread: state.thread == null ? null : structuredClone(state.thread),
    pendingRequests: structuredClone(state.pendingRequests ?? []),
  };
}

function resolveServerRequest(state, params) {
  state.pendingRequests = state.pendingRequests.filter((request) => requestKey(request.id) !== requestKey(params.requestId));
  return state;
}

function upsertPendingRequest(state, message, params) {
  const request = { id: message.id, method: message.method, params };
  const index = state.pendingRequests.findIndex((candidate) => requestKey(candidate.id) === requestKey(message.id));
  if (index >= 0) state.pendingRequests[index] = request;
  else state.pendingRequests.push(request);
  return state;
}

function reduceThreadState(thread, method, params) {
  switch (method) {
    case "turn/started":
      applyTurnStarted(thread, params);
      break;
    case "turn/completed":
      applyTurnCompleted(thread, params);
      break;
    case "item/started":
    case "item/completed":
      applyItemChange(thread, params);
      break;
    case "item/agentMessage/delta":
      applyAgentMessageDelta(thread, params);
      break;
    case "item/reasoning/textDelta":
    case "item/reasoning/summaryTextDelta":
      appendDelta(thread, params, "summary", "reasoning");
      break;
    case "item/commandExecution/outputDelta":
      appendDelta(thread, params, "aggregatedOutput", "commandExecution");
      break;
    case "error":
      applyError(thread, params);
      break;
    case "thread/status/changed":
      thread.status = params.status;
      break;
  }
}

function applyTurnStarted(thread, params) {
  upsertTurn(thread, {
    ...(params.turn ?? {}),
    id: params.turn?.id ?? params.turnId,
    status: "inProgress",
    items: params.turn?.items ?? [],
  });
}

function applyTurnCompleted(thread, params) {
  const incoming = params.turn ?? {};
  const turn = ensureTurn(thread, incoming.id ?? params.turnId);
  Object.assign(turn, incoming, { status: incoming.status ?? "completed" });
}

function applyItemChange(thread, params) {
  const turn = ensureTurn(thread, params.turnId);
  upsertItem(turn, params.item ?? {});
}

function applyAgentMessageDelta(thread, params) {
  const turn = ensureTurn(thread, params.turnId);
  let item = turn.items.find((candidate) => candidate.id === params.itemId);
  if (!item) {
    item = { id: params.itemId, type: "agentMessage", text: "" };
    turn.items.push(item);
  }
  item.text = `${item.text ?? ""}${params.delta ?? ""}`;
}

function applyError(thread, params) {
  const turn = ensureTurn(thread, params.turnId);
  turn.error = { ...(params.error ?? {}), willRetry: params.willRetry === true };
}

function requestKey(id) {
  return typeof id === "string" ? id : JSON.stringify(id);
}

function ensureTurn(thread, turnId) {
  thread.turns ??= [];
  let turn = thread.turns.find((candidate) => candidate.id === turnId);
  if (!turn) {
    turn = { id: turnId, status: "inProgress", items: [] };
    thread.turns.push(turn);
  }
  turn.items ??= [];
  return turn;
}

function upsertTurn(thread, incoming) {
  const turn = ensureTurn(thread, incoming.id);
  const items = turn.items;
  Object.assign(turn, incoming);
  if (!incoming.items?.length) turn.items = items;
}

function upsertItem(turn, item) {
  const index = turn.items.findIndex((candidate) => candidate.id === item.id);
  if (index >= 0) turn.items[index] = item;
  else turn.items.push(item);
}

function appendDelta(thread, params, field, type) {
  const turn = ensureTurn(thread, params.turnId);
  let item = turn.items.find((candidate) => candidate.id === params.itemId);
  if (!item) {
    item = { id: params.itemId, type };
    turn.items.push(item);
  }
  item[field] = `${item[field] ?? ""}${params.delta ?? ""}`;
}

function projectTurn(turn, pendingRequests) {
  const items = turn.items ?? [];
  const userItems = items.filter((item) => item.type === "userMessage");
  const agentItems = items.filter((item) => item.type === "agentMessage");
  const explicitFinal = agentItems.filter((item) => item.phase === "final_answer");
  const fallbackFinal = explicitFinal.length === 0
    ? agentItems.filter((item) => item.phase == null).slice(-1)
    : [];
  const finalItems = [...explicitFinal, ...fallbackFinal];
  const finalIds = new Set(finalItems.map((item) => item.id));
  const activityItems = items.filter((item) => (
    item.type !== "userMessage"
      && !finalIds.has(item.id)
      && !["sleep", "enteredReviewMode", "exitedReviewMode"].includes(item.type)
  ));
  const requests = pendingRequests.filter((request) => (
    request.params?.turnId == null || request.params.turnId === turn.id
  ));
  const duration = formatDuration(turn.durationMs);
  const completedWithFinal = turn.status === "completed" && finalItems.length > 0;

  return {
    id: turn.id,
    userItems,
    activityItems,
    finalItems,
    pendingRequests: requests,
    work: activityItems.length === 0 ? null : {
      label: workLabel(turn.status, duration, requests.length > 0),
      collapsible: completedWithFinal && requests.length === 0,
      initiallyExpanded: !completedWithFinal || requests.length > 0,
    },
    showThinking: turn.status === "inProgress"
      && activityItems.length === 0
      && finalItems.length === 0
      && requests.length === 0,
    error: turn.error ?? null,
  };
}

function workLabel(status, duration, waiting) {
  if (waiting) return "確認を待っています";
  if (status === "inProgress") return duration ? `${duration} 作業中` : "作業中…";
  if (status === "interrupted") return duration ? `${duration}後に停止しました` : "作業を停止しました";
  if (status === "failed") return duration ? `${duration}後に失敗しました` : "作業に失敗しました";
  return duration ? `${duration}間作業しました` : "作業しました";
}

function formatDuration(milliseconds) {
  if (!Number.isFinite(milliseconds) || milliseconds < 0) return "";
  const seconds = Math.max(1, Math.round(milliseconds / 1000));
  if (seconds < 60) return `${seconds}s`;
  const minutes = Math.floor(seconds / 60);
  const rest = seconds % 60;
  return rest ? `${minutes}m ${rest}s` : `${minutes}m`;
}

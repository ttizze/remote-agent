export function projectConversation(thread, pendingRequests) {
  return (thread?.turns ?? []).map((turn) => projectTurn(turn, pendingRequests));
}

export function reduceConversationState(state, message) {
  const next = {
    ...state,
    thread: state.thread == null ? null : structuredClone(state.thread),
    pendingRequests: structuredClone(state.pendingRequests ?? []),
  };
  const params = message?.params ?? {};
  if (message?.method === "serverRequest/resolved") {
    next.pendingRequests = next.pendingRequests.filter((request) => requestKey(request.id) !== requestKey(params.requestId));
    return next;
  }
  if (message?.method && Object.prototype.hasOwnProperty.call(message, "id")) {
    const request = { id: message.id, method: message.method, params };
    const index = next.pendingRequests.findIndex((candidate) => requestKey(candidate.id) === requestKey(message.id));
    if (index >= 0) next.pendingRequests[index] = request;
    else next.pendingRequests.push(request);
    return next;
  }
  if (!next.thread || (params.threadId && params.threadId !== next.thread.id)) return next;

  if (message.method === "turn/started") {
    upsertTurn(next.thread, {
      ...(params.turn ?? {}),
      id: params.turn?.id ?? params.turnId,
      status: "inProgress",
      items: params.turn?.items ?? [],
    });
  } else if (message.method === "turn/completed") {
    const incoming = params.turn ?? {};
    const turn = ensureTurn(next.thread, incoming.id ?? params.turnId);
    Object.assign(turn, incoming, { status: incoming.status ?? "completed" });
  } else if (message.method === "item/started" || message.method === "item/completed") {
    const turn = ensureTurn(next.thread, params.turnId);
    upsertItem(turn, params.item ?? {});
  } else if (message.method === "item/agentMessage/delta") {
    const turn = ensureTurn(next.thread, params.turnId);
    let item = turn.items.find((candidate) => candidate.id === params.itemId);
    if (!item) {
      item = { id: params.itemId, type: "agentMessage", text: "" };
      turn.items.push(item);
    }
    item.text = `${item.text ?? ""}${params.delta ?? ""}`;
  } else if (message.method === "item/reasoning/textDelta" || message.method === "item/reasoning/summaryTextDelta") {
    appendDelta(next.thread, params, "summary", "reasoning");
  } else if (message.method === "item/commandExecution/outputDelta") {
    appendDelta(next.thread, params, "aggregatedOutput", "commandExecution");
  } else if (message.method === "error") {
    const turn = ensureTurn(next.thread, params.turnId);
    turn.error = { ...(params.error ?? {}), willRetry: params.willRetry === true };
  } else if (message.method === "thread/status/changed") {
    next.thread.status = params.status;
  }
  return next;
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

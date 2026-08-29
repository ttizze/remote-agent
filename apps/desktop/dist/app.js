import { projectConversation, reduceConversationState } from "./conversation-state.js";
import { createTauriBridge } from "./tauri-bridge.js";

export function createDesktopApp({ window, bridge = createTauriBridge(window) }) {
  const { document } = window;

  const state = {
    connection: "connecting",
    projects: [],
    threads: [],
    selectedThread: null,
    pendingRequests: new Map(),
    collapsedProjects: new Set(),
    query: "",
    sending: false,
    refreshTimer: null,
  };

  let eventsBound = false;
  let eventsListening = false;
  let eventsListeningPromise = null;

  const dom = Object.fromEntries([
    "app", "refresh-button", "connection-dot", "sidebar-toggle", "review-toggle", "new-task-button", "thread-search",
    "sidebar-state", "project-list", "platform-label", "conversation-header",
    "thread-project", "thread-title", "thread-status", "header-new-task", "empty-state",
    "empty-new-task", "conversation", "messages", "scroll-anchor", "composer",
    "composer-form", "message-input", "composer-cwd", "stop-button", "send-button",
    "review-additions", "review-deletions", "review-branch", "review-files",
    "new-task-dialog", "new-task-form", "close-dialog", "cancel-new-task",
    "new-task-project", "new-task-cwd", "cwd-options", "new-task-prompt",
    "new-task-error", "create-task-button", "toast",
  ].map((id) => [camel(id), document.getElementById(id)]));

  function camel(value) {
    return value.replace(/-([a-z])/g, (_, letter) => letter.toUpperCase());
  }

  function node(tag, className, text) {
    const element = document.createElement(tag);
    if (className) element.className = className;
    if (text !== undefined) element.textContent = text;
    return element;
  }

  async function rpc(method, params = {}) {
    return bridge.request(method, params);
  }

  async function initialize() {
    bindEvents();
    try {
      await listenForCodexEvents();
      const info = await bridge.connect();
      state.connection = "connected";
      dom.app.dataset.connection = "connected";
      dom.connectionDot.title = `${info.userAgent} に接続済み`;
      dom.platformLabel.textContent = `${info.platformOs} · ${info.userAgent}`;
      await loadWorkspace();
    } catch (error) {
      state.connection = "failed";
      dom.app.dataset.connection = "failed";
      dom.connectionDot.title = "Codexに接続できません";
      showSidebarError(readableError(error));
      toast(readableError(error));
    }
  }

  async function listenForCodexEvents() {
    if (eventsListening) return;
    if (!eventsListeningPromise) {
      eventsListeningPromise = (async () => {
        await bridge.listen((payload) => handleCodexMessage(payload));
        eventsListening = true;
      })().finally(() => {
        eventsListeningPromise = null;
      });
    }
    await eventsListeningPromise;
  }

  function bindEvents() {
    if (eventsBound) return;
    eventsBound = true;
    dom.sidebarToggle.addEventListener("click", () => {
      const sidebar = document.getElementById("app-sidebar");
      const open = !sidebar.hidden;
      sidebar.hidden = open;
      dom.app.dataset.sidebar = open ? "closed" : "open";
      dom.sidebarToggle.setAttribute("aria-expanded", String(!open));
      dom.sidebarToggle.setAttribute("aria-label", open ? "サイドバーを開く" : "サイドバーを閉じる");
    });
    dom.reviewToggle.addEventListener("click", () => {
      const pane = document.getElementById("review-pane");
      const open = !pane.hidden;
      pane.hidden = open;
      dom.app.dataset.review = open ? "closed" : "open";
      dom.reviewToggle.setAttribute("aria-expanded", String(!open));
      dom.reviewToggle.setAttribute("aria-label", open ? "変更レビューを開く" : "変更レビューを閉じる");
    });
    dom.refreshButton.addEventListener("click", () => loadWorkspace(true));
    [dom.newTaskButton, dom.headerNewTask, dom.emptyNewTask].forEach((button) => {
      button.addEventListener("click", openNewTaskDialog);
    });
    dom.threadSearch.addEventListener("input", (event) => {
      state.query = event.target.value.trim().toLocaleLowerCase();
      renderSidebar();
    });
    dom.composerForm.addEventListener("submit", sendMessage);
    dom.messageInput.addEventListener("input", resizeComposer);
    dom.messageInput.addEventListener("keydown", (event) => {
      if (event.key === "Enter" && (event.metaKey || event.ctrlKey)) {
        event.preventDefault();
        dom.composerForm.requestSubmit();
      }
    });
    dom.stopButton.addEventListener("click", interruptTurn);
    dom.closeDialog.addEventListener("click", () => dom.newTaskDialog.close());
    dom.cancelNewTask.addEventListener("click", () => dom.newTaskDialog.close());
    dom.newTaskProject.addEventListener("change", syncTaskRoots);
    dom.newTaskForm.addEventListener("submit", createTask);
    window.addEventListener("keydown", (event) => {
      if ((event.metaKey || event.ctrlKey) && event.key.toLocaleLowerCase() === "k") {
        event.preventDefault();
        dom.threadSearch.focus();
      }
      if ((event.metaKey || event.ctrlKey) && event.key.toLocaleLowerCase() === "n") {
        event.preventDefault();
        openNewTaskDialog();
      }
    });
  }

  async function loadWorkspace(notify = false) {
    showSidebarLoading("プロジェクトとタスクを読み込んでいます");
    try {
      const [projects, threads] = await Promise.all([
        listAll("host/project/list", "data"),
        listAll("host/thread/list", "data"),
      ]);
      state.projects = projects.sort((a, b) => (a.position ?? 0) - (b.position ?? 0));
      state.threads = threads.sort((a, b) => timestamp(b.updatedAt) - timestamp(a.updatedAt));
      dom.sidebarState.hidden = true;
      renderSidebar();
      if (notify) toast("タスク一覧を更新しました");
    } catch (error) {
      showSidebarError(readableError(error));
    }
  }

  async function listAll(method, key) {
    const values = [];
    const seen = new Set();
    let cursor;
    for (let page = 0; page < 64 && values.length < 512; page += 1) {
      const result = await rpc(method, cursor ? { limit: 100, cursor } : { limit: 100 });
      for (const value of Array.isArray(result?.[key]) ? result[key] : []) {
        const identity = `${value.id ?? ""}:${value.cwd ?? ""}`;
        if (!seen.has(identity)) {
          seen.add(identity);
          values.push(value);
        }
      }
      if (!result?.nextCursor || result.nextCursor === cursor) break;
      cursor = result.nextCursor;
    }
    return values;
  }

  function showSidebarLoading(message) {
    dom.sidebarState.hidden = false;
    dom.sidebarState.replaceChildren(node("span", "spinner"), node("span", "", message));
  }

  function showSidebarError(message) {
    dom.sidebarState.hidden = false;
    const wrapper = node("div");
    wrapper.append(node("strong", "", "接続できません"), node("p", "", message));
    const retry = node("button", "secondary-button", "再試行");
    retry.type = "button";
    retry.addEventListener("click", initialize);
    dom.sidebarState.replaceChildren(wrapper, retry);
  }

  function renderSidebar() {
    const selectedId = state.selectedThread?.id;
    const knownProjectIds = new Set(state.projects.map((project) => project.id));
    const groups = state.projects.map((project) => ({
      project,
      threads: state.threads.filter((thread) => thread.projectId === project.id),
    }));
    const unassigned = state.threads.filter((thread) => !thread.projectId || !knownProjectIds.has(thread.projectId));
    if (unassigned.length) {
      groups.push({ project: { id: "__other", name: "その他", roots: [] }, threads: unassigned });
    }

    const fragment = document.createDocumentFragment();
    for (const { project, threads } of groups) {
      const filtered = threads.filter(matchesQuery);
      if (state.query && !filtered.length && !project.name.toLocaleLowerCase().includes(state.query)) continue;
      const group = node("section", "project-group");
      if (state.collapsedProjects.has(project.id)) group.classList.add("is-collapsed");
      const header = node("button", "project-header");
      header.type = "button";
      header.append(node("span", "chevron", "⌄"), node("strong", "", project.name), node("span", "project-count", String(filtered.length)));
      header.addEventListener("click", () => {
        if (state.collapsedProjects.has(project.id)) state.collapsedProjects.delete(project.id);
        else state.collapsedProjects.add(project.id);
        renderSidebar();
      });
      const rows = node("div", "project-threads");
      if (!filtered.length) {
        rows.append(node("div", "empty-project", state.query ? "一致するタスクなし" : "タスクはまだありません"));
      }
      for (const thread of filtered) {
        const row = node("button", `thread-row${thread.id === selectedId ? " is-selected" : ""}`);
        row.type = "button";
        const title = node("strong", "", threadTitle(thread));
        const active = isThreadActive(thread);
        const meta = node("span", active ? "active-label" : "", active ? "作業中…" : relativeTime(thread.updatedAt));
        row.append(title, meta);
        row.addEventListener("click", () => openThread(thread.id));
        rows.append(row);
      }
      group.append(header, rows);
      fragment.append(group);
    }
    if (!groups.length) fragment.append(node("div", "empty-project", "Codexプロジェクトはまだありません"));
    dom.projectList.replaceChildren(fragment);
  }

  function matchesQuery(thread) {
    if (!state.query) return true;
    return [thread.name, thread.preview, thread.cwd].filter(Boolean).join(" ").toLocaleLowerCase().includes(state.query);
  }

  async function openThread(threadId) {
    dom.threadTitle.textContent = "読み込んでいます…";
    try {
      const result = await rpc("host/thread/read", { threadId, includeTurns: true });
      state.selectedThread = result.thread ?? result;
      renderSidebar();
      renderConversation();
      await loadReview(state.selectedThread.cwd);
      requestAnimationFrame(() => dom.scrollAnchor.scrollIntoView({ block: "end" }));
    } catch (error) {
      toast(readableError(error));
    }
  }

  async function loadReview(cwd) {
    if (!cwd || !bridge.review) {
      renderReview(null);
      return;
    }
    try {
      renderReview(await bridge.review(cwd));
    } catch (error) {
      renderReview(null, readableError(error));
    }
  }

  function renderReview(review, error = "") {
    dom.reviewAdditions.textContent = `+${review?.additions ?? 0}`;
    dom.reviewDeletions.textContent = `-${review?.deletions ?? 0}`;
    dom.reviewBranch.textContent = review?.branch || "—";
    if (error) {
      dom.reviewFiles.replaceChildren(node("p", "", error));
      return;
    }
    const files = review?.files ?? [];
    if (!files.length) {
      dom.reviewFiles.replaceChildren(node("p", "", review ? "変更はありません。" : "チャットを開くとリポジトリの変更を表示します。"));
      return;
    }
    dom.reviewFiles.replaceChildren(...files.map((file) => {
      const row = node("div", "review-file");
      row.append(node("span", `review-file-status is-${file.status}`, file.status.slice(0, 1).toUpperCase()), node("span", "review-file-path", file.path));
      return row;
    }));
  }

  function renderConversation() {
    const thread = state.selectedThread;
    const hasThread = Boolean(thread);
    dom.emptyState.hidden = hasThread;
    dom.conversation.hidden = !hasThread;
    dom.composer.hidden = !hasThread;
    dom.conversationHeader.classList.toggle("is-empty", !hasThread);
    if (!thread) return;

    const project = state.projects.find((candidate) => candidate.id === thread.projectId);
    dom.threadProject.textContent = project?.name ?? compactPath(thread.cwd) ?? "Codex";
    dom.threadTitle.textContent = threadTitle(thread);
    dom.composerCwd.textContent = thread.cwd || "workspace";
    const activeTurn = currentActiveTurn();
    dom.threadStatus.textContent = activeTurn ? "作業中" : "待機中";
    dom.threadStatus.classList.toggle("is-active", Boolean(activeTurn));
    dom.stopButton.hidden = !activeTurn;
    dom.sendButton.hidden = Boolean(activeTurn);

    const fragment = document.createDocumentFragment();
    const projectedTurns = projectConversation(thread, [...state.pendingRequests.values()]);
    for (const turn of projectedTurns) fragment.append(renderTurn(turn));
    if (!(thread.turns ?? []).length) {
      fragment.append(node("div", "empty-project", "このタスクにはまだメッセージがありません"));
    }
    dom.messages.replaceChildren(fragment);
  }

  function renderTurn(turn) {
    const container = node("article", "turn");
    for (const item of turn.userItems) {
      const wrapper = node("div", "user-message");
      wrapper.append(node("div", "message-body", textLike(item) || attachmentLabel(item)));
      container.append(wrapper);
    }

    if (turn.work) {
      const details = node("details", "work-block");
      details.open = turn.work.initiallyExpanded;
      const summary = node("summary", "", turn.work.label);
      const content = node("div", "work-items");
      for (const item of turn.activityItems) content.append(renderActivity(item));
      details.append(summary, content);
      container.append(details);
    } else if (turn.showThinking) {
      const thinking = node("div", "thinking");
      const dots = node("span", "thinking-dots");
      dots.append(node("i"), node("i"), node("i"));
      thinking.append(dots, node("span", "", "考えています"));
      container.append(thinking);
    }

    for (const request of turn.pendingRequests) container.append(renderRequest(request));
    if (turn.error) container.append(node("div", "error-card", turn.error.message || "Codexでエラーが発生しました"));
    for (const item of turn.finalItems) {
      const wrapper = node("div", "assistant-message");
      wrapper.append(node("div", "message-body", textLike(item)));
      container.append(wrapper);
    }
    return container;
  }

  function renderActivity(item) {
    if (item.type === "agentMessage") return node("div", "commentary", textLike(item));
    const details = node("details", "work-item");
    if (item.status === "inProgress") details.open = true;
    details.append(node("summary", "", activityTitle(item)));
    const content = node("div", "work-content");
    if (item.type === "commandExecution") {
      if (item.cwd) content.append(node("div", "commentary", `cwd: ${item.cwd}`));
      content.append(node("pre", "", [item.command, item.aggregatedOutput].filter(Boolean).join("\n\n")));
    } else if (item.type === "fileChange") {
      for (const change of item.changes ?? []) {
        const changeNode = node("div", "file-change");
        changeNode.append(node("strong", "", `${change.kind?.type ?? "update"} · ${change.path ?? "file"}`));
        if (change.diff) changeNode.append(node("pre", "", change.diff));
        content.append(changeNode);
      }
    } else {
      const value = textLike(item) || safeStringify(item);
      content.append(node("pre", "", value));
    }
    details.append(content);
    return details;
  }

  function activityTitle(item) {
    const type = item.type;
    switch (type) {
      case "reasoning": return "Reasoning";
      case "commandExecution": return `$ ${firstLine(item.command) || "command"} · ${item.status ?? "completed"}`;
      case "fileChange": return `${item.changes?.length ?? 0} files changed · ${item.status ?? "completed"}`;
      default: return activityTitleByType(item, type);
    }
  }

  function activityTitleByType(item, type) {
    switch (type) {
      case "plan": return "計画を更新しました";
      case "mcpToolCall": return [item.server, item.tool].filter(Boolean).join(" / ") || "MCPツールを実行しました";
      case "dynamicToolCall": return `${item.tool ?? "ツール"}を実行しました`;
      case "collabAgentToolCall": return "サブエージェントを操作しました";
      case "subAgentActivity": return "サブエージェントが作業しました";
      case "webSearch": return item.query ? `Webを検索: ${firstLine(item.query)}` : "Webを検索しました";
      case "imageView": return "画像を確認しました";
      case "imageGeneration": return "画像を生成しました";
      case "contextCompaction": return "コンテキストを圧縮しました";
      default: return `Codex item · ${item.type ?? "unknown"}`;
    }
  }

  function renderRequest(request) {
    const card = node("section", "request-card");
    const title = requestTitle(request.method);
    card.append(node("h4", "", title));
    const body = request.params?.reason ?? request.params?.message ?? commandText(request.params) ?? "操作を続けるには応答が必要です";
    card.append(node("p", "", body));

    if (request.method === "item/tool/requestUserInput") {
      const form = node("form");
      for (const question of request.params?.questions ?? []) {
        const label = node("label", "request-question");
        label.append(node("span", "", question.question ?? question.header ?? "回答"));
        let input;
        if (Array.isArray(question.options) && question.options.length) {
          input = node("select");
          for (const option of question.options) {
            const optionNode = node("option", "", option.label);
            optionNode.value = option.label;
            input.append(optionNode);
          }
        } else {
          input = node("input");
          input.type = question.isSecret ? "password" : "text";
        }
        input.dataset.questionId = question.id;
        label.append(input);
        form.append(label);
      }
      const actions = node("div", "request-actions");
      const submit = node("button", "approve", "回答する");
      submit.type = "submit";
      actions.append(submit);
      form.append(actions);
      form.addEventListener("submit", async (event) => {
        event.preventDefault();
        const answers = {};
        for (const input of form.querySelectorAll("[data-question-id]")) {
          answers[input.dataset.questionId] = { answers: [input.value] };
        }
        await respondToRequest(request, { answers });
      });
      card.append(form);
      return card;
    }

    const actions = node("div", "request-actions");
    const supportedApprovals = new Set([
      "item/commandExecution/requestApproval",
      "item/fileChange/requestApproval",
      "item/permissions/requestApproval",
    ]);
    if (!supportedApprovals.has(request.method)) {
      const unsupported = node("button", "", "この要求はまだ操作できません");
      unsupported.type = "button";
      unsupported.disabled = true;
      actions.append(unsupported);
      card.append(actions);
      return card;
    }
    const approve = node("button", "approve", "許可");
    const decline = node("button", "", "拒否");
    approve.type = decline.type = "button";
    approve.addEventListener("click", () => approveRequest(request));
    decline.addEventListener("click", () => declineRequest(request));
    actions.append(approve, decline);
    card.append(actions);
    return card;
  }

  async function approveRequest(request) {
    if (request.method === "item/permissions/requestApproval") {
      await respondToRequest(request, { permissions: request.params?.permissions ?? {}, scope: "turn" });
      return;
    }
    await respondToRequest(request, { decision: "accept" });
  }

  async function declineRequest(request) {
    const decisionMethods = new Set([
      "item/commandExecution/requestApproval",
      "item/fileChange/requestApproval",
    ]);
    if (decisionMethods.has(request.method)) {
      await respondToRequest(request, { decision: "decline" });
      return;
    }
    try {
      await bridge.respondError(request.id, { code: -32000, message: "User declined the request" });
      removeRequest(request.id);
    } catch (error) {
      toast(readableError(error));
    }
  }

  async function respondToRequest(request, result) {
    try {
      await bridge.respond(request.id, result);
      removeRequest(request.id);
    } catch (error) {
      toast(readableError(error));
    }
  }

  function removeRequest(id) {
    state.pendingRequests.delete(requestKey(id));
    renderConversation();
  }

  async function sendMessage(event) {
    event.preventDefault();
    const text = dom.messageInput.value.trim();
    const thread = state.selectedThread;
    if (!text || !thread || state.sending) return;
    state.sending = true;
    dom.sendButton.disabled = true;
    try {
      const active = currentActiveTurn();
      if (active) {
        await rpc("turn/steer", {
          threadId: thread.id,
          expectedTurnId: active.id,
          input: [{ type: "text", text }],
        });
      } else {
        await rpc("thread/resume", { threadId: thread.id, cwd: thread.cwd });
        await rpc("turn/start", { threadId: thread.id, input: [{ type: "text", text }] });
      }
      dom.messageInput.value = "";
      resizeComposer();
      scheduleThreadRefresh();
    } catch (error) {
      toast(readableError(error));
    } finally {
      state.sending = false;
      dom.sendButton.disabled = false;
    }
  }

  async function interruptTurn() {
    const active = currentActiveTurn();
    if (!active || !state.selectedThread) return;
    try {
      await rpc("turn/interrupt", { threadId: state.selectedThread.id, turnId: active.id });
    } catch (error) {
      toast(readableError(error));
    }
  }

  function openNewTaskDialog() {
    dom.newTaskProject.replaceChildren();
    const standalone = node("option", "", "プロジェクトを指定しない");
    standalone.value = "";
    dom.newTaskProject.append(standalone);
    for (const project of state.projects) {
      const option = node("option", "", project.name);
      option.value = project.id;
      dom.newTaskProject.append(option);
    }
    const selectedProjectId = state.selectedThread?.projectId;
    if (selectedProjectId && state.projects.some((project) => project.id === selectedProjectId)) {
      dom.newTaskProject.value = selectedProjectId;
    } else if (state.projects.length) {
      dom.newTaskProject.value = state.projects[0].id;
    }
    syncTaskRoots();
    dom.newTaskPrompt.value = "";
    dom.newTaskError.textContent = "";
    dom.newTaskDialog.showModal();
    setTimeout(() => dom.newTaskPrompt.focus(), 0);
  }

  function syncTaskRoots() {
    const project = state.projects.find((candidate) => candidate.id === dom.newTaskProject.value);
    const roots = (project?.roots ?? []).map((root) => typeof root === "string" ? root : root.path).filter(Boolean);
    dom.cwdOptions.replaceChildren(...roots.map((root) => {
      const option = node("option");
      option.value = root;
      return option;
    }));
    dom.newTaskCwd.value = roots[0] ?? state.selectedThread?.cwd ?? "";
  }

  async function createTask(event) {
    event.preventDefault();
    const cwd = dom.newTaskCwd.value.trim();
    const prompt = dom.newTaskPrompt.value.trim();
    if (!cwd || !prompt) return;
    dom.createTaskButton.disabled = true;
    dom.newTaskError.textContent = "";
    try {
      const projectId = dom.newTaskProject.value || null;
      const started = await rpc("host/thread/start", projectId ? { projectId, cwd } : { cwd });
      const thread = started.thread ?? started;
      state.selectedThread = thread;
      renderConversation();
      dom.newTaskDialog.close();
      await rpc("turn/start", { threadId: thread.id, input: [{ type: "text", text: prompt }] });
      await loadWorkspace();
      if (!state.threads.some((candidate) => candidate.id === thread.id)) state.threads.unshift(thread);
      renderSidebar();
    } catch (error) {
      dom.newTaskError.textContent = readableError(error);
    } finally {
      dom.createTaskButton.disabled = false;
    }
  }

  function handleCodexMessage(payload) {
    let message;
    try {
      message = typeof payload === "string" ? JSON.parse(payload) : payload;
    } catch {
      return;
    }
    if (!message?.method) return;
    if (message.method === "remoteAgent/connectionClosed") {
      state.connection = "failed";
      dom.app.dataset.connection = "failed";
      toast("Codexとの接続が終了しました");
      return;
    }
    if (Object.prototype.hasOwnProperty.call(message, "id")) {
      applyConversationEvent(message);
      renderConversation();
      return;
    }
    reduceNotification(message.method, message.params ?? {});
  }

  function reduceNotification(method, params) {
    const thread = state.selectedThread;
    if (method === "serverRequest/resolved") {
      applyConversationEvent({ method, params });
      renderConversation();
      return;
    }
    if (!thread || (params.threadId && params.threadId !== thread.id)) {
      if (["thread/started", "turn/started", "turn/completed"].includes(method)) scheduleThreadRefresh();
      return;
    }

    applyConversationEvent({ method, params });
    renderConversation();
    scheduleThreadRefresh();
    requestAnimationFrame(() => dom.scrollAnchor.scrollIntoView({ block: "end", behavior: "smooth" }));
  }

  function applyConversationEvent(message) {
    const reduced = reduceConversationState({
      thread: state.selectedThread,
      pendingRequests: [...state.pendingRequests.values()],
    }, message);
    state.selectedThread = reduced.thread;
    state.pendingRequests = new Map(reduced.pendingRequests.map((request) => [requestKey(request.id), request]));
  }

  function scheduleThreadRefresh() {
    clearTimeout(state.refreshTimer);
    state.refreshTimer = setTimeout(async () => {
      try {
        state.threads = (await listAll("host/thread/list", "data"))
          .sort((a, b) => timestamp(b.updatedAt) - timestamp(a.updatedAt));
        renderSidebar();
      } catch {
        // Live conversation remains usable if the secondary list refresh fails.
      }
    }, 700);
  }

  function currentActiveTurn() {
    return [...(state.selectedThread?.turns ?? [])].reverse().find((turn) => turn.status === "inProgress") ?? null;
  }

  function requestKey(id) {
    return typeof id === "string" ? id : JSON.stringify(id);
  }

  function requestTitle(method) {
    return ({
      "item/commandExecution/requestApproval": "コマンドの承認待ち",
      "item/fileChange/requestApproval": "ファイル変更の承認待ち",
      "item/permissions/requestApproval": "追加権限の承認待ち",
      "item/tool/requestUserInput": "回答を待っています",
      "mcpServer/elicitation/request": "MCPからの確認",
      "item/tool/call": "ツールの確認",
    })[method] ?? "Codexからの確認";
  }

  function commandText(params) {
    const command = params?.command ?? params?.commands;
    if (Array.isArray(command)) return command.join("\n");
    return typeof command === "string" ? command : null;
  }

  function textLike(value) {
    if (!value) return "";
    if (typeof value.text === "string") return value.text;
    if (typeof value.summary === "string") return value.summary;
    if (Array.isArray(value.summary)) return value.summary.join("\n");
    if (typeof value.message === "string") return value.message;
    if (Array.isArray(value.content)) return value.content.map((part) => typeof part === "string" ? part : part.text ?? "").join("");
    return "";
  }

  function attachmentLabel(item) {
    const content = item.content ?? [];
    return content.length ? `添付ファイル ${content.length}件` : "メッセージ";
  }

  function threadTitle(thread) {
    return thread?.name || firstLine(thread?.preview) || "新しいタスク";
  }

  function firstLine(value) {
    return String(value ?? "").split(/\r?\n/, 1)[0].trim().slice(0, 120);
  }

  function compactPath(path) {
    if (!path) return "";
    const parts = path.split("/").filter(Boolean);
    return parts.slice(-2).join("/");
  }

  function isThreadActive(thread) {
    return thread?.status?.type === "active";
  }

  function timestamp(value) {
    const number = Number(value ?? 0);
    if (!Number.isFinite(number)) return 0;
    return number > 10_000_000_000 ? number : number * 1000;
  }

  function relativeTime(value) {
    const time = timestamp(value);
    if (!time) return "";
    const seconds = Math.max(0, Math.floor((Date.now() - time) / 1000));
    if (seconds < 60) return "たった今";
    const minutes = Math.floor(seconds / 60);
    if (minutes < 60) return `${minutes}分前`;
    const hours = Math.floor(minutes / 60);
    if (hours < 24) return `${hours}時間前`;
    const days = Math.floor(hours / 24);
    if (days < 7) return `${days}日前`;
    return new Intl.DateTimeFormat("ja-JP", { month: "short", day: "numeric" }).format(new Date(time));
  }

  function resizeComposer() {
    dom.messageInput.style.height = "auto";
    dom.messageInput.style.height = `${Math.min(dom.messageInput.scrollHeight, 190)}px`;
  }

  function safeStringify(value) {
    try { return JSON.stringify(value, null, 2); } catch { return String(value); }
  }

  function readableError(error) {
    if (typeof error === "string") return error;
    return error?.message || "不明なエラーが発生しました";
  }

  let toastTimer;
  function toast(message) {
    clearTimeout(toastTimer);
    dom.toast.textContent = message;
    dom.toast.classList.add("is-visible");
    toastTimer = setTimeout(() => dom.toast.classList.remove("is-visible"), 4200);
  }

  return { start: initialize };
}

if (typeof window !== "undefined") {
  createDesktopApp({ window }).start();
}

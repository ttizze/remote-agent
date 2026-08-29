import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

import { Window } from "happy-dom";

import { createDesktopApp } from "../dist/app.js";

async function renderedDesktop(rpcResults) {
  const window = new Window({ url: "tauri://localhost" });
  window.document.write(await readFile(new URL("../dist/index.html", import.meta.url), "utf8"));
  const calls = [];
  const bridge = {
    listen: async () => () => {},
    connect: async () => ({
      platformOs: "macos",
      userAgent: "codex-test",
    }),
    request: async (method, params) => {
      calls.push({ method, params });
      return structuredClone(rpcResults[method]);
    },
    review: async (cwd) => {
      calls.push({ method: "workspace/review", params: { cwd } });
      return structuredClone(rpcResults["workspace/review"]);
    },
    respond: async () => {},
    respondError: async () => {},
  };
  const app = createDesktopApp({ window, bridge });
  await app.start();
  return { window, calls };
}

test("公式Project配下に所属するChatを表示する", async () => {
  const { window } = await renderedDesktop({
    "host/project/list": {
      data: [{ id: "project-1", name: "remote-agent", roots: [{ path: "/work/remote-agent" }], position: 0 }],
      nextCursor: null,
    },
    "host/thread/list": {
      data: [{ id: "thread-1", projectId: "project-1", name: "TauriでCodex PC版を作る", updatedAt: 1_700_000_000 }],
      nextCursor: null,
    },
  });

  const sidebar = window.document.querySelector("[aria-label='プロジェクトとチャット']");
  assert.ok(sidebar, "公式のProject/Chatサイドバーが存在する");
  assert.match(sidebar.textContent, /remote-agent/);
  assert.match(sidebar.textContent, /TauriでCodex PC版を作る/);
});

test("サイドバーを閉じて会話領域を広げられる", async () => {
  const { window } = await renderedDesktop({
    "host/project/list": { data: [], nextCursor: null },
    "host/thread/list": { data: [], nextCursor: null },
  });

  const toggle = window.document.querySelector("[aria-controls='app-sidebar']");
  const sidebar = window.document.querySelector("#app-sidebar");
  assert.ok(toggle, "サイドバー切替が存在する");
  assert.ok(sidebar, "サイドバーが存在する");
  assert.equal(toggle.getAttribute("aria-expanded"), "true");

  toggle.click();

  assert.equal(toggle.getAttribute("aria-expanded"), "false");
  assert.equal(sidebar.hidden, true);
  assert.equal(window.document.querySelector("#app").dataset.sidebar, "closed");
});

test("Codexの主要導線を単一サイドバーへ表示する", async () => {
  const { window } = await renderedDesktop({
    "host/project/list": { data: [], nextCursor: null },
    "host/thread/list": { data: [], nextCursor: null },
  });

  const sidebar = window.document.querySelector("#app-sidebar");
  const labels = [...sidebar.querySelectorAll(".primary-nav button")].map((button) => button.textContent.trim());
  assert.deepEqual(labels, [
    "新しいチャット",
    "プルリクエスト",
    "サイト",
    "スケジュール",
    "プラグイン",
    "セキュリティ",
  ]);
  assert.equal(window.document.querySelector(".rail"), null, "独自アプリレールは表示しない");
});

test("変更レビューpaneを会話と独立して開閉できる", async () => {
  const { window } = await renderedDesktop({
    "host/project/list": { data: [], nextCursor: null },
    "host/thread/list": { data: [], nextCursor: null },
  });

  const toggle = window.document.querySelector("[aria-controls='review-pane']");
  const pane = window.document.querySelector("#review-pane");
  assert.ok(toggle, "レビューpane切替が存在する");
  assert.ok(pane, "レビューpaneが存在する");
  assert.match(pane.textContent, /変更/);
  assert.match(pane.textContent, /ローカル/);
  assert.match(pane.textContent, /コミットまたはプッシュ/);
  assert.match(pane.textContent, /ブランチを比較/);

  toggle.click();

  assert.equal(pane.hidden, true);
  assert.equal(window.document.querySelector("#app").dataset.review, "closed");
});

test("composerにCodexの実行コンテキスト操作を表示する", async () => {
  const { window } = await renderedDesktop({
    "host/project/list": { data: [], nextCursor: null },
    "host/thread/list": { data: [], nextCursor: null },
  });

  const composer = window.document.querySelector("#composer");
  const labels = [...composer.querySelectorAll("button[aria-label]")].map((button) => button.getAttribute("aria-label"));
  assert.deepEqual(labels, [
    "ファイルを添付",
    "権限を変更",
    "実行環境を変更",
    "モデルと推論レベルを変更",
    "音声入力",
    "停止",
    "送信",
  ]);
});

test("Projectから新しいChatをprimary folder付きで開始する", async () => {
  const { window, calls } = await renderedDesktop({
    "host/project/list": {
      data: [{ id: "project-1", name: "remote-agent", roots: [{ path: "/work/remote-agent" }], position: 0 }],
      nextCursor: null,
    },
    "host/thread/list": { data: [], nextCursor: null },
    "host/thread/start": { id: "thread-new", projectId: "project-1", cwd: "/work/remote-agent", turns: [] },
    "turn/start": { turn: { id: "turn-new", status: "inProgress", items: [] } },
  });

  window.document.querySelector("#new-task-button").click();
  window.document.querySelector("#new-task-prompt").value = "公式UIへ寄せる";
  window.document.querySelector("#new-task-form").dispatchEvent(new window.Event("submit", { bubbles: true, cancelable: true }));
  await window.happyDOM.whenAsyncComplete();

  const start = calls.find((call) => call.method === "host/thread/start");
  assert.deepEqual(start, {
    method: "host/thread/start",
    params: { projectId: "project-1", cwd: "/work/remote-agent" },
  });
});

test("ChatのGit変更をレビューpaneへ表示する", async () => {
  const { window, calls } = await renderedDesktop({
    "host/project/list": {
      data: [{ id: "project-1", name: "remote-agent", roots: [{ path: "/work/remote-agent" }], position: 0 }],
      nextCursor: null,
    },
    "host/thread/list": {
      data: [{ id: "thread-1", projectId: "project-1", name: "UIを修正", cwd: "/work/remote-agent", updatedAt: 1 }],
      nextCursor: null,
    },
    "host/thread/read": {
      thread: { id: "thread-1", projectId: "project-1", name: "UIを修正", cwd: "/work/remote-agent", turns: [] },
    },
    "workspace/review": {
      branch: "main",
      additions: 42,
      deletions: 7,
      files: [{ path: "apps/desktop/dist/app.js", status: "modified" }],
    },
  });

  window.document.querySelector(".thread-row").click();
  await window.happyDOM.whenAsyncComplete();

  const pane = window.document.querySelector("#review-pane");
  assert.match(pane.textContent, /main/);
  assert.match(pane.textContent, /\+42/);
  assert.match(pane.textContent, /-7/);
  assert.match(pane.textContent, /apps\/desktop\/dist\/app\.js/);
  assert.ok(calls.some((call) => call.method === "workspace/review" && call.params.cwd === "/work/remote-agent"));
});

test("初回接続失敗後の再試行でlistenerとDOMイベントを重複登録しない", async () => {
  const window = new Window({ url: "tauri://localhost" });
  window.document.write(await readFile(new URL("../dist/index.html", import.meta.url), "utf8"));
  const calls = [];
  const notificationHandlers = [];
  let connectCalls = 0;
  const bridge = {
    listen: async (handler) => {
      notificationHandlers.push(handler);
      return () => {};
    },
    connect: async () => {
      connectCalls += 1;
      if (connectCalls === 1) throw new Error("Codex unavailable");
      return { platformOs: "macos", userAgent: "codex-test" };
    },
    request: async (method, params) => {
      calls.push({ method, params });
      return { data: [], nextCursor: null };
    },
    respond: async () => {},
    respondError: async () => {},
  };
  const app = createDesktopApp({ window, bridge });

  await app.start();
  assert.equal(connectCalls, 1);
  assert.equal(notificationHandlers.length, 1);

  window.document.querySelector("#sidebar-state button").click();
  await window.happyDOM.whenAsyncComplete();
  assert.equal(connectCalls, 2);
  assert.equal(notificationHandlers.length, 1);

  calls.length = 0;
  window.document.querySelector("#refresh-button").click();
  await window.happyDOM.whenAsyncComplete();
  assert.deepEqual(calls.map((call) => call.method), ["host/project/list", "host/thread/list"]);
});

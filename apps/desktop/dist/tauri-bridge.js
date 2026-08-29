export function createTauriBridge(window) {
  const tauri = window.__TAURI__;
  const invoke = tauri?.core?.invoke;
  const listen = tauri?.event?.listen;
  if (!invoke || !listen) {
    return {
      listen: async () => {},
      connect: async () => { throw new Error("この画面はTauriアプリ内で開いてください"); },
      request: async () => { throw new Error("Tauri runtime is unavailable"); },
      respond: async () => { throw new Error("Tauri runtime is unavailable"); },
      respondError: async () => { throw new Error("Tauri runtime is unavailable"); },
    };
  }
  return {
    listen: (handler) => listen("codex-message", ({ payload }) => handler(payload)),
    connect: () => invoke("connect_codex"),
    request: (method, params) => invoke("codex_request", { method, params }),
    review: (cwd) => invoke("workspace_review", { cwd }),
    respond: (id, result) => invoke("codex_respond", { id, result }),
    respondError: (id, error) => invoke("codex_respond_error", { id, error }),
  };
}

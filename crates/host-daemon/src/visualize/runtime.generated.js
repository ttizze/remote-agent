// Generated from crates/host-daemon/src/visualize/runtime.ts; run npm run build.
(() => {
  const notice = (message) => {
    let element = document.getElementById("agent-runtime-notice");
    if (!element) {
      element = document.createElement("p");
      element.id = "agent-runtime-notice";
      element.setAttribute("role", "status");
      document.body.prepend(element);
    }
    element.textContent = message;
  };
  const unsupported = () => {
    notice("\u3053\u306ECodex\u5C02\u7528\u64CD\u4F5C\u306FBex\u3067\u306F\u5229\u7528\u3067\u304D\u307E\u305B\u3093\u3002\u4F1A\u8A71\u306E\u5165\u529B\u6B04\u304B\u3089\u4F9D\u983C\u3057\u3066\u304F\u3060\u3055\u3044\u3002");
    return Promise.reject(new Error("Codex host API is unavailable in Bex"));
  };
  window.openai = new Proxy({}, { get: () => unsupported });
  window.addEventListener("error", () => notice("\u8868\u793A\u306E\u4E00\u90E8\u3092\u5B9F\u884C\u3067\u304D\u307E\u305B\u3093\u3002\u5916\u90E8\u30E9\u30A4\u30D6\u30E9\u30EA\u3084Codex\u5C02\u7528\u6A5F\u80FD\u3078\u306E\u4F9D\u5B58\u3092\u78BA\u8A8D\u3057\u3066\u304F\u3060\u3055\u3044\u3002"));
  window.addEventListener("unhandledrejection", () => notice("\u3053\u306E\u64CD\u4F5C\u306FBex\u3067\u306F\u5229\u7528\u3067\u304D\u307E\u305B\u3093\u3002\u4F1A\u8A71\u306E\u5165\u529B\u6B04\u304B\u3089\u4F9D\u983C\u3057\u3066\u304F\u3060\u3055\u3044\u3002"));
  document.addEventListener("securitypolicyviolation", () => notice("\u5916\u90E8\u901A\u4FE1\u30FB\u8FFD\u52A0\u30D5\u30A1\u30A4\u30EB\u306E\u8AAD\u307F\u8FBC\u307F\u306F\u7121\u52B9\u3067\u3059\u3002\u5FC5\u8981\u306A\u30C7\u30FC\u30BF\u3092\u542B\u3080\u8868\u793A\u3092\u4F9D\u983C\u3057\u3066\u304F\u3060\u3055\u3044\u3002"));
  document.addEventListener("DOMContentLoaded", () => lucide.createIcons());
})()

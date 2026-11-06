// Generated from crates/agent-protocol/src/browser_input.ts; run npm run build.
(() => {
  let element = document.activeElement;
  for (let depth = 0; element && depth < 32; depth++) {
    let nested;
    try {
      nested = element.shadowRoot?.activeElement || element.contentDocument?.activeElement;
    } catch (_) {
      return null;
    }
    if (nested) {
      element = nested;
      continue;
    }
    if (element.disabled || element.readOnly) return null;
    const textInput = element.tagName === "INPUT" && ["text", "search", "email", "url", "tel", "number", "password"].includes(element.type);
    if (!textInput && element.tagName !== "TEXTAREA" && !element.isContentEditable) return null;
    const security = getComputedStyle(element).webkitTextSecurity;
    return element.type === "password" || security && security !== "none" ? "Password" : "Text";
  }
  return null;
})()

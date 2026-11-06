(origin: string, username: string, password: string) => {
  if (window !== top || location.origin !== origin || !origin.startsWith("https://")) return false;
  const visible = (element: HTMLInputElement) => !element.disabled && !element.readOnly && element.getClientRects().length > 0;
  const fields = [...document.querySelectorAll<HTMLInputElement>('input[type="password"]')].filter(visible);
  if (fields.length !== 1) return false;
  const field = fields[0];
  const form = field.form;
  if (!form || new URL(form.action || location.href, location.href).origin !== origin) return false;
  const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!;
  const fill = (element: HTMLInputElement, value: string) => {
    setter.call(element, value);
    element.dispatchEvent(new Event("input", { bubbles: true }));
    element.dispatchEvent(new Event("change", { bubbles: true }));
  };
  const users = [...form.querySelectorAll<HTMLInputElement>('input[autocomplete="username"],input[type="email"],input[name*="user" i]')].filter(visible);
  if (users.length === 1) fill(users[0], username);
  fill(field, password);
  field.focus();
  return true;
}

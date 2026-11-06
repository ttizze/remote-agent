const { test } = require("node:test");
const assert = require("node:assert/strict");
const { readFileSync } = require("node:fs");
const vm = require("node:vm");
const source = readFileSync(`${__dirname}/password-fill.generated.js`, "utf8");

function page({ origin = "https://example.test", action = "/login", frame = false, hidden = false, readOnly = false, multiple = false } = {}) {
  class Input {
    constructor() { this.events = []; }
    set value(value) { this.filled = value; }
    getClientRects() { return hidden ? [] : [{}]; }
    dispatchEvent(event) { this.events.push(event.type); }
    focus() { this.focused = true; }
  }
  const username = new Input();
  const password = new Input();
  password.readOnly = readOnly;
  password.form = { action, querySelectorAll: () => [username] };
  const window = {};
  const fill = vm.runInNewContext(`(${source})`, {
    window, top: frame ? {} : window, location: { origin, href: `${origin}/login` }, URL,
    document: { querySelectorAll: () => multiple ? [password, new Input()] : [password] },
    HTMLInputElement: Input, Event: class { constructor(type) { this.type = type; } },
  });
  return { fill, username, password };
}

test("fills one visible same-origin HTTPS form using input events, without submitting", () => {
  const { fill, username, password } = page();
  assert.equal(fill("https://example.test", "fixture-user", "fixture-value"), true);
  assert.equal(username.filled, "fixture-user");
  assert.equal(password.filled, "fixture-value");
  assert.deepEqual(password.events, ["input", "change"]);
  assert.equal(password.focused, true);
});

test("refuses a changed origin, frame, cross-origin form, hidden, readonly or ambiguous field", () => {
  for (const options of [
    { origin: "https://example.test.attacker.test" }, { origin: "https://sub.example.test" },
    { origin: "https://example.test:8443" }, { origin: "http://example.test" },
    { frame: true }, { action: "https://attacker.test/login" }, { hidden: true }, { readOnly: true }, { multiple: true },
  ]) {
    const { fill, username, password } = page(options);
    assert.equal(fill("https://example.test", "fixture-user", "fixture-value"), false);
    assert.equal(username.filled, undefined);
    assert.equal(password.filled, undefined);
  }
});

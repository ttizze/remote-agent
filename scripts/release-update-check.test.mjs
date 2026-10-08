import assert from "node:assert/strict";
import test from "node:test";

import { checkUpdate, compareVersions } from "./release-update-check.mjs";

test("compares stable and prerelease versions using semantic ordering", () => {
  assert.equal(compareVersions("1.0.0", "1.0.0"), 0);
  assert.equal(compareVersions("1.0.0-nightly.2", "1.0.0-nightly.10"), -1);
  assert.equal(compareVersions("1.0.0-nightly.10", "1.0.0"), -1);
  assert.equal(compareVersions("1.1.0", "1.0.9"), 1);
});

test("reports an update and preserves the channel URL", () => {
  assert.deepEqual(
    checkUpdate(
      {
        schema: 1,
        channel: "nightly",
        version: "0.1.0-nightly.20261008.42",
        update_url: "https://example.invalid/nightly.json",
      },
      "0.1.0-nightly.20261008.41",
    ),
    {
      update_available: true,
      current_version: "0.1.0-nightly.20261008.41",
      latest_version: "0.1.0-nightly.20261008.42",
      channel: "nightly",
      update_url: "https://example.invalid/nightly.json",
    },
  );
});

test("does not offer a stable update to an equal or newer install", () => {
  assert.equal(
    checkUpdate({ schema: 1, channel: "stable", version: "1.2.3", update_url: null }, "1.2.3").update_available,
    false,
  );
  assert.equal(
    checkUpdate({ schema: 1, channel: "stable", version: "1.2.3", update_url: null }, "1.3.0").update_available,
    false,
  );
});

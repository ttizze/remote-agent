import assert from "node:assert/strict";
import test from "node:test";

import {
  checkUpdate,
  checkUpdateState,
  compareVersions,
  fetchMetadata,
  initialUpdateState,
  nativeUpdate,
  restartDecision,
  transitionUpdateState,
  updateAction,
  validateMetadata,
} from "./release-update-check.mjs";

test("compares stable and prerelease versions using semantic ordering", () => {
  assert.equal(compareVersions("1.0.0", "1.0.0"), 0);
  assert.equal(compareVersions("1.0.0-nightly.2", "1.0.0-nightly.10"), -1);
  assert.equal(compareVersions("1.0.0-nightly.10", "1.0.0"), -1);
  assert.equal(compareVersions("1.1.0", "1.0.9"), 1);
  assert.equal(compareVersions("999999999999999999.0.0", "1000000000000000000.0.0"), -1);
  assert.throws(() => compareVersions("01.0.0", "1.0.0"), /invalid semantic version/);
  assert.throws(() => compareVersions("1.0.0-nightly.01", "1.0.0-nightly.1"), /invalid semantic version/);
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

test("retains a downloaded update across a failed check and requires a restart after install", () => {
  const metadata = {
    schema: 1,
    channel: "nightly",
    version: "0.1.0-nightly.20261008.42",
    update_url: "https://example.invalid/releases/nightly.json",
    assets: [],
  };
  let state = checkUpdateState(metadata, {
    target: "host",
    currentVersion: "0.1.0-nightly.20261008.41",
    checkedAt: "2026-10-08T00:00:00Z",
  });
  assert.equal(state.status, "available");
  assert.equal(updateAction(state), "none", "an unavailable artifact cannot be downloaded");
  state = transitionUpdateState(state, { type: "download-start" });
  state = transitionUpdateState(state, { type: "download-success", version: metadata.version });
  assert.equal(updateAction(state), "install");
  state = transitionUpdateState(state, { type: "check-failure", message: "offline", checked_at: "2026-10-08T00:01:00Z" });
  assert.equal(state.status, "downloaded");
  state = transitionUpdateState(state, { type: "install-start" });
  state = transitionUpdateState(state, { type: "install-success" });
  assert.equal(state.restart_required, true);
  assert.equal(restartDecision({ serviceInstalled: true, serviceCurrent: false }).action, "defer");
  assert.equal(restartDecision({ serviceInstalled: true, serviceCurrent: false, assumeYes: true }).action, "restart");
});

test("rejects an HTTPS metadata request that redirects to HTTP", async () => {
  await assert.rejects(
    fetchMetadata("https://example.invalid/channel.json", async () => ({
      ok: true,
      status: 200,
      url: "http://example.invalid/channel.json",
      json: async () => ({ schema: 1, channel: "nightly", version: "1.0.0" }),
    })),
    /HTTPS URL/,
  );
});

test("follows only bounded HTTPS redirect hops", async () => {
  const requested = [];
  const metadata = { schema: 1, channel: "nightly", version: "1.0.0" };
  const result = await fetchMetadata("https://example.invalid/first.json", async (url, options) => {
    requested.push({ url, options });
    if (requested.length < 3) {
      return {
        ok: false,
        status: 302,
        headers: new Map([["location", `https://example.invalid/hop-${requested.length}.json`]]),
      };
    }
    return { ok: true, status: 200, url, json: async () => metadata };
  });
  assert.equal(result.version, metadata.version);
  assert.equal(requested.length, 3);
  assert.equal(requested[0].options.redirect, "manual");
});

test("rejects an HTTPS redirect loop after the configured bound", async () => {
  let requests = 0;
  await assert.rejects(
    fetchMetadata("https://example.invalid/loop.json", async () => {
      requests += 1;
      return {
        ok: false,
        status: 302,
        headers: new Map([["location", "https://example.invalid/loop.json"]]),
      };
    }),
    /redirect limit/,
  );
  assert.equal(requests, 6);
});

test("rejects unsafe or oversized manifest assets before a consumer can select them", () => {
  const base = { schema: 1, channel: "nightly", version: "1.0.0" };
  const asset = { name: "host-linux-x86_64.tar.gz", sha256: "0".repeat(64), size: 1 };
  assert.throws(() => validateMetadata({ ...base, assets: [{ ...asset, name: "../host.tar.gz" }] }), /invalid asset/);
  assert.throws(() => validateMetadata({ ...base, assets: [{ ...asset, size: 512 * 1024 * 1024 + 1 }] }), /invalid asset/);
});

test("exposes native store links only when configured", () => {
  const base = { schema: 1, channel: "nightly", version: "0.1.0-nightly.20261008.42", update_url: null };
  const unconfigured = nativeUpdate(base, "0.1.0-nightly.20261008.41", "android");
  assert.equal(unconfigured.store_url, null);
  assert.equal(unconfigured.update_available, false);
  assert.match(unconfigured.message, /no store link/);
  const androidLink = "https://play.google.com/store/apps/details?id=dev.remoteagent.mobile";
  assert.equal(
    nativeUpdate({ ...base, native_updates: { android: { url: androidLink } } }, "0.1.0-nightly.20261008.41", "android").store_url,
    androidLink,
  );
  assert.equal(
    nativeUpdate({ ...base, native_updates: { android: { url: androidLink } } }, "0.1.0-nightly.20261008.41", "android").update_available,
    true,
  );
});

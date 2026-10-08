import assert from "node:assert/strict";
import crypto from "node:crypto";
import fs from "node:fs/promises";
import test from "node:test";
import os from "node:os";
import path from "node:path";

import {
  checkUpdate,
  checkUpdateState,
  compareVersions,
  downloadArtifact,
  fetchMetadata,
  initialUpdateState,
  installArtifact,
  nativeUpdate,
  restartDecision,
  transitionUpdateState,
  updateAction,
} from "./release-update-check.mjs";

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

test("downloads only the selected platform asset and verifies its size and checksum", async () => {
  const bytes = Buffer.from("verified host archive");
  const metadata = {
    schema: 1,
    channel: "nightly",
    version: "0.1.0-nightly.20261008.42",
    update_url: "https://example.invalid/releases/nightly.json",
    assets: [
      {
        name: "host-linux-x86_64.tar.gz",
        sha256: crypto.createHash("sha256").update(bytes).digest("hex"),
        size: bytes.length,
      },
    ],
  };
  const directory = await fs.mkdtemp(path.join(os.tmpdir(), "release-update-test-"));
  try {
    const requests = [];
    const transaction = await downloadArtifact(metadata, {
      target: "host",
      platform: "linux",
      architecture: "x64",
      destination: path.join(directory, "download", "host.tar.gz"),
      fetchImpl: async (url, options) => {
        requests.push({ url, options });
        return { ok: true, status: 200, arrayBuffer: async () => bytes };
      },
    });
    assert.equal(transaction.name, "host-linux-x86_64.tar.gz");
    assert.deepEqual(await fs.readFile(transaction.path), bytes);
    assert.equal(requests[0].url, "https://example.invalid/releases/host-linux-x86_64.tar.gz");
    assert.deepEqual(requests[0].options.headers, { accept: "application/octet-stream" });
  } finally {
    await fs.rm(directory, { recursive: true, force: true });
  }
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

test("installs into a versioned staging directory and exposes native store links only when configured", async () => {
  const directory = await fs.mkdtemp(path.join(os.tmpdir(), "release-install-test-"));
  try {
    const installed = await installArtifact(
      { path: "/tmp/verified.tar.gz", version: "0.1.0-nightly.20261008.42" },
      {
        installRoot: path.join(directory, "versions"),
        extractImpl: async (_archive, destination) => {
          await fs.mkdir(destination, { recursive: true });
          await fs.writeFile(path.join(destination, "host-daemon"), "binary");
        },
      },
    );
    assert.equal(installed.restart_required, true);
    assert.equal(await fs.readFile(path.join(installed.install_path, "host-daemon"), "utf8"), "binary");

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
  } finally {
    await fs.rm(directory, { recursive: true, force: true });
  }
});

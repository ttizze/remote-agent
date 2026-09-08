# Phoenix relay on Fly.io

The deployment files are in `apps/server`. The initial topology is one always-on
512 MiB shared-CPU Machine in Tokyo (`nrt`), with Fly terminating public TLS and
forwarding WebSockets to Phoenix on port 4000. No database or volume is required.

## Deployed service

App: `bex-relay-ttizze`, organization: `personal` (`ttizze`).
Endpoint: `wss://bex-relay-ttizze.fly.dev/socket/websocket`.
The checked-in Fly configuration targets this app. Redeploy it from the repository
root with:

```sh
nix develop --command sh -c 'cd apps/server && fly deploy --ha=false --remote-only'
```

On 2026-09-07, public WSS verification passed wrong-token rejection, exact 32 KiB
byte transfer in both directions, client reconnect and Host-disconnect propagation.
The Mac Host was switched to the public endpoint and its live connection to Fly
port 443 was verified. Its existing relay token remains in Keychain and Fly Secrets.
These checks do not establish multi-region availability or physical-iPhone behavior.
The signed Host also answered a local Codex `thread/list` request after cutover.

Public TLS exposed a missing Rustls crypto-provider feature in the native
transport. `relay-transport` now explicitly enables the ring provider. The signed
Mac Host was rebuilt with this fix; an iPhone installation built before it must
also be rebuilt and installed before using WSS. Update the phone's saved endpoint
via a new invitation from the Mac. A Rust iOS compile check is not device testing
or an app-distribution update.
The `mobile-client` check for `aarch64-apple-ios` passed using the repository's
Xcode compiler settings (`CC=/usr/bin/clang`, `CXX=/usr/bin/clang++`,
`IPHONEOS_DEPLOYMENT_TARGET=15.0`).

On 2026-09-07, the two local-relay Simulator checks for
project-list expansion and list/model restoration passed with no failures or skips.
A subsequent isolated Simulator run used the deployed public WSS endpoint with
a fresh Host identity, runner ID and pairing. Its UI scenario passed pairing,
message/reply transfer, task-list and history restoration after app restart and
foreground return, and another message/reply after restart: 1 passed, 0 failed,
0 skipped. Host/mobile transport and the public relay were real; Codex responses
came from the existing deterministic fixture.

Physical build 19 reproduced a WebSocket failure. The native-certificate loader
uses Unix certificate-file discovery on iOS, which is unsuitable for the physical
device sandbox. iOS now uses the dependency's bundled WebPKI public trust anchors;
other targets retain native roots. Certificate and hostname validation remain
enabled. Root updates require rebuilding the app with updated dependencies.
Gradle now tracks `relay-transport` as an input to both mobile Rust build tasks so
transport changes cannot silently reuse an old library.

Version 1.0 build 20 was archived, signature-verified and installed on the physical
iPhone. Public pairing succeeded, followed by a passing real-device UI check of
saved pairing, an existing Codex task's visible body, and the task list after
relaunch: 1 passed, 0 failed, 0 skipped. The installed version was confirmed as 20
after testing. The check used Wi-Fi, the real Mac Host and existing Codex history;
it did not send a new Codex prompt or verify cellular connectivity.

Build 22 resolves the larger-history rendering failure with official Codex
paging (five native turns, a 500-item initial budget and opaque cursors) and
native iPhone list virtualization. The final public-WSS Simulator run passed
all seven selected UI scenarios with zero failures/skips, including long history,
detail expansion/collapse, external updates, approvals, input requests and images.
The physical iPhone then opened the affected real conversation, displayed its
body, loaded older history on upward scroll (482 to 554 projected UI items), and
restored the task list after restart: 1 passed, 0 failed, 0 skipped. Inspected all
three screenshots in `target/qa/history-device22-images`. This used Wi-Fi and the
real Mac Host/Codex store; no new prompt was sent. Cellular and Android runtime
behavior remain unverified.

When running an archived product with XCUITest, set both `UITargetAppPath` and
the app entry in `DependentProductPaths` to the archive's `Bex.app`. Updating only
the former lets the test runner reinstall an older derived-data application.

For local verification, obtain an invitation through the running Host's
owner-only manager socket. Do not repeatedly read relay credentials with the
`security` CLI: that separate caller can trigger additional Keychain prompts.
The signed Host retains its existing Always Allow authorization as described in
the README; keep invitation payloads out of logs and remove temporary artifacts.

This is a single-region deployment. `RelayRoutes` owns connections in one BEAM
process: do not scale the app to multiple Machines or regions until Host-based
routing is implemented. `--ha=false` avoids Fly's extra Machine on first deploy.
Immediate deployment stops the old process before replacement; clients disconnect
during deployment and must reconnect. There is no high-availability claim.

## Toolchain and image

Use `nix develop` for the pinned project tools, including `flyctl`. The Dockerfile
is the narrow deployment-runtime exception: it builds a standard Mix release with
digest-pinned Hex Elixir/OTP and matching Debian images for Fly's Linux runtime.
The final image runs as `nobody`, disables Erlang distribution, and includes only
the release and runtime libraries. The allowlisted build context excludes local
credentials, tests, dependencies, and build output. Secrets are supplied at runtime.

## Creating another deployment

Log in interactively, inspect the account and organizations, then choose the app
name and organization. Creating and running Machines incurs Fly charges.
The existing app above is already deployed; do not recreate it or rotate its
secrets when updating it. The following commands are for a separate deployment.

```sh
nix develop
fly auth login
fly auth whoami
fly orgs list
read -r BEX_FLY_APP
read -r BEX_FLY_ORG
fly apps create "$BEX_FLY_APP" --org "$BEX_FLY_ORG"
```

Enter the relay token to use for this deployment without echoing it. Use a private
random token; never put it in `fly.toml`, command arguments, source control, or
logs. This token controls relay admission, while Host pairing independently
authorizes access. The current shared-token scheme is for a controlled rollout;
public self-service requires per-Host/user relay credentials and admission limits.

```sh
read -r -s REMOTE_AGENT_RELAY_TOKEN
export REMOTE_AGENT_RELAY_TOKEN
cargo xtask relay-secrets | fly secrets import --app "$BEX_FLY_APP" --stage
unset REMOTE_AGENT_RELAY_TOKEN
fly ips allocate-v4 --shared --app "$BEX_FLY_APP"
fly ips allocate-v6 --app "$BEX_FLY_APP"
cd apps/server
fly config validate --config fly.toml --app "$BEX_FLY_APP"
fly deploy --app "$BEX_FLY_APP" --ha=false --remote-only \
  --env "PHX_HOST=$BEX_FLY_APP.fly.dev"
fly status --app "$BEX_FLY_APP"
```

Use `wss://<app-name>.fly.dev/socket/websocket` and the chosen relay token in the
Mac connection settings. Generate an invitation using the public endpoint for
the iPhone/other Mac. Existing invitations contain the old endpoint; deployment
alone does not migrate saved clients. Keep the local relay running until clients
have moved successfully. Never regenerate the relay token or secret key on an
ordinary redeploy; run only the deploy command with the same app name.

Verify public TLS, rejection without credentials, authenticated byte transfer in
both directions, and reconnection before declaring the service live. A running
Machine or an HTTP 403 alone does not prove end-to-end operation.

## Local verification

```sh
nix develop --command sh -c 'cd apps/server && mix test'
nix develop --command cargo test -p relay-transport --test phoenix
docker build --platform linux/amd64 -t bex-relay:verify apps/server
```

For x86 emulation on Apple Silicon, add
`--build-arg 'ERL_FLAGS=+JMsingle true'` if the emulator cannot run the default
Erlang JIT. That build-only argument is not included in the runtime image. Local
emulated execution also needs `-e 'ERL_FLAGS=+JMsingle true'`; native Fly execution
does not inherit this setting.

## Multiple regions

Before adding Machines, implement an authenticated assignment of each Host to a
region and Machine. Both Host and controller must use that same assignment;
nearest-region load balancing alone breaks rendezvous. Route the HTTP WebSocket
upgrade to the assigned Machine with `fly-replay`. On failure both peers must
resolve a new assignment and reconnect, with ownership protected against stale
assignments. Do not replay application sends or approvals during recovery.
Adding regions also requires measuring latency, concurrent routes, memory,
backpressure, transfer costs and recovery after a Machine/region failure.

References: [Fly configuration](https://fly.io/docs/reference/configuration/),
[request routing](https://fly.io/docs/networking/dynamic-request-routing/),
[secrets](https://fly.io/docs/apps/secrets/).

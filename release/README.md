# Release engineering

`.github/workflows/nightly-release.yml` builds the Host for Linux, Windows, and
macOS, packages the signed macOS desktop bundle, and can build the signed
Android and iOS artifacts. A schedule runs the nightly channel from `main`.
Manual runs can select `nightly`, `preview`, or `stable`; publication is opt-in
for manual runs and is rejected from non-`main` refs.

The workflow uploads Actions artifacts for every successful build. A trusted
scheduled or manual run with publication enabled creates the concrete GitHub
release and attaches `<channel>.json`, which contains the commit, version,
checksums, and update URL. Metadata does not claim publication before the
GitHub release command succeeds. `scripts/release-update-check.mjs` consumes
that manifest as a pure comparison and does not contact GitHub unless the caller explicitly supplies `--metadata-url`.

Host archives embed the lockfile-pinned Agent SDK and bridge, but do not copy a
Nix-store Node executable into the artifact. The runtime requires Node.js 18 or
newer, available as `node` in `PATH` or beside the Host executable. Each Host
archive contains `NODE-RUNTIME-REQUIREMENT.txt` with this requirement and the
Node version observed by the packaging job.

The signed mobile and Apple distribution paths require repository secrets. The
workflow intentionally fails with a named missing input when a scheduled run
cannot prepare a keystore, signing certificate, provisioning profile, or
App Store Connect/Play credential. The Apple setup scripts keep an explicitly
empty-password keychain and temporary certificate material under a mode-700
`RUNNER_TEMP` directory; no keychain-password secret is required. No credential
values are written to logs or checked-in files. Mobile store upload is limited
to the trusted scheduled path or an explicitly requested manual run from
`main`.

## Update transactions

`scripts/release-update-check.mjs` is the shared update contract for Host and
desktop consumers. It validates release metadata, selects the platform asset,
bounds release notes, and keeps pure check/download/install state transitions
separate from network, filesystem, archive, and restart effects. The effectful
helpers accept injected effects so contract tests never contact GitHub or
replace a running process. A download is accepted only after both the manifest
size and SHA-256 match; installation extracts into a private version directory
and returns `restart_required` for the owning supervisor to decide when to hand
off.

The CLI supports local metadata (`--metadata`) and a configured HTTPS manifest
(`--metadata-url`), with optional `--download` and `--install-dir` staging. It
never restarts a Host or desktop process by itself. Native Android and iOS
checks expose a Play/TestFlight URL only when release metadata was given an
explicit `ANDROID_STORE_URL` or `IOS_STORE_URL` repository variable. Missing
variables produce no native link and do not imply that a store release exists.

## Naming audit

The release/build/CI-owned generic names now use `APP_*`, `IOS_*`, and
`MACOS_*` where there is no external consumer. In particular,
`APP_CODE_SIGN_IDENTITY` and `APP_XCODE_DEVELOPER_DIR` replaced their former
product-prefixed internal names in the release and native CI paths.

The following identifiers remain because changing them without the owners of
their consumers would break the current format or application identity:

- `BEX_BUILD_REVISION` is compiled into the shared diagnostics and bindings.
- `BEX_CARGO_TARGET_DIR` is an Xcode build setting consumed by the iOS project
  and the test runner.
- `BEX_IOS_SWIFT_CACHE_PATHS` is consumed by the iOS build tooling and remains
  the single cache variable; no compatibility alias is provided.
- `BEX_IOS_TEST_*`, `BEX_PAIRING_*`, and the `target/qa/Bex-*` fixtures are
  consumed by the native acceptance runner.
- `Bex.app`, the iOS project/scheme, bundle identifiers, and `bex-*` Cargo
  packages are application or dependency identities.

Those paths are outside this release-owned rename and should move only with a
functional consumer migration and an approved product identity change.

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

`scripts/release-update-check.mjs` is the shared metadata and state contract for
Host and desktop consumers. It validates release metadata delivered over HTTPS,
selects the platform asset, bounds release notes, and keeps pure check state
transitions separate from network and supervisor effects. The production Host
owns verified streaming downloads, archive validation, installation, and the
restart handoff; this script intentionally does not duplicate that installer.
After a verified install, Host startup consumes `transactions/host.json` and
starts the versioned executable under `installed/host/<version>`. Desktop
startup consumes the corresponding desktop marker only when the local Host
registry is stopped, then starts the versioned Linux/Windows executable or
macOS app-bundle binary. A running Host and its active work keep the bundled
Desktop executable in place until a later launch.

Rust Host builds receive `APP_RELEASE_METADATA_URL` and
`APP_RELEASE_CHANNEL`, and the channel version in `APP_UPDATE_VERSION` from the trusted release workflow. Local builds leave
the URL unset and report updates as disabled until an operator supplies an
explicit HTTPS manifest configuration. This avoids a local build claiming a
release that has not been published. Verified transaction state is kept under
the Host's private update directory so an interrupted download can be retried
after a Host restart.

The CLI supports local metadata (`--metadata`) and a configured HTTPS manifest
(`--metadata-url`) for comparison and diagnostics. It never downloads, installs,
or restarts a Host or desktop process by itself. Native Android and iOS checks
expose a Play/TestFlight URL only after the corresponding mobile upload succeeds;
missing publication data produces no native link and does not imply that a store
release exists.

Mobile packages carry the full release version used by the update protocol
separately from their store-facing build number. iOS keeps the numeric
`CFBundleShortVersionString` and `CFBundleVersion` values required by Apple while
passing `APP_UPDATE_VERSION` and `APP_RELEASE_CHANNEL` through the archive; the
Android build uses the same full version and a separate numeric `versionCode`.

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

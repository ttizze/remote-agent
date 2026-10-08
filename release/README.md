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
that manifest as a pure comparison and does not contact GitHub.

The signed mobile and Apple distribution paths require repository secrets. The
workflow intentionally fails with a named missing input when a scheduled run
cannot prepare a keystore, signing certificate, provisioning profile, or
App Store Connect/Play credential. No credential values are written to logs or
checked-in files. Mobile store upload is limited to the trusted scheduled path
or an explicitly requested manual run from `main`.

## Naming audit

The release/build/CI-owned generic names now use `APP_*`, `IOS_*`, and
`MACOS_*` where there is no external consumer. In particular,
`APP_CODE_SIGN_IDENTITY`, `APP_XCODE_DEVELOPER_DIR`, and
`IOS_SWIFT_CACHE_PATHS` replaced their former product-prefixed internal names
in the release and native CI paths.

The following identifiers remain because changing them without the owners of
their consumers would break the current format or application identity:

- `BEX_BUILD_REVISION` is compiled into the shared diagnostics and bindings.
- `BEX_CARGO_TARGET_DIR` is an Xcode build setting consumed by the iOS project
  and the test runner.
- `BEX_IOS_TEST_*`, `BEX_PAIRING_*`, and the `target/qa/Bex-*` fixtures are
  consumed by the native acceptance runner.
- `Bex.app`, the iOS project/scheme, bundle identifiers, and `bex-*` Cargo
  packages are application or dependency identities.

Those paths are outside this release-owned rename and should move only with a
functional consumer migration and an approved product identity change.

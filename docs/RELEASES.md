# Release operations

Develop on `main`. A revision that passes both Portable checks and Native clients
produces a numbered GitHub prerelease, `nightly-N`, containing Mac, Windows,
Android, Linux Host/CLI and iPhone Preview packages.

On macOS and Windows, select **Nightly** in Settings → Update channel to receive
these candidates. **Stable** is the default. Returning to Stable waits for a newer
stable build; it never automatically downgrades the installed app.

Stable and Nightly share BEX's existing application-data directory, conversations,
settings and local Host. Desktop debug builds use its `dev` subdirectory and an
isolated Host by default. Run `scripts/dev-env.sh just dev` on macOS to launch that
instance alongside the installed release. `AGENT_STATE_DIR` selects an explicit
state directory; `AGENT_ISOLATED_HOST` explicitly selects Host isolation. Provider
caches and project worktrees keep their existing shared locations.

## iPhone variants

| App | Bundle ID | Distribution |
| --- | --- | --- |
| BEX | `dev.remoteagent.mobile.ios` | App Store and production TestFlight |
| BEX Preview | `dev.remoteagent.mobile.ios.preview` | Nightly IPA, signed for registered iPhones |
| BEX Dev | `dev.remoteagent.mobile.ios.dev` | Local development builds |

All three can coexist, with separate preferences, snapshots, Keychain access
and URL schemes (`bex`, `bex-preview`, `bex-dev`). TestFlight for the production
ID replaces the App Store app; Preview does not. The existing production beta
link remains https://testflight.apple.com/join/UJbK9Ww7.

Download `bex-iOS-preview.ipa` from a Nightly and install it using Apple
Configurator or Xcode. Preview uses Ad Hoc distribution, so the iPhone must be
registered and enabled in the Apple Developer account before the release builds.
A later device registration takes effect in the next Nightly; existing profiles
are retained for already installed versions. Preview does not require an App Store
Connect app record or beta review. Enable Push Notifications on the Preview app's
registered bundle ID. APNs on a Host uses its configured `bundle_id`; a Host
configured for a different variant cannot send background updates to Preview.

To archive the local development app, use the signing-keychain preflight and:

```sh
scripts/dev-env.sh just ios-archive \
  "$PWD/target/release-dev/BexDev.xcarchive" "$PWD/target/release-dev/DerivedData" \
  -xcconfig "$PWD/apps/mobile/iosApp/Config/Dev.xcconfig" \
  DEVELOPMENT_TEAM=K65K9J8686
```

Select Apple Development signing for direct installation. If uploading a Dev
build, use the BEX Dev App Store Connect record (`6820657007`) and a build number
higher than its existing uploads. Pair Dev with the development Host explicitly.

## Stable release

After testing Nightly, run **Production release → Run workflow** on `main`. It
selects the latest published Nightly's exact source commit and rebuilds every
platform for Stable with a new release build number. It creates a new `prd-N` tag;
its number does not have to match the selected Nightly. `release.json` records
both the source commit and the selected Nightly tag. Do not manually create or
move production tags, or replace published assets.

The iPhone Stable rebuild uses the production bundle ID and is uploaded to its
existing TestFlight groups and submitted for App Review. The rebuilt binary does
not need another external beta review before App Review. Release after approval
is enabled for this selected build. The scheduled Production workflow finishes
pending approvals and publishes the matching GitHub release, advancing Stable.
An already approved version keeps its selected build: publish a pending initial
version before releasing the next version. The first new production release
switches the public iPhone download from TestFlight to App Store.

Retry failed build jobs through GitHub Actions. Never move a published tag. For
an App Review rejection, inspect the rejection and create a corrected Nightly.
If Android failed after the other packages were staged, run **Android candidate**
on `main` with the existing draft tag. It rebuilds the same verified source with
the current packaging workflow. After it succeeds, resume package verification
and publication with:

```sh
GITHUB_REPOSITORY=ttizze/remote-agent scripts/dev-env.sh node scripts/release-candidate.ts seal nightly-N
```

Android Nightly and Stable use the same distribution key and package ID. Older
APKs signed with another key require reinstallation before entering this channel.

Linux packages include Host and CLI with their shared-library closure. Extract
the archive and run `bash install.sh` to install under `~/.local`. Provide Node.js
and the selected provider CLIs as for a source installation. Updating a running
Host requires its existing update and continuation procedure; replacing a
package alone does not update that process.

For public BEX releases, retain the configured territories (US, Japan and China,
excluding France) and existing encryption declarations in App Store Connect.

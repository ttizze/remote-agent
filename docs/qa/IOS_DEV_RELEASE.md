# BEX Dev for iOS

BEX Dev is a separate TestFlight app (`com.ttizze.b-codex.dev`). It can be
installed alongside production BEX (`dev.remoteagent.mobile.ios`). Its
preferences, snapshots, and default Keychain access group are separate. Pair it with the development Host explicitly.

Build the device core with `nix develop . --command scripts/build-agent-ios.sh device`.
Archive the existing Bex scheme in Release with
`-xcconfig apps/mobile/iosApp/Config/Dev.xcconfig`, a fresh archive/derived-data
path, and the dedicated BEX signing keychain. The xcconfig owns the independent
dev build number; increment it before subsequent dev uploads.

Before upload, verify `CFBundleIdentifier = com.ttizze.b-codex.dev` and
`CFBundleDisplayName = BEX Dev` in the archive. Export using the distribution
profile registered for that bundle ID. Upload to the **BEX Dev** App Store
Connect record, then verify both Apple processing (`VALID`) and the internal
test group state (`IN_BETA_TESTING`). Processing success alone is insufficient.

The app uses the same encryption as BEX. Apply its existing export-exemption
setting to the dev build in App Store Connect. Build the development Host from
the same revision. Never use the ordinary BEX record for dev releases.

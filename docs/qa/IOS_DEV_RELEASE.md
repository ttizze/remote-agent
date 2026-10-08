# BEX Dev for iOS

BEX Dev is a separate TestFlight app (`dev.remoteagent.mobile.ios.dev`, App Store
Connect app ID `6820657007`). It can be installed alongside BEX; its preferences,
snapshots, and default Keychain access group are separate. Its amber icon has a
`DEV` label and is selected by
`Dev.xcconfig`. Pair it with the development Host explicitly.

Archive with the reproducible command and fresh archive/derived-data paths:

```sh
nix develop . --command just ios-archive \
  "$PWD/target/release-dev/BexDev.xcarchive" "$PWD/target/release-dev/DerivedData" \
  -xcconfig "$PWD/apps/mobile/iosApp/Config/Dev.xcconfig" \
  DEVELOPMENT_TEAM=K65K9J8686 \
  OTHER_CODE_SIGN_FLAGS="--keychain /Users/tt/.appstoreconnect/signing/BEX.keychain-db"
```

This builds the device core and archives the existing Bex scheme in Release.
The command uses only the checked-in package versions and trusts their build
plugins from the first build, including SwiftTerm's build-info generator.
Passing the development team also covers the package's build tools. Unlock the
dedicated BEX signing keychain before archiving. The xcconfig owns the independent
dev build number; increment it before subsequent dev uploads.

For a direct installation on a registered device, use `CODE_SIGN_IDENTITY="Apple Development"`
and the keychain containing that development identity. Install the app from
`BexDev.xcarchive/Products/Applications`, verify its signature, and launch it on
the device.

Before upload, verify `CFBundleIdentifier = dev.remoteagent.mobile.ios.dev` and
`CFBundleDisplayName = BEX Dev` in the archive. Export using the distribution
profile registered for that bundle ID. Upload to the **BEX Dev** App Store
Connect record, then verify both Apple processing (`VALID`) and the internal
test group state (`IN_BETA_TESTING`). Processing success alone is insufficient.

The app uses the same encryption as BEX. Apply its existing export-exemption
setting to the dev build in App Store Connect. Build the development Host from
the same revision. Never use the ordinary BEX record for dev releases.

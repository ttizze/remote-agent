# Remote Agent

Remote Agent lets a Mobile Client control a Codex instance running on a trusted PC Host. The PC Host keeps Codex credentials, Workspace files, and authoritative conversation history on the computer; the Mobile Client is a remote display and control surface with a non-authoritative Mobile Cache.

The first product slice targets Codex, a Rust Host Daemon on macOS, shared Kotlin application logic, an Android Compose UI, and a native iOS SwiftUI app.

## Development

Enter the reproducible development environment and run the Rust tests:

```sh
nix develop .
cargo test --workspace
```

On macOS, the Host Daemon uses the Codex bundled with ChatGPT Desktop when it is installed, then falls back to `codex` on `PATH`. On other platforms it uses `codex` on `PATH`. Use `--codex <PATH>` to select a specific Codex executable.

Build the macOS Host with its stable local code identity through the repository script. Stable signing prevents each rebuilt binary from becoming a new Keychain client and repeatedly requesting access to the Host Identity item:

```sh
nix develop . --command ./scripts/build-host-macos.sh
```

The script selects the sole `Apple Development` identity in the user keychain. Set `HOST_CODE_SIGN_IDENTITY` explicitly when more than one is installed.

The mobile build links the Rust QUIC client into each native application. The Nix shell includes the Rust standard libraries for iOS devices, Apple Silicon iOS Simulator, and the supported Android ABIs:

```sh
nix develop .
./gradlew :apps:mobile:iosSimulatorArm64Test
```

Android additionally requires an Android SDK and `cargo-ndk`:

```sh
cargo install cargo-ndk --locked
./gradlew :apps:mobile:assembleDebug
```

See [the implementation plan](docs/IMPLEMENTATION_PLAN.md) for scope and milestones.

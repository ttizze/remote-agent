# Windows / Windowsで使う

BEX runs on Windows 11. The ZIP includes the desktop app, Host, CLI, provider
supervisor, and Claude SDK bridge. You do not need Rust to use a downloaded ZIP.
The package is currently unsigned.

## Install and run

1. Download `bex-windows-x86_64-release.zip` from the GitHub release assets.
   New releases publish this asset after the Windows build and tests succeed.
   For a pull request or main build, download the `bex-windows-x86_64-dev`
   artifact from the successful **Native clients** Windows job. That artifact
   contains a development build, not a release build.
2. Install Git for Windows, Node.js, and the agents you want to use (Codex
   and/or Claude Code). Ensure their executables are on your Windows PATH,
   and sign in with your own accounts. Install Microsoft Edge WebView2 Runtime
   if it is not already present.
3. Extract the whole ZIP into a writable folder. Keep all files together;
   moving only `bex-desktop.exe` breaks Host and provider startup.
4. Open `bex-desktop.exe`. Pair a phone through the app's invitation flow.
   Do not run another Host against the same account and state at the same time.

For CLI pairing, use `host-daemon.exe invite` and keep the invitation private.
To update, finish active tasks and stop the desktop and Host, then extract the
new package into a new folder. Your account and conversation state lives outside
this package. These ZIPs are for x86-64 Windows; native ARM64 builds are not included.

## Update Codex CLI

In BEX settings, select Codex and choose **最新版に更新** under **Codex CLI**.
BEX runs the selected Host's `codex update`, verifies the installed version,
and reports failures in the same panel. **状態を確認** also re-reads the installed
version after an update performed outside BEX. Finish active conversations and
restart BEX and its Host after updating so new sessions use the new executable.
The CLI must support `codex update`; otherwise update it using its installer.

## Build from source

Install the MSVC Rust toolchain and Visual Studio C++ Build Tools with the
Windows SDK, in addition to the runtime prerequisites above. Use the Rust
version pinned by the repository's Nix flake (as shown in the **Native clients**
toolchain job).

```powershell
./scripts/package-windows.ps1
# Faster local development package:
./scripts/package-windows.ps1 -Profile dev
```

The script builds desktop, Host, supervisor, and CLI from the same checkout,
then writes the ZIP and SHA-256 checksum under `target/windows-packages/`
(or the Cargo-configured target directory). `-SkipBuild` packages already-built
executables and is intended for CI immediately after verification.

## 日本語

Windows 11で利用できます。ZIPにはデスクトップ、Host、CLI、プロセス監視、
Claude SDKの付属ファイルが含まれます。利用するだけならRustは不要です。
現在のパッケージは署名されていません。

1. GitHub Releasesの `bex-windows-x86_64-release.zip` を取得します。
   この設定の導入後、新しいリリースでWindowsのビルド・テスト成功後に公開されます。
   PRやmainの開発版は、成功したNative clientsの `bex-windows-x86_64-dev`
   アーティファクトから取得できます。
2. Git for Windows、Node.js、使うエージェント（Codex／Claude Code）をインストールし、
   WindowsのPATHから実行できるようにして、自分のアカウントでログインします。
   WebView2 Runtimeがない場合もインストールしてください。
3. ZIPをすべて展開し、付属ファイルを同じフォルダーに置いたまま
   `bex-desktop.exe` を開きます。スマホはアプリの招待機能でペアリングします。
4. 更新時は実行中のタスクを終え、デスクトップとHostを停止してから新しいZIPを展開します。

対象はx86-64 Windowsです。ソースビルドにはMSVC版Rust、Visual Studioの
C++ Build ToolsとWindows SDKを用意し、上記PowerShellコマンドを実行します。

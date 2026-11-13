# Windows / Windowsで使う

BEX runs on Windows 11. The EXE installer includes the desktop app, Host, CLI,
provider supervisor, and Claude SDK bridge. You do not need Rust to install it.
The package is currently unsigned.

## Install and run

1. Download `bex-windows-x86_64-setup.exe` from https://getbex.dev.
2. For Windows projects, install Git for Windows, Node.js, and your agents (Codex and/or Claude Code); sign in with your own accounts. For WSL projects, install these inside WSL as described below. Install Microsoft Edge WebView2 Runtime if missing.
3. Run the EXE installer. It installs BEX, Host, supervisor, CLI and the Claude SDK together in `%LOCALAPPDATA%\Programs\BEX`, with a Start menu shortcut. No administrator privileges or ZIP extraction are required.
4. Open BEX from Start and pair a phone through its invitation flow.

Before updating, finish active tasks and exit BEX and its Host, then run the new installer. Account and conversation data remains outside the install directory and is not removed by uninstalling. These previews are unsigned and target x86-64 Windows 11. ZIP remains an optional developer artifact.

## WSL projects

The desktop and the Windows Host run natively on Windows. WSL projects use a
separate Linux Host in their owning distribution. Choose **WSL (Ubuntu)** (or
another installed distribution) in the Host selector, or select a folder under
`\\wsl.localhost\Ubuntu\home\…` in the Windows folder dialog. BEX starts the
matching WSL Host and converts the folder to its Linux path. Git, worktrees,
agents and terminals then run inside that distribution. Windows projects
continue to use the Windows Host.
The last selected local environment is restored when BEX opens again; choose
the Windows entry to return to native Windows projects.

Install Git, Node.js and your agent CLIs and sign in **inside WSL**. Windows and
WSL accounts, conversations and settings are separate. The Windows package
includes the matching Linux Host and its libraries; WSL does not need Rust or
Nix. The current x86-64 Windows package supports x86-64 WSL distributions.
After WSL restarts, BEX discovers its new address and reconnects. Startup errors
appear in BEX; the Host log is `~/.cache/bex/wsl/host.log` inside the distribution
(or beneath `$XDG_CACHE_HOME` when configured).
When the bundled Host changes, BEX saves active work, restarts the WSL Host with
its existing settings, and continues each interrupted conversation once in the
same session. Keep other desktop instances updated too; an older connected
instance will not replace a Host that another instance has just updated.

WSL内のフォルダーをWindowsの選択画面で開くと、対応するWSLのHostを自動で起動して接続します。Git・エージェント・ターミナルはWSL内で動きます。Git、Node.js、エージェントのインストールとログインもWSL内で行ってください。Windows側のプロジェクトと接続先は引き続き利用できます。

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

Workspace file search uses the FFF Rust engine with its pure-Rust ripgrep
backend; no Zig toolchain or libclang setup is required for file search.

On Linux, build the WSL payload from the same clean commit as the Windows app:

```sh
scripts/dev-env.sh --native cargo build --locked -p host-daemon -p agent-process --bin host-daemon --bin agent-provider-supervisor
scripts/dev-env.sh --native node scripts/package-wsl-runtime.ts target/debug target/wsl-runtime
scripts/dev-env.sh --native node scripts/verify-wsl-runtime.ts target/wsl-runtime
```

Copy `target/wsl-runtime` to the Windows builder. CI supplies this payload
automatically; release packages build it with the release profile.

```powershell
# First build scripts/package-wsl-runtime.ts on Linux from this same commit,
# then supply the resulting runtime directory:
./scripts/package-windows.ps1 -WslRuntimeDirectory C:\build\wsl-runtime
# Faster local development package:
./scripts/package-windows.ps1 -Profile dev -WslRuntimeDirectory C:\build\wsl-runtime
```

Install Inno Setup 6.3+ (or supply `-InnoCompiler path/to/ISCC.exe`).
The script builds desktop, Host, supervisor, and CLI from the same checkout,
then writes the EXE installer, optional ZIP, and SHA-256 checksums under `target/windows-packages/`
(or the Cargo-configured target directory). `-SkipBuild` packages already-built
executables and is intended for CI immediately after verification.

## 日本語

getbex.dev からWindows用EXEインストーラーを取得して実行します。BEX本体・Host・CLI・Claude SDKをまとめて配置し、スタートメニューにBEXを登録します。ZIPの展開やRustの導入は不要です。Git、Node.js、利用するエージェントとWebView2は別途必要です。

更新前に実行中のタスクを終え、BEXとHostを終了してから新しいEXEを実行してください。会話・アカウントのデータはインストール先の外に保存されます。対象はWindows 11・x86-64です。

Claude sign-in opens from the requesting BEX client. The Host suppresses the CLI's automatic browser launch on Windows and Unix, so the user receives only the manual authorization URL. After approving, copy the code from the web page into BEX and choose Connect.

## DeepSeek Harness

Install Node.js 22.19 or newer and `npm install -g @deepseek-ai/dsh@0.2.1-rc.2` on the Host PC. In BEX, open **設定 → エージェント → DeepSeek** and use **接続先のAPIキー** (also available from the model picker). Choose DeepSeek, OpenRouter, NanoGPT, OpenCode, OpenAI, Anthropic, Gemini, Groq, Mistral or xAI, then enter and save its API key. **APIキーを取得** opens the selected provider’s key page. Keys are masked, stored on the connected Host separately from conversations, and never returned to clients. Saved keys can be replaced or removed individually; another provider’s key is preserved. Wait for active DSH tasks to finish before changing credentials. BEX enables the corresponding DSH routes, and their models appear in the model picker after saving. All devices connected to that Host can use these routes. Route status identifies BEX-managed keys, external official credentials and missing configuration. BEX-managed keys take precedence for its Harness process.

For NanoGPT, save its key under **NanoGPT** and choose **NanoGPT · DeepSeek V4.1 Flash** (or the Thinking variant). This route uses `https://nano-gpt.com/api/v1` and the `deepseek/deepseek-v4.1-flash` model ID. Reasoning choices are resolved separately for the selected model through Harness; catalog reads do not change the selected model.

For an OpenCode Go subscription or OpenCode Zen balance, save the API key from `https://opencode.ai/auth` under **OpenCode**. The one key enables both routes; their models appear as **OpenCode Go · …** and **OpenCode Zen · …**, and usage counts against the plan or balance of the route you choose.

Choose the **DeepSeek公式** route to use the official `deepseek-flash` model. Model names come from Harness's live catalog. BEX uses the official ACP interface for prompts, reasoning, tool results, permissions, cancellation and resuming the same native session after a Host restart. Additional messages wait until the current task finishes; fork is unavailable. Permission defaults belong to Harness; permission requests are answered in BEX.

For a separate installation, pass `host-daemon --deepseek C:\path\to\dsh\lib\bin.js`. `AGENT_DEEPSEEK_NODE` selects its Node runtime. The standard Windows installation also detects a BEX-managed Harness under `%LOCALAPPDATA%\BEX\deepseek-harness`.

Host・Windows版・Android版は同じ対応版へ更新してください。DeepSeek用APIキーをPCのBEXで設定すると、そのHostへ接続するAndroid端末からも利用できます。

## ブラウザのログインとChromeのブックマーク

BEXのWindowsブラウザは専用のWebView2プロファイルにログイン状態を保存します。最初はBEX内でサイトにログインしてください。パスワードの保存と自動入力はWebView2の標準機能を使います。

「Chromeのブックマークを取り込む」からプロファイルを選ぶと、そのブックマークを取り込めます。「ブックマーク」から開くことができ、同じURLを再取り込みすると名前を更新します。「取り込んだブックマークを削除」はBEXに保存したブックマークだけを削除します。Chromeの元データは変更しません。

Chromeのログイン状態、パスワード、Cookie、パスキーの取り込みには対応していません。取り込んだブックマークはローカルの`browser-bookmarks.json`、ブラウザの状態は`browser-profile`に保存します。端末間でブラウザの状態を同期する機能はありません。

## 使用量とリセット

利用上限のエラーに表示される「使用量・リセット時刻」から、設定のエージェント画面を開けます。使用量とローカル時刻のリセット予定を確認し、「使用量を再取得」で表示を更新できます。Codexには「追加クレジット・上限を管理」も表示され、BEXのブラウザでChatGPTの管理画面を開きます。ログイン中のアカウントを確認して操作してください。利用枠そのものをBEXから強制リセットする機能ではありません。提供元から取得した使用量には最大60秒のキャッシュがあります。

# NSH クライアント

Mac・Windows のシェルは NSH 0.5.0 と Bex 用の修正を使います。会話のスクロール領域は Mac が AppKit/TextKit、Windows が Win32/Rich Edit を所有します。iPhone の UI は `apps/mobile/iosApp` の Swift 実装を使います。
既存の `agent-core::ClientSessions` を使って Host に接続します。会話、下書き、送信、承認、モデル設定は共通 core の状態と操作を使います。

Mac アプリは同梱の Host を使います。Host が停止していれば起動し、アプリの更新は設定の「一般」から確認・適用します。
アプリのデータは Host と同じ状態ディレクトリの `nsh-client` に置きます。ソースからのビルドは配布版と別の隔離 Host とデータを使います。

Mac アプリをビルドして起動（Developer ID で署名します）:

```sh
scripts/dev-env.sh --native just dev
```

Developer ID のない環境では、`AGENT_CODE_SIGN_IDENTITY=-` で ad hoc 署名の確認用バンドルを作れます。配布はできません。

稼働中 Host に接続せず Mac 版を検証するには、隔離 Host を起動します。プロバイダー監視用の実行ファイルも Host と同じ場所に必要です。

```sh
scripts/dev-env.sh cargo build --locked -p host-fixture -p agent-process --bins --target aarch64-apple-darwin
scripts/dev-env.sh env AGENT_FIXTURE_LOCAL_HOST=1 target/aarch64-apple-darwin/debug/agent-ui-fixture target/nsh-host-native target/aarch64-apple-darwin/debug/agent-codex-fixture target/nsh-host-native-port 8000
```

別のターミナルから、ビルド済みのアプリを同じ隔離領域で起動します。

```sh
scripts/dev-env.sh env AGENT_STATE_DIR="$PWD/target/nsh-host-native" AGENT_ISOLATED_HOST=1 target/debug/bex.app/Contents/MacOS/bex
```

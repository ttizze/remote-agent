# NSH クライアント

Mac と iPhone で同じ Rust UI を動かす NSH 0.4.0 のクライアントです。macOS 26 / iOS 26 以降を対象にしています。
Mac の配布アプリ `bex.app` はこのクライアントです。iPhone 版は評価用のプレビューです。
既存の `agent-core::ClientSessions` を使って Host に接続します。会話、下書き、送信、承認、モデル設定は共通 core の状態と操作を使います。

Mac アプリは同梱の Host を使います。Host が停止していれば起動し、アプリの更新は設定の「一般」から確認・適用します。
アプリのデータは Host と同じ状態ディレクトリの `nsh-client` に置きます。ソースからのビルドは配布版と別の隔離 Host とデータを使います。

Mac アプリをビルドして起動（Developer ID で署名します）:

```sh
scripts/dev-env.sh --native just dev
```

Developer ID のない環境では、`AGENT_CODE_SIGN_IDENTITY=-` で ad hoc 署名の確認用バンドルを作れます。配布はできません。

iPhone Simulator 用アプリをビルド:

```sh
scripts/dev-env.sh just nsh-ios
```

生成物は `target/nsh-preview/ios-simulator/Bex NSH Preview.app` です。
Simulator には対象のデバイス ID を指定してインストールします。

```sh
scripts/dev-env.sh xcrun simctl install DEVICE_ID 'target/nsh-preview/ios-simulator/Bex NSH Preview.app'
scripts/dev-env.sh xcrun simctl launch DEVICE_ID com.bex.nsh-preview
```

実機用は `com.bex.nsh-preview` 専用の Development／Ad Hoc プロビジョニングプロファイルと、その証明書を指定します。
通常の Bex と別のアプリとして入り、既存のデータは保持されます。

```sh
AGENT_IOS_PROFILE=/path/to/preview.mobileprovision \
AGENT_IOS_SIGN_IDENTITY='Apple Distribution: …' \
scripts/dev-env.sh just nsh-ios ios
scripts/dev-env.sh xcrun devicectl device install app --device DEVICE_ID 'target/nsh-preview/ios/Bex NSH Preview.app'
scripts/dev-env.sh xcrun devicectl device process launch --device DEVICE_ID com.bex.nsh-preview
```

署名用キーチェーンを指定する場合は `AGENT_CODE_SIGN_KEYCHAIN` を設定します。接続先 Host はアプリ内でペアリングします。

ナビゲーション・ツールバー・一覧・検索・ボタン・メニューは UIKit／AppKit の標準コントロールを使います。
NSH の native component API はコントロールへのデータとイベントの接続に使います。Swift の部品は `apple/` の `BexNshNative.framework` で、アプリの Frameworks から読み込みます。
アプリアイコンは既存の素材、操作アイコンは OS のシステムシンボルです。音声の録音は AVFAudio、文字起こしは既存の Host の処理を使います。

稼働中 Host に接続せず Mac 版を検証するには、隔離 Host を起動します。プロバイダー監視用の実行ファイルも Host と同じ場所に必要です。

```sh
scripts/dev-env.sh cargo build --locked -p host-fixture -p agent-process --bins --target aarch64-apple-darwin
scripts/dev-env.sh env AGENT_FIXTURE_LOCAL_HOST=1 target/aarch64-apple-darwin/debug/agent-ui-fixture target/nsh-host-native target/aarch64-apple-darwin/debug/agent-codex-fixture target/nsh-host-native-port 8000
```

別のターミナルから、ビルド済みのアプリを同じ隔離領域で起動します。

```sh
scripts/dev-env.sh env AGENT_STATE_DIR="$PWD/target/nsh-host-native" AGENT_ISOLATED_HOST=1 target/debug/bex.app/Contents/MacOS/bex
```

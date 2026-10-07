# Bex

[English](README.md) | [日本語](README.ja.md)

Bexは、信頼できるコンピューター上のCodexとClaude Codeを、Macアプリ、iPhoneアプリ、Androidアプリ、またはヘッドレスCLIから操作するアプリです。Rust製のHostデーモンがエージェントのプロセスを管理します。すべてのクライアントはiroh経由で同じJSONL RPCに接続し、同じRustの`Store`へ操作を送ります。用語は[CONTEXT.md](CONTEXT.md)、設計判断は[docs/adr](docs/adr)、動作変更は[CHANGELOG.md](CHANGELOG.md)を参照してください。

irohは公開リリースを使い、netwatch 0.19.3は[UDPの再バインド回復](https://github.com/n0-computer/net-tools/pull/235)を含むフォークのコミットに固定しています。UDPの再バインド失敗時は、非同期I/Oを終了せず、100ミリ秒から5秒までの指数バックオフで再試行します。ソケットを明示的に閉じると回復処理を中止します。修正が含まれる上流リリースを採用したら、このパッチを削除します。

## 対応OS

Linux（CIの基準はUbuntu 24.04）を除き、BEXは一般提供されている最新のOSメジャーバージョンと、その安定版のマイナー・パッチリリースのみをサポートします。ベータ、RC、旧メジャー、将来のメジャーは対象外です。Apple・Androidの最小配置バージョンで旧OSへのインストールを防ぎますが、新しいOSを実行時に強制停止する上限チェックはありません。

**2026-09-13**に確認した基準:

| プラットフォーム | 対応メジャー | ビルド・テストの基準 |
| --- | --- | --- |
| iPhone / iPad | iOS / iPadOS 26 | 安定版Xcode 26。アプリ、UIテスト、Rust・Swiftバインディングの配置先は26.0。iOS 26 Simulatorを使用 |
| macOSのデスクトップ・Host・CLI | macOS Tahoe 26 | Rustの配置先、アプリ・ヘルパーバンドルの最小OS、Swiftヘルパーの対象は26.0 |
| Android | Android 17（API 37） | 最小・コンパイル・対象SDKは37。Nixのビルド・テストSDKも37 |
| Windowsのデスクトップ・Host・CLI | Windows 11、Windows Server 2025 | `windows-2025` CIでServer 2025上のビルド・テストを実行。Windows 11のUI受け入れテストは別途必要 |
| Linuxのデスクトップ・Host・CLI | Ubuntu 24.04をCI基準に使用 | GitHubホストの`ubuntu-24.04`と、固定した`nix develop .#native`環境 |

出典: [Appleのセキュリティリリース](https://support.apple.com/en-ca/100100)、[Android 17の公開](https://android-developers.googleblog.com/2026/06/Android-17.html)、[API 37のSDK設定](https://developer.android.com/about/versions/17/setup-sdk)、[Windowsクライアントのリリース](https://learn.microsoft.com/en-us/windows/release-health/windows11-release-information)、[Windows Serverのリリース](https://learn.microsoft.com/en-us/windows/release-health/windows-server-release-info)。Windowsの25H2・26H1などの機能更新は、新しいOSメジャーとは扱いません。

Android 17では、LAN接続を開く前に付近のデバイスへのアクセス許可を求めます。拒否した場合は再試行・設定ボタンを備えた許可画面に留まり、インターネット経由でも続行できます（LAN専用Hostには許可が必要です）。設定から許可を与えて戻るとアプリが開きます。インターネット経由の選択はActivityの再生成後も保持し、新規起動では許可を再確認します。保存済みHostと下書きは既存の保存領域に残ります。

新しいメジャーが一般提供されたら、提供元の公開を確認し、これらの基準をまとめて更新して、古い互換処理を削除します。対応を宣言する前にクライアントの受け入れテストを実行してください。SDKやCIランナーに含まれるだけのベータ・RCを採用してはいけません。この方針はMarkdown描画ライブラリの選択とは独立しています。

## ディレクトリ構成

| パス | 役割 |
| --- | --- |
| `crates/agent-core` | モデル、型付きRPC操作、`Snapshot`、`Store`、会話表示、iroh接続、診断。全クライアントの状態を所有 |
| `crates/agent-ffi` | Swift・Kotlinが使う`agent-core`のUniFFIバインディング |
| `crates/host-daemon` | ペアリング、認可、Host RPC、Codex・Claude Codeの実行、ワークツリー、音声入力、ファイル・ターミナル操作 |
| `crates/codex-app-server` | Codex App Serverとの通信 |
| `crates/bex-process` | プロバイダーに依存しないプロセス監視とPTYバックエンド |
| `crates/xtask` | ネイティブ受け入れテスト、ビルド出力の整理、接続診断 |
| `apps/desktop` | GPUI製のMacアプリ。Alacrittyのネイティブターミナルを搭載 |
| `apps/mobile` | UniFFI Storeを使うiOS（SwiftUI、`iosApp/`）とAndroid（Compose、`src/`） |

クライアントは不変の`Snapshot`を描画し、表示内容を再計算しません。`agent-core::presentation`が生成する`RenderedConversation`の行を、GPUIは直接、モバイルはバインディング経由で使います。新しいロジックはクライアントではなくcoreに追加してください。[ADR 0005](docs/adr/0005-rust-store-and-one-iroh-client-path.md)を参照してください。
端末への保存は[クライアント状態の保存契約](docs/CLIENT_STATE_STORAGE.md)に従います。

セッション構造、制限、ローカルデータ、検証項目は[Conversation runtime](docs/SESSION_RUNTIME.md)に記載しています。

## Hostの起動

会話にはGitと、利用可能なエージェントが最低1つ必要です。Codexは任意です。macOSではChatGPT Desktop同梱のCodexを優先し、次にPATH上の`codex`を使います。`--codex <path>`で明示した実行ファイルが最優先です。

```sh
scripts/dev-env.sh cargo build --locked -p bex-process --bin bex-provider-supervisor
scripts/dev-env.sh cargo run -p host-daemon -- --name 'BEX Host'
```

| オプション | 内容 |
| --- | --- |
| `--state-dir` | 新しいHostの認証情報ディレクトリ。未指定なら記憶したディレクトリを使用（macOSの初期値は`~/Library/Application Support/app.bex.BEX/`）。通常Hostを2つ起動するための指定ではありません |
| `--codex-home` | 別のCodex保存領域を選択 |
| `--claude <path>` | Claude Codeの実行ファイル。既定値はPATH上の`claude`。デスクトップは設定済みの`BEX_CLAUDE`を渡します |
| `--relay-url <url>…` / `--no-relay` | 独自のirohリレー、または隔離テスト用のローカルアドレスのみを使用 |
| `--isolated --state-dir <directory>` | 明示的に分離した開発・テストHost。自身のディレクトリロックも保持します |

通常起動では、デスクトップとデーモンの状態ディレクトリが異なっていても、OSユーザーごとに1つのHostを共有します。プラットフォームのデータディレクトリに`host-instance.json`と短時間の探索ロックを置き、各Hostは稼働中ずっと自身のディレクトリをロックします。再起動後も同じHostの識別情報とディレクトリを使います。競合する起動は認証情報を作る前に拒否し、デスクトップは登録済みHostに認証して接続します。

同じコンピューター上のクライアントは、リレーとアドレス検索を無効にしたIPv4ループバックQUICで接続します。`host.ticket`にはループバックアドレスのみを含め、リモート用招待は通常のLAN・インターネット用エンドポイントを使います。どちらも同じ認証済みRPCとHostの状態を使用します。デスクトップは接続断でもStoreを保持し、現在のローカルHostのアドレスを再探索して、250ミリ秒から5秒の指数バックオフで復帰します。復帰時はHostの状態を読み直し、結果が不明な送信を自動再送しません。停止済みのHostをバックグラウンドの復帰処理で起動することもありません。

バックアップが必要で、Gitにコミットしてはいけないファイルは、識別鍵の`identity.keys`（64バイト、所有者のみアクセス可能）と、招待・許可リスト・リモートチケットを含む`trust.json`です。エラーは`logs/host.jsonl`（デスクトップは`logs/desktop.jsonl`）に追記し、5 MiBごとにローテーションして4世代を保持します。認証情報は可能な範囲で伏せます。

Codexのイベント処理終了は、プロセスの片付け前に`host.codex.event_stream_stopped`として記録します。停止原因、通信エラー・欠落イベント数、プロバイダーのインスタンス、最終処理シーケンス、停止要求の有無を含みます。要求された停止は`info`、予期しない終了は`error`です。会話本文は記録しません。

接続障害では`network.change`、`network.socket.rebound`、`network.socket.rebind_failed`、`network.socket.closed`、`network.endpoint.receive_failed`、QUICの`network.*.io_error`も記録します。固定したiroh・netwatch・noqから、エラー種別と数値のOSエラーコードを記録し、依存ライブラリのメッセージ、アドレス、ペイロードは保存しません。同じ障害は操作ごとに30秒で1件までとし、コードの変更、ネットワーク変更後、再バインド成功後の障害は直ちに記録します。`previous_suppressed`は省略した反復回数です。ネットワーク変更・再バインドで制限をリセットした場合は、操作をまたいだ省略回数を示します。接続性能タイムラインを無効にしても、これらの記録は残ります。調査前の再起動は避け、Host・Desktop双方のローテーション済みログ、時刻、PID、ビルドリビジョンを確認してください。

### ヘッドレスLinuxとiPhoneのペアリング

固定したLinux環境でHostと監視用実行ファイルをビルドします。

```sh
nix develop .#native --command cargo build --locked --release -p host-daemon -p bex-process
target/release/host-daemon --name 'Linux development'
```

別のシェルから、同じOSユーザーとして管理コマンドを実行します。稼働中Hostを探索して認証し、別のデーモンを起動したり認証情報を書き換えたりしません。

```sh
target/release/host-daemon status
target/release/host-daemon invite
# qrencodeがある場合、招待をQRコードとして表示:
target/release/host-daemon invite | qrencode -t ANSIUTF8
# statusに表示されたノードIDで端末のアクセスを取り消す:
target/release/host-daemon revoke <node-id>
```

iPhoneで**PC一覧 → PCを追加 → QRコードを読み取る**を開くか、**QRの内容を手入力**に招待JSONを貼り付けます。招待の有効期限は既定で7日、登録できる新規端末は1台です。Hostの起動時に`--invitation-days <1–90>`を指定すると、新しく発行する招待の期限だけを変更できます。端末ごとに別の招待を発行し、出力をログやGitへ保存しないでください。登録済み端末は招待の期限後も再接続できます。Hostの識別情報と登録済み端末は再起動後も保持します。隔離開発Hostでは、`invite`・`status`の前にも同じ`--isolated --state-dir <directory>`を指定します。

インターネット経由の接続には既定のirohリレーを有効にします。Git、エージェントの実行ファイル、ChromiumがPATH上にある開発ユーザーでサービスを動かし、そのユーザーのエージェントアカウントを認証してください。`bex-provider-supervisor`は`host-daemon`と同じディレクトリに置きます。iPhoneから操作するのはLinux上のファイル、ターミナル、ブラウザーです。プロジェクトのタスク開始時はLinux側のチェックアウトを選びます。systemdでは`KillSignal=SIGINT`を使い、Hostがプロバイダーを正常終了できるようにします。

信頼できる共有環境では、`codex login --with-api-key`の標準入力でチーム用APIキーを設定できます。Hostはこれを**API key**アカウントとして検出し、再起動後も選択を保持します。API料金はChatGPTのサブスクリプション使用量と別なので、サブスクリプションの残量は表示しません。共有Codex設定で`forced_login_method = "api"`を指定するとAPI認証を維持できます。ペアリングした全端末がHostのアカウントと認証情報を共有し、タスクごとのワークツリーでソース変更を分離します。

ワークツリー設定の`bex-worktrees.json`と、プロジェクトに属さないチャットの`chats/`は、通常`$CODEX_HOME`または`~/.codex`のCodexプロジェクト状態の隣に保存します。新規ワークツリーの既定パスは`<original-repository>/.worktree/session-XXXXX/<repository-name>`です。独自ディレクトリを設定すると`.worktree`部分を置き換えます。別のワークツリーから開始しても、チェックアウトのフォルダー名はリポジトリ名を使います。既存ワークツリーは移動しません。既定の`.worktree`はGitのローカル`info/exclude`で除外します。設定画面はMacの**設定 → ワークツリー**、iPhoneの**タスク一覧 → … → 設定 → ワークツリー**です。

iPhoneから保存済みPCを削除するには、**タスク一覧 → PC一覧 → 接続を解除**を開いて確認します。そのiPhoneの認証鍵を削除し、再起動後も再接続しません。Hostの会話データは保持します。Macの**設定 → 端末と接続 → 接続を解除**は、端末のアクセスを取り消して接続を閉じます。再接続には再ペアリングが必要です。iPhoneでPCを削除しても、Macにある以前の端末エントリーは削除しません。

## クライアントのビルド

```sh
# Mac（署名証明書が必要。BEX_CODE_SIGN_IDENTITYで選択）
scripts/dev-env.sh just build-desktop-macos && open target/Bex.app

# iPhone（iOS 26）: Simulator用ライブラリをビルドしてXcodeを開く
scripts/dev-env.sh scripts/build-agent-ios.sh simulator
open apps/mobile/iosApp/Bex.xcodeproj

# Android 17（API 37）
scripts/dev-env.sh ./gradlew :apps:mobile:assembleDebug
```

Rustのソースを変更したら、iOS用ライブラリも再ビルドしてください。デスクトップの下書きとログは`BEX_STATE_DIR`を使いますが、Host探索で別の認証情報ディレクトリを選ぶことがあります。デスクトップを隔離するには`BEX_ISOLATED_HOST=1`と`BEX_STATE_DIR`の両方が必要です。個人のプロバイダー状態をテストが読み込まないよう、別のCodexホームかフィクスチャ実行ファイルも使ってください。

`scripts/dev-env.sh just dev`は、プロバイダーのアカウントと会話履歴を共有する別のローカルHostを起動します。再ビルド前に実行中タスクを確認し、古い開発Hostを停止してください。ウィンドウを閉じるだけではHostは停止しません。同じ会話を2つのHostで同時に実行しないでください。

## 会話の操作

会話管理は T3 orchestration-v2 を Rust に移植しています。GPUI・SwiftUI・Compose
は共通の棚、タイムライン、承認・質問、キュー、モデル・実行モードを表示します。
通常送信は実行中ならキューへ入り、steer・restart・stop は明示的に選びます。
要件は [表示契約](docs/DESKTOP_CONVERSATION_DISPLAY_CONTRACT.md)、範囲は
[移植計画](docs/t3-port/PLAN.md) に記録しています。

### Claude Code

追加アカウントはHostの状態ディレクトリ内に独立したClaude設定ディレクトリを持ち、認証情報はClaude Codeが管理します。各`projects`ディレクトリはネイティブの会話記録へリンクするため、アカウント変更で履歴は失われません。選択はHostの再起動後も保持し、次のターンから適用します。実行中のターンは元のアカウントで完了します。

HostにNode.js 18以上とClaude Codeを通常のパッケージ管理でインストールし、BexからClaudeのサブスクリプションアカウントを追加するか、Host上の既存の`claude auth login`を使います。新しい会話を作成し、入力欄のモデル選択で**Claude · …**を選びます。モデル名と対応する推論強度は、インストールしたCLIの初期化応答から取得し、選択中アカウント用にキャッシュします。アカウント変更時にキャッシュを無効化します。Claude実行ファイルがない場合もCodexモデルは表示し、プロバイダーの失敗は利用可能なモデルとともに報告します。不完全な一覧でも保存済みモデルの選択を保持し、新規下書きは利用可能なモデルを選びます。

HostはNode子プロセスから固定版のClaude Agent SDKを呼びます。SDKが変更していないCLIを動かし、Hostがアプリの承認判断とプロセス管理を担当します。Claudeが認証情報を管理し、BexはサブスクリプショントークンをCodexへコピーしたり、Anthropicの推論APIを直接呼んだりしません。Claudeのターンにはサブスクリプション認証が必要で、APIキー認証・未認証では送信前に拒否します。Hostの環境でAPIキーを設定している場合は、その上書きを外してください。Codexのアカウント選択はCodexにのみ適用します。Anthropicの[認証](https://code.claude.com/docs/en/authentication)、[プログラム実行](https://code.claude.com/docs/en/headless)、[組み込み条件](https://code.claude.com/docs/en/legal-and-compliance)を参照してください。

テキスト、画像、ファイル参照、ツール承認、質問、中断、キューとsteerは、共通のRust会話状態機械と認証済み接続を使います。Hostは会話の事実を保存し、登録済みprojectの既存Codex・Claude記録を取り込みます。クライアントの再接続では、保存したcursorからsnapshotと事実を同期します。

会話内で次のターンのproviderを変更し、完了したrunからforkし、side chatを開き、作業を委任できます。HostはproviderプロセスとPTYを独立して所有し、providerが失敗してもペアリング、ファイル、作業場所の操作を維持します。Claude SDKがCLIとの通信を担当し、Rustがアプリの状態、要求の配送、永続的な操作を管理します。移植の残作業と検証範囲は[実装状況](docs/t3-port/IMPLEMENTATION.md)を参照してください。

Macで会話内のファイルリンクを開くと、FilesパネルにHost上の親ディレクトリとエディターを表示し、未保存の編集とリビジョン確認を保持します。Side Chatは独自パネルでファイル・差分を表示し、下書きを保ったまま会話へ戻れます。

Macで**ブラウザ → Chromeから取り込む**を開き、Chromeのプロファイルを選ぶと、CookieをBexのブラウザーへコピーします。macOSが**Chrome Safe Storage**のキーチェーンアクセスを求めたら許可してください。Chromeのデータベースは変更せず、ドメイン・名前・パスでCookieを統合し、保存を確認してから現在のページを再読み込みします。永続CookieはBex再起動後も期限を保持し、セッションCookieはセッション内のみ有効です。継続同期ではなく1回のコピーなので、ログインを更新するには再取り込みします。期限切れ・トップレベルサイトで分割されたCookieは除外して件数を表示します。ローカルストレージや端末に紐付く認証など、Cookie以外の状態はコピーしないため、Bex内でのログインが必要なサイトもあります。

Cookie取り込みはmacOS Chromeのデータベースバージョン23・24に対応します。読み取り・属性の回帰確認は`scripts/dev-env.sh cargo test --locked -p bex-desktop --bin bex-desktop chrome_tests`で実行します。macOS 26の`scripts/dev-env.sh just macos-e2e`は、使い捨てWebKit保存領域で、実際のBrowser画面、暗号化フィクスチャ、アクセス拒否後の再試行、認証付きHTTP、HttpOnly保護、新規プロセスでの永続性を検証します。固定したlb-wryの`set_cookie`でドメインの範囲が失われる問題も記録しており、そのため取り込みはWebKitのネイティブCookieストアを直接使います。

デスクトップの会話一覧・タイトルはメッセージ送信とターン完了後に更新します。実行ヘッダーは経過時間と最新の操作を表示し、コマンド出力にはコピー操作があります。変更要約・ファイルを押すと差分を開き、ファイル選択や長い未変更部分の展開ができます。音声入力中は、キャンセル・停止・送信付きの波形を表示します。Escapeで録音を中止して下書きを保持します。

Macの設定は、Bexが作成したワークツリーと会話を**作成済みのワークツリー**に表示します。削除前に別の会話へ移動し、削除を確認します。実行中ターン、未回答の要求、未確定の送信、追跡中ターミナル、変更済み・未追跡・無視対象ファイル、ワークツリーロック、detached HEADがある場合はHostが拒否します。削除後もブランチ、会話履歴、一覧のエントリーを保持します。次のメッセージ送信時は、元リポジトリのローカル`main`から作った新しいブランチで、同じパスへ再作成します。永続化するのはワークツリーと元プロジェクトのパスのみで、以前のブランチ・削除状態は保存しません。

**マージ済みを自動削除**は既定で無効です。有効にするとHostが毎分確認し、作成時点より先のコミットがあり、現在のHEADがローカル`main`に含まれる、未使用で変更のないBexワークツリーを削除します。プロバイダーの履歴へまだ現れていない実行中セッションも含め、同じ保護条件を適用します。使用中のものは後で再確認し、新しい未統合コミットがあれば対象から外します。活動・Git状態を確認できなければ保持します。ブランチ・会話は残り、次のメッセージで再作成できます。

音声入力は、停止時に認識した文章を下書きへ追加し、送信時には下書き・添付とともに送ります。文字が認識されなかった正常終了は静かに終わり、送信を選んでいても下書き・添付を保持します。録音、接続、不正な応答、文字起こしの失敗はエラーを報告しますが、下書きは破棄しません。

## Agent peer CLIとスキル

[`tools/agent-peer`](tools/agent-peer)は、Claude・Codexへの相談CLI、スキル、テスト、単独Nixパッケージの共有ソースです。BEX側の利用箇所とともにここで開発します。このディレクトリだけでもmacOS・Linuxでビルドできます。

```sh
nix profile install .#agent-peer
agent-peer-install-skills
```

公開リポジトリ`ttizze/agent-peer`には、このディレクトリだけをMITライセンスで含めます。コミット済みリビジョンをsubtree splitで公開し、BEXの残りのソース・履歴を除外します。

```sh
peer_commit=$(git subtree split --prefix=tools/agent-peer HEAD)
git push git@github.com:ttizze/agent-peer.git "$peer_commit:refs/heads/main"
```

変更はここで行い、subtreeを再公開します。VPSには`nix profile install github:ttizze/agent-peer/<commit>`で固定コミットを入れ、エージェントのサービスユーザーで`agent-peer-install-skills`を実行します。プロバイダーCLIと認証は各コンピューター内に保持します。詳細は単独パッケージのREADMEを参照してください。

## 検証

T3 移植では変更 crate の単体・property test とネイティブのビルドだけを確認します。
CI 待ち、cargo-mutants、E2E、Simulator UI テストは実行しません。
旧会話の型・fixture・検証ランナーは削除しました。

```sh
scripts/dev-env.sh cargo nextest run -p agent-domain -p agent-providers -p agent-runtime -p agent-protocol -p agent-transport -p agent-core -p host-daemon -p bex-desktop --lib --bins --features agent-core/bindings
scripts/dev-env.sh scripts/build-agent-ios.sh simulator
nix develop .#android --command ./gradlew :apps:mobile:assembleDebug
```

一般のテスト方針は [TEST_MAINTENANCE.md](docs/TEST_MAINTENANCE.md) を参照してください。

## GitHubの保守

[mainのルールセット](.github/main-ruleset.json)に、GitHubへ適用する保護設定を記録しています。mainへの直接pushを許可します。任意のPRでは、Bexのマージ済みワークツリー整理でコミットの祖先関係を保つため、マージコミットを使い、squash・rebaseによるマージは無効にしています。

[Dependabot](.github/dependabot.yml)は毎週月曜09:00（日本時間）に、Cargo（ワークスペース・agent-peer）、Gradle、XcodeのSwiftパッケージ、npm、2つのNix flake、GitHub Actionsを確認します。マイナー・パッチ更新はエコシステムごとにまとめ、メジャー更新は個別PRにします。Nixの入力はまとめます。通常のバージョン更新は公開から7日待ち、セキュリティ更新にはこの待機を適用しません。更新PRも通常の変更と同じネイティブ検証を実行し、保守担当者が確認してCI成功後に統合します。自動マージは無効です。統合済みのリモートブランチは自動削除します。

Lucideの更新は[可視化ランタイムのREADME](crates/host-daemon/src/visualize/README.md)のコマンドで生成し、同READMEのバージョン・取得元・チェックサムも更新してください。npmのmanifest・lockfileと一緒に、生成したバンドル・ライセンスをコミットします。CIは再生成して古いバンドルを検出します。リポジトリ内へ取り込んだRust・Androidソースと、固定したnetwatchの回復パッチは、上流の変更を手動で確認する必要があります。

Dependabotの脆弱性通知・セキュリティ更新、秘密情報のスキャン・push保護を有効にしています。Actionsは完全なコミットSHAに固定し、既定トークンは読み取り専用で、自分のPRを承認できません。脆弱性の非公開報告方法は[SECURITY.md](SECURITY.md)を参照してください。

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
| `crates/agent-cli` | スクリプト・結合テスト用のヘッドレスクライアント |
| `crates/host-fixture` | 決定的に動作するCodex・Claudeの子プロセスと、テスト用の隔離iroh Host |
| `crates/xtask` | ネイティブ受け入れテスト、ビルド出力の整理、接続診断 |
| `apps/desktop` | GPUI製のMacアプリ。Alacrittyのネイティブターミナルを搭載 |
| `apps/mobile` | UniFFI Storeを使うiOS（SwiftUI、`iosApp/`）とAndroid（Compose、`src/`） |

クライアントは不変の`Snapshot`を描画し、表示内容を再計算しません。`agent-core::presentation`が生成する`RenderedConversation`の行を、GPUIは直接、モバイルはバインディング経由で使います。新しいロジックはクライアントではなくcoreに追加してください。[ADR 0005](docs/adr/0005-rust-store-and-one-iroh-client-path.md)を参照してください。
端末への保存は[クライアント状態の保存契約](docs/CLIENT_STATE_STORAGE.md)に従います。

セッション構造、制限、ローカルデータ、検証項目は[Session runtime](docs/SESSION_RUNTIME.md)に記載しています。

## Hostの起動

会話にはGitと、利用可能なエージェントが最低1つ必要です。Codexは任意です。macOSではChatGPT Desktop同梱のCodexを優先し、次にPATH上の`codex`を使います。`--codex <path>`で明示した実行ファイルが最優先です。

Claudeとの会話はNode.js上の公式Agent SDKが実行します。HostのPATHにNode.js 22以降を置くか、`BEX_NODE`で実行ファイルを指定してください。Nixの開発環境はNode.jsを含みます。SDKはnpmのlockfileに固定し、Hostへ同梱するコードを生成するため、実行時にnpmでインストールする必要はありません。

```sh
scripts/dev-env.sh cargo build --locked -p bex-process --bin bex-provider-supervisor
scripts/dev-env.sh node scripts/install-claude-sdk.mjs target/debug
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
nix develop .#native --command cargo build --locked --release -p host-daemon -p bex-process -p agent-cli
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

ワークツリー設定の`bex-worktrees.json`と、プロジェクトに属さないチャットの`bex-chats/`は、通常`$CODEX_HOME`または`~/.codex`のCodexプロジェクト状態の隣に保存します。新規ワークツリーの既定パスは`<original-repository>/.worktree/session-XXXXX/<repository-name>`です。独自ディレクトリを設定すると`.worktree`部分を置き換えます。別のワークツリーから開始しても、チェックアウトのフォルダー名はリポジトリ名を使います。既存ワークツリーは移動しません。既定の`.worktree`はGitのローカル`info/exclude`で除外します。設定画面はMacの**設定 → ワークツリー**、iPhoneの**タスク一覧 → … → 設定 → ワークツリー**です。

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

Mac・iPhoneでは、アシスタントの文章を選択して下書きに引用したり、サイドチャットで質問したりできます。iPhoneのサイドチャットを閉じると、元の会話と下書きへ戻ります。Macには右クリックのコピー・Google検索、自分のメッセージのホバーによるコピー・入力欄への戻しもあります。コマンドの実行内容は実行中・再表示後とも最初は折り畳み、明示的に展開した状態は保持します。[会話表示の契約](docs/DESKTOP_CONVERSATION_DISPLAY_CONTRACT.md)を参照してください。

Mac・iPhoneでは、枠のないFast、モデル名、推論強度の操作をマイク・送信の直前に置きます。モデル名を押すと、エージェント・アカウントの行と、検索できるモデル一覧を開きます。アカウント行は報告された週次使用枠を表示し、切り替えや、追加・ログイン・確認付きサインアウトの管理画面へ進めます。設定からも同じ管理画面に入れます。アカウント変更では、対応するモデル設定、本文、添付を保持したまま一覧を更新します。認証とプロバイダーごとのアカウント選択はHostが所有し、接続済みクライアントで共有します。現在はCodex・Claudeに対応し、Pi・第三者の接続アダプターは未対応です。Claudeではログインページを開き、返された認証コードをBexへ貼り付けます。

### Claude Code

追加アカウントはHostの状態ディレクトリ内に独立したClaude設定ディレクトリを持ち、認証情報はClaude Codeが管理します。各`projects`ディレクトリはネイティブの会話記録へリンクするため、アカウント変更で履歴は失われません。選択はHostの再起動後も保持し、次のターンから適用します。実行中のターンは元のアカウントで完了します。

Hostに通常のパッケージ管理でClaude Codeをインストールし、BexからClaudeのサブスクリプションアカウントを追加するか、Host上の既存の`claude auth login`を使います。新しい会話を作成し、入力欄のモデル選択で**Claude · …**を選びます。モデル名と対応する推論強度は、インストールしたCLIの初期化応答から取得し、選択中アカウント用にキャッシュします。アカウント変更時にキャッシュを無効化します。Claude実行ファイルがない場合もCodexモデルは表示し、プロバイダーの失敗は利用可能なモデルとともに報告します。不完全な一覧でも保存済みモデルの選択を保持し、新規下書きは利用可能なモデルを選びます。

Hostは変更していないCLIを、ストリーミングJSON入出力と独自の権限コールバックで動かします。Claudeが認証情報を管理し、BexはサブスクリプショントークンをCodexへコピーしたり、Anthropicの推論APIを直接呼んだりしません。Claudeのターンにはサブスクリプション認証が必要で、APIキー認証・未認証では送信前に拒否します。Hostの環境でAPIキーを設定している場合は、その上書きを外してください。Codexのアカウント選択はCodexにのみ適用します。Anthropicの[認証](https://code.claude.com/docs/en/authentication)、[プログラム実行](https://code.claude.com/docs/en/headless)、[組み込み条件](https://code.claude.com/docs/en/legal-and-compliance)を参照してください。

テキスト、PNG・JPEG・GIF・WebP画像、ファイル参照、ツール承認、ユーザーへの質問、中断、後続ターンは、Codexと同じStore・認証済み接続を使います。作業ディレクトリ選択と自動ワークツリー設定も共通です。永続的な会話の唯一の保存元はClaudeのネイティブ記録です。Hostは`--claude-home`、`CLAUDE_CONFIG_DIR`、または`~/.claude`を推論実行なしで読み、Bex独自の会話記録は書きません。クライアントを切断しても実行は続き、再接続ではイベントを再送せず、現在有効な未回答の要求を含む最新の状態を開きます。

CodexとClaudeの切り替えには新しい会話を作成します。Claudeへの後続入力は、現在のターンの完了・中断後に送れます。実行中は入力欄でこの制限を示し、下書きを保持します。ClaudeからCodexへの委任、Claudeのフォーク・サイドチャットは未実装です。Codexの起動失敗・終了でも、Host、Claudeの実行、ペアリング、ファイル、作業場所の選択は停止しません。HostはCodexと独立してPTYを所有し、接続ごとに制限し、切断・停止時に片付けます。音声入力とCodexアカウント操作にはCodexが必要です。Codexの会話活動を確認できない場合はワークツリー削除を禁止します。CLI連携はClaude Code 2.1.266で検証しています。

Macで会話内のファイルリンクを開くと、FilesパネルにHost上の親ディレクトリとエディターを表示し、未保存の編集とリビジョン確認を保持します。Side Chatは独自パネルでファイル・差分を表示し、下書きを保ったまま会話へ戻れます。

Macで**ブラウザ → Chromeから取り込む**を開き、Chromeのプロファイルを選ぶと、CookieをBexのブラウザーへコピーします。macOSが**Chrome Safe Storage**のキーチェーンアクセスを求めたら許可してください。Chromeのデータベースは変更せず、ドメイン・名前・パスでCookieを統合し、保存を確認してから現在のページを再読み込みします。永続CookieはBex再起動後も期限を保持し、セッションCookieはセッション内のみ有効です。継続同期ではなく1回のコピーなので、ログインを更新するには再取り込みします。期限切れ・トップレベルサイトで分割されたCookieは除外して件数を表示します。ローカルストレージや端末に紐付く認証など、Cookie以外の状態はコピーしないため、Bex内でのログインが必要なサイトもあります。

Cookie取り込みはmacOS Chromeのデータベースバージョン23・24に対応します。読み取り・属性の回帰確認は`scripts/dev-env.sh cargo test --locked -p bex-desktop --bin bex-desktop chrome_tests`で実行します。macOS 26の`scripts/dev-env.sh just macos-e2e`は、使い捨てWebKit保存領域で、実際のBrowser画面、暗号化フィクスチャ、アクセス拒否後の再試行、認証付きHTTP、HttpOnly保護、新規プロセスでの永続性を検証します。固定したlb-wryの`set_cookie`でドメインの範囲が失われる問題も記録しており、そのため取り込みはWebKitのネイティブCookieストアを直接使います。

デスクトップの会話一覧・タイトルはメッセージ送信とターン完了後に更新します。実行ヘッダーは経過時間と最新の操作を表示し、コマンド出力にはコピー操作があります。変更要約・ファイルを押すと差分を開き、ファイル選択や長い未変更部分の展開ができます。音声入力中は、キャンセル・停止・送信付きの波形を表示します。Escapeで録音を中止して下書きを保持します。

Macの設定は、Bexが作成したワークツリーと会話を**作成済みのワークツリー**に表示します。削除前に別の会話へ移動し、削除を確認します。実行中ターン、未回答の要求、未確定の送信、追跡中ターミナル、変更済み・未追跡・無視対象ファイル、ワークツリーロック、detached HEADがある場合はHostが拒否します。削除後もブランチ、会話履歴、一覧のエントリーを保持します。次のメッセージ送信時は、元リポジトリのローカル`main`から作った新しいブランチで、同じパスへ再作成します。永続化するのはワークツリーと元プロジェクトのパスのみで、以前のブランチ・削除状態は保存しません。

**マージ済みを自動削除**は既定で無効です。有効にするとHostが毎分確認し、作成時点より先のコミットがあり、現在のHEADがローカル`main`に含まれる、未使用で変更のないBexワークツリーを削除します。プロバイダーの履歴へまだ現れていない実行中セッションも含め、同じ保護条件を適用します。使用中のものは後で再確認し、新しい未統合コミットがあれば対象から外します。活動・Git状態を確認できなければ保持します。ブランチ・会話は残り、次のメッセージで再作成できます。

音声入力は、停止時に認識した文章を下書きへ追加し、送信時には下書き・添付とともに送ります。文字が認識されなかった正常終了は静かに終わり、送信を選んでいても下書き・添付を保持します。録音、接続、不正な応答、文字起こしの失敗はエラーを報告しますが、下書きは破棄しません。

## ヘッドレスCLI

```sh
agent-cli --ticket <endpoint-ticket> --identity-file <32-byte-key> [--invitation <uuid>] list
agent-cli <connection> send <thread-id> <text> --client-message-id <id> [--model m] [--effort e]
agent-cli <connection> approve 7 --decision 2               # 数値の要求ID
agent-cli <connection> approve '"request-id"' --decision 2  # 文字列の要求ID
```

ローカルのフィクスチャではチケットの代わりに`--stdio <fixture-executable>`を使います。結果はJSONとして標準出力へ、エラーは標準エラーへ出力します。

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

```sh
scripts/dev-env.sh just unit-tests                  # 全ユニットテスト。Simulator・エミュレーターは起動しない
scripts/dev-env.sh cargo test --workspace           # 振る舞いコーパスを含むRustテスト
scripts/dev-env.sh just iroh-e2e                    # 隔離iroh接続で実デーモンを検証
scripts/dev-env.sh just android-e2e                 # Android 17: 接続許可、Markdown、永続化、Host復帰
scripts/dev-env.sh just ios-e2e [TestMethod…]        # フィクスチャHostに対するSimulator XCUITest
scripts/dev-env.sh just conversation-ui            # 選択、サイドチャット、実行表示の回帰確認
scripts/dev-env.sh just macos-e2e                  # ネイティブBrowserの認証と永続化
scripts/dev-env.sh just quality [apple|rust|kotlin|swift]  # 既定はMac Host・デスクトップとiPhone
```

Claudeの契約テストは`scripts/dev-env.sh cargo test --locked -p host-fixture --test claude`です。先に監視用実行ファイルをビルドしてください。決定的に動作する外部CLI境界と、実際のStore・iroh・Hostルーティング・ネイティブ会話ファイル・隔離Git環境を使います。Claude Code 2.1.266の匿名化した記録でもネイティブ形式との互換性を確認します。任意実行の`live_claude_subscription_completes_and_resumes_through_store_and_host`は実際の認証済みCLIを使います。`BEX_LIVE_CLAUDE_PROGRAM`に絶対パスを設定し、`-- --ignored --exact`を付けて実行すると、サブスクリプションでの推論、Host再起動後の再開、中断、中断後の入力を確認できます。

`scripts/dev-env.sh just unit-tests`で全ローカルユニットテストを実行してから、PR作成やCI完了待ちを必須にせず、mainへ直接統合します。Native clients GitHub Actionsはmainへのpush、任意のPR、手動実行で全検証を行い、CIの失敗はmain上で修正します。force push・main削除は管理者を含め全員に禁止します。検証完了を報告するには、対象コミットのCI成功と、変更のない作業ツリーが必要です。コミットでローカルのバックグラウンド検証は起動しません。ユニットテストと手動の調査コマンドはローカルで使えます。Linux CIはGitHubホストのUbuntu 24.04と`nix develop .#native`を使い、同じ基準でツールチェーンを取得します。Windowsも固定したflakeのRustバージョンを使います。Lintはベースラインなしの既定基準で、ルールの例外にはレビューが必要です。

Android CIは、固定したNix SDKとUbuntu 24.04でKotlinのチェック・ユニットテスト、アプリ・計装テストAPKのビルド、KVMを使ったエミュレーター受け入れテストを行います。Apple CIはXcode 26.6の`macos-26` Apple Siliconランナーで、ローカルと同じ既定の`just quality`を実行します。共通Rust・Swiftチェックの後、Mac Browser E2EとiPhoneの会話テストを並行実行し、iPhoneは2組の隔離Simulator・Hostを使います。CargoとXcodeのderived dataをキャッシュし、失敗時を含めログとXcode結果バンドルを7日間保持します。Linux・Windows・Android・Mac・iPhoneの検証はNative clients CIで実行します。Androidの調査には`just android-e2e`・`just quality kotlin`も使えます。

開発・テストビルドは、全変数のデバッグ情報を含めず、ファイル名・行番号付きバックトレースを保持します。デバッガーで変数が必要なら`CARGO_PROFILE_DEV_DEBUG=full`を指定してください。品質チェックではRustのインクリメンタルコンパイルを無効にし、通常のローカルビルドのインクリメンタル状態はKacheが管理します。

`default`・`native`のNixシェルは、バージョンを固定したKacheをローカルRustビルドのラッパーとして使います。内容に基づく共有キャッシュで、Cargoのインデックス・ロック・ビルド出力を分離したままワークツリー間で対応するコンパイル結果を再利用します。Linux・macOSでは対応するRust実行ファイルやビルドスクリプトの実行結果もキャッシュし、頻繁に変わるクレートでは適応型インクリメンタル処理を使えます。`scripts/dev-env.sh kache stats --last-build`で再利用を確認し、`scripts/dev-env.sh kache explain`でミスの原因を調べられます。`KACHE_CACHE_DIR`で共有キャッシュの保存場所を指定できます。`bex.buildRoot`が設定されている場合、`scripts/dev-env.sh`は既定で`<buildRoot>/.kache`を使い、同じディスク上の出力をcopy-on-writeで復元できるようにします。それ以外ではmacOSの`~/Library/Caches/kache`、Linuxの`~/.cache/kache`を使います。`CI`が設定された環境ではこのラッパーを使わず、既存のCargoキャッシュを使います。

`just unit-tests`は、ネイティブバインディングを有効にしたRustワークスペースのライブラリ・バイナリテスト、単独agent-peerのCargoテスト、macOSのヘッドレスSwift Markdownテストを実行します。RustはNix固定のcargo-nextestでクレートをまたいで並行実行し、Swiftも並行で進めます。両方の結果を待って失敗を集計します。Androidには現在JVMユニットテストがなく、計装テストはCIで実行します。CIも結合・E2E確認の前に同じコマンド・Rust機能構成を使い、Nixのagent-peerパッケージも確認します。監視用実行ファイルはテスト前にビルドします。`agent-ffi/bindgen`はバインディング生成時だけ有効にしてください。ユニットテストはなく、ワークスペーステストに含めると不要な依存構成でネイティブライブラリを置き換えます。整理・接続診断・ネイティブテストはRustの`cargo xtask`で管理します。macOSのプロセス調査はOS付属の`lsof`、LinuxはNix版を使います。Swift Markdownテストは、ソース・バインディング・コンパイラー・SDK・ランナーが一致するときのみビルドを再利用し、毎回現在のRustライブラリへアサーションを実行します。iOS UIテストは初期化済みの空Simulatorを複製して隔離Hostと組み合わせ、`BEX_IOS_TEST_WORKERS=1`–`10`で並行数を指定できます。リポジトリ共通ロックで、ワークツリー・Cargo出力をまたぐiOS確認を直列化します。

`scripts/dev-env.sh`は`flake.nix`・`flake.lock`・Kacheのパッケージ定義・プラットフォームが同じなら、ワークツリー間で固定Nix環境を再利用します。キャッシュ利用時はNixを起動せず、共有プロファイルで固定ツールをGCから保護します。Cargoのインデックス・ロック・`target`はワークツリーごとに持ち、同じワークツリーのテスト・開発は同じキャッシュ・出力を使います。固定済みの依存ソースとアーカイブだけを既存キャッシュからコピーし、可能ならAPFSのcopy-on-write・reflinkで容量を共有します。変更済みソースは再コンパイルし、未変更の出力は再利用します。テストは必要なヘルパー・バインディングをビルドし、開発アプリのビルド・インストール・再起動は開発環境へ適用するときに行います。

新規ワークツリーのビルド出力を外付けディスクへ置くには、マウント済みディスクにディレクトリを作り、`git config --local bex.buildRoot /absolute/path/to/builds`を設定します。このローカル設定は全ワークツリーで共有します。`scripts/dev-env.sh`は新しい`target`を外付け上の専用ディレクトリへのシンボリックリンクにし、Cargo・Xcode・生成バインディング・キャッシュの既存パスを維持します。既存`target`は移動しません。移す場合は使用していない間に移動して元の場所をリンクへ置き換えてください。指定ディレクトリは事前に存在する必要があり、ディスク取り外しなどで利用できなければコマンドは失敗します。`git config --local --unset bex.buildRoot`で設定を削除すると、その後の新規ワークツリーはローカル出力へ戻ります。

完了した`just quality`は毎回、登録済みワークツリーの未使用Cargo出力を整理します。3日以上更新のないプロファイルを削除し、残りも未使用出力が合計32 GiB以下になるまで古い順に削除します。`scripts/dev-env.sh just clean-builds --dry-run`でJSONの計画を確認し、`--dry-run`なしで適用できます。通常の`target`と、その直下のCargoキャッシュを調べ、リンク先のキャッシュは追いません。

整理はCargoのビルド・成果物ロックを保持し、ロックのinodeを維持します。稼働中バイナリ、使用中ワークツリー、ロック中ビルドは未使用容量に含めません。検証後の保持方針なので、使用中ビルドは一時的に容量を超えることがあります。削除できるのは認識済みのCargo `debug`・`release`出力のみです。バックアップ・検証記録はその外へ保存してください。アプリバンドル、`target/qa`、要約、ソースは保持し、整理したプロファイルは再ビルドで復元します。

`crates/host-fixture::test_support`は隔離Hostの起動・接続・停止を共有します。`bex-ui-fixture DIRECTORY CODEX PORT_FILE [STREAM_DELAY_MS]`は認証情報をメモリーに保持してHostとループバックのペアリングを動かし、Codexは標準入出力の境界を検証する子プロセスとして残します。`ios-e2e`は終了時にこのプロセスと新しいSimulatorを削除し、Xcodeのderived data・結果を`target/qa`へ残します。失敗・スキップ・テスト欠落はコマンドを失敗させます。`android-e2e`も新規エミュレーターとHostを管理し、許可拒否・再試行・実LAN通信を先に確認してから、出荷するJNIライブラリとフィクスチャHostでMarkdown・Store永続化・モデル復帰を検証します。ログと許可画面の画像を`target/qa`へ残し、欠落・失敗を拒否します。

Simulator・フィクスチャの実行では、実機、本番キーチェーン、カメラ、実際のCodexアカウントは検証できません。

Unixの隔離Hostフィクスチャは、直接の`cargo test`でも、接続開始前にファイル記述子のソフト上限を最低4096へ引き上げます。より高い上限とハード上限は保持し、ハード上限が不足していれば起動を明示的に失敗させます。80クライアントの受け入れテストにも対応します。

## GitHubの保守

[mainのルールセット](.github/main-ruleset.json)に、GitHubへ適用する保護設定を記録しています。mainへの直接pushを許可します。任意のPRでは、Bexのマージ済みワークツリー整理でコミットの祖先関係を保つため、マージコミットを使い、squash・rebaseによるマージは無効にしています。

[Dependabot](.github/dependabot.yml)は毎週月曜09:00（日本時間）に、Cargo（ワークスペース・agent-peer）、Gradle、XcodeのSwiftパッケージ、npm、2つのNix flake、GitHub Actionsを確認します。マイナー・パッチ更新はエコシステムごとにまとめ、メジャー更新は個別PRにします。Nixの入力はまとめます。通常のバージョン更新は公開から7日待ち、セキュリティ更新にはこの待機を適用しません。更新PRも通常の変更と同じネイティブ検証を実行し、保守担当者が確認してCI成功後に統合します。自動マージは無効です。統合済みのリモートブランチは自動削除します。

Lucideの更新は[可視化ランタイムのREADME](crates/host-daemon/src/visualize/README.md)のコマンドで生成し、同READMEのバージョン・取得元・チェックサムも更新してください。npmのmanifest・lockfileと一緒に、生成したバンドル・ライセンスをコミットします。CIは再生成して古いバンドルを検出します。リポジトリ内へ取り込んだRust・Androidソースと、固定したnetwatchの回復パッチは、上流の変更を手動で確認する必要があります。

Dependabotの脆弱性通知・セキュリティ更新、秘密情報のスキャン・push保護を有効にしています。Actionsは完全なコミットSHAに固定し、既定トークンは読み取り専用で、自分のPRを承認できません。脆弱性の非公開報告方法は[SECURITY.md](SECURITY.md)を参照してください。

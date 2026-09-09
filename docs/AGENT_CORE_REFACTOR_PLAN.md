# agent-core 移行計画

## ステータス

- 対象: Mac (GPUI)、iOS (SwiftUI)、Android (Compose)、host-daemon、mobile-client、relay-transport、Phoenix リレー(`apps/server`)、xtask
- 目的: エージェント操作と会話状態の実装を Rust の 1 crate に一本化し、ミュータブルな状態の所在を各プロセス 1 箇所に限定する。自作している汎用処理を標準機能とライブラリに置き換える。通信層は iroh に載せ替え、SSH と自前リレーを廃止する
- 成功条件: 各 PR が単独で行数マイナスまたはゼロであること。完了時点で Kotlin common、手書き FFI、desktop の `serde_json::Value` 操作、重複する JSON-RPC peer、SSH、Phoenix リレーが消えていること
- ADR: 本計画は ADR 0001 の「Kotlin owns mobile reconciliation, cache and presentation state」と「embedded SSH runs inside that route」「Phoenix」を覆す。着手時に superseding ADR を追加する。ADR 0002(同時制御の許可)は変更しない

## 進捗

- 完了: SSH 接続を `relay-transport::ssh` に、RPC とファイル転送を `agent-core::client` に移動。daemon と既存のネイティブ境界は新実装を参照し、ビルドが通っている
- 方針変更: `relay-transport::ssh` はこれ以上磨かない。iroh 採用時に削除する。`agent-core::client` の peer は汎用ストリーム上に置き、transport と切り離す
- 未着手: iroh の検証、モバイルの手書き FFI 置き換え

## 現状

### 層と行数

| 層 | 実装 | 行数 |
|---|---|---|
| Mac UI + 状態 + RPC | Rust / GPUI、全部 `serde_json::Value` | 約 6,000 |
| host-daemon | Rust | 約 6,900(うちファイル内テスト約 1,700) |
| iOS UI | SwiftUI | 約 6,000 |
| Android UI | Compose | 約 1,500 |
| モバイル調整層 | Kotlin Multiplatform commonMain | 約 8,200 |
| モバイル通信 | Rust `mobile-client`、手書き C ヘッダ + JNI | 約 2,200 |
| 通信基盤 | `relay-transport`、SSH ゲートウェイ、デバイス認証、ペアリング | 約 3,300 |
| リレーサーバ | Phoenix (Elixir)、Fly.io 東京 1 台 | 別ディレクトリ |
| xtask | Rust | 約 3,600 |

### 重複しているロジック

- リクエスト組み立てと応答検証: desktop の `app.rs`、Kotlin の `CommonCodexClient.kt`、Swift の `CodexModelSettings.swift` の 3 箇所。
- 会話状態の reducer: desktop の `conversation.rs`(Value ベース)と Kotlin の `ConversationTransitions.kt` + `AppStateReducer.kt`(型付き)の 2 実装。
- JSON-RPC のリクエスト ID 対応付け: `codex-app-server/peer.rs`、`mobile-client/rpc.rs`、`desktop/rpc.rs`、`xtask/fixture/server.rs` の 4 実装、合計約 1,900 行。desktop 版だけ std スレッド + `Mutex` + 同期 `UnixStream` で、他は tokio。
- イベント種別の判定: 6 ファイルに分散。
- 永続化: Kotlin の `MobileStateCodec` のみが独自形式で保持。

### 根本原因

1. Rust 側に型付きモデルがない。desktop は `["key"]` 参照が 503 箇所、`text(x, "key")` が 75 箇所あり、検証と変換を手書きしている。daemon の `codex_rpc/service.rs` も 103 箇所ある。
2. KMP は UI を共有していない。iOS は SwiftUI、Android は Compose なので、Kotlin common は純粋な「状態と調整」層である。状態を Rust に置いた時点で存在理由がなくなる。
3. 状態を変更できる場所が層ごとにある。desktop の `&mut Value` 操作、Kotlin の controller、Swift の `ObservableObject` がそれぞれ状態を持ち、互いの整合を都度取っている。
4. 通信を SSH と Phoenix で組んだ時点で、リレー、鍵交換、ペアリング、多重化が全て自前になった。SSH 接続あたり RPC subsystem が 1 つという制約も、常にリレー経由になる遅延も、回線切替時の再接続競合もここから来ている。

## 目指す形

### agent-core crate

Rust に `agent-core` を 1 つ作り、状態の正本にする。中身は次の 4 つ。

1. **型付きモデル。** serde 構造体に `#[serde(flatten)] extra: Map<String, Value>` を付けて未知フィールドを保持する。応答検証はデシリアライズに置き換え、手書きの Value 検査を消す。daemon もクライアントと同じプロトコルを話すので、同じモデルを使う。
2. **`agent-core::client`。** JSONL peer と操作、ファイル転送。peer は `AsyncRead + AsyncWrite` を受ける tokio ベースの 1 実装のみで、transport を知らない。Codex app-server への stdio 上流、ローカルの unix socket、iroh ストリームの全てで同じ peer を使う。操作は「メソッド名、params 型、result 型」の対応として定義し、desktop、mobile、daemon、xtask の fixture サーバが同じ実装を使う。
3. **会話ストア。** スナップショット、reducer、履歴マージ、保留リクエスト、ドラフト、永続化をここに置く。
4. **公開面は 3 種類だけ。** 不変スナップショットの取得、intent の送信、イベントの購読。

### 不変性の設計

「ミュータブルをゼロにする」ではなく「ミュータブルな場所を各プロセスに 1 箇所だけにする」。データとリソースを同じ型に入れないことが原則である。

agent-core は次の 3 つを分けて持つ。

1. **`Snapshot`。** 会話、スレッド一覧、モデル、保留リクエスト、ドラフトを持つ純粋なデータ型。`Clone + PartialEq + Serialize + Deserialize` を derive し、`&mut self` メソッドを持たない。永続化は `serde_json::to_vec(&snapshot)` で完結する。
2. **`reduce(&Snapshot, Event) -> (Snapshot, Vec<Effect>)`。** 純粋関数で I/O をしない。`Effect` は「このリクエストを送れ」「このスレッドを watch しろ」という値。テストはこの関数の入出力をフィクスチャで検証する。
3. **`Store`。** `RwLock<Arc<Snapshot>>` を 1 つだけ持ち、`apply(event)` で reduce を呼んで差し替え、返ってきた Effect を実行する。ソケット、ハンドル表、タイマー、購読者はここに閉じ込める。agent-core で `&mut self` を持つ型は `Store` だけとする。

性能上の注意: ストリーミングの delta は毎秒数十回来る。`Snapshot` は `Vec<Arc<Turn>>` のように turn と item を `Arc` で持ち、変わった経路だけ差し替える。足りなければ `Store` 内で delta を溜めて描画フレームごとに publish する。これは `Store` の内部実装であり、外から見た不変性は変わらない。

### 通信層: iroh

SSH、Phoenix リレー、自前のデバイス認証を iroh に置き換える。iroh 1.0(2026 年 6 月)はワイヤープロトコルと Swift / Kotlin バインディングの安定性を保証しており、公開リレーで直近 30 日に 2 億以上のエンドポイントが作られている。

| 今の実装 | 行数 | iroh での相当物 |
|---|---|---|
| `relay-transport` + Phoenix リレー(`apps/server`、Fly 運用) | 634 + Elixir 一式 | 内蔵リレー。当面は n0 の公開リレー、レート制限が問題になれば Fly の同じ場所に `iroh-relay` バイナリを置く。約 9 割の環境で直結に昇格する |
| SSH ゲートウェイ + モバイル側 transport + `russh` | 412 | QUIC 接続に ALPN を付ける。shell や PTY を拒否するコードは不要 |
| デバイス認証、Host identity、`auth.rs` | 504 | NodeId が Ed25519 公開鍵そのもの。Host はペア済み NodeId の allowlist を持つだけで、暗号化は QUIC の TLS 1.3 |
| ペアリング QR、トークン | 約 400 | iroh の ticket(NodeAddr + リレー URL)に使い捨てトークンを添える |

期待される効果は、遅延(常にリレー経由から直結へ)、接続確立(3 段の handshake から QUIC の 1 RTT へ)、Wi-Fi と LTE の切替(再接続からパスマイグレーションへ)、多重化(SSH 接続あたり RPC 1 本から、ストリーム単位で無制限へ)の 4 点である。JSONL の RPC は帯域を使わないので、効くのは遅延と再接続の安定性である。

**ストリームの使い方は 2 段階で進める。**

1. **ストリーム = セッション。** ビューごとに 1 本の双方向ストリームを開き、その上に JSONL を流す。既存の peer をそのまま使い、プロトコルは変えない。transport だけの差し替えなので PR が小さく、失敗しても戻せる。
2. **ストリーム = 呼び出し(後続)。** リクエストごとに双方向ストリームを開いて params を書き、result を読んで閉じる。リクエスト ID の対応付けが不要になり、通知は Host から client への片方向ストリーム 1 本、承認要求は Host が開く双方向ストリームになる。ローカル socket と Codex 上流には JSON-RPC が残るので、1 が安定してから判断する。

**識別子が変わるので全端末の再ペアリングが必ず発生する。** 利用者が少ないうちに 1 回で済ませる。

**検証(2 日、peer 統合と並行)。** Mac daemon と iPhone を iroh でつなぐ最小の実験で次の 3 点を確認し、採否を決める。

1. 自宅 Wi-Fi と LTE のそれぞれで直結に昇格するか、リレー止まりか。
2. 接続確立にかかる時間。今の SSH handshake 込みの数秒との比較。
3. Wi-Fi から LTE に切り替えたときに接続が維持されるか。

通らなければ iroh を見送り、「フォールバック: daemon のリモート多重化」を実施する。

### UI 層

UI 層は `Arc<Snapshot>` を受け取って描くだけにする。状態変更は必ず `store.dispatch(intent)` 経由とし、UI 層に setter や controller を作らない。

- **SwiftUI。** `@Observable` なオブジェクトを 1 つだけ置き、`snapshot` を publish する。`CodexModelSettings` のような機能ごとの `ObservableObject` は消す。`[String: Any]` で JSON を受けている 23 箇所は UniFFI の型に置き換わる。
- **Compose。** `State<Snapshot>` を 1 つ持ち、controller 類は作らない。
- **GPUI。** Entity は仕組み上ミュータブルなので、View には `Arc<Snapshot>` とスクロール位置や入力中テキストなど UI 固有の状態だけを持たせる。`conversation.rs` の `append_text(&mut Value)` や `upsert(&mut turn)` は reduce に吸収する。

### FFI

UniFFI で生成する。C ヘッダ、JNI の `Java_...` 関数、Kotlin の `expect/actual`、JSON 文字列のコマンドは全て削除する。Rust の型定義から Swift と Kotlin のバインディングを生成し、async とコールバックもそこに載せる。iroh のモバイルバインディング(`iroh-ffi`)も UniFFI 製なので、ツールチェーンは 1 つで済む。

### Kotlin common の扱い

削除する。Swift は UniFFI 経由で直接 Rust を呼び、Android は UniFFI の Kotlin バインディングを使う。Kotlin/Native の cinterop、XCFramework ビルド、`MobileStateCodec` の独自永続化形式が不要になる。ネイティブ側に残るのは描画とプラットフォーム API(カメラ、通知、ファイル、Keychain)だけである。

### desktop

「ビューごとに接続」のままにする。`ConversationView` の切り出しは行うが、複数ビューで接続を共有する配線は作らない。iroh ではビューごとにストリームを開くだけで済む。

減るのは状態と RPC のロジックで、`app.rs` の `new`、`event`、`reduce`、`submit`、`load_detail`、`send_turn`、`load_older`、`refresh_models`、`supported_model_settings` と `conversation.rs`、`rpc.rs` が対象になる。合計約 2,000 行が消え、そのうち 700 行前後が `agent-core` に 1 回だけ現れる。`view.rs` の 2,900 行は GPUI のビルダー記法の長さなので、型付きに変えても行数は変わらない。減らすなら `item` や `chat` の中の表示種別分岐を `Snapshot` 側で投影済みにして、描画関数を「投影済みの列を並べるだけ」にする。それでも 2 割程度である。

### host-daemon

減るのは 1 割程度で、そこを狙って設計を歪めない。worktree、ファイル操作、レビュー、アカウント、音声認識、リモート Host 管理は他に重複のない機能実装なので移動先がない。減らせるのは `codex_rpc/service.rs` と `desktop_projects/*` の Value 操作(合計約 170 箇所)を型付きモデルに置き換える分で 300〜500 行、それに iroh 採用で消える `ssh_gateway.rs` と `device_auth.rs` の約 600 行である。

**フォールバック: daemon のリモート多重化(iroh 不採用時のみ)。** リモート Host の `ssh_gateway.rs` は SSH 接続 1 本につき RPC subsystem を 1 つしか受け付けない(`rpc_session_id` が単一の `Option`)。リモート Host 側の制限を緩める方式は、全リモート Host の更新が必要で SSH の受け口も広がるため採らない。クライアント側 daemon の `serve_local` の `Remote` 分岐で、profile ごとに relay セッションを 1 本持ち、`routing.rs` の `SessionRouter` で下流を多重化する。`SessionRouter` は Codex app-server に結合しておらず再利用できる。注意点は、ローカル接続ごとの watchKey 所有権の追跡、最後のローカル接続が閉じたときの relay 切断と保留リクエストの失敗、SSH の 8 チャネル上限が転送の合計にかかること、の 3 つである。

### 自作している汎用処理の置き換え

Rust 側は `similar`、`pulldown-cmark`、`russh`、`ring`、`tungstenite`、`reqwest`、`notify`、`qrcode` を使っており、全体としては筋が良い。置き換えるのは次の箇所。

| 場所 | 自作している内容 | 代替 | 削減見込み |
|---|---|---|---|
| JSON-RPC peer 4 実装 | リクエスト ID 対応付けと再接続 | `agent-core::client` の peer | 約 1,400 行 |
| `host-daemon/command_line.rs` | `set_once`、`next_non_empty_value` を持つ引数パーサ | `clap` の derive | 約 200 行 |
| `host-protocol/rpc.rs` の `rewrite_top_level_id` | JSON の `id` のバイト位置をポインタ演算で求めて置換 | `Box<RawValue>` + `#[serde(flatten)]` で読んで書き直す | 約 60 行 |
| `xtask/ios.rs`、`macos.rs`、`command.rs` | `xcrun simctl` と `xcodebuild` の呼び出しを Rust で包んだもの | シェルスクリプトか `just` | 約 700 行 |
| `desktop/app.rs` の `basename` | パス末尾の取り出し | `Path::file_name` | 数行 |
| `MobileController.kt` のメッセージ ID | `Random.nextLong()` を 2 つ連結 | `kotlin.uuid.Uuid.random()`(Kotlin common 削除までの暫定) | 数行 |
| `GatewayResult` と独自 `fold` | Result 型の再発明 | `kotlin.Result`(同上) | 数十行 |
| `CodexJsonFields.kt` 経由の JSON 走査 74 箇所 | 手動の JSON 読み取り | `@Serializable` の型付き受信(同上) | 数百行 |
| `ConversationScrollPosition.swift` 197 行 | `UIScrollView` 直叩きの末尾追従 | `scrollPosition(id:)` と `defaultScrollAnchor(.bottom)` | 要実機確認。UIKit のレース回避と書かれているため、置き換え前に再現手順を残す |

自作のままでよいもの: `dictation.rs` の WAV ヘッダ 10 行、`codex_rpc/routing.rs` の proxy ID 書き換え(ローカル socket と Codex 上流で引き続き使う)、xtask の fixture サーバ群(UI テスト用の偽 Host)。Phoenix チャネルのフレーム解析は iroh 採用で不要になる。

## 進め方

各 PR が単独で行数マイナスまたはゼロになる順で切る。1 PR が 1 週間以内にマージできない大きさなら切り方が大きすぎると判断する。

### 1. peer の統合(進行中)

- `agent-core::client` の peer を `AsyncRead + AsyncWrite` 上の汎用実装にし、`codex-app-server/peer.rs`、`mobile-client/rpc.rs`、`desktop/rpc.rs`、`xtask/fixture/server.rs` を置き換えて削除する。テストは `tokio::io::duplex` で transport なしに書く。
- ファイル転送も汎用ストリーム上に置く。
- `command_line.rs` を `clap` に、`rewrite_top_level_id` を `RawValue` に置き換える。
- `relay-transport::ssh` は触らない。iroh を混ぜない。
- 受け入れ条件: リクエスト ID を対応付ける実装が workspace に 1 つ。desktop から std スレッドの RPC が消える。peer のテストが transport に依存しない。

### 1'. iroh 検証(1 と並行、2 日)

- 上記「通信層: iroh」の 3 点を確認し、採否を決める。
- 採用なら 2 へ。不採用なら「フォールバック: daemon のリモート多重化」を 2 の代わりに実施する。

### 2. iroh 置き換え

- PR B: iroh transport。Mac daemon の受け口(ALPN)、モバイルの接続、NodeId の allowlist、QR ペイロードの変更。ビューごとに双方向ストリーム 1 本、その上に JSONL(ストリーム = セッション)。両端を同時に切り替えるので再ペアリングが発生する。
- PR C: SSH、Phoenix(`apps/server`)、`relay-transport`、`device_auth.rs`、`ssh_gateway.rs`、`mobile-client/transport.rs` の削除と superseding ADR。
- B と C は同じリリースに入れ、PR は分けてレビューする。
- 受け入れ条件: リポジトリから `russh` と `tokio-tungstenite` が消える。同一 Host に 2 ビューから接続してもリレーセッションは 1 本。Wi-Fi と LTE の切替で会話が切れない。

### 3. 型付きモデルと操作

- `agent-core` に型付きモデルと操作を入れる。
- desktop の `app.rs` からリクエスト組み立てと応答検証を消し、`agent-core` の関数を呼ぶ。daemon の `service.rs` と `desktop_projects/*` も同じモデルに切り替える。
- 受け入れ条件: desktop と daemon の `["key"]` 参照が半減以下。操作ごとのフィクスチャテストが Rust にある。

### 4. 会話ストア

- `Snapshot`、`reduce`、`Store` を `agent-core` に入れる。
- desktop の `conversation.rs` を削除し、`ConversationView` は `Arc<Snapshot>` を描くだけにする。
- 受け入れ条件: `agent-core` で `&mut self` を持つのは `Store` のみ。Rust 内に会話 reducer が 1 つだけ。

### 5. UniFFI 導入とモバイル接続

- UniFFI を導入し、`Store` をバインディング経由で公開する。手書きの C ヘッダ、JNI、cinterop を削除する。
- Swift と Android を `Store` に接続し、Kotlin common を次の順に削除する: `CommonCodexClient` → `ConversationTransitions` → `AppStateReducer` → `MobileStateCodec` → `HostSessionCoordinator` → 残り。
- 永続化は `Snapshot` の serde 出力に切り替える。既存形式からの移行は 1 回だけ読み込むコードを用意し、次のリリースで削除する。
- 受け入れ条件: `apps/mobile/src/commonMain` が空。手書きの `extern "C"` と `Java_...` 関数がゼロ。Swift の `[String: Any]` がゼロ。

### 6. xtask とツールチェーン

- `xtask/ios.rs`、`macos.rs`、`command.rs` をシェルスクリプトか `just` に置き換える。fixture サーバ群は残す。
- iOS/Android の最低バージョン、AGP、Gradle の更新は上記と混ぜず、単独の PR にする。

### 7. ストリーム = 呼び出し(任意)

- 2 が安定した後、リクエストごとのストリームに移行して peer の ID 対応付けをクライアント向けから外す。ローカル socket と Codex 上流の JSON-RPC は残る。効果と 2 種類のプロトコルが並存するコストを見て判断する。

## レビュー基準

全ての PR に次を適用する。

- データ型に `Job`、ソケット、コールバック、ロックを入れない。
- `agent-core` で `&mut self` を持つのは `Store` だけ。
- UI 層に setter や controller を作らない。状態変更は `store.dispatch(intent)` 経由。
- 同じ RPC メソッド名が 2 つ以上のクライアント実装に現れない。
- peer と操作は transport を知らない。iroh、unix socket、stdio のどれに載せても同じコード。
- リクエスト ID の対応付け、引数解析、UUID 生成、Result 型、暗号化 transport、リレーを新たに書かない。標準機能か既存 crate を使う。
- 行数が増える PR は、増える理由を本文で説明する。

## 期待される効果

| 削減対象 | 概算 |
|---|---|
| Kotlin common | 約 8,000 行 |
| 通信基盤(SSH、リレー、デバイス認証、ペアリング) | 約 2,500 行 + Phoenix リレー一式 |
| desktop の状態と RPC ロジック | 約 2,000 行(うち 700 行は agent-core に 1 回だけ移る) |
| JSON-RPC peer の重複 | 約 1,400 行 |
| xtask の xcrun ラッパー | 約 700 行 |
| daemon の Value 操作 | 300〜500 行 |
| 手書き FFI(C ヘッダ、JNI、cinterop) | 数百行 |
| Swift の状態ロジックと untyped JSON | 数百行 |
| その他(clap、RawValue、basename 等) | 約 300 行 |

減らないもの: `view.rs` の描画コード、daemon の機能実装、xtask の fixture サーバ。運用面では Fly 上の Phoenix リレーと Kotlin/Native、cinterop、XCFramework の各工程がなくなる。

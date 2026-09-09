# agent-core 移行計画

## ステータス

- 前提: 未リリース。後方互換は考慮しない。「常に動く状態を保つ」より「理想の構造に到達する」を優先する
- 対象プラットフォーム: デスクトップは macOS、Linux、Windows(GPUI)。モバイルは iOS(SwiftUI)と Android(Compose)。UI は 3 本で確定
- 目的: エージェント操作と会話状態を Rust の 1 crate(`agent-core`)に一本化し、ミュータブルな状態の所在を各プロセス 1 箇所に限定する。通信は iroh に統一し、SSH、Phoenix リレー、unix socket を廃止する。自作している汎用処理を標準機能とライブラリに置き換える
- 唯一の規律: `agent-core` のテストとヘッドレス CLI クライアントが常に通ること。UI が一時的に壊れることは許容する
- ADR: ADR 0001 の「Kotlin owns mobile reconciliation」「embedded SSH」「Phoenix」「owner-only Unix socket」を覆す superseding ADR を追加する。「daemon は別プロセスで UI を閉じても Codex を止めない」「Codex のスキーマ適応(ADR 0004)」「同時制御の許可(ADR 0002)」は維持する

## 進捗

- 完了: SSH 接続を `relay-transport::ssh` に、RPC とファイル転送を `agent-core::client` に移動。ビルドは通っている
- 方針変更: `relay-transport::ssh` は以後触らず、iroh 採用時に削除する。`agent-core::client` の peer は汎用ストリーム上に置く
- 未着手: iroh 検証、`Snapshot` / `Store`、UniFFI、Linux / Windows ビルド

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
- JSON-RPC のリクエスト ID 対応付け: `codex-app-server/peer.rs`、`mobile-client/rpc.rs`、`desktop/rpc.rs`、`xtask/fixture/server.rs` の 4 実装、合計約 1,900 行。
- クライアント経路: Mac UI は unix socket、スマホは relay + SSH で、daemon の `serve_local` が Local / Manager / Remote の 3 分岐を持つ。
- イベント種別の判定: 6 ファイルに分散。
- 永続化: Kotlin の `MobileStateCodec` のみが独自形式で保持。

### 根本原因

1. Rust 側に型付きモデルがない。desktop は `["key"]` 参照が 503 箇所、daemon の `codex_rpc/service.rs` も 103 箇所あり、検証と変換を手書きしている。
2. KMP は UI を共有していない。Kotlin common は純粋な「状態と調整」層で、状態を Rust に置いた時点で存在理由がなくなる。
3. 状態を変更できる場所が層ごとにある。desktop の `&mut Value`、Kotlin の controller、Swift の `ObservableObject` がそれぞれ状態を持つ。
4. 通信を SSH と Phoenix で組んだ時点で、リレー、鍵交換、ペアリング、多重化が全て自前になった。ローカルとリモートで経路が違うため、daemon の受け口も 2 系統ある。
5. macOS 固有コード(`security-framework`、`platform.rs` の osascript、`Dictation.swift`)が散在しており、Linux と Windows に持っていけない。

## 目指す形

### 全体像

```
Codex app-server ──stdio JSON-RPC──▶ host-daemon ◀──iroh (JSONL JSON-RPC)──┬── desktop (GPUI, mac/linux/win)
                                        │                                  ├── iOS (SwiftUI)
                                        └── SessionRouter                  └── Android (Compose)
                                                                            全クライアントが agent-core::Store を持つ
```

- プロトコルは全経路で JSON-RPC の JSONL 1 種類。Codex 上流、daemon、全クライアントで同じ peer を使う。
- クライアント経路は iroh 1 種類。同じマシン上の desktop もループバックで iroh 接続し、daemon 起動時に共有する鍵で自動ペアリングする。unix socket と `LocalHeader` の 3 分岐は消える。管理 API(招待の発行、リモート Host の登録)はローカル端末の NodeId にだけ許可する。
- 状態の正本はクライアント側の `agent-core::Store`。UI は `Arc<Snapshot>` を描き、`store.dispatch(intent)` を呼ぶだけ。

### agent-core crate

1. **型付きモデル。** serde 構造体に `#[serde(flatten)] extra: Map<String, Value>` を付けて未知フィールドを保持する。応答検証はデシリアライズに置き換える。daemon も同じモデルを使う。
2. **`agent-core::client`。** `AsyncRead + AsyncWrite` 上の JSONL peer、操作、ファイル転送。transport を知らない。操作は「メソッド名、params 型、result 型」の対応として定義する。
3. **`agent-core::transport`。** iroh エンドポイントの生成、ペアリング ticket、NodeId allowlist、ストリームの開閉。ここだけが iroh を知る。
4. **`Snapshot`。** 会話、スレッド一覧、モデル、保留リクエスト、ドラフトを持つ純粋なデータ型。`Clone + PartialEq + Serialize + Deserialize` を derive し、`&mut self` メソッドを持たない。永続化は `serde_json::to_vec(&snapshot)` で完結し、バージョン番号も移行コードも持たない。
5. **`reduce(&Snapshot, Event) -> (Snapshot, Vec<Effect>)`。** 純粋関数で I/O をしない。テストはフィクスチャ corpus で行う。
6. **`Store`。** `RwLock<Arc<Snapshot>>` を 1 つだけ持ち、`apply(event)` で reduce を呼んで差し替え、Effect を実行する。ソケット、タイマー、購読者はここに閉じ込める。agent-core で `&mut self` を持つ型は `Store` だけ。
7. **ヘッドレス CLI クライアント(`agent-cli`)。** 接続、スレッド一覧、送信、承認応答ができるだけの最小クライアント。UI が壊れている期間の結合テストと、Linux / Windows での daemon 検証に使う。

性能上の注意: ストリーミングの delta は毎秒数十回来る。`Snapshot` は turn と item を `Arc` で持ち、変わった経路だけ差し替える。足りなければ `Store` 内で delta を溜めて描画フレームごとに publish する。

### 通信層: iroh

iroh 1.0(2026 年 6 月)はワイヤープロトコルと Swift / Kotlin バインディングの安定性を保証している。

| 今の実装 | 行数 | iroh での相当物 |
|---|---|---|
| `relay-transport` + Phoenix リレー | 634 + Elixir 一式 | 内蔵リレー |
| SSH ゲートウェイ + モバイル transport + `russh` | 412 | QUIC 接続に ALPN |
| デバイス認証、Host identity、`auth.rs` | 504 | NodeId の allowlist。暗号化は QUIC の TLS 1.3 |
| ペアリング QR、トークン、プロトコルバージョン | 約 400 | iroh ticket + 使い捨てトークン。バージョン番号は持たない |
| unix socket、`LocalHeader`、desktop `rpc.rs` | 約 600 | ループバックの iroh 接続 |

ストリームの使い方は「セッション = 双方向ストリーム 1 本、その上に JSONL」で確定する。リクエストごとにストリームを開く方式は採らない。Codex 上流が JSON-RPC なので daemon は透過的な中継で済み、プロトコルが全経路で 1 つになる。

**リレー。** 公開リレーは US 東西、EU、シンガポールの 4 か所で日本にはない。開発中は公開リレー、配布時は Fly 東京に `iroh-relay`(月 3〜4 ドル)を置き、クライアントのリレー一覧で先頭に指定する。約 9 割の環境で直結に昇格するので、リレー経由の転送量は小さい。

**検証(2 日、着手前)。** Mac daemon と iPhone を iroh でつなぎ、次の 3 点を確認する。通らなければ iroh を見送り、末尾の「フォールバック」を採る。

1. 自宅 Wi-Fi と LTE のそれぞれで直結に昇格するか。
2. 接続確立にかかる時間。今の SSH handshake 込みの数秒との比較。
3. Wi-Fi から LTE への切替で接続が維持されるか。

### UI 層

UI 層は `Arc<Snapshot>` を受け取って描くだけにする。UI 層に setter や controller を作らない。

- **GPUI(macOS、Linux、Windows)。** Entity は仕組み上ミュータブルなので、View には `Arc<Snapshot>` とスクロール位置や入力中テキストなど UI 固有の状態だけを持たせる。`app.rs`、`conversation.rs`、`rpc.rs` は捨て、`view.rs` は描画に専念させる。ビューごとに iroh ストリームを 1 本開く。接続の共有はしない。
- **SwiftUI(iOS)。** `@Observable` なオブジェクトを 1 つだけ置き、`snapshot` を publish する。`[String: Any]` で JSON を受けている 23 箇所は UniFFI の型に置き換わる。
- **Compose(Android)。** `State<Snapshot>` を 1 つ持つ。

### プラットフォーム抽象

desktop は `platform/{macos,linux,windows}.rs` 以外に `cfg(target_os)` を書かない。

| 機能 | 今 | 置き換え |
|---|---|---|
| 秘密情報の保存 | `security-framework`(macOS Keychain) | `keyring` crate(Keychain、secret-service、Credential Manager) |
| ファイル選択 | osascript | `rfd` |
| 音声入力の録音 | `Dictation.swift` | OS ごとの薄い adapter。認識は daemon 側のまま |
| daemon の状態ディレクトリ | macOS 前提のパス | `directories` crate |

### FFI

UniFFI で生成する。C ヘッダ、JNI、Kotlin の `expect/actual`、JSON 文字列のコマンドは全て削除する。`iroh-ffi` も UniFFI 製なのでツールチェーンは 1 つで済む。

### Kotlin common の扱い

削除する。Swift は UniFFI 経由で直接 Rust を呼び、Android は UniFFI の Kotlin バインディングを使う。Kotlin/Native の cinterop、XCFramework ビルド、`MobileStateCodec` が不要になる。ネイティブ側に残るのは描画とプラットフォーム API(カメラ、通知、ファイル、Keychain)だけ。

### host-daemon

減るのは iroh 採用で消える `ssh_gateway.rs`、`device_auth.rs`、`serve_local` の分岐(約 900 行)と、`service.rs` と `desktop_projects/*` の Value 操作を型付きモデルに置き換える分(300〜500 行)。worktree、ファイル操作、レビュー、アカウント、音声認識は他に重複のない機能実装なので残る。`SessionRouter` は iroh 接続ごとのセッションにそのまま使う。Linux と Windows で動かすため、上記のプラットフォーム抽象を daemon にも適用する。

### 自作している汎用処理の置き換え

| 場所 | 自作している内容 | 代替 | 削減見込み |
|---|---|---|---|
| JSON-RPC peer 4 実装 | リクエスト ID 対応付けと再接続 | `agent-core::client` の peer | 約 1,400 行 |
| `host-daemon/command_line.rs` | 手書き引数パーサ | `clap` の derive | 約 200 行 |
| `host-protocol/rpc.rs` の `rewrite_top_level_id` | JSON のバイト位置をポインタ演算で置換 | `Box<RawValue>` + `#[serde(flatten)]` | 約 60 行 |
| `xtask/ios.rs`、`macos.rs`、`command.rs` | `xcrun` と `xcodebuild` の Rust ラッパー | シェルスクリプトか `just` | 約 700 行 |
| `desktop/app.rs` の `basename` | パス末尾の取り出し | `Path::file_name` | 数行 |
| `ConversationScrollPosition.swift` 197 行 | `UIScrollView` 直叩きの末尾追従 | `scrollPosition(id:)` と `defaultScrollAnchor(.bottom)` | 要実機確認 |

Kotlin common 側の自作(`GatewayResult`、`CodexJsonFields.kt`、`Random` によるメッセージ ID)は Kotlin common ごと消えるので個別対応しない。自作のままでよいもの: `dictation.rs` の WAV ヘッダ、`codex_rpc/routing.rs` の proxy ID 書き換え、xtask の fixture サーバ群。

## 進め方

「常に動く状態を保つ」順序は採らない。先に中核を完成させ、全 UI を一度に載せ替える。各段階の完了条件は `agent-core` のテストと `agent-cli` の結合テストが通ることで、旧 UI が動くことは条件にしない。

### 0. iroh 検証(2 日)

上記 3 点を確認して採否を決める。

### 1. agent-core の完成

- 型付きモデル、`client`(peer、操作、ファイル転送)、`transport`(iroh)、`Snapshot`、`reduce`、`Store`、永続化。
- `agent-cli` を作り、fixture サーバと `tokio::io::duplex` でテストする。
- 完了条件: フィクスチャ corpus が全て通る。`agent-cli` が fixture サーバに対して接続、一覧、送信、承認応答をこなす。`&mut self` を持つ型が `Store` だけ。

### 2. daemon の載せ替え

- iroh エンドポイント、NodeId の allowlist、ローカル端末の自動ペアリング、`SessionRouter` の接続。型付きモデルへの切り替え。`clap`、`keyring`、`directories` の導入。
- 削除: unix socket、`LocalHeader`、`ssh_gateway.rs`、`device_auth.rs`、`relay-transport`、`mobile-client`、`codex-app-server/peer.rs`、Phoenix(`apps/server`)。
- 完了条件: `agent-cli` が macOS、Linux、Windows の daemon に対して動く。iPhone 実機から `agent-cli` 相当の操作が通る(この時点ではモバイル UI は未接続で構わない)。リポジトリから `russh` と `tokio-tungstenite` が消える。

### 3. desktop の載せ替え

- `app.rs`、`conversation.rs`、`rpc.rs` を捨て、`Store` の上に書き直す。`view.rs` は `Arc<Snapshot>` を描く形に直す。`platform/` にプラットフォーム抽象を入れ、macOS、Linux、Windows でビルドとスモークテストを通す。
- 完了条件: 3 OS で起動し、ローカル daemon に自動ペアリングして会話できる。`["key"]` 参照が `view.rs` の描画分岐以外にない。

### 4. mobile の載せ替え

- UniFFI バインディングを生成し、Kotlin common を丸ごと削除。SwiftUI と Compose は `Store` を購読するだけにする。永続化は `Snapshot` の serde 出力。
- 完了条件: `apps/mobile/src/commonMain` が空。手書きの `extern "C"` と `Java_...` がゼロ。Swift の `[String: Any]` がゼロ。iOS と Android の実機で会話、承認、履歴、添付が通る。

### 5. xtask とツールチェーン

- `xtask/ios.rs`、`macos.rs`、`command.rs` をシェルスクリプトか `just` に置き換える。fixture サーバ群は残す。Linux と Windows のビルドを CI に加える。
- iOS/Android の最低バージョン、AGP、Gradle の更新は単独の PR にする。

## レビュー基準

- データ型に `Job`、ソケット、コールバック、ロックを入れない。
- `agent-core` で `&mut self` を持つのは `Store` だけ。
- UI 層に setter や controller を作らない。状態変更は `store.dispatch(intent)` 経由。
- 同じ RPC メソッド名が 2 つ以上のクライアント実装に現れない。
- peer と操作は transport を知らない。iroh を知るのは `agent-core::transport` だけ。
- `cfg(target_os)` は `platform/` の下にしか書かない。
- リクエスト ID の対応付け、引数解析、UUID 生成、Result 型、暗号化 transport、リレー、秘密情報の保存を新たに書かない。標準機能か既存 crate を使う。
- プロトコルのバージョン番号と移行コードを書かない。リリース時に付ける。

## 期待される効果

| 削減対象 | 概算 |
|---|---|
| Kotlin common | 約 8,000 行 |
| 通信基盤(SSH、リレー、デバイス認証、ペアリング、unix socket) | 約 3,000 行 + Phoenix リレー一式 |
| desktop の状態と RPC ロジック | 約 2,000 行(うち 700 行は agent-core に 1 回だけ移る) |
| JSON-RPC peer の重複 | 約 1,400 行 |
| xtask の xcrun ラッパー | 約 700 行 |
| daemon の Value 操作 | 300〜500 行 |
| 手書き FFI(C ヘッダ、JNI、cinterop) | 数百行 |
| Swift の状態ロジックと untyped JSON | 数百行 |

減らないもの: `view.rs` の描画コード、daemon の機能実装、xtask の fixture サーバ。増えるもの: `agent-cli`(数百行)、`platform/` の Linux と Windows 実装。運用面では Phoenix リレー、Kotlin/Native、cinterop、XCFramework の各工程がなくなる。

## フォールバック(iroh 不採用時のみ)

リモート Host の `ssh_gateway.rs` は SSH 接続 1 本につき RPC subsystem を 1 つしか受け付けない。クライアント側 daemon の `serve_local` の `Remote` 分岐で profile ごとに relay セッションを 1 本持ち、`routing.rs` の `SessionRouter` で下流を多重化する。注意点は、ローカル接続ごとの watchKey 所有権の追跡、最後のローカル接続が閉じたときの relay 切断、SSH の 8 チャネル上限が転送の合計にかかること、の 3 つ。Windows 対応のため unix socket は named pipe に置き換える必要が別途生じる。

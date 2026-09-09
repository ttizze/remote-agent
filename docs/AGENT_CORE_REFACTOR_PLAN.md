# agent-core 移行計画

## ステータス

- 対象: Mac (GPUI)、iOS (SwiftUI)、Android (Compose)、host-daemon、mobile-client
- 目的: エージェント操作と会話状態の実装を Rust の 1 crate に一本化し、ミュータブルな状態の所在を各プロセス 1 箇所に限定する
- 成功条件: 各 PR が単独で行数マイナスまたはゼロであること。完了時点で Kotlin common、手書き FFI、desktop の `serde_json::Value` 操作が消えていること
- ADR: 本計画は ADR 0001 の「Kotlin owns mobile reconciliation, cache and presentation state」を覆す。着手時に superseding ADR を追加する

## 現状

### 層と行数

| 層 | 実装 | 行数 |
|---|---|---|
| Mac UI + 状態 + RPC | Rust / GPUI、全部 `serde_json::Value` | 約 6,000 |
| host-daemon | Rust | 約 6,900 |
| iOS UI | SwiftUI | 約 6,000 |
| Android UI | Compose | 約 1,500 |
| モバイル調整層 | Kotlin Multiplatform commonMain | 約 8,200 |
| モバイル通信 | Rust `mobile-client`、手書き C ヘッダ + JNI | 約 2,200 |

### 重複しているロジック

- リクエスト組み立てと応答検証: desktop の `app.rs`、Kotlin の `CommonCodexClient.kt`、Swift の `CodexModelSettings.swift` の 3 箇所。
- 会話状態の reducer: desktop の `conversation.rs`(Value ベース)と Kotlin の `ConversationTransitions.kt` + `AppStateReducer.kt`(型付き)の 2 実装。
- イベント種別の判定: 6 ファイルに分散。
- 永続化: Kotlin の `MobileStateCodec` のみが独自形式で保持。

### 根本原因

1. Rust 側に型付きモデルがない。desktop は `["key"]` 参照が 503 箇所、`text(x, "key")` が 75 箇所あり、検証と変換を手書きしている。
2. KMP は UI を共有していない。iOS は SwiftUI、Android は Compose なので、Kotlin common は純粋な「状態と調整」層である。状態を Rust に置いた時点で存在理由がなくなる。
3. 状態を変更できる場所が層ごとにある。desktop の `&mut Value` 操作、Kotlin の controller、Swift の `ObservableObject` がそれぞれ状態を持ち、互いの整合を都度取っている。

## 目指す形

### agent-core crate

Rust に `agent-core` を 1 つ作り、状態の正本にする。中身は次の 4 つ。

1. **型付きモデル。** serde 構造体に `#[serde(flatten)] extra: Map<String, Value>` を付けて未知フィールドを保持する。応答検証はデシリアライズに置き換え、手書きの Value 検査を消す。
2. **RPC peer と操作。** JSONL peer は 1 実装のみ。操作は「メソッド名、params 型、result 型」の対応として定義し、desktop と mobile が同じ関数を呼ぶ。
3. **会話ストア。** スナップショット、reducer、履歴マージ、保留リクエスト、ドラフト、永続化をここに置く。
4. **公開面は 3 種類だけ。** 不変スナップショットの取得、intent の送信、イベントの購読。

### 不変性の設計

「ミュータブルをゼロにする」ではなく「ミュータブルな場所を各プロセスに 1 箇所だけにする」。データとリソースを同じ型に入れないことが原則である。

agent-core は次の 3 つを分けて持つ。

1. **`Snapshot`。** 会話、スレッド一覧、モデル、保留リクエスト、ドラフトを持つ純粋なデータ型。`Clone + PartialEq + Serialize + Deserialize` を derive し、`&mut self` メソッドを持たない。永続化は `serde_json::to_vec(&snapshot)` で完結する。
2. **`reduce(&Snapshot, Event) -> (Snapshot, Vec<Effect>)`。** 純粋関数で I/O をしない。`Effect` は「このリクエストを送れ」「このスレッドを watch しろ」という値。テストはこの関数の入出力をフィクスチャで検証する。
3. **`Store`。** `RwLock<Arc<Snapshot>>` を 1 つだけ持ち、`apply(event)` で reduce を呼んで差し替え、返ってきた Effect を実行する。ソケット、ハンドル表、タイマー、購読者はここに閉じ込める。agent-core で `&mut self` を持つ型は `Store` だけとする。

性能上の注意: ストリーミングの delta は毎秒数十回来る。`Snapshot` は `Vec<Arc<Turn>>` のように turn と item を `Arc` で持ち、変わった経路だけ差し替える。足りなければ `Store` 内で delta を溜めて描画フレームごとに publish する。これは `Store` の内部実装であり、外から見た不変性は変わらない。

### UI 層

UI 層は `Arc<Snapshot>` を受け取って描くだけにする。状態変更は必ず `store.dispatch(intent)` 経由とし、UI 層に setter や controller を作らない。

- **SwiftUI。** `@Observable` なオブジェクトを 1 つだけ置き、`snapshot` を publish する。`CodexModelSettings` のような機能ごとの `ObservableObject` は消す。
- **Compose。** `State<Snapshot>` を 1 つ持ち、controller 類は作らない。
- **GPUI。** Entity は仕組み上ミュータブルなので、View には `Arc<Snapshot>` とスクロール位置や入力中テキストなど UI 固有の状態だけを持たせる。`conversation.rs` の `append_text(&mut Value)` や `upsert(&mut turn)` は reduce に吸収する。

### FFI

UniFFI で生成する。C ヘッダ、JNI の `Java_...` 関数、Kotlin の `expect/actual`、JSON 文字列のコマンドは全て削除する。Rust の型定義から Swift と Kotlin のバインディングを生成し、async とコールバックもそこに載せる。

### Kotlin common の扱い

削除する。Swift は UniFFI 経由で直接 Rust を呼び、Android は UniFFI の Kotlin バインディングを使う。Kotlin/Native の cinterop、XCFramework ビルド、`MobileStateCodec` の独自永続化形式が不要になる。ネイティブ側に残るのは描画とプラットフォーム API(カメラ、通知、ファイル、Keychain)だけである。

### desktop

「ビューごとに `Rpc::connect`」のままにする。`ConversationView` の切り出しは行うが、複数ビューで接続を共有する配線は作らない。

### host-daemon

リモート Host への接続多重化は daemon の `serve_local` の `Remote` 分岐で行う。ローカル分岐が既に「共有サービス + 接続ごとのセッション」になっているので、profile ごとにリレーセッションを 1 本持ち、ローカル接続を同じ仕組みで多重化する。これにより Mac の各ビュー、ターミナル、iOS、Android が変更なしで恩恵を受ける。

## 進め方

各 PR が単独で行数マイナスまたはゼロになる順で切る。1 PR が 1 週間以内にマージできない大きさなら切り方が大きすぎると判断する。

### 1. 型付きモデルと操作

- `agent-core` に型付きモデルと操作を入れる。
- desktop の `app.rs` からリクエスト組み立てと応答検証を消し、`agent-core` の関数を呼ぶ。
- 受け入れ条件: desktop の `["key"]` 参照が半減以下。操作ごとのフィクスチャテストが Rust にある。

### 2. 会話ストア

- `Snapshot`、`reduce`、`Store` を `agent-core` に入れる。
- desktop の `conversation.rs` を削除し、`ConversationView` は `Arc<Snapshot>` を描くだけにする。
- 受け入れ条件: `agent-core` で `&mut self` を持つのは `Store` のみ。Rust 内に会話 reducer が 1 つだけ。

### 3. UniFFI 導入とモバイル接続

- UniFFI を導入し、`Store` をバインディング経由で公開する。
- Swift と Android を `Store` に接続し、Kotlin common を次の順に削除する: `CommonCodexClient` → `ConversationTransitions` → `AppStateReducer` → `MobileStateCodec` → `HostSessionCoordinator` → 残り。
- 永続化は `Snapshot` の serde 出力に切り替える。既存形式からの移行は 1 回だけ読み込むコードを用意し、次のリリースで削除する。
- 受け入れ条件: `apps/mobile/src/commonMain` が空。手書きの `extern "C"` と `Java_...` 関数がゼロ。

### 4. daemon のリモート多重化

- `serve_local` の `Remote` 分岐を profile ごとの共有セッションに変える。
- 受け入れ条件: 同一 profile へ 2 つのローカル接続を張ってもリレーセッションが 1 本。

### 5. ツールチェーン更新

- iOS/Android の最低バージョン、AGP、Gradle の更新は上記と混ぜず、単独の PR にする。

## レビュー基準

全ての PR に次を適用する。

- データ型に `Job`、ソケット、コールバック、ロックを入れない。
- `agent-core` で `&mut self` を持つのは `Store` だけ。
- UI 層に setter や controller を作らない。状態変更は `store.dispatch(intent)` 経由。
- 同じ RPC メソッド名が 2 つ以上のクライアント実装に現れない。
- 行数が増える PR は、増える理由を本文で説明する。

## 期待される効果

| 削減対象 | 概算 |
|---|---|
| Kotlin common | 約 8,000 行 |
| desktop の Value 操作と検証 | 1,000 行以上 |
| 手書き FFI(C ヘッダ、JNI、cinterop) | 数百行 |
| Swift の状態ロジック | 数百行 |

ビルド面では Kotlin/Native、cinterop、XCFramework の各工程がなくなる。

# Bex 会話ランタイムの設計（叩き台）

機能と見た目は T3 Code Nightly（commit `4ee6bfd`）と同じにする。内部構造は T3 を写さず、作り直す。正しさは、T3 の外から見た挙動のテストと replay transcript で判定する。

## T3 から引き継がないもの

- V1 からの移行の名残（共有イベント表、legacy 取り込み、使われない表と RPC）。
- 1 万行の Orchestrator に集まったコマンド処理。
- 複数の書き込み経路と、競合を防ぐための個別の条件付き書き込み（`writeIfRunCurrent` など）。
- adapter によるエンティティの組み立てと ID の採番。provider ごとに同じ意味の処理が重複している。
- エンティティを丸ごと運ぶイベントと、それを補うための差分の特別扱い、coalescer、配信時の切り詰め。

## 構成

| crate | 役割 | I/O |
| --- | --- | --- |
| `bex-domain` | ID、エンティティ、コマンド、事実（イベント）、スレッドの状態機械、事実から projection を作る fold。Host とクライアントで共有する。 | なし |
| `bex-providers` | provider の通信を、正規化した provider コマンドと provider イベントに相互変換する。Codex app-server と Claude（SDK の制御手順を移植）。 | provider プロセスの stdio |
| Host ランタイム（`host-daemon` 内） | スレッドごとの actor、SQLite、effect の実行、provider セッション管理、購読と同期、履歴の取り込み。 | SQLite、プロセス、Git、ファイル |
| `agent-core` | 接続、購読、fold、表示用データ。UniFFI でモバイルへ公開する。 | iroh |
| `agent-transport` | iroh と framing。変更しない。 | ネットワーク |

## 中核: スレッドの状態機械

スレッドごとに 1 つの actor が、すべての入力を順番に処理する。書き込むのはその actor だけなので、条件付き書き込みや楽観的な競合検出は要らない。

```text
入力:  Command（クライアント） | ProviderEvent（provider、attempt で識別） | EffectResult | Timer | Recover
処理:  ThreadMachine::step(&State, Input) -> Step { facts, effects, reply }
適用:  State = facts.fold(State, apply)
```

- `step` は純粋関数にする。時刻と ID の元は入力から受け取る。同じ入力から同じ結果になるので、再実行と replay が安全になる。
- 古い attempt からの provider イベントや effect の結果は、状態機械が attempt を照合して捨てる。
- 起動時は各 actor に `Recover` を渡す。実行中だった run の扱いと、キューを止めるかどうかは、T3 の復旧規則と同じ結果になるように状態機械が決める。

## 事実（イベント）

エンティティ丸ごとではなく、起きたことを小さく記録する。

- 例: `ThreadCreated`、`ThreadRenamed`、`RunQueued`、`RunStarted`、`AttemptStarted`、`ItemStarted`、`ItemTextAppended { offset, text }`、`ItemCompleted`、`RequestOpened`、`RequestResolved`、`RunFinished { status }`、`CheckpointCaptured`、`RolledBack { to }`。
- projection は事実の fold で作る。Host とクライアントは同じ fold を使う。
- ストリーミングの差分は普通の事実になる。特別な coalescer や丸ごと再送は要らない。高頻度の追記は、actor が短い間隔でまとめて 1 つの事実にしてよい。

## 永続化

SQLite（WAL）。actor の 1 ステップごとに、次を 1 つのトランザクションで確定する。

- `facts(thread_id, thread_seq, global_seq, kind, payload)`: 正本。
- `receipts(command_id, thread_id, result)`: 同じコマンドの再送に同じ結果を返す。
- `outbox(effect_id, thread_id, kind, payload, status, attempts, available_at)`: 未実行の effect。
- `thread_snapshots(thread_id, thread_seq, blob)`: 読み込みを速くするためのキャッシュ。事実から作り直せる。
- `shell(thread_id, …)`: 一覧用の要約。事実から作り直せる。

## effect

状態機械が出した effect を、実行役が outbox から取り出して実行し、結果を `EffectResult` として actor に戻す。

- 種類: provider の開始・steer・停止・応答・rollback・fork・compact、checkpoint の保存・復元、worktree の準備、タイトル生成、添付の削除、他スレッドへのコマンド。
- 実行は少なくとも 1 回。結果の採否は状態機械が決める。外部への操作が重複して困るもの（Codex の revert など）は、相対ではなく絶対の指定にする。

## provider 層

adapter は通信の翻訳だけをする。ID の採番とエンティティの組み立ては状態機械が 1 か所で行う。

- 入力: `ProviderCommand`（開始、steer、停止、要求への応答、rollback、fork、compact、モデル変更）。
- 出力: `ProviderEvent`（ターン開始・終了、項目の開始・差分・完了、承認要求、質問、計画、トークン使用量、subagent、エラー）。
- adapter が持つ状態は、通信の対応付け（JSON-RPC の id、ブロック番号、native の turn id）だけにする。
- Claude は、T3 が使っている `@anthropic-ai/claude-agent-sdk` の版を取得し、CLI との制御手順を移植した小さなモジュールの上に作る。

## 同期

T3 と同じ契約にする。snapshot、`afterSequence` からの再送、synchronized の印、長い履歴のページング。送る中身はエンティティではなく事実になる。

## クライアント

`agent-core` は、Host ごとの接続、shell と thread の購読、fold、表示用データ（棚、タイムラインの行、コンポーザーの状態）を持つ。表示用データは projection からの純粋関数で作り、変わった部分だけ作り直す。各ネイティブ UI はそれを描画するだけにする。

## 正しさの判定

- T3 のテストのうち、外から見た挙動を確かめるもの（コマンドを投げて projection を確かめる、replay transcript を流して結果を確かめる）を移植し、期待値は変えない。T3 の内部の形を確かめるテストは、同じ挙動を外から確かめる形に書き換える。どちらでもないものは対象外にし、理由を `PORT_MAP.md` に書く。
- 状態機械の不変条件（実行中の run は 1 つ、キューの順序、要求の解決は 1 回など）は proptest で確かめる。

## 今のブランチから残すもの・作り直すもの

- 残す: `agent-transport`、ペアリング、Host の会話以外の機能、各クライアントの画面のコード、`PORT_MAP.md` のファイル一覧。
- 作り直す: `crates/orchestration`（`bex-domain` と Host ランタイムへ分ける）、`crates/provider-adapters`（`bex-providers` へ）、`host_rpc/service.rs` の会話部分、`agent-core` の同期と状態管理。

## 決定事項

1. **他スレッドにまたがる操作**（fork、merge back、委任、subagent の子スレッド）は、親 actor が effect で子 actor へコマンドを送る saga にする。調整役の actor は置かない。書き込み役が 1 スレッドに 1 つという前提を崩さないため。
2. **事実は細かくする**。エンティティ丸ごとの upsert にはしない。
3. **クライアントは Host の確定を待って表示する**。楽観的な仮の状態は作らない。必要になってから足す。
4. **ID は入力から決定的に作る**。再実行と replay で同じ結果になるようにする。

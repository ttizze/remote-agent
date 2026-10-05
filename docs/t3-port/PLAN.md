# T3 Code の Rust＋ネイティブ移植計画

2026年10月5日。T3 Code Nightly `0.0.46-nightly.20261004.2652`（commit `4ee6bfd50ef4a089440d5c3662db2298da9cc50e`）を仕様として、Bex を作り直す。

参照ソースは `/tmp/t3code-ref` にチェックアウト済み。存在しなければ次で取得する。

```sh
git clone https://github.com/pingdotgg/t3code.git /tmp/t3code-ref && git -C /tmp/t3code-ref checkout 4ee6bfd50ef4a089440d5c3662db2298da9cc50e
```

同じディレクトリの調査資料を先に読む。T3 のソースを読み直す前に、ここに答えがないか確認する。

- `T3_ORCHESTRATION_SPEC.md`: T3 の契約型、コマンド、イベント、SQLite スキーマ、キュー、steer、stop、承認、rollback、provider adapter、購読と同期、履歴取り込み。
- `T3_UI_SPEC.md`: T3 の配色、文字、寸法、デスクトップとモバイルの画面構成、各操作の UI。
- `BEX_ARCHITECTURE_MAP.md`: 現在の Bex の構成。残すもの、置き換えるもの、消すもの。

## 方針

既存の会話管理を改造しない。T3 の orchestration-v2 を新しい crate へ移植し、古い会話管理は削除する。互換性、旧形式の移行、旧実装の併存は作らない。

残すのは次だけ。

- iroh の通信（`agent-transport` の transport・framing・connection・transfers）。ALPN は型が変わったら上げる。
- QR による初回ペアリング、登録済み端末の再接続、取消し（`host_identity.rs`、`host_runtime.rs` の汎用部分、各クライアントの QR・鍵保管）。
- provider プロセス管理（`bex-process`、`codex-app-server`）。
- 会話以外の Host 機能（terminal、files、worktree、browser、dictation、accounts）。会話側の型に依存している箇所だけ新しい型へ付け替える。

T3 との接続互換性は持たない。Web 版は作らない。T3 Connect は iroh で置き換える。

## 設計

### 1. `crates/orchestration`（新規）

T3 の `packages/contracts/src/orchestrationV2.ts` と `apps/server/src/orchestration-v2` を Rust へ移植する。I/O を持つ部分と純粋な部分を分ける。

- **契約型**: id、`AppThread`、`Run`、`RunAttempt`、`ExecutionNode`、`ProviderSession`、`ProviderThread`、`ProviderTurn`、`ConversationMessage`、`TurnItem`（全 variant）、`RuntimeRequest`、`PlanArtifact`、`CheckpointScope`、`Checkpoint`、`ContextTransfer`、`ContextHandoff`、`ThreadShell`、`ThreadProjection`、ドメインイベント、コマンド。serde で定義し、Host とクライアントで共有する。
- **decider**: コマンドとプロジェクションから、イベント列と effect 列を作る純粋関数。T3 の `Orchestrator.dispatchOnce` と `CommandPolicy` に対応する。
- **projector**: イベントをプロジェクションへ upsert する純粋関数。T3 の `client-runtime/src/state/orchestrationV2Projection.ts` が最短の仕様。Host とクライアントで同じ関数を使う。
- **store**: SQLite（rusqlite）。イベントログ、コマンド受付記録、プロジェクション、effect outbox を一つのトランザクションで確定し、その後に publish する。テーブルは T3 の V2 スキーマに合わせ、不要な列は削る。
- **effect worker**: outbox から effect を取り出し、provider adapter を呼ぶ。スレッドごとに直列。Host 再起動時は T3 の `reconcileAfterProcessLoss` と `ProviderRuntimeRecovery` と同じ扱いにする。実行中の run は終端にし、キューは hold する。

### 2. provider adapter（`crates/orchestration` 内のモジュール、または新規 crate）

T3 の `ProviderAdapter.ts` の trait を移植する。adapter は完全なエンティティを作り、イベントとして返す。

- **Codex**: `codex app-server` を stdio JSON-RPC で使う。T3 の `CodexAdapterV2.ts` の対応表どおりに、`thread/start`・`thread/resume`・`turn/start`・`turn/steer`・`turn/interrupt`・`thread/fork`・`thread/revert`・承認要求と通知を変換する。runtimeMode の対応も T3 に合わせる。既存の `codex-app-server` crate と `agent-transport::peer` を使う。
- **Claude**: Rust 用の SDK はないので、`claude` CLI の stream-json モードを使う。起動フラグは既存の `host-daemon/src/claude/process.rs` を流用する。イベントの対応、承認、AskUserQuestion、ExitPlanMode は T3 の `ClaudeAdapterV2.ts` に合わせる。

### 3. Host の RPC

`host_rpc` の会話部分（`routing.rs` の会話側、`session_actor.rs`、`submission.rs`、`agent.rs`、`codex.rs`、`native.rs`、`requests.rs`、`claude.rs` の会話部分、`service.rs` の会話 dispatch）を削除し、T3 のオーケストレーション RPC に置き換える。

- `dispatchCommand`、`launchThread`、`subscribeShell(afterSequence)`、`subscribeThread(threadId, afterSequence)`、`getThreadProjection`、`getTurnItem`、履歴ページ、`searchThreads`、`getTurnDiff`。
- 購読は既存の「応答ストリームを開いたまま後続フレームを送る」方式を使う。snapshot → synchronized → event の順に送り、`afterSequence` からの再送と snapshot へのフォールバックは T3 の `ws.ts` と `ThreadStream.ts` の条件に合わせる。
- 先に `SessionRouter` から接続管理の部分を独立した型へ切り出す。terminal がそれに依存しているため。
- エンコードは既存の Postcard を使う。中身が任意の JSON になる値（dynamic tool の入出力など）は JSON 文字列で包む。

### 4. 既存履歴の取り込み

初回起動時に自動で行う。T3 の `AgentSessionScanner` と `AgentSessionImporter` と同じ範囲にする。

- Codex の `~/.codex/sessions/**/rollout-*.jsonl` と Claude の `~/.claude/projects/**/*.jsonl` を走査する。
- ユーザーとアシスタントのメッセージだけを `thread.created`、`message.updated`、`turn-item.updated` として書き込む。
- `provider-thread.updated` に native のセッション id を入れ、次のターンで native セッションを再開する。
- 元ファイルは変更しない。取り込みは一覧表示を止めず、バックグラウンドで進め、shell 購読に反映する。読めないファイルは飛ばして続ける。
- ツール、承認、計画の詳細は取り込まない。T3 も取り込んでいない。

### 5. クライアント共通層（`agent-core` を作り直す）

T3 の `packages/client-runtime` を移植する。

- 接続、再接続、PC ごとの shell 購読、thread 購読、sequence カーソル、再購読、アイドル thread のキャッシュ。
- projector は `crates/orchestration` の関数を使う。
- 表示用データ: サイドバーの棚（Pinned、Active、Working、Snoozed、Settled）と行の状態、タイムラインの行（ユーザー、アシスタント、作業ログのまとまり、計画、承認、質問、差分の要約）、コンポーザーの状態（送信、steer、queue、stop のどれを出すか）。クライアントで同じ判断を重複させない。
- UniFFI でモバイルへ公開する。デスクトップは Rust から直接使う。
- 接続とペアリングの関数（`bindings/mod.rs` の invitation 関連）、terminal、files、browser の operation は残す。

### 6. ネイティブ UI

3 クライアントとも、会話一覧、会話画面、コンポーザー、キュー、承認、質問、モデルと実行モードの選択を作り直す。寸法と色は `T3_UI_SPEC.md` に合わせる。配色は共通層に置き、各クライアントはそれを読む。

- デスクトップ（GPUI）: T3 web の v2 サイドバー、チャット列 736px、ヘッダー 52px、コンポーザー、右パネル（Diff、Terminal、Files、Browser）。
- iOS（SwiftUI）と Android（Compose）: T3 mobile の一覧、会話画面、コンポーザー、キューのシート、承認と質問のカード、設定。
- ペアリング画面、terminal、files、browser の既存画面は残し、色だけ合わせる。

## 範囲と順番

### M1: 会話の中核（この PR で必ず完成させる）

1. `crates/orchestration`: 契約型、decider、projector、SQLite store、effect worker。コマンドは thread の作成、アーカイブ、削除、settle、snooze、pin、visit、未読、改名、`message.dispatch`（全 dispatchMode）、`run.interrupt`、キュー操作（resume、reorder、cancel、edit、promote-to-steer）、`runtime-request.respond`、`thread.user-input.dismiss`、runtimeMode、interactionMode、モデル選択、`provider.switch`。
2. Codex adapter と Claude adapter。
3. Host の RPC を置き換え、古い会話管理を削除する。
4. 既存履歴の初回自動取り込み。
5. クライアント共通層の作り直し。
6. デスクトップ、iOS、Android の会話 UI を T3 の見た目で作り直す。
7. 古い会話の型、テスト、fixture、文書（`docs/SESSION_RUNTIME.md`、`docs/BEX_PROTOCOL_DESIGN.md`、`docs/BEX_PROTOCOL_NATIVE_CONTRACTS.md` の会話部分）を削除する。`docs/DESKTOP_CONVERSATION_DISPLAY_CONTRACT.md` は T3 準拠の要件へ書き換える。製品要件が変わったため、ここは書き換えてよい。

### M2: 会話の拡張

checkpoint と rollback（git ref に保存）、fork、merge back、provider の handoff、compaction、subagent と委任、計画モードの Implement と Refine、添付（画像とファイル）、turn diff。

### M3: 周辺機能

Git 操作と worktree、PR 連携（まず GitHub）、scheduled tasks、usage、設定画面の全項目、Nightly 配布。

M1 を完成させてから M2 へ進む。M1 の途中で M2 以降に手を出さない。

## 進め方（速度のための規則）

- 作業はこの worktree の現在のブランチで行う。main へのマージや push はしない。完成したらブランチを push して PR を作る。途中で main を取り込み直さない。PR を作る直前に一度だけ取り込む。
- テストは変更した crate に絞って回す（例: `scripts/dev-env.sh cargo nextest run -p orchestration`）。全体の `scripts/dev-env.sh just unit-tests` は、M1 の各段階の区切りとコミットの前だけに回す。
- CI の結果待ち、cargo-mutants、ローカル E2E、Simulator を使う UI テストはしない。iOS と Android は、ビルドが通ることだけを確認する（`scripts/build-agent-ios.sh simulator` と xcodebuild の build、`./gradlew :apps:mobile:assembleDebug`）。
- 古い会話の仕組みに合わせたテストと fixture は、直そうとせずに削除する。新しい crate のテストは、T3 の状態遷移（キュー、stop、steer、承認、再起動からの復旧、同じコマンド id の再送）を検証する。proptest は decider と projector に使う。
- 段階の区切りごとにコミットする。
- 稼働中の Host、main、他の worktree には触れない。
- 不明点で止まらない。T3 の挙動に合わせて決め、判断した内容をこのファイルの末尾の「判断の記録」に追記する。

## 判断の記録

- 2026-10-05: 今回のユーザー指示を優先し、テストは変更した crate に限定する。段階区切りでも全 workspace テスト、cargo-mutants、CI 待ち、E2E は実行しない。現在のブランチに段階ごとにコミットし、main・稼働中 Host・他の worktree は変更しない。
- 2026-10-05: native wire 契約の enum は Postcard が直接復号できる外部タグ形式を使う。T3 の識別子とイベント名・コマンド名は保持するが、T3 との wire 互換性は設けない。dynamic tool の任意 JSON だけをバイナリでは JSON 文字列で包む。
- 2026-10-05: SQLite の書込と publish を同じ owner lock で直列化する。購読 receiver の登録と snapshot／replay 読出しもその lock 内で行い、登録時のイベント欠落と publish 順序逆転を防ぐ。effect は thread ごとに outbox の rowid 順で処理する。
- 2026-10-05: ローカル開発環境は macOS 標準 Bash 3 では Nix が出力する `;&` を読み込めないため、flake の固定 Bash 5 で `scripts/dev-env.sh` を実行する。共有 dev-env キャッシュを修正せず、他の worktree に影響させない。

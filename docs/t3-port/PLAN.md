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
- 2026-10-05: provider adapter は `crates/provider-adapters` に置く。`orchestration → agent-transport → agent-protocol → orchestration` という循環を避けるため。adapter は完全なイベントを送出し、Host が run／attempt の所有権を検証して store へ反映する。
- 2026-10-05: Claude の ExitPlanMode は T3 と同じく、空でない計画を保存して `deny` を返し、後続ターンのユーザー指示を待たせる。AskUserQuestion の複数選択は CLI が要求する文字列へ変換する。native session の再開は CLI の `--resume` を使い、プロセスの終了確認まで capacity permit を保持する。
- 2026-10-05: 新しい会話 RPC は Postcard の型が変わるため ALPN を streams/6 にする。接続の identity・通知配送は会話 owner から独立させ、terminal と共有する。provider 通知は store で現在の run／attempt を検証してから受理する。
- 2026-10-05: 初回 transcript 走査は読み取り専用・最近30日・各 provider 最新100ファイル・各会話200発言とする。最初のユーザー発言を残し、native session UUID と実際の cwd を保存する。取り込み済み会話は上書きしない。取り込み時の既定モデルは固定 T3 ソースの gpt-6-astra と claude-fable-5-1 を使う。
- 2026-10-05: provider-thread が native session を持つ段階で app thread の activeProviderThreadId を更新する。キュー作成だけの placeholder は更新しない。T3 のサーバー側更新規則を共通 projector に含め、Host とクライアントを一致させる。
- 2026-10-05: 会話の launch では既存 Host の worktree 自動作成設定を維持する。M2 の新しい workspace strategy はまだ実装しない。Host 検証は --lib の単体テストに限定し、実 Host を起動する integration test は実行しない。terminal 単体テストに必要な supervisor は同じ worktree にビルドする。
- 2026-10-05: agent-core の旧 reducer・operation・会話描画・旧形式の保存状態を削除し、V2 の snapshot／replay と共通 projector に置き換える。PC ごとに Store を持ち、選択中 thread と16件の idle cache を保持する。history を追加しても partial timeline の watermark を維持する。
- 2026-10-05: クライアントの mutation RPC は thread ごとに受付順で直列化する。未確認の command／launch は同じ ID のまま保存して再接続時に再送する。既存 thread の通常送信は queue-after-active を使い、実行が無ければ Host が即時開始する。これにより購読更新より先に次の送信が来ても T3 の既定 queue 動作を保つ。
- 2026-10-05: 棚分け・作業ログ・承認と質問・コンポーザー・provider capability による操作可否は agent-core で判断する。ネイティブ側は共通レコードと固定 T3 の theme palette を描画する。受信キューは64件で backpressure をかける。新しく書いた下書きや別 thread への移動は、遅い送信応答で上書きしない。
- 2026-10-05: shell に最新 run の requestedAt／completedAt と active run の startedAt を含める。Active の並び順は T3 の return 時刻、Working は最後のユーザー発言時刻を使う。承認待ちなどで Working を離れた観測時刻だけクライアントに保持し、削除時に除去する。snooze の早期解除は rename 等の更新時刻でなく run の終了時刻を見る。
- 2026-10-05: M1 の棚は計画に指定された5種類を表示し、固定 T3 の Working shelf 有効時の分類と順序を採用する。T3 の設定既定値は無効だが、全設定を扱う M3 で切替を公開する。Pinned の移動は共通層で T3 の base26 fractional key を計算し、未採番の列だけ必要時に採番する。Snoozed は解除予定順、Settled は終了・settle 時刻順で、改名では順序を変えない。
- 2026-10-05: GPUI の旧会話 UI と補助状態を削除し、共通の ConversationView を描画する。下書きの widget 編集は revision で遅い receipt から保護し、共通 owner は snapshot を publish してから receipt を完了する。履歴追加時は先頭の表示位置を保持し、末尾近くを見ている場合だけ新着へ追従する。
- 2026-10-05: files のバイナリ転送と音声入力は会話以外の機能として残す。転送は共通 owner の task が既存の hash／size 検証付き iroh transfer を呼ぶ。音声の転記は録音開始時の下書きに追記し、新しい入力を上書きしない。端末の保存先は orchestration 専用にして旧会話形式を読まない。
- 2026-10-05: SwiftUI の旧 conversation presentation・side chat・media import を削除し、共通 ConversationView と棚を直接描画する。M1 の基本設定には接続・provider accounts・既存 worktree 設定を残す。キューはシート、承認と質問はカード、下書きは native widget の revision で保護する。T3 と同じ DM Sans 3書体を両モバイルが共有する静的資産として同梱し、OFL と取得元・hash を記録する。
- 2026-10-05: user-input.dismiss は固定 T3 と同じく message 応答の質問にだけ表示する。構造化質問は回答または stop、承認は decision の選択を使う。files の保存は編集開始時の revision を共通 owner に保持し、再読込で楽観ロックの基準を変えない。保存中の追加入力は receipt 後も保持する。
- 2026-10-05: Compose の旧 projection・添付・会話 widget を削除し、共通 ConversationView と5棚を描画する。Host 切替時の receipt は取消し、下書きとファイル編集の native buffer は revision で保護する。Android の Java・SDK・NDK・Rust target は専用の Nix android shell に固定し、AGP の Kotlin source set を認識しない formatter には main source 専用 task を指定する。UI テストは実行せず、両 ABI の assembleDebug を確認する。
- 2026-10-05: M1 の最後に旧 session 契約・RPC・項目転送・composer catalog・headless CLI・Host fixture と、それを前提にした UI テスト／runner を削除する。現行の RPC、保存、表示契約の文書に置き換え、CI の呼出しも削除済み crate に依存させない。接続・転送と peripheral Host のテストは残す。macOS のプロセス終了確認には BSD の状態列を返す /bin/ps を使用する。変更 crate の216テスト中215件は通過し、修正した終了確認を含む6件の再検証も通過した（既存の3件は skip）。
- 2026-10-05: M1 は `af584c44` で旧実装の削除まで完了し、同じ revision の Host・GPUI・iOS・Android ビルドを確認した。その後 M2 の root checkpoint と turn diff を実装する。ref 名は調査資料の省略表記より固定ソースを優先し、scope ID の SHA-256 の先頭32桁の hex 文字列を base64url にする。
- 2026-10-05: checkpoint は private Git index と fsync 付き専用 ref に保存する。provider 開始前に baseline、完了後に durable outbox で capture を行う。run 完了・node・timeline item・次のキュー開始を同じ SQLite transaction に含める。停止の terminal status と再起動後の queue hold は保持する。再送で保存済み ref を書き換えず、古い attempt と未実行の queued run の capture は受理しない。
- 2026-10-05: Git の通常 index・HEAD・cwd 外の変更は checkpoint 作成で変更しない。cone sparse checkout と unborn HEAD を扱う。非 cone の private index 再構築は false deletion を避けて error checkpoint とし、会話は継続する。この制限と rollback 等の未実装は実装状況と PR に明記する。
- 2026-10-05: getTurnDiff と ready root checkpoint の範囲選択を共通 core へ追加し、GPUI・SwiftUI・Compose の Diff 画面で表示する。空白差分無視は RPC/core で扱う。範囲・thread が変わった後の遅い応答は owner が捨てる。M2 の残りと M3 は未実装として記録する。
- 2026-10-05: PR 前に main を一度だけ取り込んだ。更新された旧会話 fixture・UI test・Maestro runner とその専用依存／設定は削除を維持する。削除済み fixture だけが使う Host cleanup interval の引数も残さない。WebKit の独立テストの改善、Dependabot のスケジュール、unit-tests の追加 target 引数は維持する。main 自体と他 worktree は変更しない。
- 2026-10-05: PR #55 の追加依頼を優先し、origin/main の `2af069b3` を merge commit で取り込み、全体の `scripts/dev-env.sh just unit-tests`、workspace と agent-peer の clippy/fmt を実行する。main の CI queue、最後の成功した main からの変更検出、Mac/iPhone の分離を維持する。削除済み会話 runner の fixture・Markdown cache・Simulator shard・Chrome wrapper を除去し、現行の単体テストと native build に合わせる。M2・M3 の追加実装は行わない。
- 2026-10-05: dev-env の line 2074 は破損ではなく Nix の `;&` を Bash 3 で評価した構文エラー。リポジトリの script で env.sh の引用済み BASH store path だけを読み、固定 Bash に exec してから環境を評価する。共有キャッシュは変更・削除せず、初回生成と Nix なしの再利用、引数・TMPDIR・終了コードの保持を隔離した fixture で確認した。
- 2026-10-05: CI の依存境界テストは旧 protocol の Tokio 非依存を前提にしていた。新しい orchestration は契約・SQLite・async worker を同じ crate で所有するため、その推移的 Tokio 依存を許可し、orchestration 自身もクライアント・Host・transport から独立する検証対象に追加する。製品コードや M2/M3 の範囲は変更しない。
- 2026-10-05: 新しい追加依頼に従い同じ PR #55 で M2 の残りを完成させてから M3 へ進む。途中は変更 crate の単体・property test を使い、機能区切りで commit する。最後だけ origin/main を一度 merge し、全 unit-tests・clippy・fmt を通して push/PR 更新する。実 Host、main、他 worktree、CI 待ち、mutants、E2E、Simulator UI は対象外。
- 2026-10-05: rollback は固定 T3 の admission と実行時の隔離検証、Codex の paginated history に対する thread/revert、Claude の assistant UUID と --resume-session-at、private index による Git 復元、run/node の rolled_back と checkpoint stale を移植する。失敗と成功の反映は受付 ID で保護し、desktop の Edit from here で files を戻すか選ぶ。T3 mobile にない rollback UI は追加しない。wire 契約変更で ALPN を streams/7 にする。

- 2026-10-05: fork/merge back は source point と転送を一つの transaction で保存し、provider は最初の送信まで起動しない。同じ provider は Codex thread/fork または Claude --fork-session/--resume-session-at、それ以外や再開失敗は固定 T3 の16000-byte枠の intact message context を使う。戻った provider は lastRunOrdinal 以降の delta、merge back は fork の追加履歴を渡す。転送の消費は actual turn 開始イベントと同じ transaction に入れ、Claude init 自体では消費しない。mobile の fork/merge back は固定ソースにもあるため3クライアントへ公開する。

- 2026-10-05: /compact は Codex thread/compact/start と Claude の native command を使う独立した maintenance turn とし、Git capture と steer を行わない。空会話の compact を拒否し、/logout は最後の native provider の account owner へ渡す。desktop の Implement/Refine は固定 T3 の PLEASE IMPLEMENT THIS PLAN と plan/default mode を使い、active source plan の完了と run の sourcePlanRef を同じ transaction に保存する。新しい案は前の未完了案を superseded にする。Implement in a new thread は source project/branch/worktree と現在の model を使う。mobile には Implement/Refine を増設せず /plan と /default を公開する。

- 2026-10-05: ユーザー指示で M2/M3 を aacd3737 から中断し、M1 のレビュー87件への対応を先に行う。残った実装のテストと各不具合の再発テストを戻し、対応と非対応の理由を PR55_REVIEW.md に記録する。main は取り込まず、最後に全 unit-tests・clippy・fmt とブランチ push/PR 更新だけを行い、M2/M3 は再開しない。

- 2026-10-05: M1 レビューに従い、orchestration の既定 feature を純粋な契約・判断に分離する。SQLite/effect worker は Host の runtime feature、provider trait は adapter feature だけで有効にし、feature を追跡した dependency boundary test を使う。前の「推移的 SQLite/Tokio を client に許可する」判断を置き換える。
- 2026-10-05: 固定 T3 の runtimeModeConfig、confirmThreadDelete、ComposerPendingApprovalPanel と Sidebar を照合し、pending request は composer drawer、解決済み request は work log とする。空の Working/Snoozed は省くが Pinned/Active/Settled は残す。Active は Working 有効時の復帰時刻順とし、UI にない ActiveReorder を新しく追加しない。
- 2026-10-05: 連続した provider delta は固定 T3 の50ms coalescing を使い、64KiBで途中 flush、item/turn/lifecycle の境界で順序を保つ。guarded delta は現在 run のみを検証し、subscriber 共通 shell cache を差分更新する。完全な projection event の形式は維持するため、低速 stream の保存量が厳密に線形になったとは扱わない。
- 2026-10-05: native draft の保護対象は core の draft context key と最新 receipt revision で決める。会話・queue edit・Host の切替後に古い receipt を適用しない。モバイルの background projection は一つの task が最新 snapshot まで追いつく形にし、連続更新で毎回 cancel する debounce は使わない。
- 2026-10-05: 今回は retained integration tests を nextest の通常 target に戻し、最後にユーザー指定の全 unit-tests を実行する。Swiftlint/detekt の閾値や baseline は変えず、宣言的な native layout の長さ・分岐だけ局所コメント付き例外にする。clean-builds は他 worktree を変更しない dry-run に限る。Swift/Kotlin の純粋な buffer unit と Android の保存 owner の JVM test を追加し、Simulator UI/E2E/CI 待ちは行わない。
- 2026-10-05: 全 target の unit-tests に手動 WebKit probe が一度混入したため、custom harness の list/ignored 契約を直し通常 nextest では skip とする。削除した Host 会話ループの停止ログを待つ診断テストは、現在の Codex pump の復旧・原因記録・payload 非記録テストへ移す。macOS の binary 起動がローダー内で遅れる fixture は初回 initialize のみ30秒、起動後の通信期限は3〜5秒のままにする。
- 2026-10-05: レビュー対応の実装・テスト `08b882ec` で全 unit-tests 306件と agent-peer の5群が通過（外部・手動の5件 skip）。workspace/agent-peer の clippy・fmt、actionlint、Swiftformat/Swiftlint と Swift 2件、ktfmt/detekt と Android 3件、iOS/Android build を確認した。xtask の fixture 起動枠も cancellation/cleanup の期限と分離し30秒にする。対応・非対応の理由を PR に更新してブランチを push し、M2/M3 は再開しない。

- 2026-10-05: ユーザーの2回目レビューを優先し、前回の「M2/M3は再開しない」を更新する。R2 の各指摘と前回の部分修正・誤判断を現在のコードで確認し、修正・回帰テスト・全 unit-tests/clippy/fmt・push/PR 更新の後、M2 の subagent/委任・添付・nested checkpoint scope を完成させる。M3、main の取り込み、稼働中 Host、他 worktree、CI 待ち、mutants、E2E/Simulator UI は対象外。
- 2026-10-05: 取り込み会話の worktreePath は固定 T3 の importer と同じ null とする。effect は error/panic/期限切れ lease を含め5回で終端にし、同じ thread の後続 effect を解放する。Start と Restart の停止を同じ owner で監視する。rollback は一件だけを受け付け、完了は rollback フィールドだけを現在の thread に反映する。
- 2026-10-05: R2 の rollback は、実際に受理された native turn ID を絶対境界に使い、再試行時に境界の存在を確認する。ファイルを scope cwd に限定して先に退避・復元し、provider の失敗時は補償する。Git checkout ごとの journal は途中終了から復旧する。submodule・パス衝突は変更前に拒否し、checkpoint の capture/diff/restore の範囲を揃える。
- 2026-10-05: queue のユーザー item は固定 T3 と同じく promote 時に作る。実行へ昇格した run の ordinal は既存の最大値より大きくし、実行順と rewind 順を一致させる。直前の状態は before-run checkpoint ID と parentCheckpointId で関連付け、欠番を ordinal−1 で補わない。rollback 後の stale refs を削除する。
- 2026-10-05: fork の継承表示は作成 transaction で固定して SQLite に保存し、親の現在の visible items を再走査して作り直さない。merge back は最新の Completed/Waiting run を明示して、より新しい active run がある間は拒否する。同じ差分の重複 transfer を作らず、未消費の古い merge を supersede する。provider handoff は直前に実行された run を基準にする。compact は未送信の handoff を消費しない。

- 2026-10-05: R2 の下書き復元は Host の rollback 完了 sequence と成功状態を確認してから、操作後の入力へ追記する。fork/merge は navigation だけ行い、両側の下書きを移動しない。モデルや mode は text 編集の payload に含めず core の現在値を保持する。mobile は list で開始し、保存済み selection を visit しない。Store の revision は Store ID 内だけで比較する。配送 Unknown は同じ ID で再試行し、利用者の「Stop retrying」で未確認 command を外せる。
- 2026-10-05: 固定 T3 Sidebar の Pinned/Active は通常表示では高さ0の drag marker、Working/Snoozed は既定で折り畳み、選択中の行だけ残す。hero と Edit from here の文言も固定ソースに合わせる。desktop terminal entity の終了は共通 Store へ Detach を送り、Browser は同じウィンドウ内で保持する。

- 2026-10-05: R2 の連続出力は assistant/reasoning/plan/command の suffix と byte offset を event 化し、replay を冪等化する。不変 projection collection と device state を Arc で共有し、変更 collection だけ owner が更新する。wire event が増えたため ALPN を streams/8 に上げる。
- 2026-10-05: Claude prompt の admission は process supervisor が送信直前に判断し、UUID echo/result の所有権を追跡する。CLI が early echo を出す時だけ confirmation 前の root output を保留し、result-only/旧 echo mode は停止させない。
- 2026-10-05: R2 で前回 D20 の shelf header の判断を撤回する。固定 T3 の Pinned/Active は通常見えない drag marker、Working/Snoozed は既定 collapsed とし、開いている thread の行を保持する。
- 2026-10-05: R2 の AttachmentCleanup は thread ID を hash した Host 所有 directory の削除に置き換える。任意 workspace のアップロードは会話 asset と混同しない。添付の claim/表示/入力は次の M2 で実装し、同じ thread directory を利用する。
- 2026-10-05: M2 の app-owned 委任は、固定 T3 の self-contained task、live model catalog、親より広げない permission/mode、永続の task result と完了 mailbox を使う。async は steer/優先 queue、wait の timeout/disconnect は子を止めず wake に切り替える。terminal result の read は delivery を acknowledge し、cancel は original task とその委任継続だけを止め、後から追加した通常 run は止めない。restart/stop/delete/rollback/recovery では mailbox と委任先の所有権を閉じる。MCP は Host 所有の loopback bridge と thread/provider に固定したランダム scope を用い、認証情報を argv・保存・ログに出さない。M3 の scheduled tasks や worktree launch はまだ広告しない。契約の拡張に合わせ ALPN を streams/9 にする。
- 2026-10-05: Agents roster、進行件数、モデル・状態・詳細・子 thread の選択先は agent-core で計算する。固定 T3 の threadSubagents.ts と ThreadAgentsSheet を基準に desktop の menu/work log、iOS/Android の Agents sheet と composer pill を接続し、mobile に T3 にない agent 管理操作は追加しない。
- 2026-10-05: native subagent は固定 T3 の runless な子履歴・親側 task/node と CLI の実 thread/turn ID を使う。親の return 後も購読し、task_started でだけ再開する。早着した子出力を bounded queue で保留し、停止・再起動・rollback では所有する子だけを閉じる。子 thread の入力は native turn 完了まで待たせ、共有する親の provider session を子の所有物として保存しない。Claude の native 完了応答は T3 の adapter_buffered continuation と同じ通常 queue/checkpoint 開始経路で取り込み、追加 prompt は CLI へ送らない。
- 2026-10-05: M2 添付は固定 T3 の 100件、画像10 MiB/合計80 MiB、ファイル50 MiB、写真の長辺2048px/JPEG品質85%に合わせる。iroh の検証付きバイナリ転送で pending asset を作り、command の所有 thread へコピーして claim する。Codex localImage と Claude base64 image を adapter で生成し、native UI は core の upload/receipt/draft 判断を描画する。fork の参照中 asset は親削除後も保持する。モバイルには T3 の写真/ファイル選択のみを追加する。
- 2026-10-05: M2 の nested checkpoint は固定 CheckpointService の nullable run、独立 scope ordinal、nullable appRunOrdinal、汎用 baseline/capture/restore/diff を実装する。自動 scope は固定 T3 と同じ root のみで、モバイルに nested 操作を追加しない。nested capture は thread/node/provider/attempt と parent scope の所有権、パスと循環を検証し、専用 outbox で再取得・復旧しても root run の完了やキュー昇格を起こさない。nested rollback は scope 境界だけを無効化し、無関係な provider turn を巻き戻す要求を拒否する。root rollback は巻き戻した run の子 scope の ref も削除する。Restart は baseline が既にあっても root scope を新しい node に結び直す。
- 2026-10-05: 添付 cleanup は最後の fork の削除時に削除済みの祖先も再確認する。live thread の参照は残し、live 祖先の directory は削除しない。全走査の追加ではなく、既存の参照収集と同じ owner transaction で最大128段の lineage を読み、対象 directory のみを冪等に整理する。
- 2026-10-05: R3 レビューを d948b812 と中断時の未コミット変更に照合して修正する。M3・main 取り込み・実 Host の変更は行わず、関連単位で commit、最後に全 unit-tests/clippy/fmt と push/PR 更新を行う。provider の制御失敗は native 停止の証拠にしない。stop は出力 admission を閉じても、実際の停止確認まで子の実行所有権を保持する。Restart は新 attempt への切替時に旧子の表示を終端化する。
- 2026-10-05: Claude の fallback と同一 Start の再取得に別の event namespace を付け、入力を一度しか渡さない。steer は元 prompt の gate を維持し、元と steer 両方の UUID を受理する。固定 T3 にない8プロセスの制限と eviction は削除する。wake の上限超過は実 terminal frame を必ず保持し、切り詰めを明示した終端エラーとして扱う。rollback は live な native 子と pending rollback 中の新規実行を拒否する。

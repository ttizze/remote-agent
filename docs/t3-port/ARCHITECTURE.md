# 会話ランタイムの設計

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
| `agent-domain` | ID、エンティティ、コマンド、事実（イベント）、スレッドの状態機械、事実から projection を作る fold。Host とクライアントで共有する。 | なし |
| `agent-providers` | provider の通信を、正規化した provider コマンドと provider イベントに相互変換する。Codex app-server と Claude（SDK の制御手順を移植）。 | provider プロセスの stdio |
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
- 作り直す: `crates/orchestration`（`agent-domain` と Host ランタイムへ分ける）、`crates/provider-adapters`（`agent-providers` へ）、`host_rpc/service.rs` の会話部分、`agent-core` の同期と状態管理。

## 決定事項

1. **他スレッドにまたがる操作**（fork、merge back、委任、subagent の子スレッド）は、親 actor が effect で子 actor へコマンドを送る saga にする。調整役の actor は置かない。書き込み役が 1 スレッドに 1 つという前提を崩さないため。
2. **事実は細かくする**。エンティティ丸ごとの upsert にはしない。
3. **クライアントは Host の確定を待って表示する**。楽観的な仮の状態は作らない。必要になってから足す。
4. **ID は入力から決定的に作る**。再実行と replay で同じ結果になるようにする。

## 実装時の判断（新設計）

- 2026-10-05: 新設計は `agent-domain` と `agent-providers` に置く。段階 3〜5 の承認までは Host・既存クライアントは接続し直さない。旧方針の未コミット policy/order は保持し、新設計にはその挙動とテストを取り込む。
- 2026-10-05: 時刻と ID の seed は `InputEnvelope` に明示する。receipt は projection に含めず、actor が永続化した receipt を再送入力に渡す。fingerprint が異なる同一 command ID は拒否する。
- 2026-10-05: 事実のテキスト offset は UTF-8 の byte offset とする。Rust の Host とクライアントで同じ fold を使うため、T3 の JavaScript 文字位置との wire 互換性は不要。表示されるテキストと順序は変えない。
- 2026-10-05: キューの発言は受け付け時に保存するが、タイムライン項目は昇格時に作る。項目 ordinal はスレッド全体で単調に増やす。通常の追記は既存 item と attempt のみ参照し、履歴や累積テキストを複製しない。
- 2026-10-05: fork は作成時の履歴・portable context を子への command に固定する。rollback の結果は要求 ID と checkpoint だけを確定し、途中の rename 等を上書きしない。rollback 中は新規実行・resume・retry・fork・merge back を拒否する。
- 2026-10-05: provider 層は app の ID や entity を生成しない。native key の対応だけを扱い、子スレッドへの書き込みは親の状態機械から saga effect で行う。独自の provider capacity、wake buffer の件数・byte 上限、steer UUID gate は設けない。
- 2026-10-05: Claude の prompt echo の早期／result-only／未対応の判定と、先行した provider ターンの run への帰属は状態機械に置く。翻訳層はフレームの UUID・origin・turn 数だけを正規化する。steer は `PromptOffered` を発行せず、進行中 prompt の所有者を変えない。provider continuation は既に流れてきた出力を受け取り、CLI へ新しい prompt を送らない。
- 2026-10-05: 発言の作成者と作成元は明示的な入力にする。自動発言の重複した boolean は持たず、agent 作成の発言でキュー優先順位を決める。CLI の background roster と通知は domain の事実として記録する。
- 2026-10-05: native fork は親に固定した子作成 command を記録してから provider effect を実行し、成功結果を受けて子へ送る。native セッションがない場合は portable context を使う。成功結果の再送は子作成を繰り返さない。
- 2026-10-05: rollback の絶対 head は checkpoint に固定する。Claude の assistant UUID は翻訳層の累積 cursor から terminal result に付けず、所有者が決まった frame の事実として attempt に記録する。先行した peer ターンや子の cursor が、別の run の rollback 境界にならないため。
- 2026-10-05: provider は native の親子関係だけを保持し、深い子の通知を `Child` の経路として正規化する。各 actor は直下の子だけへ effect を送る。session 共有のルーティング用 app entity を adapter に作らない。
- 2026-10-05: 停止 command は対象の native thread/turn を明示する。起動 RPC の応答より先に停止した場合、翻訳層は未送信 prompt を取り消すか、native turn ID が判明してから interrupt する。プロセス終了は `SessionClosed` 入力で確定し、終了フレームのない Claude の停止も run とツールを terminal にする。
- 2026-10-05: checkpoint 保存失敗は captures の対象を消さず outbox の再試行に任せる。停止 run の status は保存待ちでも interrupted/cancelled を保ち、captures が残る間は後続 run を始めない。rollback 済み run の遅い保存結果は採用しない。
- 2026-10-05: 添付は image/file の種別を持ち、captured-window の accessibility を明示的な入力にする。T3 と同じ JSON の囲み・省略規則・入力文字数制限で provider prompt を構成する。空の todo と proposed plan を区別し、plan 本文の追記は offset 付き事実、本文と steps の置換は別の事実にする。
- 2026-10-05: native 子の承認は親スレッドの要求として記録する。要求に native 親子経路を保存し、子の timeline には承認カードを作らない。CLI initialize が返す未解決の承認・resume dialog は再登録し、同じ native request ID の再通知は二重表示しない。
- 2026-10-05: background の終了報告は wake 発言の notification metadata として保存する。発言自体は保持し、timeline は通知として表示する。複数の終了報告は T3 の Notification.ts と同じ集約を行う。報告は prompt の世代で管理し、次の世代まで保持する T3 の規則を移植する。
- 2026-10-05: Claude の task ID と tool-use ID の対応を保持し、SendMessage で tool-use ID が変わっても同じ子を再開する。再開の run/attempt/prompt と startedAt を更新し、古い progress を消す。子の snapshot にある実際の model は子の selection と親の task に反映し、孫は所有する子の model を引き継ぐ。
- 2026-10-05: Claude の foreground な子の Bash は root の background roster と wake に含めない。native local_bash の background 報告は native session の root に帰属し、TaskStop で subagent と command が終了した場合も T3 と同じ通知にまとめる。
- 2026-10-05: 正規化された native TextDelta と plan の追記も、既存項目がある場合は scratch projection を複製せず offset 付きの事実だけを返す。別 provider instance の終了済み attempt の root 出力は、現在の run を更新しない。
- 2026-10-05: 質問の回答は text と choices を区別し、添付の引用付き参照を T3 と同じ形で追加する。ファイルの可用性確認は段階 3 の effect executor の責務とし、domain には確認済みの path を渡す。message 型の質問回答は server 作成の通常発言として Auto の配送規則に従う。
- 2026-10-05: native session の現在の ID は instance ごとに fold が所有する。rollback 成功結果には作り直した native binding を含め、後続ターンはその ID を使う。checkpoint に head がない後発 instance も絶対先頭へ rollback する。
- 2026-10-06: context handoff は役割・出典・status を持つ歴史項目の固定 snapshot とする。予算、画像/file の見積もり、項目を丸ごと省略する優先順位、取得用 coverage は T3 の ContextHandoffBudget/Delivery に従う。上限は T3 の 16k tokens/64k bytes と同じで、現在のユーザー入力は切り詰めない。
- 2026-10-06: history delivery は provider effect より前に pending を記録する。Codex の injection 成功は turn/start の前に injected として fold し、明示的な -32601 だけ inline に切り替える。inline は入力の受け付け後に確定する。pending のまま失敗した native session には再送せず、Host から session reset を受けた後に再試行する。native RPC reply の事実を確定してから次の outbound を送る順序は段階 3 で守る。
- 2026-10-06: handoff の設定値と既知の model window は明示的な HandoffPolicy 入力で受ける。context occupancy とターンの課金用 usage は別に扱い、Codex は last、Claude は assistant snapshot の cache read/write を含む値を context budget に使う。
- 2026-10-06: ターンの main-agent 使用量は context snapshot と分けて事実にする。Codex の累積 total の差分・last fallback・遅延通知の baseline 更新は domain が所有し、Claude の result の complete/partial/unavailable と cache/thinking の正規化は純粋関数にする。native 子の使用量は子 actor に保存し、親の使用量には合算しない。
- 2026-10-06: provider エラーに native item key がない場合、domain が入力 seed から項目を採番する。adapter の RPC カウンターを表示項目 ID に流用しない。MCP の表示名は UTF-16 長・空白処理、アイコンは HTTP(S) URL の規則を T3 と同じにする。
- 2026-10-06: 通知は Message の metadata を正本とし、activity_items が user item を通知カードへ投影する。保存する item に同じ通知 metadata を複製しない。fork の固定履歴には投影後の通知カードを含め、handoff の historical message には通知を含めない。
- 2026-10-06: 委任の完了配送は親 run ごとの cohort とする。cancel はその配送の task ID だけを dispose し、親の停止は該当 attempt の未配送 cohort と queued wake を dispose/cancel する。完了済み task の status/result は停止で上書きしない。wake の本文・通知の文言は T3 と同じにする。
- 2026-10-06: checkpoint scope は workspace 準備結果として明示し、run 開始時の native baseline head と一緒に固定する。保存 effect に固定した scope/head を渡し、成功結果は欠けていた 0 / run-1 baseline と実行後 checkpoint を同じ step で fold する。現在の workspace scope が変わっても古い run の結果はその run の scope にだけ適用する。Git baseline の実体作成と共有 workspace の復元安全性は段階 3 の effect executor が担当する。
- 2026-10-06: Codex 0.156 の rollback は paginated history と `beforeTurnId` を使う。effect は保存済みの inclusive head を指定し、翻訳層が newest-first のページからその直後の境界を解決する。T3 の件数指定 helper を内部には持ち込まないため、ページ取得件数は同版 helper の最大ページサイズ 100 を使う。目的の head に既に戻っていれば revert を送らず、head 不明・cursor 循環では部分的な revert を行わない。
- 2026-10-06: native fork の context transfer は作成時の履歴を固定し、最初の dispatch で `NativeFork` の配送結果を記録する。native session を失った場合は同じ履歴を portable delivery に使う。fork marker は細かい項目開始・完了の事実として保存し、表示層は synthetic 行として扱う。
- 2026-10-06: Codex の reasoning は native item の開始だけでは行を作らない。非空の summary/content を独立した part index で翻訳し、空の native reasoning と異なる stream の結合を避ける。これは固定版の reasoning coalescer と同じ表示・順序になる。
- 2026-10-06: Claude の prompt echo 能力は native session ID ではなくプロセスごとの観測である。Host は新しいプロセスを開いたときに、instance と対象 attempt を明示した `RuntimeOpened` を actor に渡す。再開時は Unknown に戻し、同じ session の以前の echo 能力で `/compact` 前の wake を別 run に割り当てない。古い attempt の起動結果は無視する。
- 2026-10-06: 委任は子 actor の作成・元発言の開始をひとつの command とし、その発言 ID を親子の固定 anchor にする。子が元 run を確定したときだけ、親への結果と context transfer を送る。後の follow-up は元結果を上書きしない。task_status は元 task と子の確定 runs/items/messages、親の transfers から純粋に導出し、稼働・queue・最新 terminal result を別々に返す。app-owned の boolean は保存せず anchor の有無から導出する。
- 2026-10-06: interrupt の RPC 失敗だけでは run・子・background roster を終端化しない。停止要求は記録するが、子の terminal は子 actor の確認に従う。root の停止確認だけで子のカードを閉じず、子が生きている間は rollback を拒否する。プロセス終了は明示的な SessionClosed で確定し、古い root の停止後も該当する子の終了報告は受け付ける。再開前に止めた history injection は、unsupported fallback でも prompt を送らない。
- 2026-10-06: Claude CLI の tools / permission callback / thinking summaries / settings / MCP config / resume 境界は SDK 0.3.276 の制御手順に合わせる。追加 system prompt は initialize に渡す。プロセスの環境は明示的な map とし、SDK と同じ識別値・NODE_OPTIONS と DEBUG の処理を純粋に行う。再接続時は native task/tool/親経路の対応だけを actor の確定値から復元し、翻訳器の旧プロセス状態を引き継がない。
- 2026-10-06: provider handoff は model 選択時ではなく dispatch 時に固定する。queued run 自身の selection と、対象 native session が最後に受け取った completed/failed/interrupted run を基準に差分を選ぶ。queued/cancelled/rolled-back run は文脈に含めない。native session を失った場合は full history に戻し、他 instance の配送結果で履歴を消費しない。full と delta の戦略は TransferKind で区別する。
- 2026-10-06: Codex wire は approval/sandbox override、MCP の instructions/additionalContext/config、managed token session の serviceTier 抑止を明示的な入力で受ける。thread/start・resume・fork・rollback の再開は同じ config 構築を使い、turn/start は各 run の selection/runtime を使う。Host の session 管理は native の能力・認証を解決し、Claude の再利用時は set_model / set_permission_mode の reply を待ってから Start を実行する（段階 3）。
- 2026-10-06: 再起動で失われた provider background work は run の事実に記録する。通知は同じ instance の後続ターンに付け、completed attempt で配送を確認する。compaction、別 provider、rolled-back run は配送確認に使わない。ラベル 160 UTF-16 units・通知 10 件は固定版の上限を保持する。旧形式の ID 欠落への fallback は作らない。
- 2026-10-06: 自動継続の設定は Recover と ContinueRestart の明示的な入力にする。未完了の root turn と、開始前に再度中断された継続だけを対象にし、自己宛ての command effect で継続する。新しいユーザー発言、停止、maintenance、provider 変更、archive が先行した場合は採用しない。held queue があるだけでは新しいユーザー発言を queue に入れない。Host は保存した native binding の再開可能性と現在の設定を解決して Recover を入力し、継続 effect の実行時にも現在の設定を enabled として渡す（段階 3）。無効化されていた場合の委任の取消結果も状態機械が返す。
- 2026-10-06: background work の通知も portable context もない Codex の継続は input を空にして native turn を再開する。Claude は同じ継続文を通常の prompt として送る。未配送の通知は中断された継続の chain をたどって保持する。委任先の継続結果は元の依頼の anchor に返し、task_status の childRunId は元 run、latestTerminalRunId は継続 run を示す。復旧した native 子は owner を閉じて旧出力を拒否するが、provider が所有する子という関係は保持する。
- 2026-10-06: 段階末の全体検証で、既存 Host の音声入力 fixture が loopback の discovery 接続を provider の通信として数える問題を再現した。fixture は TLS と対象 HTTP route を識別してから既存のヘッダー・PCM・応答の期待値を検証する。無関係な GET を最初に入れて再発を確認し、既存の時間制限は変更しない。Host の製品処理には変更を加えず、cold-start テストの環境変数だけを製品名を含まない名前へ更新した。
- 2026-10-06: 段階 3 の前に domain / provider の peer review 指摘と Host が必要とする不足を直した。checkpoint は保存結果に ready / missing / error を持ち、Git でない workspace や保存の失敗でも run を確定してキューを進める。ready でない checkpoint は rollback 先にしない。rollback は provider の巻き戻し・ファイル復元・不要になった ref の削除を1つの effect で行い、結果も1つにする。後の checkpoint は stale にし、rollback した run の保存待ちを消す。queued run は rollback の対象にしない。巻き戻す provider は、境界より後に run がある instance だけにする（T3 は active な provider thread だけを巻き戻すが、本設計は instance ごとに native session を持つため）。
- 2026-10-06: native session の再開に失敗した Start（`ProviderFailed.session_lost`）と、同じ native thread への配送が不確かな状態の Start は、native session を捨てて新しい session と portable history で同じ run を続ける。T3 の resume fallback と uncertain delivery guard に相当する。provider が受け付けなかった失敗・中断の入力は、同じ native session への差分 handoff で渡す。telemetry がない場合の占有量推定は、受け付けた自分の履歴と配送済みの他 instance の履歴、到達した入力の添付分を数える。inline の文脈・restart note の後ろに `User message:` を付ける組み立ては `provider_prompt` に集め、Codex と Claude が同じ規則を使う。
- 2026-10-06: fork は provider が完了した run（completed / waiting）で、thread と同じ instance の場合だけ native fork にする。失敗・中断・取消の run は固定した portable history を使う。native fork が失敗しても子は作り、同じ固定履歴を portable context として渡す（T3 は fork を最初の発言まで遅らせるため失敗は run の失敗になるが、本設計は fork 時に子を作るので子を残す）。fork の継承履歴は表示中の項目だけを固定する。T3 は rollback 済みの source run の項目も継承に含めるが、表示と一致しないため採らない。merge back は queued 中の消費を拒否する T3 の制限を持たず、次の run の開始時に配送する。
- 2026-10-06: restart は旧 attempt を superseded にして run を starting に戻すだけで、native の子を終了扱いにしない。子のイベントは task が記録した attempt で子 actor に届ける。停止後に届いた Claude の result はラップされていても run を確定する。provider failure は同じ provider の queue を hold し、usage limit の失敗はキューの昇格と resume を止める。archive / settle / delete は T3 が取り消すものを取り消し、provider session の切り離しと terminal cleanup を effect にする。添付の削除は T3 と同じく自 thread の発言の添付を対象にし、他 thread の参照確認は行わない。
- 2026-10-06: 委任の完了配送は親 run の cohort として扱う。always の完了は実行中の親 turn に steer し、settled_only は生成元の run の終了だけを待つ。queued の wake があれば後続の兄弟はその wake に加わり、運んだ run の終了で delivered にする。native の task は再開ごとに generation を進め、古い generation の子の結果を捨てる。
- 2026-10-06: タイトル生成は title seed 付きの最初の発言、または maintenance だけの後の最初の発言で要求する。rename・archive・新しい要求が古い要求を無効にし、空・`New thread`・同じタイトルの結果は現在のタイトルを保つ。session の import は元の発言時刻を facts に残して settled の thread を作り、native session に bind する。thread の workspace は fact にし、fork・委任・native 子が引き継ぐ。
- 2026-10-06: fact の大きさは作成時に抑える。長いテキストは 1 MiB ごとの追記に分け、内容は切り詰めない。4 MiB を超える tool の JSON は大きさだけに置き換え、読める出力は項目のテキストに残す。failure の文言と code は T3 の UTF-16 の上限（4096 / 128）にする。
- 2026-10-06: Codex の turn 終了時に実行中の command は background work として残し、遅れた完了で行を更新して通知付きの wake を作る。Stop は終わった turn を interrupt せず terminal を止め、一覧で終了を確認する。Claude の background roster は置き換えとして扱い、roster から消えた作業の後着の報告も名前を保って通知する。usage limit の通知は待ち時間を入力の時刻から作るため、domain が文面を作る。
- 2026-10-06: replay harness は、翻訳層が送る frame がないときだけ記録から利用者の操作を作り、生成した frame を T3 の replay.ts と同じ正規化で次の `expect_outbound` と比較する。期待されない frame が残れば失敗にする。Claude は SDK の記録語彙（prompt.offer / query.interrupt / permission.response）に変換して比較し、query.open では thread が持つ session と境界での resume を確認する。fork / rollback / merge の graph replay は1つの記録プロセスに複数の native thread があり、本設計の thread ごとの session と JSON-RPC ID の空間が異なるため、従来どおり記録を種にして境界の command を検証する。
- 2026-10-06: Claude の forkSession は SDK 0.3.276 の transcript 変換を純粋関数として移植した。project directory の解決・読み書きと mode 0600 は Host が行う。fork 後のタイトルの推定は記録の customTitle / aiTitle と最初の prompt を使い、SDK の貼り付け展開は持ち込まない（Claude の session 一覧の表示にだけ使われる）。
- 2026-10-06: 段階 3 の Host は2つの前提を守る。attempt と effect の ID の元になる envelope key を thread をまたいで一意にする（`{thread}#{input_seq}`）。継続の effect は実行時に現在の設定で `enabled` を置き換える。
- 2026-10-06: Host ランタイムは `crates/agent-runtime` に置く。Host の I/O（プロセス起動、Git、worktree、添付）は trait で注入し、`cargo test -p agent-runtime` を Host の重い依存なしで回せるようにする。
- 2026-10-06: project は fact log の外にあるので、再開した shell 購読は最初に生きている project の完全な一覧（`ShellUpdate::Projects`）を送る。クライアントはそこにない project を消す。切断中の改名・追加・削除はこれで届く。
- 2026-10-06: effect の結果は、その outbox 行が同じ worker の lease で実行中のときだけ確定する。取り消された effect の結果は事実を残さない。T3 の executor のテスト（EffectWorker のうち handler の振る舞いを確かめるもの）は、各 handler を実装する段で移植する。
- 2026-10-06: provider の Start effect は frame を書き終えた時点で成功にする。後から届いた拒否は所有 attempt への `ProviderFailed` として入れ、`thread/resume` の失敗と、Claude の resume 起動で initialize の応答前に終了した場合は `session_lost` にする。生きた session がない Interrupt は `SessionClosed` で停止を完了させ、Steer は後続ターンにする。
- 2026-10-06: Claude のプロセスは、native session と起動時にしか決まらないフラグが一致するときだけ再利用し、model と permission mode は応答を待って揃える。session は実行中の run か未解決の request がある間は busy、background work か未終了の native task がある間は上限 4 時間まで保持する。
- 2026-10-06: 履歴の取り込みは T3 と同じく project ごとに 100 件の transcript を上限にし、登録済み project のルートと一致する cwd だけを取り込む。除外するディレクトリは綴りと実パスの両方で判定する。project を追加したときは Host がその project の取り込みを呼ぶ。
- 2026-10-06: 会話の wire は `agent-protocol` の `conversation` に agent-domain の型で定義し、旧 orchestration の型を使わない。dispatch は command ID で冪等な `Committed`（reply と global / thread sequence）を返し、クライアントはその sequence が thread の購読に届いてから結果を表示する。購読は応答の stream を開いたまま、thread は Snapshot / Facts / Synchronized を、shell は Snapshot / Projects / ThreadUpdated / ThreadRemoved / ProjectUpdated / ProjectRemoved / Synchronized を送る。1 step の facts が frame の上限を超えるときは順序を保って複数の Facts に分ける。Host 専用の command は protocol でも拒否する。旧 RPC は段階 4/5 で消すまで並存させ、Postcard の型が変わるので ALPN を streams/10 にする。
- 2026-10-06: Host の I/O は `agent-runtime` の `HostOperations` 1つに集める。対象は project と設定、実パス、Git の checkpoint ref、worktree と setup、添付、terminal、タイトル生成のモデル呼出し。Git の checkpoint 操作は trait に残し、host-daemon が既存の `checkpoints.rs` で実装する。ref 名は T3 の `checkpointRefForScopeOrdinal` と同じ形を runtime が決める。scope ID は thread と cwd から、checkpoint ID は scope と ordinal から導出する。
- 2026-10-06: checkpoint の baseline は T3 と同じく provider ターンの開始直前に取得する。`max(0, ordinal-1)` の ref がなければ作り、native head と一緒に `checkpoint_baselines` に記録する。保存 effect は欠けている 0 / run-1 の baseline を ref の有無で ready / missing にする。Git でない workspace は missing、保存の失敗は error として run を確定する。保存の後に Host へ workspace の更新を通知する（T3 の RunFinalization）。rollback 済みの run と、保存を待っていない run は保存しない。
- 2026-10-06: rollback effect は復元するファイルを先に置き、元のファイルを退避する。provider の巻き戻しが成功してから確定し、失敗したら元に戻す。T3 は provider の後に復元するが、外部の巻き戻しの後で復元が失敗して食い違う順序を避けるため採らない。復元の分離確認は T3 と同じく、worktree が scope と一致することと、他の生きている thread の worktree（ない場合は project root）・checkpoint scope・稼働中の provider プロセスの cwd と包含関係がないことで行う。session は thread ごとなので、複数 thread が共有する provider session の例外は持たない。session を作り直す instance には `NativeSessionReset` を入力する。最後の試行の失敗は T3 と同じ文言の `RollbackFailed` にする。
- 2026-10-06: launch は T3 と同じく最初の発言を常に `DeferStart` で受け付ける。`PrepareWorkspace` が worktree を作成するか記録済みの worktree を再利用し、workspace と checkpoint scope を bind し、project の setup を実行してから `effect:{id}` の `ReleasePrepared` を送る。失敗は `Workspace preparation failed during …` の `FailPrepared` にし、`RetryPrepared` は同じ effect を再発行する。thread と発言の ID は command ID から UUIDv5 で導出する。`launches` 表で同じ command の再送を T3 の規則で判定する（別 thread・別 project・削除済み・無関係な receipt は拒否）。発言のない launch は workspace の準備をバックグラウンドで行う。branch 名の生成と rename、scratch folder、setup の進捗表示と取消、非同期 setup、空 thread の再利用は持たない。
- 2026-10-06: `SendToThread` は `effect:{id}` で送り、`ContinueRestart` の enabled を実行時の設定で置き換える。最後の試行でも送れない継続は、`effect:{id}:declined` の `ContinueRestart { enabled: false }` で委任元に取消の結果を返す（T3 の recoverDelegatedTask）。
- 2026-10-06: タイトル生成は T3 の prompt、会話の要約、sanitize を純粋関数として移植し、モデル呼出しだけを Host に渡す。prompt にある製品名は一般的な表現に置き換えた。最初の発言のタイトルは outbox の再試行で最大 3 回、再生成は 1 回試し、失敗したら現在のタイトルを保つ。リンク先の要約は Host が任意で返す。
- 2026-10-06: 起動は、outbox の process-bound effect の取消、復旧が必要な thread への `Recover { Startup }`、effect worker、初回の import の順に行う。それまでクライアントの command は待たせる。停止は worker と provider プロセスを止めてから `Recover { Shutdown }` を入力し、process-bound effect を取り消す。T3 の prepareForShutdown と同じく、停止時にも実行中の root ターンの継続 effect を記録し、次の起動後に実行する。このため domain の `Recover` は trigger によらず同じ継続判定を行う。
- 2026-10-06: provider 層の不足を直した。拒否された `thread/resume` はその operation として報告し、session 側の補正を削除した。新しいプロセスでの Codex の compact は、保存済みの thread を resume してから送る。`ProtocolError::Remote` に native request ID を持たせ、応答待ちは ID で照合する。`ProviderOperation::SetRuntimeMode` を追加した。replay の正規化 helper は `test-support` feature で共有する。
- 2026-10-06: workspace は fact になったので `thread_workspaces` 表を削除した。添付の削除は他 thread の参照を確認しないので、`attachment_refs` 表も削除した。
- 2026-10-06: runtime の shell 行は domain の `ThreadShell` を型として持ち、project は `HostProject { id, name, root }` にした。SQLite の payload 列は `ThreadShell` の JSON のまま残し、検索と workspace の参照はその JSON を読む。Host 専用の command は agent-domain の `host_only_command` が全 variant を分類し、`Import` を含める。runtime の actor と protocol の `Dispatch::validate` が同じ関数を使う。
- 2026-10-06: Host の会話は `host-daemon` の `conversation` が新しい runtime で答える。`Dispatch::validate`、メッセージ添付の claim、actor の順に処理し、reply の `command-id-conflict` と `thread-not-found` を `CommandIdConflict` と `ThreadNotFound` にする。作成されていない thread の読み取り・購読も `ThreadNotFound` にする。launch の失敗は runtime の `LaunchFailure` で分類する。claim できない添付は新しいエラー `AttachmentUnavailable` にする。購読は最初の item を応答にし、再送が空で完了印も求められていなければ空の `Facts` を返す。facts は `fact_updates` で frame に収まるよう分け、Host 専用の文脈は runtime が配信前に除く。旧 `orchestration/*` RPC は `method_not_found` を返す。
- 2026-10-06: 添付の path はクライアントの値を使わず、Host の保管場所から決める。pending の upload は thread の保管場所へ複写して `chat:` の ID にする。削除は保管場所の中だけを対象にする。upload の manifest と `UploadedFile` は段階 4 まで旧 protocol の型を使う。
- 2026-10-06: provider のプロセスは thread と instance ごとに process supervisor の下で起動する。Codex は `codex app-server --listen stdio://` を共有 app-server と同じ `CODEX_HOME` で起動し、MCP server（`orchestration`、`browser`）を thread config の `mcp_servers` で渡す。Claude は `ClaudeLaunch` の引数と SDK と同じ環境を使い、選択中のアカウントの credential storage を `CLAUDE_SECURESTORAGE_CONFIG_DIR` に渡す。workspace のない thread は project の root で動く。session が閉じたら MCP の token を失効させる。
- 2026-10-06: turn diff は T3 の `CheckpointDiffQuery` に従い、完了した run の ready な checkpoint だけを対象にする。ordinal 0 は thread の最初の checkpoint scope の baseline ref にする。Git の diff は T3 と同じく `a/`・`b/` の prefix を固定し、patch は 10 MB で打ち切り、numstat は上限を超えたら失敗にする。checkpoint の保存は T3 と同じく ref を上書きし、既存の ref は runtime が保存前に確認する。
- 2026-10-06: 再起動後の自動継続（T3 `continueThreadsAfterServerUpdate`）は T3 の既定と同じく無効にする。Host に設定がないため常に無効になる。タイトル生成は T3 の既定の model（Codex `gpt-6-luna`、effort `low`）で `codex exec` を使い、Codex がない Host では Claude（`claude-haiku-4-5`）を使う。
- 2026-10-06: Host の起動は provider の有効化、`Conversation::open`、project の読み込み、`Runtime::start`（復旧・effect worker・初回 import）、MCP tool の受付の順に行い、endpoint はその後に command を受ける。停止は接続の終了後に runtime を止め（復旧の記録を含む）、browser と terminal を止める。project の登録は shell 購読に知らせ、その project の import を始める。
- 2026-10-06: MCP の agent tool は runtime の状態と client と同じ command（`Delegate`、`AcknowledgeTask`、`DisposeTask`、`SetTaskWake`、`Send`、`Interrupt`）で動く。`task_status` は domain の `delegated_task_status` から作る。domain の `Delegate` は親の mode を引き継ぐので、tool の mode 指定は親を超えないことの検証だけに使う。
- 2026-10-06: 製品名を含まない名前に揃えるため、chat の作業ディレクトリを `chats/`、project の一覧を `projects.json`、添付の保管場所を `attachments/` にした。既定の chat project の ID は `chats`。旧来の場所からの移行は行わない。
- 2026-10-06: 段階 3 の executor の peer review 指摘を直した。rollback は復元するファイルを置き、provider を巻き戻し、`RollbackFinished` を actor に確定してから元のファイルを破棄し、古い checkpoint ref を削除する。確定の後に失敗した試行は、再実行で `HostOperations::finish_restore` と ref の削除だけを続ける。確定の前の失敗は元のファイルを戻す。rollback が残した stale の ref は、次の run の baseline が削除してから取り直す（Host の capture は既存の ref を上書きしない）。provider を巻き戻す前に `RollbackRewindStarted` を記録し、rollback が失敗したらその instance の native session を捨てて portable history で続ける。ファイル復元は分離の確認から確定まで `WorkspaceFence` を持ち、launch・workspace 準備・fork と委任の子の作成による workspace の bind はその間待つ。
- 2026-10-06: fork の子は親の checkpoint scope を引き継がない。workspace 準備を通らずに始まる run（import、fork、委任の子）は、provider ターンの開始時に thread と cwd から自分の scope を得る（T3 の root run scope）。保存時の baseline は ready のものだけを省き、missing・error・stale は作り直す。`Recover` には outbox で保存が待機中か実行中の run を `capturing` として渡し、それ以外の waiting run は取り消す（T3 ProviderRuntimeRecoveryService）。保存の最後の試行は、記録済みの native head が読めなくても結果を返す。
- 2026-10-06: `RetryPrepared` は attempt のない failed run のうち、`workspace_preparation_failed` の失敗項目を持つものだけを受け付け、その項目を cancelled にする（T3 dispatchPreparedRunRetry）。準備の最後の試行で進捗を記録できなかった場合は `update thread` の `FailPrepared` にする。setup の完了は `launches.status = 'prepared'` に記録し、再試行では setup を再実行しない。`HostOperations::create_worktree` には thread ごとの冪等性を求める。worktree 作成と記録の間に process が止まっても、同じ要求で同じ checkout が返るようにするため。
- 2026-10-06: `launches` 行は create が受理されてから書く。行がある launch は必ず thread を持ち、create の receipt だけがある launch は再送で行を書いて続ける（T3 は受理された create から replay を判定する）。同じ command の競合では、create に勝った側の行と strategy を準備が使う。発言のない launch の workspace 準備は Runtime が所有する。停止時に止めて終了を待ち、次の起動で未完了の行から再開する。
- 2026-10-06: `SendToThread` は拒否された command を配送済みにしない。`AcceptFork` と `AcceptDelegation` の拒否は再試行しない失敗、`ContinueRestart` の拒否は再試行する失敗にする。それ以外の command の拒否は宛先の判断として確定する。最後まで届かない作成と継続は `EffectResult::ThreadCommandFailed` として送信元に返し、worker が outbox 行の確定と同じ commit で入力する（入力に失敗すれば再試行する）。委任は task を失敗にし、継続は委任元に取消の結果を返す。fork の子が作れなかった場合、親に残す状態はない。
- 2026-10-06: Runtime の起動は `Starting` を確保してから復旧する。停止は進行中の起動と、受け付け済みの client 操作（dispatch・launch・import）の終了を待つ。effect worker、launch の準備、初回 import は中断して終了を待ってから provider process を止め、`Recover { Shutdown }` を入力する。T3 の「停止の記録中に完了した run」の移植テストは、停止が provider を止めている間にターンを完了させて確かめる。
- 2026-10-06: terminal の後始末は T3 ResourceCleanupService と同じく thread ID で行う（`HostOperations::cleanup_terminals(thread)`、失敗は再試行）。質問回答の添付は `Runtime::dispatch` が実在を確かめ、ないものは path を空にして domain が `attachment-unavailable` で拒否する。receipt のある再送は確かめない。message 型の質問の後続発言は、添付の参照を加えた回答から作る。provider への書き込みが詰まった session の停止（指摘 7）は session の担当で直す。
- 2026-10-06: terminal は T3 と同じく thread が所有する。Host の後片付けは thread の terminal handle（`thread_terminal_handle`）で閉じる。クライアントが作業ディレクトリから作る handle は段階 4 で thread の handle に切り替える。それまで後片付けは何も閉じず、他の thread の terminal を閉じることはない。
- 2026-10-06（利用者の承認）: T3 と利用者に見える挙動を変えるのは次だけにする。初回の既存履歴の自動取り込み、rollback でファイルを先に置いて provider の巻き戻し後に確定する順序、最初の発言のない launch の準備を再起動後も続けること、データの置き場所と MCP サーバー名から製品名を外すこと。native fork の失敗は T3 と同じく run の失敗にし、provider を切り替えた後の rollback は T3 と同じ条件で拒否する。これ以外の T3 との差は T3 に戻す。今後、利用者に見える差を入れるときは先に承認を得る。
- 2026-10-06: provider session の review 指摘を直した。Start は spawn・initialize・設定の整合の後、turn を送る直前に attempt がまだ run の現在の attempt かを確かめる（T3 ProviderTurnStartService）。Codex で別の attempt の root turn が進行中なら、その終了を `reply_timeout` まで待ってから次の turn を送る（T3 interruptAndAwaitTerminal）。Claude の先行ターンの帰属は状態機械が持つので待たない。受信フレームは、返信なら request を送った attempt に、保持された background work の key ならそれを始めた attempt に、それ以外は現在の所有者に届ける。steer の拒否は request ID で発言に対応付ける。
- 2026-10-06: provider の stdin は session ごとの writer task が書く。`write_timeout`（30 秒）を超えるか失敗した書き込みは session を閉じる。出力の読み取りと Close は書き込みに待たされない。actor への commit の失敗は backoff 付きで 8 回まで再試行し、それでも確定できなければ session を失敗として閉じる。翻訳器だけが進んだまま続けない。プロセスの終了は、そのプロセスで動いたすべての attempt に `SessionClosed` で届け、所有者を最後にする。終了時に未解決の要求は T3 の release と同じく expired・not resumable にし、カードを failed にする。受信フレームは idle の期限を延ばさず、pin の上限は background の通信量に関係なく効く。
- 2026-10-06: Claude の Stop は interrupt の応答を `reply_timeout` まで待ってから必ずプロセスを閉じる（T3 ClaudeAdapterV2 は interrupt の後に query を閉じる）。result が来なくても `SessionClosed` で run とツールが終わり、settled な root の background shell もプロセスと一緒に終わる。set_model / set_permission_mode は応答を待ち、受け付けられた値だけをプロセスの値とする。CLI が init / status で報告した permission mode を記録し、次の prompt の前に thread の mode に戻す。起動フラグの変更でプロセスを作り直す必要があっても、同じ native session の background work が動いている間は T3 と同じ文言で拒否する。skills は Start と Steer のたびに読み直す。
- 2026-10-06: Codex の `thread/resume` が archived で拒否されたら、`thread/unarchive` の後に一度だけ resume し直す（T3 CodexAdapterV2）。native thread のない compact は `thread/start` の後に送る。新しい Claude session は prompt を送る前に `SessionBound` を確定する。Claude の native fork は transcript を書く前に、親が `ForkSessionReserved` で fork 先の session を確定する。import の所有者判定はこの事実も見るので、Host が作った session を import が先に取り込まない。

# BEX プロトコルと native 契約の照合

確認日：2026-09-30。対象 HEAD：`f32e5100` と、この作業ツリーの実装差分。
設計の判断は [`BEX_PROTOCOL_DESIGN.md`](BEX_PROTOCOL_DESIGN.md) を参照する。
native の照合根拠と、実装後の検証範囲を記録する。認証済み native CLI での推論実行とは区別する。

## 1. 確認に使ったもの

- ローカルの `codex-cli 0.139.0` が生成した experimental を含む JSON Schema。
- 確認に使った Claude Code は `2.1.281`。制御入力について BEX の adapter と
  [Anthropic の SDK 型](https://github.com/anthropics/claude-agent-sdk-python/blob/main/src/claude_agent_sdk/types.py)・
  [利用者入力の公式資料](https://code.claude.com/docs/en/agent-sdk/user-input) を照合した。
- Python SDK にない制御 subtype もあるため、Anthropic の
  [TypeScript SDK 0.3.220 の公開型](https://app.unpkg.com/@anthropic-ai/claude-agent-sdk@0.3.220/files/sdk.d.ts) も確認した。
- elicitation は [MCP 2025-11-25 の仕様](https://modelcontextprotocol.io/specification/2025-11-25/client/elicitation) と生成スキーマを照合した。
- BEX の Codex/Claude adapter、JSONL writer、Router、core の投影、iOS の要求表示、native fixture を確認した。

Codex の schema を生成する再現コマンド：

```sh
codex app-server generate-json-schema --experimental --out /tmp/bex-protocol-audit-codex-0.139.0
```

生成ファイルは作業ツリーへ追加していない。主要ファイルの SHA-256：

| ファイル | SHA-256 |
| --- | --- |
| ServerRequest.json | `664be1b545a48580e57c7c480f8e4f3c2818fc3eb3822111c90b2e46bd21822c` |
| v2/ThreadReadResponse.json | `8f0de659960c66fe0a3900cfe4fc8d9cee2ddf4a18159615aba3800f14261a91` |
| v2/ReasoningTextDeltaNotification.json | `e2e7aea35c4d95b4ad8eeafe15629113f69ebfc2be085cd4316ae02871b5b110` |
| v2/ReasoningSummaryTextDeltaNotification.json | `7bd24d5d1cad5b09f6d3fe7a40de6a2d0c3218678bad6199e704c4696597325b` |

native バージョンを固定していることの証明ではない。adapter の対象とした契約を記録している。
Claude の SDK 資料は CLI の当該バージョンから生成した schema ではないため、
現在利用していない提案権限・追加 subtype を有効化する前には native 境界テストが必要である。

## 2. Codex の要求と回答

生成した ServerRequest には十種類があった。利用者への要求と Host の内部サービスを分ける。

| native メソッド | BEX の扱い | 必須の保持・回答対応 |
| --- | --- | --- |
| item/commandExecution/requestApproval | Approval(Command) | thread/turn/item、command/cwd/reason、選択肢、追加権限・ルール変更の内容 |
| item/fileChange/requestApproval | Approval(FileChange) | thread/turn/item、reason、grantRoot。native 契約の四 decision を adapter が生成 |
| item/permissions/requestApproval | Permission | thread/turn/item、cwd、要求権限、許可範囲。回答は permissions と turn/session scope |
| item/tool/requestUserInput | Question | thread/turn/item、質問 ID、header、question、options、isOther、isSecret。native 回答は ID → answers の配列 |
| mcpServer/elicitation/request | Elicitation | thread、任意の turn、serverName、form/URL 本文。action と form content を戻す |
| item/tool/call | ToolExecution | thread/turn、callId、tool、任意の namespace、arguments。回答は success と text/image の列 |
| account/chatgptAuthTokens/refresh | Host の account owner | 現行コードも会話要求の前に分岐している。token を公開要求へ入れない |
| attestation/generate | adapter の未対応応答 | 現行 BEX に担当サービスはない。承認 UI や汎用 JSON 回答へ流さない |
| applyPatchApproval | adapter の未対応応答 | 現行 BEX の v2 呼出しでは採用しない。旧 API を共通契約に増やさない |
| execCommandApproval | adapter の未対応応答 | 同上 |

### 選択肢の具体化

Command approval は四つの文字列 decision に加え、execpolicy の変更と network policy の変更を持つ。
単に「このセッションで承認」と表示すると、永続ルールの変更を誤って説明する。
ChoiceId に対応する native 値は Host 内で保持し、説明には変更対象・allow/deny・保持範囲を含める。
セッションのキャッシュ、ターンだけの権限、永続ルールは別の効果である。

`availableDecisions` は nullable。省略時の補完は Codex adapter の契約として行う。
FileChange の現在の schema には availableDecisions がなく、decision は四種類である。
Permissions の範囲は turn と session。初期 UI で turn のみを提供する場合も、BEX の契約は範囲を捨てない。
未知の権限を既知のネットワーク・ファイル権限へ縮約して許可しない。説明できる allow choice のみ発行する。

`startedAtMs` は approval/permission の native 必須値だが、BEX の回答を決める値ではない。
必要な表示時刻として保存しても、選択肢や RequestId の同一性には使わない。

### 会話単位の elicitation

MCP 要求の turnId は nullable である。Router は RequestTarget::Session を発行元の生存で検査し、
任意の turn が実行中であることを要求しない。
URL の elicitationId と native の JSON-RPC request ID と BEX の RequestId はそれぞれ別物である。
native 側の相関情報は adapter の回答対応に保持する。

## 3. Claude の制御入力

現在の `can_use_tool` は tool_name、input、tool_use_id と native request_id を受け取る。
AskUserQuestion 以外は Approval にする。追加権限提案を伴う場合でも、
任意の native permission 更新をクライアントが作る契約にはしない。

| native 入力 | BEX の扱い | native 回答 |
| --- | --- | --- |
| Bash / Write / Edit / NotebookEdit | 対象が型付きの Approval | allow は元の input を updatedInput に戻す。deny は明示的な拒否 |
| その他のツール | Approval(Tool) | ツール名と引数を保持。native が提供しない許可範囲を補わない |
| AskUserQuestion | Question | 公開質問 ID を元の question 文へ、選択肢 ID を元の label へ戻す。原 questions を保持 |
| elicitation | Elicitation | mcp_server_name、mode、requested_schema または URL、相関 ID。MCP の回答型に戻す |
| request_user_dialog | 未対応 kind は native の cancelled | 現行 BEX は supportedDialogKinds を宣言していない。汎用 Raw 回答を追加しない |
| control_cancel_request | 同じ発行元の要求を閉じる | 新たな利用者回答は送らない |

AskUserQuestion の multiSelect と自由入力を保持する。
同じ質問文・選択肢ラベルを BEX の ID として使わない。
native が質問文で回答を識別するため、同一要求内で質問文が衝突した場合は、
native が区別できると確認できない限り曖昧な回答を送らない。
SDK 資料上は複数選択を配列でも返せる。現行 adapter の comma join は native 境界の書式であり、
BEX 内の回答は選択肢の列を維持する。

現在使っていない権限提案の「記憶」を公開する場合は、元の提案と destination を保存して
選択肢から戻す。今回の中立化だけで新たに有効化した扱いにはしない。

Worker は elicitation を中立要求へ写し、未対応 dialog は native の cancelled を返す。
未知の制御入力を汎用の利用者回答へ流さない。
initialize 応答に未解決要求が含まれる接続方式を将来利用する場合は、
同じ発行元/request_id の再通知と合わせて一回だけ登録する。今回の fresh process 起動と同一視しない。

## 4. Item のフィールド写像

Codex の ThreadItem schema は十六種類。専用表示の補助 item は adapter が追加している。

| native item | BEX 本文 | 保持する主な値 |
| --- | --- | --- |
| userMessage | UserMessage | content、clientId、text の要素・添付参照 |
| agentMessage | AssistantText | text、phase、必要な引用情報 |
| hookPrompt | HookPrompt | fragments。通常の利用者送信と混同しない |
| reasoning | Reasoning | content と summary のそれぞれの part |
| plan | Plan | text |
| commandExecution | CommandExecution | command、cwd、commandActions、output、exitCode、source、必要な process 相関・duration |
| fileChange | FileChange | path、add/delete/update、move_path、diff、status |
| mcpToolCall | ToolCall | server/tool/arguments、status、result/error、duration、必要な app resource 参照 |
| dynamicToolCall | ToolCall | tool/namespace/arguments、status、success、contentItems、duration |
| collabAgentToolCall | Subagent | tool、送信元・宛先会話、各 agent 状態、prompt/model/effort |
| webSearch | WebSearch | query、action |
| imageView | ImageView | 画像参照。native のパスをクライアントのローカルパスと扱わない |
| imageGeneration | ImageGeneration | status、result、savedPath、必要な revisedPrompt |
| enteredReviewMode / exitedReviewMode | Review | 開始/終了と review の内容 |
| contextCompaction | ContextCompaction | 圧縮が行われたこと |

現行表示はさらに automaticApprovalReview、nativeAttachment、subAgentActivity、sleep を扱う。
自動承認確認の status、hook の成功/失敗、subagent の関連履歴参照、隠す行の方針を保持する。
nativeAttachment は全てを固定した閉じた enum に展開せず、専用の hook 等を写像し、残りを Custom に保持する。

Claude では live と history が同じ content_item を利用している。
Bash/Write/Edit/NotebookEdit の新しい写像も両方で同じ純粋な変換関数を使う。
tool_result は tool_use_id で同じ item を更新する。
is_error と content、persistedOutputPath、agentId は、型を変えても必要な結果・遅延参照として残す。

| Claude 入力 | 本文に保持する値 | 推測してはいけないもの |
| --- | --- | --- |
| Bash | command、実行先 cwd、出力、結果の成否 | content から推測した exitCode |
| Write | file_path、提案 content、実行結果 | 元のファイルを確認しない add/update の断定 |
| Edit | file_path、old_string/new_string、replace_all、実行結果 | 実際の行番号・適用箇所数を推測した diff |
| NotebookEdit | notebook_path、cell の指定、source、edit_mode、実行結果 | notebook 全体の変更 diff の捏造 |
| 未知の block | 種類と元の値を Custom に保持 | 無言の破棄 |

結果から確定できない変更は、提案と実行結果を区別して表示する。
FileChange が unified diff を必須とする設計にはしない。
差分を読めない時でもパスと分かる変更内容を失わず、既存の詳細取得の導線を維持する。

## 5. 状態と差分

Codex の turn は inProgress/completed/failed/interrupted。
commandExecution と fileChange はさらに declined を持つ。
MCP/dynamic/subagent の status に declined はないが、共通 ItemStatus は拒否を表せる必要がある。
ユーザー・assistant・reasoning 等は native 本文に status を持たない。
開始/完了通知の証拠がなければ Unknown を使い、単にフィールドがないことを実行中と解釈しない。

ThreadStatus の activeFlags は waitingOnApproval/waitingOnUserInput。
Host が持つ要求から分かる待機状態は core が計算する。
要求本文が届いていない時の provider の待機証拠は別に保持し、架空の回答ボタンを生成しない。
notLoaded は adapter の resume 判断で必要だが、通信切断や履歴欠落と同じ状態にはしない。

| native delta | BEX の対象 | 必要な制約 |
| --- | --- | --- |
| agentMessage/delta | AssistantText | 本文 variant が一致 |
| reasoning/textDelta | ReasoningContent(contentIndex) | 非負の有効な part 添字 |
| reasoning/summaryTextDelta | ReasoningSummary(summaryIndex) | 本文と要約を混ぜない |
| reasoning/summaryPartAdded | ReasoningSummary の part 追加 | native の指定した part を用意し、既存 part を消さない |
| commandExecution/outputDelta | CommandOutput | exitCode や command へ書き込まない |
| fileChange/outputDelta | FileOutput | diff や変更パスへ書き込まない |

adapter は native の part を作るイベントを正規化してから append を発行する。
上限のない添字へ resize せず、異常な添字は typed error と再取得の対象にする。
前案の単一 Reasoning フィールドでは、この区別が消えるため修正した。

## 6. エラーと再試行の写像

表示契約は原案の八分類より細かい。現在の説明を保つため、中立型にも個別の分類を残す。

| native の根拠 | BEX の分類・証拠 |
| --- | --- |
| contextWindowExceeded / sessionBudgetExceeded | ContextLimit / SessionLimit |
| usageLimitExceeded | UsageLimit |
| serverOverloaded | Overloaded |
| unauthorized / authentication_failed | Auth |
| cyberPolicy / misalignmentPolicyViolation | Policy |
| badRequest / invalid_request | InvalidInput |
| sandboxError / threadRollbackFailed / activeTurnNotSteerable | Sandbox / Rollback / InputUnavailable |
| internalServerError / server_error | Internal。混雑の追加証拠がなければ Overloaded と断定しない |
| HTTP/stream 接続・切断・再試行上限 | Network。willRetry と再接続の進行を別に保持 |
| Claude rate_limit と、上限を特定できない 429 | RateLimited。利用期間の上限が確認できる場合のみ UsageLimit |
| Claude billing_error / 未分類のエラー | Other に識別子と説明。料金問題を利用上限だと推測しない |

willRetry は自動再送の許可ではなく、provider の再接続の観測である。
再試行中の HTTP 429/混雑は既存契約どおり混雑・再接続として表示する。
通常のエラー分類とは別に、再試行の理由・回数・上限・追加説明を渡す。
エラーでない利用上限の警告は、会話を Failed にしない。
Claude Worker は result の構造を adapter 内で分類し、ExecutionError を返す。
Claude の system/api_retry には attempt、max_retries、retry_delay_ms、error_status がある。
この subtype を中立の再試行状態へ写し、HTTP status がない接続失敗も扱う。
これらの native 識別子は adapter に閉じ、core は BEX の分類と再試行の証拠だけを見る。

## 7. 回答配送の証拠

確認した実装は
`agent-transport/src/peer.rs` の enqueue/write_loop、同 jsonl.rs の write_line、
`host-daemon/src/claude.rs` の respond/Worker、同 claude/process.rs の write、
`host-daemon/src/host_rpc/service.rs` の answer_request。

Codex の send_raw 成功は JSONL の全体・改行を書いて flush が終わった証拠。
Claude の respond receipt も Worker が process.write を完了した証拠。
どちらにも回答を受理した ACK という意味はない。
Claude も receipt 後は Sent を保ち、tool の実行・結果や対象の終了を解決の証拠として扱う。

| 失敗地点・観測 | 証拠 | BEX の結果 |
| --- | --- | --- |
| 検証・encode・宛先選択で拒否、まだ送信処理を始めていない | NotSent | 有効な要求を保持する |
| queue への enqueue が失敗して command が渡っていない | NotSent | 有効なら Awaiting に戻す |
| queue に渡した後のタイムアウト・caller cancellation | 書込みが進んでいる可能性 | Unknown。再 enqueue しない |
| write/flush が失敗または writer cancellation | 一部書込みの可能性 | Unknown。0 byte と証明できる API がなければ未送信扱いにしない |
| 完全な write/flush が成功 | 完全な書込み | Sent。再回答不能、解決待ち |
| Codex serverRequest/resolved | native 解決 | 要求を閉じる。成功実行したことまでは推測しない |
| Claude tool 実行/結果、control cancellation、対象の終了 | 要求の継続が不要または不可能 | 同じ要求を閉じる |

send_raw/respond が未送信の証拠を返せない失敗は Unknown に倒す。
診断文字列の文言や、caller のタイムアウトだけを証拠に NotSent を返さない。
native I/O を途中で取り消しても、キューに残った command は送られる場合がある。
claim を消して別端末の再送を許すと重複実行になる。

### 入力 echo の相関

Claude の初回・追加送信とも、BEX 発行の ClientInputId を user.uuid に渡す。
Host の user item と native 履歴の ID を一致させ、live overlay を失っても照合できる。
Codex は同じ値を clientUserMessageId に渡す。
Host に新しい永続対応 DB を作らず、クライアントに残る pending の ID と履歴で照合できるようにする。
履歴に一致がないことだけでは未送信を証明できないため、その場合は Unknown のままにする。
native start/steer/queue は Host 私有操作とし、公開する SubmissionReceipt の TurnId と入力 ID を分ける。

## 8. fixture と native の差

fixture は表示・操作の回帰を検証する材料であり、native の現行 schema そのものではない。
今回の移行では次を修正した。

- command の custom decision と permission の fileSystem/network.enabled を native 契約に合わせた。
  BEX の typed Answer のテストと、native 回答の境界テストを分けた。
- iOS・Android・desktop は五種類の RequestBody を扱い、未分類要求への Raw 回答を削除した。
  JSON 編集は intrinsically open な form 値・tool result に限り、core で型付き Answer を生成・検証する。
- reasoning の contentIndex/summaryIndex を保持し、不正な差分は状態を変更せず再取得する。
- Claude の未知 block を Custom に保持し、FileChange の提案・実行結果を区別した。
- Claude の native uuid と ClientInputId を揃え、live overlay なしの履歴取得と Host 再起動後の照合を検証した。
- Swift/Kotlin の保存・一覧・会話テストを、構造化 SessionRef と ItemContent に移した。

表示の受け入れ条件は変更していない。fixture の schema 修正によって UI の振る舞いを緩めない。

## 9. 実装後の検証範囲

Nix のプロジェクト環境で次を実行した。通常テストに proptest を含む。

```sh
cargo test --locked --no-fail-fast \
  -p agent-protocol -p agent-transport -p agent-core -p host-daemon \
  -p host-fixture -p bex-desktop -p agent-cli -p codex-app-server \
  --features agent-core/bindings
cargo clippy --locked --workspace --all-targets --features agent-core/bindings -- --no-deps -D warnings
cargo fmt --all --check
```

全体テストは成功した。主な内訳は core の unit 103、Store 47、desktop 38、Host の unit 92、
Claude 境界 16、Codex account 3、iroh Host 境界 34。外部サービス・認証を必要とする ignored は実行していない。
最後の整理後も Host/fixture の再実行は成功した。追加した履歴エラーの回帰を含め、iroh Host 境界は 35 passed、1 ignored。
core の要求索引を削除した追加リファクタリング後も、core・Store・desktop・Host・fixture・CLI の
通常・結合テストを再実行し、全て成功した。要求の表示・回答・切断・復元の受け入れ条件は維持した。

検証には以下を含む。

- 同じ native ID を持つ provider の会話・下書き・購読・モデル選択の分離、構造化保存・復元。
- typed Answer の variant/ChoiceId/schema、二端末 claim、Session 対象 elicitation、発行元 ID の再利用。
- Sent/Unknown と送信タスク cancellation。native エラーの delivery 値を未送信の証拠にしないこと。
- indexed reasoning delta、不正差分からの再取得、拒否状態、Claude の提案と実行結果、未知 block の保持。
- native 操作を Host 内に閉じた Submit、追加送信、resume、入力 echo、再起動後の履歴照合。
- 汎用 provider 呼出しの拒否、共通入力の native 変換、typed RpcFailure。
- 明示的な「最初の入力がまだない」エラーだけを空の履歴へ写し、他の失敗・取得不能と区別すること。

focused cargo-mutants の結果：

| 対象 | caught | missed | unviable | timeout |
| --- | ---: | ---: | ---: | ---: |
| SessionRef::validate | 7 | 0 | 0 | 0 |
| typed Answer / form / native 回答境界 | 47 | 0 | 2 | 0 |
| reasoning append_part / Host submission_target | 8 | 0 | 1 | 0 |
| native の空履歴判定と owner の分岐 | 7 | 0 | 0 | 0 |
| repeated-turn の履歴展開（補足監査） | 1 | 0 | 0 | 0 |

native 履歴のエラー分類は、純粋な関数だけを選んだ監査で owner 経路のテスト不足が見つかった。
Host を通る回帰テストを追加し、結果に使われない私有 Thread の ID を除去した。
追加の owner 経路監査では、判定関数と history の guard の七 mutation を全て検出した。
ツールが選んだ八 mutation のうち一つは対象外の repeated-turn の hydration だった。
[struct field の削除が regex filter を無視する既知の問題](https://github.com/sourcefrog/cargo-mutants/issues/632) に該当し、
その一件を既存の repeated-turn 履歴回帰で別途監査し、1 caught、0 missed を確認した。
最初の実行結果は 7 caught、1 missed として保持する。補足監査を合わせ、選ばれた八 mutation は全て検出した。

Host・desktop・CLI、Swift/iOS、Kotlin/Android のビルドは成功した。
Swift の format/lint と Kotlin の format/detekt も成功した。
iOS は Codex の会話・承認・再接続・追加送信・履歴・モデル・account/fork の十二 UI ケースと、
Codex が利用できない状態での Claude 単独・モデル変更・履歴復元の一ケースを検証した。
承認ボタンの accessibility ID 重複を修正した後の再実行を含め、十三ケースは全て成功した。
Android API 37 の隔離エミュレータで、ネットワーク許可・保存・再接続・Markdown・会話移動・一覧・
可視化・端末操作の十一ケースが全て成功した。
一覧の表示キーを Android の保存可能な値へ変換し、同じ native ID の Codex/Claude 会話を
独立に表示・選択できることも検証した。操作と保存は構造化 SessionRef を使う。

認証済み Codex/Claude CLI による実際の推論、physical device、公開 relay の soak は今回実行していない。
稼働中の Host は再起動せず、UI 検証にはこの作業ツリーからビルドした隔離 fixture を使った。
「全 native 機能の実行が検証済み」という意味ではない。

# BEX の provider 中立プロトコル

2026-09-30。PR #27 の方針を、現行コードの操作・表示・配送の責務に照らして具体化し、実装した設計。
対象は Codex と Claude、および既存の BEX クライアントである。実装と検証範囲は末尾に記録する。
native スキーマとコードの照合結果は [`BEX_PROTOCOL_NATIVE_CONTRACTS.md`](BEX_PROTOCOL_NATIVE_CONTRACTS.md) に記録する。

## 1. 結論

BEX が定義するのは、会話の参照、入力の送信、実行の観測、利用者への要求と回答の契約である。
provider の SDK 全体を共通化する必要はない。

責務は次の三つに固定する。

| 所有者 | 責務 |
| --- | --- |
| provider adapter | native の履歴・通知・要求を BEX の値に変換する。BEX の回答を native の回答に変換する |
| Host | 実行、未解決要求、配送証拠、購読を所有する。回答を排他的に claim し、I/O を行う |
| agent-core | BEX の値から次のクライアント状態と表示を計算する。各クライアントはその結果を描画する |

adapter が Codex の JSON を作ってから共通型へ読み直す経路は廃止する。
変換関数は必要な入力値を受け取り、変換結果を返す。変換のために Router や Store を渡さない。
Host と core は同じ純粋な更新関数を利用し、それぞれ自分の状態へ結果を適用する。

native history は履歴の正本とする。Host に会話 DB、永続イベントログ、replay 基盤は追加しない。
実行中の情報と配送証拠は native history だけでは判断できないため、Host が所有する。

## 2. ID は構造を保つ

```rust
struct SessionRef { provider: ProviderKind, id: String }
enum DraftKey { Session(SessionRef), Local(String) }
```

SessionRef は Rust、Postcard、UniFFI、Swift、Kotlin のすべてで構造を保つ。
prefix、既定の Codex、JSON を詰めた String、同じ会話の別形式の ID は使わない。
CLI の入力・出力、ログ、UI フレームワークの保存可能な表示キーに文字列が必要な場合だけ境界で書式化する。
表示キーから provider を再解釈せず、操作・保存には元の SessionRef を使う。
構造化キーを持つ JSON の保存形式は `[key, value]` の列にする。旧形式の移行経路は作らない。

TurnId、ItemId、RequestId、ClientInputId は取り違えを防ぐ newtype とする。
native のターンと item の ID は opaque な値として保持し、アクセス時には SessionRef と組にする。
グローバルな item lookup が必要なら `(SessionRef, TurnId, ItemId)` を使う。
ID に provider や会話を再度埋め込まない。

SessionRef のスコープは接続先 Host の保存領域である。
複数 Host のデータを一つの map に混ぜる場合は、その所有者が既存の接続先スコープと組にする。
接続先の切替で隔離される map にまで HostId を追加しない。

native ID に含まれる `claude:` 等はそのまま値として扱う。
不正な参照は外部入力を受ける境界で拒否する。内部の型変換ごとに stringify/parse を挟まない。

## 3. 操作は利用者の意図に合わせる

会話の操作は Create、List、Open、ReadItem、Submit、Interrupt、Fork、Rename とする。
名前は `host/session/...` に統一する。モデル・アカウント・ファイル・端末・ブラウザの独立した操作は残す。

- Create は provider を必須にする。既存の会話への操作では SessionRef が provider を決める。
- Submit は SessionRef、ClientInputId、入力、ターンの設定を受け取る。
- start、resume、steer、queue の選択は Host が実際の実行状態から判断する。
- provider とモデルの所属が一致しない指定や、実行中に対応しない操作は明示的に拒否する。
- 未登録メソッドの汎用 Provider 呼び出し・通知は公開契約から削除する。

モデル一覧と選択は `ModelRef { provider, id }` を使う。同じ native モデル ID を両 provider が返しても、
選択・既定値・推論設定は別々に扱う。`claude:` 等の文字列を解釈して provider を決めない。
Codex が返す推論バックエンドの `modelProvider` と、BEX の ProviderKind は別の値である。

操作できるかどうかを core が一か所で計算し、UI はその結果を使う。
Host は操作時に再検査する。古い UI の capabilities は操作の許可証にならない。
capabilities は実装されている操作の差だけを表し、provider の全機能一覧にはしない。

ClientInputId は入力送信の相関 ID であり、provider に存在しない exactly-once 保証にはしない。
この ID は BEX が発行する UUID とし、Codex の clientUserMessageId と Claude の user.uuid に同じ値を渡す。
native 入力のために別の UUID を生成しない。これにより履歴の echo からも元の ClientInputId を復元できる。
任意の provider の UUID を利用者送信として解釈せず、pending 入力との一致を照合する。
同じ ID の重複送信は Host が持つ実行・配送証拠の範囲で抑止する。
Sending、Accepted、Rejected、Unknown は配送の証拠であり、ターンの実行状態とは別である。
Accepted は Host が入力を受け付け、native の送信処理を完了したことを表す。
provider が生成を開始した、成功した、履歴へ永続化したという意味を付けない。
配送証拠の退役は、完了と入力 echo の照合に基づいて行う。
Unknown をタイムアウトだけで Rejected に変えたり、自動で入力を再送したりしない。
Host 再起動後に判定できない送信は、履歴の入力 echo で照合し、照合できなければ不明を表示する。

## 4. 要求は共通の外枠と、対応する本文・回答にする

```rust
struct Request {
    id: RequestId,
    target: RequestTarget,
    delivery: RequestDelivery,
    body: RequestBody,
}
enum RequestTarget {
    Session,
    Turn { turn_id: TurnId, item_id: Option<ItemId> },
}
```

Request は会話の snapshot/update 内に置き、SessionRef はその外側で一度だけ持つ。
turn/item は adapter が分かる場合に指定する。分からない ID は捏造せず、core は会話単位の要求として表示する。
Session 対象の elicitation は実行中 turn を必須にしない。発行元との有効な接続を所有しているかで回答可能性を検査する。
Turn 対象では、その turn と発行元が有効であることを検査する。別の実行中 turn を根拠に回答を許可しない。

| RequestBody | 回答の型 | 境界 |
| --- | --- | --- |
| Approval | ChoiceId | コマンド・ファイル変更・ツール操作を許可する |
| Permission | ChoiceId | 明示された権限と範囲の選択肢から選ぶ |
| Question | 質問 ID ごとの回答 | 自由入力・単一選択・複数選択を区別する |
| Elicitation | Accept の値、Decline、Cancel | form と URL の要求を区別する |
| ToolExecution | 成否と text/image の結果 | 既存の dynamic tool call に対応する |

原案の四種類だけでは既存の `item/tool/call` が欠落するため、ToolExecution を明示する。
これは任意の native メソッドを通す口にはしない。Host 内で実行するツールは Host が処理し、
現行 UI が回答するツールは対応する型で公開する。要求の名前だけを見て勝手に自動実行へ変更しない。

Approval と Permission は Host が発行した opaque な ChoiceId を回答に使う。
添字、native の decision JSON、単純な allow bool は送らない。
選択肢には表示用の説明と中立の意味を持たせる。意味は表示に使い、回答は ID で識別する。
同じ「許可」でも付与する権限や期間が違う選択肢は別 ID にする。
native の追加権限・ルール変更も、内容を説明できる明示的な選択肢として出す。
存在しない「セッション中は許可」を共通の既定値で補わない。
native が選択肢一覧を省略する場合は、その要求の native 契約に定義された選択肢を adapter が補う。
共通層で全 provider に同じ既定値を与えない。

Question の ID と選択肢 ID は要求内で一意にする。表示文言を公開 ID として使わない。
回答は FreeText / SingleChoice / MultipleChoices とし、秘密入力か、自由入力を許すかを保持する。
adapter は ID を native の質問 ID・質問文・選択肢ラベルへ戻す。複数選択を一つの文字列として core に保存しない。

Elicitation の form は、BEX が描画・検証できるフィールド型へ adapter が変換する。
対応できない schema を無検証の入力フォームへ落とさない。URL 方式は URL と確認操作を別に扱う。
対応 schema は文字列、数値/整数、bool、単一選択、複数選択の平坦なフィールドとする。
必須、文字数、形式、数値範囲、選択数、既定値、選択肢の値と表示名を保持し、Host 側でも検査する。
表示名付き enum の native 表記の違いは adapter で吸収する。任意の入れ子 JSON Schema エンジンは追加しない。
利用者の入力 JSON が必要な場合でも、その値を受け取れる variant だけで許可する。汎用 Raw 回答は削除する。

native server request をすべて利用者への要求とみなさない。
認証 token refresh 等は Host の担当サービスへ渡し、token を会話 snapshot やログへ出さない。
Claude の elicitation は同じ Elicitation へ写す。
request_user_dialog は未対応 kind を native の cancelled で返し、未実装の kind を capabilities で宣言しない。
未知の要求は承認不能とする。adapter が native の失敗・拒否応答を返し、BEX には診断を残す。
未知の item を表示できることと、未知の要求を実行してよいことは別の契約である。

## 5. 未解決要求は一か所で所有する

Host の実行所有者が RequestId ごとに、公開 Request と adapter 用の回答対応を一緒に持つ。
native request ID、送信先、元のツール入力、ChoiceId と native 値の対応は非公開である。
公開 snapshot はこの状態の投影であり、別々に更新する公開要求 map と native map を正本にしない。
provider 側の pending map にも同じ要求の寿命管理を重複させない。

native request の同一性は provider 名と番号だけでは足りない。
発行元の実行インスタンスと native ID の組で照合する。
別プロセスの ID 再利用や、終了した実行からの遅延通知で別の要求を解決しない。

回答の処理順は次に固定する。

1. 接続の認証・権限、要求の存在、実行の生存、回答と要求の型、選択肢や入力制約を検査する。
2. 所有者の排他的な更新内で Awaiting から Sending へ一度だけ claim する。
3. ロックを外して native I/O を行う。I/O を await したまま状態ロックを保持しない。
4. 完全な write/flush が確認できたら Sent にする。これは native の受理を表さない。
5. native の解決通知、対象ツールの実行・結果、要求の cancellation または対象寿命の終了で要求を閉じる。

RequestDelivery は Awaiting / Sending / Sent / Unknown の四つとする。
Sent は送信済みで再回答不能。core の回答待ち件数には含めず、native の解決まで Host に対応情報を保持する。
Unknown も再回答不能だが、Sent と違い完全な書込みの証拠を持たない。
別途 native の受理 ACK を捏造したり、ACK がない provider に待機タイムアウトを追加したりしない。

| 結果 | 状態と動作 |
| --- | --- |
| 不正回答・期限切れ・別 variant | 拒否。未解決の有効な要求を消費しない |
| 複数端末の同時回答 | 一つだけ claim。他の回答は拒否 |
| native へ未送信だと証明できる失敗 | 同じ有効な要求を Awaiting に戻せる |
| 一部送信・送信後切断・結果を確認できない | Unknown。自動再送しない |
| 送信タスクの cancellation | 未送信の証拠がなければ Unknown。Sending に取り残さない |
| native の解決・cancellation・対象寿命の終了 | 閉じて回答対応を破棄。遅延回答は拒否。Session 対象を無関係な turn の終了で消さない |
| クライアントの切断 | Host の実行と要求は維持。他端末・再接続で観測できる |
| Host の再起動 | 古い RequestId は失効。生きた native 実行が要求を再発行した場合のみ新しい ID を発行 |

検証・変換は純粋な関数で行う。排他 claim、状態更新、送信は所有者が行う。
await 中の要求解決にも耐えるよう、送信結果は claim した同じ要求にだけ反映する。

## 6. Item は外枠と本文に分ける

```rust
struct Item {
    id: ItemId,
    status: ItemStatus,
    client_input_id: Option<ClientInputId>,
    body: ItemContent,
}
enum ItemContent {
    Inline { body: Box<ItemBody> },
    Deferred { summary: Box<ItemBody> },
}
```

本文は UserMessage、AssistantText、Reasoning、ToolCall、CommandExecution、FileChange、Subagent と、
現行の専用表示を持つ画像生成・画像表示・Web 検索・計画・圧縮・レビュー・hook・自動承認確認・エラーに分ける。
既存の表示が異なるものを Custom にまとめて受け入れ条件を変えない。
共通外枠に置くのは全種類で意味が同じフィールドだけとし、時刻などは必要性を確認して配置する。

- Codex の MCP/dynamic tool call は ToolCall。呼出し先、名前、引数、結果を持つ。
- Claude Bash は CommandExecution。入力・出力・終了コードを混同しない。
- Claude Write/Edit/NotebookEdit は FileChange。分かる変更内容だけを出す。
- 差分が取得できない場合は欠落を明示する。推測した diff を実際のファイル変更の証拠として扱わない。
- 未知の item は Custom に provider、種類名、元の値を保持し、汎用表示を行う。

AssistantText は Commentary / Final / Unknown の phase を保持する。
Reasoning は本文の各 part と要約の各 part を区別する。既存の表示が参照する情報を単一 text へ潰さない。
ToolCall は MCP の結果と dynamic の text/image 結果、成否、namespace を失わずに保持する。
Subagent は関係する SessionRef と状態を保持し、他の会話 ID を裸の文字列に戻さない。

遅延本文は「本文なし」の別フラグと本文 Option の組を増やさず、
同じ本文スロットの Inline / Deferred として表す。
Deferred でも表示に必要な種類と要約を保持する。読み出しは会話・turn・item の ID で行う。
native のファイルパスをクライアントが読み出す契約にはしない。

更新は Item の置換を基本にし、頻繁なテキスト更新だけを型付き append にする。
Append は turn、item、本文フィールドを指す。
フィールドは AssistantText / ReasoningContent(index) / ReasoningSummary(index) / CommandOutput / FileOutput とする。
native の contentIndex と summaryIndex を保持し、part の追加通知も処理する。
ファイル変更の実行 output と実際の diff は別であり、FileOutput を diff に append しない。
対象不在や本文 variant の不一致は型付きエラーにし、元の状態を変更しない。
core は購読を更新して再取得する。Host は不正な更新を適用せず、診断と接続の回復を行う。

ToolCall の引数・結果、Custom の値など本質的に開いたデータだけに JSON を使う。
Postcard は既存の JSON boundary、UniFFI は既存の JsonValue 変換を使う。
SessionRef や通常の item を JSON に包む共通化はしない。

## 7. 状態とエラーは別々の事実にする

TurnStatus は Running / Completed / Failed / Interrupted / Unknown とする。
ItemStatus は Running / Completed / Failed / Declined / Interrupted / Unknown とする。
Declined は実行の拒否であり、実行して失敗した Failed や途中停止の Interrupted と混同しない。
Unknown は履歴が状態を提供しない場合の観測値であり、実行中と推測する根拠にはしない。
会話の実行状態、履歴取得の完全性、接続の状態、要求の配送は別々に保持する。
native history だけで実行の生存を断定せず、Host の所有中実行を優先する。
現在の loaded/notLoaded 等の native 状態をそのまま一つの共通状態 enum に詰めない。

ターン失敗は中立の分類、表示用メッセージ、再試行の証拠、必要な retry-after を持つ。
分類は RateLimited、UsageLimit、Overloaded、ContextLimit、SessionLimit、Auth、Network、Policy、
InvalidInput、Sandbox、Rollback、InputUnavailable、Internal、Other。
後半の分類も現在の表示契約が個別に扱う。八種類へ縮約して固有の説明を失わない。
現行の固有メッセージを失わないよう Other に provider のコード・診断を保持する。
単に HTTP 429 というだけで混雑と利用上限を同一視しない。
「再試行可能」と「provider が現在再試行中」は別であり、分類から自動再送を開始しない。
再試行中はその理由、試行回数/上限、追加説明を保持する。
原因が Network でも再接続中の混雑の証拠がある場合は、既存契約どおり混雑中の表示にできる。
core に native の HTTP/code の再解釈をさせず、adapter がこの証拠を中立の値へ写す。
Claude の assistant.error、result の api_error_status、利用上限の通知を文字列化する前に分類する。
上限の警告を、それだけでターン失敗として発行しない。

RPC の入力不正・未対応・要求失効・配送不明も BEX の失敗コードで表す。
native エラー JSON を公開するだけの失敗型はやめる。失敗コードだけで利用者への説明を失わない。
`RpcFailure { code, message, delivery, execution }` を Postcard でも型付きで送る。
native の履歴取得方針に必要なエラーコードは adapter が `execution.provider_code` から読む。
表示用メッセージを JSON として読み直して制御を判断しない。

## 8. 購読は現在状態の観測に限定する

Open は native history と Host の live 状態を合わせた snapshot と、その後の updates を同じ購読で返す。
snapshot を確定する処理と購読登録には現在の Host の排他境界を使う。
確定後の update を snapshot より先に適用させない。
再 Open は購読を取り替え、古い購読の update は無視する。
切断・更新不整合・遅い受信側の容量超過は再取得で回復する。

購読 ID を session の恒久 ID や履歴の revision と混同しない。
順序が保たれる現行の購読 stream に、永続 sequence、ack、再送ログは追加しない。
別 stream の遅延した操作結果は core の操作チケットと選択対象を確認して適用する。
履歴の欠落や取得不能を、空の完全な会話として表示しない。

## 9. 実装前に固定する受け入れ条件

| 境界 | 必須の検証 |
| --- | --- |
| ID | 同じ native ID の Codex/Claude の会話・下書き・要求が独立。各境界で構造を保持 |
| adapter | 現行の native 入力 fixture 全種類の写像表。未対応要求の拒否と未知 item の保持 |
| 回答 | variant 不一致、不正 ChoiceId、過剰権限、不正 form を拒否し要求を保持 |
| 競合・寿命 | 二端末 claim、送信中解決、未送信失敗、部分送信、cancellation、切断、ID 再利用 |
| 入力 | receipt と echo の逆順、queue、Unknown、再接続、Host 再起動で重複実行を捏造しない |
| 更新 | 不正 append で状態不変、再取得、古い購読の通知を無視 |
| 表示 | Claude Bash/Edit と Codex の同種表示。既存の desktop/native 受け入れ条件を保持 |
| wire/保存 | Postcard と任意 JSON の実際の encode/decode、構造化キーの保存・復元 |

normal tests に proptest を含め、回答検証・claim・寿命・更新分岐を focused cargo-mutants で監査する。
Host と影響するクライアントを同じ revision でビルドする。設計文書の更新だけでは検証済みにならない。

## 10. 実装と検証範囲

1. native fixture と表示の種類から、RequestBody/Answer、ItemBody、状態のフィールド写像を確定する。
2. SessionRef と ID を全境界へ通す。UniFFI の JSON String 化を構造化型へ置き換える。
3. Request/Answer と単一の pending 所有者へ移す。旧 native map と公開 map の重複を取り除く。
4. 状態・エラー、Item と更新を移す。移行した部分の旧 JSON 変換と推測を削除する。
5. native 操作を adapter 私有型へ移し、汎用 Provider 経路と旧ワイヤ名を削除する。

五段階のコードを実装した。構造化参照、typed Request/Answer、単一の pending 所有者、
中立の Item・状態・エラー、Host による入力経路の選択を利用する。
汎用 Provider 呼出し・通知、native の公開 start/resume/steer/queue、旧ワイヤ名を削除した。
native の入力 JSON は Host 境界でだけ生成する。Swift/Kotlin は同じ core の投影と回答検証を使う。

通常テストに会話・モデルの同一 native ID の provider 分離、回答の variant・選択肢・form 制約、
二端末 claim、Session 対象 elicitation、発行元 ID 再利用、送信中 cancellation、
receipt/echo の逆順、完了後の遅延 echo、不正 append と再取得、古い item 本文の拒否を含めた。
既存の desktop 表示契約を変更せず、旧形式のテスト入力を現在の BEX 契約へ更新した。
各段階で呼出し元を含めて再レビューし、不要な map・helper・forwarding を削除した。
追加の整理で core の全会話要求の索引も削除した。会話内の Request だけを正本とし、
回答処理・desktop の入力欄・CLI はそこから直接読み出す。索引の同期・破棄・変更検知は不要になった。

focused cargo-mutants の対象と結果、最終ビルド・結合テストの結果は
[`BEX_PROTOCOL_NATIVE_CONTRACTS.md`](BEX_PROTOCOL_NATIVE_CONTRACTS.md) の検証欄に記録する。
実際の認証済み Codex/Claude に推論を送る live テストは、この作業では実行していない。
native schema、保存済み native transcript、所有する fixture を通した検証の範囲で判断する。

## 11. Host と adapter の実装境界（2026-10-03）

Host は `HashMap<ProviderKind, Arc<dyn Agent>>` で二つの実装を引く。
`Agent` は会話の list/open/create/submit/interrupt/answer、models、capabilities、
catalog、worktree 内の稼働確認と event stream を提供する。汎用プラグイン機構は追加しない。
`Identity` はアカウント一覧、ログイン状態機械と usage を提供する。操作とログイン結果は
provider を明示し、選択中アカウントは `Accounts.selected[provider]` に保持する。
アカウント ID の prefix は Host/core の routing に使わない。
会話作成の事前検証も adapter が担当する。Claude は CLI の起動を入力送信まで遅延でき、
Codex が利用不能なら Host が worktree を作る前に拒否する。
会話一覧のページ走査は `session_pages` に集約し、タイトル一覧・worktree 一覧・Codex の稼働確認で
同じ終了条件とカーソル反復の拒否を使う。途中まで取得したページの利用可否は呼び出し元が決める。
アカウント一覧は `Identity::list` から直接取得し、ログイン操作の Call/Body を経由しない。
再初期化が成功した provider の過去の起動エラーは破棄する。

adapter は native の通知を中立の `AgentChange` に変換する。Host が自分の Router に適用してから
adapter に処理済みを返す。Claude の worker は native の断片を組み立てる実行中の値だけを持ち、
完了後はその本文を解放する。item 更新は共通の純粋な更新処理に一度だけ適用し、
別の作業用配列への二重適用はしない。adapter へ Router を渡す経路は廃止した。
回答は未送信の準備と送信 future を分け、キャンセル時の Awaiting/Unknown/Sent の証拠を保つ。
Claude の追加入力も、キュー受理後に書き込み確認を失った場合は Unknown を返す。
キューへ渡す前の拒否だけを NotSent として扱う。

Host は adapter の `SessionState` と cwd から start/steer/queue を決める。
送信前の読み取りは `SubmissionState` に会話と中立の `needs_reload` を返す。
Host は自身の実行台帳を重ねて route を決め、workspace 再作成も合わせて reload を指示する。
Codex は同じ読み取りの証拠を resume に使い、送信時の重複読み取りや新たな状態キャッシュを持たない。
start/steer/queue の native 送信と入力変換も一か所に置き、専用メソッドを経由する転送層は持たない。
Codex の `inProgress`/`notLoaded` 等は adapter 内で解釈する。Claude の追加入力は native の
queue に対応する。未知の native エラー分類は `ErrorCategory::Provider(Value)` として保持し、
core はその中身を再解釈しない。既存の中立な quota/context/policy 等の分類と表示も保持する。
モデル・端末の wire も `host/model/list` と `host/terminal/...` に統一した。
`historyMode` と `itemsView` は公開モデルから除去した。native のページング方式と
読み込み済みかの判定は Codex adapter 内だけで使い、Host/core は中立な履歴・項目状態を受け取る。

プロジェクト登録は Host の state directory にある `bex-projects.json` が正本である。
登録は ProjectStore が排他的・原子的に保存する。native project catalog や Codex の projectId を
使わず、adapter の作成操作へは cwd とモデル設定を渡す。worktree の稼働は Host の実行台帳と
各 adapter の確認を合わせる。確認に失敗した場合は削除しない。Claude の transcript だけでは
外部プロセスの終了を証明できないため、所有する稼働・idle process を観測できない会話がある
worktree は確認不能として扱う。

composer catalog は adapter ごとの候補を Host が合成する。Invocation は provider を持ち、
core が現在の会話・draft の provider の候補だけを使用する。取得エラーも provider ごとに保持する。
Claude は native initialize の
commands を読み、選択されたスキルを `/name` として送る。入力テキストにすでにある invocation を
二重に追加しない。native の dispatch は [Claude の公式 SDK 契約](https://code.claude.com/docs/en/agent-sdk/slash-commands#dispatch-commands-by-name)
に合わせる。

音声入力のクライアントネイティブ化は取りやめた。`host/dictation/transcribe`、音声転送、
Codex の認証を使う既存の文字起こしは維持する。

`host-fixture/tests/adapter_conformance.rs` は同じ会話シナリオを Codex/Claude の両 fixture に走らせる。
作成・送信・追加入力・中断・承認・質問・履歴・再接続/resume・異常終了と他 provider の継続を検査する。
Codex の turn ID が見えない場合の queue と、一方の adapter 初期化失敗も実際の Host 経由で検査する。

検証結果（2026-10-03）:

- 最終コードの共通 adapter 適合テストは 5 件すべて通過した。
  ページをまたぐ一覧・カーソル異常時の他 provider 継続と、resume 時の状態読み取りの再利用も確認した。
- Host 単体 102 件、iroh Host 結合 35 件、Claude 結合 17 件、Codex アカウント結合 3 件は
  通過した。Host/fixture の通常回帰一式も通過し、整理後の共通シナリオ・Claude 結合を再確認した。
  実アカウント・長時間 soak の明示実行用テストは通常どおり除外する。
- `submission.rs` の focused cargo-mutants は 2 caught、2 unviable、missed/timeout は 0。
- 共通化した `session_pages` の focused cargo-mutants は 2 caught、1 unviable、missed/timeout は 0。
  unviable は Rust が許可しない let-chain の `||` 置換であり、テストでの検出とは別に扱う。
- Claude の追加入力・current turn 読み取り・更新処理の focused cargo-mutants は 5 caught。
  missed/unviable/timeout は 0。書き込み確認喪失と、最終 block・retry 時の更新を検査した。
- UniFFI を含む core 122 件、store 結合 54 件、CLI の iroh 結合 4 件は通過した。
  protocol・transport・Codex App Server の回帰テストも通過した。
- Rust/Swift の整形、変更した Swift の strict lint、workspace 全対象の Clippy は通過した。
  今回の整理後も Host/fixture の全対象 Clippy を警告なしで再確認した。
- desktop の 43 件と Host・desktop の本番ビルドは通過した。
- iOS Simulator のアカウント分離・要求表示の UI テストは 2 件とも通過し、skip は 0。
  今回の整理後もログイン取消・再開始と Claude アカウント追加の 2 件が通過し、skip は 0。
  ログイン操作には ID と provider を組で渡し、待機後も同じログインか確認する。
- Android APK と ktfmtCheck は指定変更前に通過した。以後の Android 検証はユーザー指定により
  CI だけで行う。CI の Android ジョブは維持した。
- Windows の拡張子なし CLI 指定は共通 adapter シナリオで検査する。Windows 実行は CI の対象で、
  この macOS 上のローカル検証には含めない。

稼働中の Host は再起動していない。fixture を通した検証であり、実アカウントへの推論は行っていない。

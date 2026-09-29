# Bex：provider 中立プロトコルの設計方針

作成日：2026-09-29

対象：`ttizze/remote-agent`

確認したコミット：`8dc1c737`

状態：設計方針。
コード変更、テスト実行、稼働 Host への反映は、この文書の作成に含まない。

## 1. 決定

クライアントと Host のあいだの契約から Codex の形を取り除き、Bex 自身のドメイン型に置き換える。
内容は次の五つである。

- **ワイヤ名**：すべて `host/session/...` 系に揃え、Codex のメソッド名を廃止する。
- **Item**：種類を typed enum にする。
  UserMessage、AssistantText、Reasoning、ToolCall、CommandExecution、FileChange、Subagent、Error と、未知の項目を保持する Custom を持つ。
- **ServerRequest**：`method: String` をやめ、Approval、Permission、Question、Elicitation の enum にする。
  選択肢はアダプタが埋める。
- **ターンエラー**：RateLimited、Overloaded、Auth、Network と、未分類を保持する Provider からなる中立型にする。
- **SessionRef**：文字列 prefix の解釈と Codex を既定とする扱いをなくし、provider を常に明示する。

native history を正本とし、Host は所有中の実行と未解決要求だけを持つという既存の設計は維持する。
この変更は型と境界の整理であり、会話 DB、イベントログ、履歴 replay、プロトコルのバージョン交渉は追加しない。

## 2. 現状

Codex の JSON の形が、事実上の内部スキーマになっている。
Claude のアダプタも Codex の形を偽装して出力している。

- Claude の Bash 承認は Codex の `item/commandExecution/requestApproval` に、Write と Edit は `item/fileChange/requestApproval` に変換され、それ以外のツールは独自の `claude/tool/requestApproval` になる（`crates/host-daemon/src/claude.rs:1117`）。
- Claude のツール呼び出しは、Bash や Edit も含めてすべて `mcpToolCall` として出力される（`crates/host-daemon/src/claude.rs:1349`）。
  そのため Claude のコマンド実行は、Codex のコマンド実行と同じ表示にならない。
- Item は、種類を `Option<String>` で持ち、全種類のフィールドを Option で並べた一つの構造体である（`crates/agent-protocol/src/models.rs:147`）。
- ServerRequest は、Codex のメソッド名と生の JSON パラメータを持つ（`crates/agent-protocol/src/operations.rs:346`）。
  回答の検証もメソッド名の文字列比較で分岐する。
- ターンエラーは生の JSON であり、表示コードが Codex 固有の `codexErrorInfo` を読んで十五種類ほどに分類している。
  Claude のエラーはメッセージしか持たないので、この分類に乗らない。
- ターンの状態も文字列であり、Host の送信経路判定や実行保持の判断が `"inProgress"` や `"userMessage"` と比較している（`crates/host-daemon/src/host_rpc/session_actor.rs:29`）。
- SessionRef は、prefix のない ID を Codex とみなす（`crates/agent-protocol/src/session.rs:30`）。

## 3. 設計の詳細

### 3.1 ワイヤ名と Codex 形の呼び出し

クライアントとの通信は Postcard で符号化されるので、ワイヤに流れるのは enum の番号であり、メソッド名の文字列は流れない。
名前の変更自体は、ログと fixture に効くだけの小さな作業である。

実質的な作業は、Codex 形の呼び出しをクライアント契約から外すことにある。
ターン開始、steer、queue、resume は、現在 Host から Codex へ送る内部呼び出しとしてしか使われていない。
これらを Codex アダプタの私有型に移し、契約の表から削除する。
未登録のメソッドを受ける汎用の `Provider` 呼び出しも、同時に削除する。

fork、rename、モデル一覧、端末の入力と resize と kill のように、クライアントが使い続ける操作は `host/session/...` や `host/terminal/...` の名前に揃える。

### 3.2 Item の外枠と本体

Item は enum 単体ではなく、共通の外枠と種類ごとの本体に分ける。
ID、状態、クライアント入力 ID、遅延取得する本文への参照、時刻は、どの種類にもある。
enum 単体にすると、ID で項目を引くたびに全 variant を match することになる。

```rust
pub struct Item {
    pub id: ItemId,
    pub status: ItemStatus,
    pub client_input_id: Option<ClientInputId>,
    pub deferred_body: Option<DeferredBody>,
    pub body: ItemBody,
}

pub enum ItemBody {
    UserMessage { .. },
    AssistantText { .. },
    Reasoning { .. },
    ToolCall { server: Option<String>, name: String, arguments: Json, result: Option<Json> },
    CommandExecution { .. },
    FileChange { .. },
    Subagent { .. },
    Error { .. },
    Custom { provider: ProviderKind, kind: String, value: Json },
}
```

提案の八種類では、現在の表示が個別に扱っている種類が収まらない。
画像生成と画像表示、Web 検索、計画、コンテキスト圧縮、レビューモードの開始と終了、hook プロンプトがそれにあたる。
専用の表示や制御がある種類には variant を用意し、Custom は汎用表示だけで済むものに限る。
Custom には provider と元の種類名を持たせ、「未対応の項目」として表示できるようにする。

Codex の `mcpToolCall` と `dynamicToolCall` は ToolCall にまとめる。
Claude の Bash は CommandExecution に、Write と Edit と NotebookEdit は FileChange に写す。
これで Claude と Codex の表示が揃う。
ただし Claude の Edit 入力から差分を組み立てる処理は新しく必要になる。

文字列の差分更新は、対象の variant とフィールドを型で指す。
対象の item の種類とフィールドが合わない差分は、型付きのエラーにする。

Postcard は自己記述的な形式ではないので、Custom と ToolCall の任意 JSON は既存の JSON 境界モジュールを通して符号化する。
UniFFI 側も、既存の任意 JSON の変換器を使う。

### 3.3 ServerRequest と Answer

ServerRequest を enum にするなら、Answer も対応する型にする。

| 要求 | 意味 | 回答 |
|---|---|---|
| Approval | 特定の操作を実行してよいか。対象はコマンド、ファイル変更、ツールの enum | 選択肢の ID |
| Permission | サンドボックスの範囲を広げてよいか | 許可または拒否 |
| Question | 利用者への質問 | 質問 ID ごとの回答 |
| Elicitation | MCP サーバーからの入力要求 | スキーマに沿った値 |

Approval と Permission の境界は、この表の定義で固定する。

選択肢は添字ではなく、不透明な ID と中立の意味を持たせる。
意味は「一回だけ許可」「このセッション中は許可」「拒否」「中止」の四つである。
Host は自分が出した選択肢の集合で回答を検証し、native の値への対応を Host の中だけに置く。
クライアントは native の値を見ない。
生の値をそのまま返す `Raw` の回答は廃止する。

要求には対象のターン ID と item ID を持たせる。
現在は表示コアが要求とターンを推測で紐付けているが、この紐付けはアダプタが知っている事実なので、型で渡せば推測が不要になる。

### 3.4 ターンエラー

提案の分類では、利用者が取るべき行動の違いを表せない。
少なくとも次を分ける。

| 種類 | 利用者の行動 |
|---|---|
| RateLimited | 待つ |
| UsageLimit | アカウントを切り替える、または上限の回復を待つ |
| Overloaded | 待つ |
| ContextWindowExceeded | 会話を圧縮する、または新しい会話を始める |
| Auth | ログインし直す |
| Network | 接続を確認する |
| PolicyViolation | 入力を変える |
| Provider | 未分類。provider の元の値を保持する |

どの種類にも、再試行するか、再試行までの時間、表示用メッセージを共通のフィールドとして持たせる。
HTTP ステータスや provider 固有のエラーコードからの分類は、各アダプタが行う。

### 3.5 状態と ID

Item の種類だけを型にしても、ターンの状態が文字列のままなら、Host の制御判断は文字列比較に残る。
ターンの状態、item の状態、会話の状態も、この変更で enum にする。

ID は newtype にする。
対象は SessionRef、ターン ID、item ID、要求 ID、クライアント入力 ID である。
要求 ID は現在 `serde_json::Value` なので、native の値は Host の中に閉じ込め、クライアントには Bex が発行する ID だけを見せる。

SessionRef から prefix の解釈をなくすので、Snapshot の会話キーと下書きのキーも文字列から SessionRef に変わる。
新しい会話の開始でも provider を明示する。
製品は未リリースなので、保存形式の移行コードは書かない。

## 4. 影響範囲

触る範囲は広い。

- 表示コアの本体は、Codex の種類名を八十箇所以上参照している。
- desktop の会話ビューと iOS の行表示は、種類名の文字列で分岐している。
- Host の fixture、wire golden fixture、振る舞いコーパスの JSON は、Codex の形で書かれている。

デスクトップ会話表示契約（`docs/DESKTOP_CONVERSATION_DISPLAY_CONTRACT.md`）の受け入れ条件は変えずに通す。
表示結果の行の種類（`agent` や `user` など）も文字列だが、これは表示層の型なので、この変更とは別に扱う。

## 5. 進め方

レビューしやすい順に、次の五段階に分ける。
各段階は Host とクライアントを同じリビジョンでビルドし、単独でマージできる単位にする。

1. **SessionRef と ID の newtype**：機械的な変更で、後続すべての土台になる。
2. **ServerRequest と Answer**：規模が小さく、承認の安全性に直接効く。
3. **状態の enum とターンエラー**：Host の制御判断から文字列比較をなくす。
4. **Item の外枠と本体**：Claude のツールの写像もここで行う。
5. **Codex 形の呼び出しの除去と名前の統一**：前の段階で契約が中立になってから行う。

## 6. 検証

各段階で、変更した型の性質を proptest で検査し、変更したロジックは cargo-mutants で監査する（`docs/TEST_MAINTENANCE.md`）。
少なくとも次を回帰テストにする。

| テスト | 合格条件 |
|---|---|
| Codex と Claude で同じ native ID を使う | 会話、item、要求が衝突しない |
| Host が出していない選択肢 ID で回答する | Host が拒否し、要求は未解決のまま残る |
| 同じ承認に複数端末から回答する | 一つだけが claim される |
| Claude の Bash と Edit の実行 | Codex と同じ種類の item として表示される |
| 未知の種類の item を受け取る | 内容を失わず、「未対応の項目」として表示される |
| 種類の合わない文字列差分を受け取る | 型付きのエラーになり、既存の本文を壊さない |
| Claude と Codex の上限到達エラー | 同じ種類に分類される |
| 既存の会話表示の受け入れテスト | 期待値を変えずに成功する |

コミット後は `nix develop . --command cargo xtask quality-status --wait` で、そのコミットの結果が `passed` かつ `workingTreeDirty: false` であることを確認する。

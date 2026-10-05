# Bex：コア再設計の実装計画

この文書は2026年9月の設計・実装記録。現在の移行方針と進捗は[T3 native rewrite plan](T3_NATIVE_REWRITE_PLAN.md)、現在の会話所有と参照形式は[Session runtime](SESSION_RUNTIME.md)に従う。

作成日：2026-09-16

対象：`ttizze/remote-agent`

確認したmain：`4def0f2bce7b834812c3b77cc436860a5239d06a`

状態：実装指示書。コード変更・テスト実行・実機への反映は、この文書作成には含まない。

## 1. 今回の決定

前回の再設計案のうち、**先頭の3項目だけを実装対象にする。** 全面作り直しではなく、既存の動作とテストを残して順番に置き換える。

| 順序 | 実装すること | 得たい結果 |
|---|---|---|
| 1 | 大容量の項目本文取得と、Storeの制御更新を分離する | 本文転送が遅くても、会話更新・承認・停止が進む |
| 2 | provider中立の契約、明示的なルーティング、型付きエラーに整理する | Codex/Claude固有の事情が共通処理へ漏れず、送信結果不明を正しく扱える |
| 3 | PTY・Workspace・共通プロセス管理のCodex依存を外す | Codexが使えなくても、Claudeと独立したPC機能が動く |

**最初の着手点は `ReadItem` と `Store::run`。`SessionActor`やネイティブUIの再実装から始めない。**

### 今回は行わないこと

以下は後回しとする。この計画の前提作業としても追加しない。

- 接続時のプロトコル互換性交渉、旧アプリ対応、更新・配布手順の再設計。
- 端末永続化の分離、保存形式の全面変更、キャッシュDBの導入。
- UIの責任分割・画面再設計、長い会話全般の描画性能改善、通知・診断機能の拡張。

また、新providerの追加、ブラウザの再設計、リレー運用の変更、認証・課金方式の変更も今回へ混ぜない。SwiftUI・Compose・GPUIには、共通型・既存操作への接続に必要な最小限の変更だけを行う。

大容量転送による制御更新の停止は今回の対象である。これは、後回しにした「長い会話全般の性能改善」とは分ける。

## 2. 基準と、維持する設計

前回レビューの基準は `c5edd685`（PR #19マージ後）。文書作成時に確認したmainは `4def0f2b`（PR #20マージ後）で、両コミットの差分はデスクトップの設定・アカウント画面に限られる。今回の中心であるStore・項目取得・Hostルーティングには、その間の変更はない。[コミット差分][compare]

実装開始時には現在のHEADと作業ツリーを再確認する。別作業による変更を戻したり、未コミットの作業を上書きしたりしない。

### 維持するもの

Rust共通Store、共通presentation、iroh、ネイティブUIを維持する。会話の正本はprovider native history、Hostの会話状態は所有中の実行と未解決要求に限定する。現在の設計でも、`SessionActor`はnative historyを保持せず、実行中状態を管理する役割に絞られている。[SessionActor][actor] [既存のセッション設計][migration]

| 情報 | 引き続き所有する場所 |
|---|---|
| 過去の会話、provider固有の永続セッション | 各provider |
| Bexが所有する実行、未解決要求、実行中の入力ID | Host |
| ペアリング、Bex設定、Bex作成worktreeの管理情報 | Host |
| 下書き、未保存の手動編集、表示用キャッシュ | 現在のクライアント保存方式のまま |
| フォーカス、IME、文字選択、スクロール、ウィジェット | 各ネイティブUI |

「Hostは会話を保存しない」を「Hostは設定も保存しない」に拡張しない。下書きを保護する既存処理は維持するが、そのための新しい永続化方式は導入しない。

### 再導入しないもの

Bex独自の会話DB、永続イベントログ、履歴replay、Hostの会話キャッシュ、外部rollout監視、独自の履歴revision・欠番修復、端末別承認aliasを追加しない。汎用プラグイン機構、未実装providerの足場、独自のジョブ管理基盤も作らない。

PR #19で修正済みの「履歴取得失敗時に現在の実行を消す問題」と「巨大open応答で接続が切れる問題」を、未修正の問題として再実装しない。既存の退行防止テストと、大容量本文・画像の内容一致を守る。[PR #19][pr19]

## 3. 現状で確認した変更点

以下は静的なコード確認である。実機の遅延量や性能改善率を測定した結果ではない。

| 現状 | 根拠となるコード | 今回の扱い |
|---|---|---|
| `ReadItem::run`がRPC応答後に`resolve`の完了まで待ち、かつ`ORDERED = true` | `state/operations/threads.rs` | 制御応答と本文転送を別段階にする |
| Storeが順序付き応答の操作完了を待ってから後続イベントを適用する | `store.rs`の`run`、`completion_sequence`、`Execution::call` | 必要な順序保証は残し、本文転送をその待ち合わせから外す |
| `claude:`による判定と、未処理RPCのCodex転送が残る | `host_rpc/service.rs` | providerを明示した契約と公開操作一覧へ整理する |
| 共通層でprovider別のcapabilitiesを固定的に決める | `session.rs`の`SessionRef::capabilities` | adapterが根拠を持って能力を返す |
| 配達不明の判定にエラー文字列の解釈が使われる | `store.rs`、`host_rpc/service.rs` | 制御判断を型付き結果へ移す |
| ターミナル起動・音声認識・worktree操作の一部がCodexを直接要求する | `host_rpc/service.rs` | PC機能を分離し、残る依存は明示する |

根拠：[項目取得][threads]、[Store][store]、[Hostルーティング][service]、[Sessionの型・capabilities][session]。

## 4. 目標の責任分担

```text
SwiftUI / Compose / GPUI / CLI       ← 基本維持
                 │
Client Store / Presentation          ← 共通状態と表示規則を維持
                 │
Bexの明示的な操作・結果の型
                 │
iroh：制御通信 + 既存のバイナリ転送   ← 通信方式を増やさない
                 │
Host
  ├─ 接続・認可・明示的なRPCルーティング
  ├─ Session調停：所有中の実行・未解決要求
  ├─ Workspace：File / Git / PTY
  ├─ 共通プロセス寿命管理
  └─ provider adapters
       ├─ Codex  → app-server / native history
       └─ Claude → CLI / native history
```

この図は責任境界であり、箱ごとに新しいcrate・actor・traitを作る指示ではない。既存モジュールの整理で成立するならそれを優先する。

---

## 5. 第1段階：本文転送でStoreの制御更新を止めない

### 5.1 問題と、残す順序保証

現状では、順序付きの`ReadItem`の完了にバイナリ転送の完了が含まれる。その応答位置でStoreが待つため、後続の会話更新や承認要求の適用も待つ構造になっている。[項目取得][threads] [Store][store]

一方、`session/open`の応答を適用してから、その購読に属する更新を適用する順序保証は必要である。全RPCの順序制御をなくしたり、`ReadItem`の`ORDERED`を単に`false`へ変更したりするだけでは完了としない。

### 5.2 実装方針

処理を次の二段階に分ける。

```text
[制御段階：既存の必要なwire順序を守る]
  RPC応答を受け取る
    → ID・概要・本文参照を検証する
    → 現在の項目と取得状態へ反映する
    → 本文取得を別のeffectとして開始する
    → このRPCの順序待ちを解除する

[本文段階：後続の制御イベントを待たせない]
  既存のバイナリ経路で本文を取得する
    → サイズ・digest・対象IDを検証する
    → まだ有効な取得要求かを確認する
    → 有効なら対象本文を適用する
```

小さいinline項目は従来どおり制御段階で適用する。本文がdeferredである項目だけ、独立した本文取得へ進める。

接続所有者・有効期限・件数上限・サイズ上限・digest検証・一時ファイルの解放は既存の転送処理を再利用する。新しいHTTP経路、Blobサービス、永続保存先を作らない。[既存の転送制約][migration]

### 5.3 古い本文を、現在の項目へ上書きしない

本文取得完了を無条件で`upsert_item`へ流さない。少なくとも次を識別する。

| 識別対象 | 目的 |
|---|---|
| 接続・Host・storage scopeの世代 | 切断・再接続・接続先変更をまたいだ結果を適用しない |
| `SessionRef`、turn ID、item ID | 別セッション・別項目の本文を混入させない |
| 項目単位の取得トークンと取得元の世代 | 再取得、項目更新、削除後に戻った古い結果を適用しない |

既存のepochやオブジェクト識別を使える箇所では再利用する。新しい世代番号が必要でも、端末内の項目取得に限定する。Hostの履歴revisionや履歴同期プロトコルには拡張しない。

digest一致は転送内容の完全性の確認であり、その本文が現在も最新であることの証明ではない。取得中の`Item`更新、`Text`更新、再openでの置換、削除を考慮する。

**本文を取得中にも届くdeltaの扱いを、実装前に決めてテストする。** 既にある新しい本文は消さない。本文の先頭が未取得なら、後から届いたdeltaだけを完全な本文として扱わない。古い取得を無効化した後も本文が不足する場合は、最新状態に対する再取得を確実に行う。連続更新のたびに無制限の再取得を開始せず、一つにまとめるか、更新が落ち着いた時・turn完了時に取得する。黙って破棄したまま永久に未取得状態にしない。

### 5.4 部分失敗と操作完了

本文のtimeout・取得失敗・検証失敗はその項目の取得失敗として扱い、正常な制御接続と別セッションの更新を止めない。承認・停止・次の操作は続けられるようにする。

既存UIが`ReadItem`の完了を「本文を表示できる」と解釈している場合、その意味を黙って「本文取得開始」に変えない。Storeのwire順序待ちと、呼び出し元へ返す操作完了を切り離す。既存の詳細表示で成功・失敗を正しく扱うための最小限の接続変更は行ってよい。新しい画面設計は不要。

転送の同時数と保留数は有限とし、同じ対象の取得要求をまとめる。承認や送信を落とす変更、全Storeのキュー基盤の作り直しには広げない。

### 5.5 必須テスト

| テスト | 合格条件 |
|---|---|
| 本文転送を意図的に停止し、その後にText更新を送る | 転送を再開する前にText更新がStoreへ適用される |
| 同じ状態で承認要求を送る | 本文転送完了前に要求が現れ、回答がHostへ届く |
| 同じ状態で停止操作を行う | 本文転送完了前に停止要求がHostへ届き、結果が反映される |
| Aの本文を転送中にBのセッションを更新する | Bの更新がAの転送に待たされない |
| 同じitemを新しい内容へ更新してから古い本文を返す | 新しい内容・status・要求状態を巻き戻さない |
| 未取得の本文へdeltaが届いて取得が無効化される | 不完全な内容を完全と表示せず、最終的に正しい本文を取得できる |
| 転送中に再open・再接続・削除・接続先変更を行う | 古い結果を新しい対象へ適用せず、必要な再取得は行われる |
| 転送timeout、異なるID、digest不一致 | 誤った本文を適用せず、制御通信は使用できる |
| 既存の巨大本文・画像・ツール出力のfixture | 内容一致、deferred取得、接続維持の既存条件を満たす |
| `session/open`とその直後の更新 | snapshotより先にupdateを適用する退行がない |

転送を止めるbarrierなどで順序関係を決定的に再現する。単に「数秒以内なら成功」だけのテストにせず、**転送未完了のまま制御処理が進んだこと**を検証する。タイムアウトはテストのハング防止として使う。

### 5.6 主な変更箇所と完了条件

主な対象は `crates/agent-core/src/state/operations/threads.rs`、`crates/agent-core/src/store.rs`、`crates/agent-core/src/client.rs`の`ItemResponse`、既存のtransfers処理、関連fixture。必要ならHostの詳細応答へ最小限の識別情報を追加するが、既存情報で足りる場合はwire形式を変更しない。

**完了条件：既存の順序保証と大容量取得の正しさを保ったまま、バイナリI/Oが後続の制御イベント適用の障壁にならない。**

---

## 6. 第2段階：provider中立の契約と、明示的なルーティング

### 6.1 SessionRefを内部まで通す

`SessionRef { provider, id }`を共通の識別単位にする。providerのnative IDを省略・置換せず保持する。provider名をIDの文字列prefixやモデル名から繰り返し推測しない。

現在の保存済みID・UI引数のために文字列形式が必要な箇所は、明示的な入出力境界で一度だけ変換する。端末永続化を今回変更しないための限定的な変換は残してよいが、新しい共通処理やadapter内部へprefix判定を増やさない。

これは「旧アプリ互換プロトコルの実装」ではない。現在のアプリを壊さず、内部の責任境界を整理するための措置である。

### 6.2 Codexを既定の転送先にしない

共通契約には、セッション一覧・作成・open・項目詳細・入力・停止・要求回答と、現在提供している機能を明示する。

Hostは受信したRPCを次のいずれかへ分類する。

| 操作の種類 | 所有者 |
|---|---|
| ペアリング・接続管理・認可 | Host |
| ファイル・Git・PTY | HostのWorkspace機能 |
| 共通セッション操作 | `SessionRef`で選んだadapterとSession調停 |
| 現在公開しているprovider固有機能 | 名前と対応providerを明示したadapter操作 |
| 不明な操作 | method-not-found相当の明示的なエラー |

未知のrequest・notificationをCodexへ素通ししない。Hostが処理すべき管理操作やprovider向けresponseを取り違えない。既存の認可を通ってから、公開済みの操作だけを実行する。

ただし、素通しを削除する前に、Client・CLI・テストが使っているメソッドを列挙する。アカウント操作、fork/side chat、rename、steer、queueなどを未登録のまま壊さない。provider固有機能が必要なことと、無制限の転送を残すことは別である。[現在のrouting][service]

既存メソッドを名前だけ全面改名する必要はない。まず型・所有者・呼び出し先を明示する。

### 6.3 adapterの責任

adapterは、native historyの読取、native protocolの変換、provider固有の実行方式・request対応を扱う。共通層はBexとしての操作と結果を扱う。

Codexの共有app-serverと、ClaudeのCLIプロセス再利用を、同じ内部方式へ無理に統一しない。対等なadapterとは、外から見た責任が対等という意味である。

最初はCodex/Claudeの明示的なenumや小さい境界で十分。動的provider registry、プラグインローダー、未来のprovider用の空実装は作らない。

共通モデルでは、制御判断に必要なprovider・status・履歴取得状態・本文取得状態・要求の配達状態を型で扱う。`extra`内の任意JSONや表示用文字列を制御規則として参照しない。未知のprovider固有情報は、保存・表示可能な補足データとして残してよい。未対応の内容を黙って消さない。

### 6.4 capabilitiesの所有者を移す

共通層が「Codexだから可能」「Claudeだから不可能」と固定判断するのをやめる。能力はadapterが、実装済み機能と確認できたprovider情報に基づいて返す。操作時にもHost側で検査する。

「能力として対応しているか」「providerが現在使えるか」「このセッション状態で今操作できるか」を混同しない。起動応答等で確認できない能力を、希望的に有効化しない。必要ならadapter内の保守的な対応表を使うが、共通Clientへprovider別対応表を複製しない。

実行中の指示変更、次の入力の予約、停止後の再送は意味が違う。今回使う操作の区別は型で保持し、未提供の機能を新しく実装するところまでは広げない。

### 6.5 エラー文字列で配達状態を判定しない

表示用メッセージとは別に、制御判断に使う型を定義する。少なくとも次の区別が必要である。

| 区別 | 動作 |
|---|---|
| 入力不正、未対応操作、認可拒否 | 副作用を開始せず、確定した拒否として返す |
| provider不在・停止 | 該当providerの操作だけを失敗させる。配達状態は別途保持する |
| 対象が変化した、要求が解決済み | 古い操作を拒否し、現在の状態を再取得できる |
| 送信前に確実に失敗した | 未送信として扱い、下書きを保持する |
| 送信を試みたが受付・実行結果を確認できない | 配達不明として扱い、自動再送しない |

エラー種別と配達状態は必要に応じて分ける。「provider unavailableなら必ず未送信」「timeoutなら必ず失敗」と推測しない。型の名前や配置は既存コードに合わせ、全リポジトリのエラー処理の全面置換にはしない。

`error.contains("unknown")`や`starts_with("submission outcome unknown:")`による判断を対象経路からなくす。providerからの不明なエラー文は補足情報として保持し、未送信の証拠には使わない。

### 6.6 入力と承認の安全性を維持する

入力IDの重複排除は、現状どおり実行中の範囲で保証する。完了後・Host再起動後のexactly-onceを保証するためにreceipt DBや永続送信キューを追加しない。未確認入力・承認の自動再送も追加しない。[現在の配達契約][migration]

承認はprovider・session・turn・native requestの所有関係で検査し、最初の有効な回答だけをclaimする。同時回答、解決済み要求、実行終了後の要求を安全に拒否する。Host共通層の未解決要求を正本とし、adapter内部のnative応答先との対応付けは必要最小限に保つ。

### 6.7 必須テストと完了条件

| テスト | 合格条件 |
|---|---|
| CodexとClaudeで同じnative IDを使う | セッション・項目・承認が衝突しない |
| 未登録のrequest / notificationを送る | Codexへ転送されず、契約どおり拒否される |
| 既存の公開操作を一覧化して試す | 利用中の機能を明示的な登録漏れで壊さない |
| capabilitiesが無効な操作を直接RPCで送る | UIの無効化に頼らずHostが拒否する |
| 送信前失敗と、送信直後の切断 | 未送信と配達不明を型で区別する |
| エラーメッセージの文言だけを変える | 制御動作・再送判断が変わらない |
| 複数端末から同じ承認へ回答する | 一つだけがclaimされ、重複回答は実行されない |
| 一方のproviderの一覧・履歴読取が失敗する | 他方の結果を残し、不完全な結果を空一覧と誤認しない |
| 第1段階の転送中にセッションが更新される | 本文の鮮度判定と制御更新の非停止を維持する |

主な対象は `session.rs`、`models`、`client`、必要な`state/operations`、`host_rpc/service.rs`、Codex/Claude adapter、関連fixture。

HostがClient専用のStoreやpresentationへ依存しなくて済むよう、共有契約の所在を整理する。crate分割は必要性が示せる場合に限り、分割そのものを成果にしない。

**完了条件：共通処理のprovider推測、未知RPCの既定Codex転送、文字列による配達不明判定が対象経路からなくなり、既存機能と安全性が維持される。**

---

## 7. 第3段階：PC機能をCodexから独立させる

### 7.1 PTYはHost自身が所有する

`host/terminal/start`からCodexの`process/spawn`へ転送する構造をやめ、HostがPTYを起動・所有する。入力、出力、resize、終了、エラーを既存のクライアント操作へ接続する。[現在のterminal経路][service]

起動・終了・割り込みのために別の遠隔接続経路は追加しない。既存のiroh認可とプロセスsupervisorの仕組みを再利用できるか確認する。必要なPTY実装はプラットフォーム境界へ閉じ込め、ClientにOS別の制御規則を持ち込まない。

PTYの所有接続、作業ディレクトリ、プロセスhandle、終了状態をHostで追跡する。操作権限のない接続から別端末のhandleを使用できないようにする。

クライアント切断時・明示終了時・Host終了時の既存の寿命契約を維持する。今回の変更で「切断後も永続するターミナル」へ機能変更しない。起動途中の切断でも、後から未管理のプロセスが残らないようにする。

### 7.2 Workspaceの安全性をprovider分岐から切り離す

ファイル読書き、Gitのreview、worktree設定・一覧はHostのWorkspace機能として整理する。Codexの起動・認証を、その全体の前提条件にしない。

Bexが所有する実行とPTYについては、共通の実行・プロセス情報からworktree利用状況を確認する。ディレクトリ削除と、新しい実行・PTY・ファイル書込みの開始が競合しないよう、既存の排他と再検査を維持する。

**Codexに依存しなくするために、worktree削除の安全確認を弱めない。**

Bexの管理外から使われている可能性や、provider側の活動を確認できない場合には、破壊的操作を保守的に拒否してよい。変更・未追跡ファイル・ignoredファイル・locked・detached HEADなどに対する既存の削除保護も残す。

したがって、「Codex停止中でもファイル一覧・編集・Git確認・Claudeの作業が使える」と「Codexの状態を確認できなくても必ずworktreeを削除できる」は同じ要件ではない。後者は要求しない。

### 7.3 共通supervisorはprovider用クレートへの偶然の依存にしない

現在の共通プロセスsupervisorは `codex-app-server` に同居し、Claude側からも利用されている。[Claudeのプロセス起動][claude-process] [既存のプロセス所有契約][migration]

PTY・Codex・Claudeで共有するのは、起動したプロセスの寿命管理と後始末である。providerのメッセージ形式、認証、履歴読取までは共有supervisorへ入れない。

配置は既存モジュールからの小さい切り出しを優先する。ビルド・同梱スクリプトの必要な修正は行うが、更新機構・配布基盤の再設計へは広げない。Host終了時の子プロセス終了と、provider終了後の子プロセスcleanupを維持する。

### 7.4 音声入力：境界分離と、実際の非依存化を混同しない

音声入力の共通操作は、特定の会話providerではなく独立した入力機能として扱う。`HostRpcService`が直接Codexを取り出して認識処理へ渡す結合を、音声入力側の境界へ閉じ込める。[現在のdictation経路][service]

ただし、現在の認識処理自体がCodexを使うため、wrapperを挟むだけで「Codexがなくても音声認識できる」とは扱わない。今回、新しい有料API・追加認証・別の認識エンジンを無断で導入しない。

既存の認識経路を維持する場合、音声入力だけを利用不可として明示し、Claude・PTY・Workspaceを巻き込まない。この状態は「音声入力の責任境界を分離済み、認識backendのCodex依存は残存」と報告する。完全な独立化の達成に数えない。

今回の機能上の必達はPTYと非破壊的なWorkspace操作の独立動作である。音声認識backendの置換は別の機能選定を要するため、この指示書だけでは実施しない。

### 7.5 必須テストと完了条件

| 条件 | 合格条件 |
|---|---|
| Codex未導入、Claude利用可でHostを起動 | ペアリング、Claudeの一覧・open・送信・停止が使える |
| Codexが起動に失敗する、または途中で終了する | Claudeの実行・未解決要求・接続を巻き込まない |
| 同じ条件でPTYを使う | 起動、入出力、resize、終了ができる |
| 同じ条件でWorkspaceを使う | ファイル一覧・読書き・Git reviewが利用できる |
| PTY起動途中に接続が切れる | 孤児プロセスや未解放の所有記録を残さない |
| 無関係な接続からPTYのhandleを指定 | 認可されず、対象プロセスを操作できない |
| 実行・PTY・書込みとworktree削除が競合する | 使用中ディレクトリを削除しない |
| 外部利用の有無を確認できないworktreeを削除する | 安全側に拒否し、理由を返す |
| Host・provider・PTYを終了する | 既存のprocess group / Job Object等のcleanupを維持する |
| 音声入力backendだけが利用不可 | 音声入力に限定した失敗となり、他機能・下書きを壊さない |

**完了条件：Codexを停止・未導入にしたfixtureで、Claude・PTY・非破壊的なWorkspace操作の独立動作を実証する。音声認識や破壊的操作に残る依存・制限は別項目として報告する。**

---

## 8. 進め方とレビュー単位

第1段階、第2段階、第3段階の順で進める。各段階をレビュー可能な単位で完了させ、後段の都合で前段の修正を巨大PRへ埋めない。第3段階が大きい場合は、共通プロセス管理・PTY、Workspace、音声入力の境界整理に分ける。

各段階の開始時に、変更する責任、消す旧経路、維持する動作、追加するテストを短く列挙する。既に対応済みなら重複実装せず、該当コードとテストで確認する。

### 実装上の制約

既存のtask worktreeを再利用し、他セッションの未コミット作業を保持する。承認済みの会話表示・操作の受け入れ条件を、テストを通すためだけに緩めない。Client側へ共通判断を複製せず、coreの結果を利用する。[プロジェクトルール][agents]

UIの責任分割は対象外だが、型変更や非同期完了の意味を接続するための最小限の修正・既存UIテストは必要である。「UIは後回し」を理由にClientをコンパイル不能にしない。

互換性・更新手順の再設計は行わないが、現在の「Hostと影響するClientを同じrevisionでビルドする」ルールは維持する。稼働中Hostや実機の再起動・置換は別の操作として扱い、実装だけで自動実施しない。実行中タスクの確認なしにHostを停止しない。[プロジェクトルール][agents]

## 9. 検証と報告

各段階で、対象を絞った退行テストを先に追加し、その後に既存のworkspaceテストと必要なClient検証を行う。コマンドは実装時点のREADME・AGENTSを正とする。

現在の基準コマンド：

```sh
# fixture等で使う既存supervisorをビルド
scripts/dev-env.sh cargo build --locked -p bex-process --bin bex-provider-supervisor

# Rustのテスト・整形・静的検査
scripts/dev-env.sh just unit-tests
scripts/dev-env.sh cargo fmt --all -- --check
scripts/dev-env.sh cargo clippy --locked --workspace --all-targets -- -D warnings

# mainへpushした後、そのcommitのCI結果を確認
gh run list --workflow native.yml --branch main
```

supervisorの配置を変更した段階では、ビルドコマンドとfixtureの参照も実装に合わせて更新する。存在しなくなった旧コマンドを手順に残さない。

全ユニットテストをローカルで通してmainへ統合する。PRやCI成功をマージ条件にはしない。mainへのpush後にNative clients CIを実行し、失敗はmain上で修正する。全体の検証済みと報告するときは、現在のcommitのCI成功とcleanな作業ツリーを確認する。コミット後のローカルQAは自動実行しない。以前のcommitの成功を流用しない。実認証のprovider推論、Windowsのプロセス管理、iOS/Android/desktopの受け入れ検証は、実施した環境・範囲を区別して報告する。未実施を合格に数えない。[プロジェクトルール][agents] [既存の検証手順][readme]

### 各段階の提出物

| 提出物 | 内容 |
|---|---|
| 実装差分 | 変更した責任と、削除した旧経路 |
| テスト差分 | どの失敗・競合を再現し、何を保証したか |
| 検証結果 | commit、コマンド、結果、環境、未実施事項 |
| 残る制約 | 配達不明、非対応機能、provider依存などを具体的に記載 |
| 変更規模 | 本体・テスト・生成物・文書を分けた増減と、その理由 |

行数削減の固定目標は置かない。ファイル移動・renameを削減成果に数えず、wrapperや新しい状態管理を増やした場合は、何を置換してどの責任を減らしたかを説明する。機能削除や退行防止テストの弱体化で数字を作らない。

## 10. 最終チェック

| 必須 | 確認する内容 |
|---|---|
| 第1段階 | 本文転送中でも会話更新・承認・停止が進み、古い本文で最新状態を壊さない |
| 第2段階 | 共通処理がproviderを推測せず、未知RPCを転送せず、文字列で配達状態を決めない |
| 第3段階 | Codex未導入・停止中でもClaude・PTY・非破壊的Workspace操作が使える |
| 既存保証 | native historyを正本にし、下書き・承認・内容一致・認可・cleanupを維持する |
| スコープ | 互換性交渉・更新基盤・永続化分離・UI再設計・通知診断を追加していない |
| 報告 | 実測・テスト結果と、未検証・残る依存を区別している |

**この計画の目的は、独自の同期・管理機構を増やすことではない。既存の土台を維持しながら、本文転送の待ち合わせ、providerの境界、PC機能の所有者を正す。**

---

## 参照コード

参照は文書作成時のコミットへ固定している。実装時には最新HEADとの差分も確認する。

- [AGENTS.md][agents]
- [README.md][readme]
- [Session runtime / migration][migration]
- [PR #19][pr19]
- [前回レビュー後の差分][compare]
- [ReadItem / ReadThread][threads]
- [Store][store]
- [SessionRef / SessionChange / capabilities][session]
- [HostRpcService][service]
- [SessionActor][actor]
- [Claudeのプロセス起動][claude-process]

[agents]: https://github.com/ttizze/remote-agent/blob/4def0f2bce7b834812c3b77cc436860a5239d06a/AGENTS.md
[readme]: https://github.com/ttizze/remote-agent/blob/4def0f2bce7b834812c3b77cc436860a5239d06a/README.md
[migration]: https://github.com/ttizze/remote-agent/blob/4def0f2bce7b834812c3b77cc436860a5239d06a/docs/SESSION_RUNTIME_MIGRATION.md
[pr19]: https://github.com/ttizze/remote-agent/pull/19
[compare]: https://github.com/ttizze/remote-agent/compare/c5edd685b4a13605ea8ac54bca70ba26d20aab36...4def0f2bce7b834812c3b77cc436860a5239d06a
[threads]: https://github.com/ttizze/remote-agent/blob/4def0f2bce7b834812c3b77cc436860a5239d06a/crates/agent-core/src/state/operations/threads.rs
[store]: https://github.com/ttizze/remote-agent/blob/4def0f2bce7b834812c3b77cc436860a5239d06a/crates/agent-core/src/store.rs
[session]: https://github.com/ttizze/remote-agent/blob/4def0f2bce7b834812c3b77cc436860a5239d06a/crates/agent-core/src/session.rs
[service]: https://github.com/ttizze/remote-agent/blob/4def0f2bce7b834812c3b77cc436860a5239d06a/crates/host-daemon/src/host_rpc/service.rs
[actor]: https://github.com/ttizze/remote-agent/blob/4def0f2bce7b834812c3b77cc436860a5239d06a/crates/host-daemon/src/host_rpc/session_actor.rs
[claude-process]: https://github.com/ttizze/remote-agent/blob/4def0f2bce7b834812c3b77cc436860a5239d06a/crates/host-daemon/src/claude/process.rs

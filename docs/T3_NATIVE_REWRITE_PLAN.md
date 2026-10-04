# BexのT3準拠ネイティブ再実装計画

2026年10月5日。実装と検証を進めるための計画。全機能の再現とNightly配布を完了するまで、部分実装を完了扱いにしない。

## 確定した要件

T3 Codeの機能と動作を基準に、Rustのサーバー・共通クライアント層とネイティブUIでBexを再実装する。既存コードの大部分を置き換えてよく、既存の会話管理や表示仕様を固定条件にしない。各ネイティブアプリでの操作結果、復旧時の挙動、機能の利用条件を再現する。

- 通信はirohを維持する。QRによる初回接続、登録済み端末の再接続、アクセス取消しを維持する。
- デスクトップ、iOS、Androidを対象にする。Web版は作らない。T3サーバーへの接続互換性も不要。
- T3 Connectのクラウドログインとトンネルはirohの接続・リレーで置き換える。T3の外部サービスへ接続する実装は作らない。
- 既存のCodex・Claude会話を初回に自動取り込みする。元の履歴・認証情報は変更しない。
- 旧RPC、旧クライアント、旧会話管理との並行運用や互換経路は作らない。初回取り込みは製品要件であり、旧実装を残す理由にしない。
- Nightlyはビルドだけでなく、利用者への配布と更新まで実装する。
- 全クライアントの見た目もT3を基準にする。画面構成、配色、余白、文字、アイコン、各操作状態を照合する。
- 完成後もmainへマージしない。作業ブランチのPRまで作成し、Nightlyはその確定コミットから配布する。
- 計画を先に確定し、その後の実装・検証・配布も継続して行う。段階ごとの再承認は要求されていない。

## 参照するT3の版

基準は[T3 Code Nightly 0.0.46-nightly.20261004.2652](https://github.com/pingdotgg/t3code/releases/tag/v0.0.46-nightly.20261004.2652)。参照コミットは`4ee6bfd50ef4a089440d5c3662db2298da9cc50e`、公開は2026年10月4日18時23分UTC。以降の上流変更は別の差分として扱い、実装途中で完成条件を自動的に広げない。

主な一次資料は次のとおり。

- [責任分担と永続化](https://github.com/pingdotgg/t3code/blob/4ee6bfd50ef4a089440d5c3662db2298da9cc50e/docs/internals/overview.md)
- [操作・状態・イベントの契約](https://github.com/pingdotgg/t3code/blob/4ee6bfd50ef4a089440d5c3662db2298da9cc50e/packages/contracts/src/orchestrationV2.ts)
- [RPCの機能一覧](https://github.com/pingdotgg/t3code/blob/4ee6bfd50ef4a089440d5c3662db2298da9cc50e/packages/contracts/src/rpc.ts)
- [クライアント接続・同期](https://github.com/pingdotgg/t3code/blob/4ee6bfd50ef4a089440d5c3662db2298da9cc50e/docs/internals/connection-runtime.md)
- [providerの制約](https://github.com/pingdotgg/t3code/blob/4ee6bfd50ef4a089440d5c3662db2298da9cc50e/docs/internals/providers.md)
- [利用者向け機能文書](https://github.com/pingdotgg/t3code/tree/4ee6bfd50ef4a089440d5c3662db2298da9cc50e/docs/user)
- [Nightlyのビルド・配布](https://github.com/pingdotgg/t3code/blob/4ee6bfd50ef4a089440d5c3662db2298da9cc50e/.github/workflows/release.yml)

ソースから移植した部分にはT3のMIT著作権表示を残し、同梱する依存資産の通知を配布物へ含める。

## 全機能を追跡する方法

`docs/t3-parity.json`に基準コミット、利用者向け文書の機能、RPC、会話操作の一覧を固定する。これは実装済み機能の証明ではなく、見落としを防ぐ参照台帳である。実装時に各項目へBexの担当モジュール、対象クライアント、受け入れ条件、検証結果を対応付ける。

台帳の状態は未確認・設計済み・実装済み・検証済み・代替・対象外を区別する。検証済みには基準コミットと実際に確認した操作を記録する。対象外はWeb専用の配信面、T3への接続互換性、irohで置き換えた接続手段に限定する。デスクトップがT3のWeb UIを利用していることを理由に、その機能を対象外にしない。

| 機能群 | 再現する内容 |
| --- | --- |
| 初回設定と取り込み | PC接続、providerの検出と設定、プロジェクト探索、既存会話の自動取り込み、途中失敗からの継続 |
| プロジェクトとPC | 複数PC、同一リポジトリの分類、複数チェックアウト、PCを指定した操作、リソースに基づく新規タスク割当て、リモート専用デスクトップ |
| 会話一覧 | 作成、検索、ピン、並べ替え、既読・未読、アーカイブ、削除、完了整理、自動整理、snoozeとwake、参照・リンク |
| 会話表示 | メッセージ、思考、計画、ツールと出力、差分、添付、子エージェント、background work、失敗・再試行、履歴ページング |
| 入力 | 送信、実行中steer、停止して再実行、永続キュー、キュー編集・順序変更・削除・再開、モバイルのoffline queue |
| コンポーザー | 下書き、引用、送信履歴、過去入力の編集、prompt stash、ファイルと画像、context chips、コマンド・skills、音声入力 |
| 質問と承認 | 権限モード、要求の表示・回答、質問の添付、MCP/appアクセス、同時回答の調停、再接続後の未回答要求 |
| 高度な会話操作 | fork、merge back、provider・モデル変更、portable handoff、コンパクション、委任と完了通知、checkpointとrollback |
| providerとアカウント | Codex、Claude、Cursor、Grok、OpenCode、Antigravity、Pi、ACP registry、複数instance・アカウント、認証・更新・モデル・設定・skills・制限 |
| Gitとworktree | 新規プロジェクト、clone、publish、branch、worktree、setup、status、stage、commit、push、pull、差分、既定branchの更新、利用中worktreeの保護 |
| ソース管理サービス | GitHub、GitLab、Forgejo/Gitea、Bitbucket、Azure DevOpsのアカウント、PR/MR、レビュー・コメント・check・merge・閲覧済み・stack・watch・会話への紐付け |
| エディターとファイル | 一覧、検索、読書き、競合、外部エディター起動、workspace外の成果物、画像・動画・HTML・PDF |
| ターミナル | 永続するPTY、複数タブ、rename、resize、入力・出力、選択範囲のcontext添付、切断・終了・再接続 |
| ブラウザーとpreview | 共有ブラウザー、profile/session取り込み、タブ、ライブpreview、注釈と要素選択、agentによる操作、画面内・浮動表示 |
| 端末操作 | iOS Simulator・Android Emulatorの探索、起動、画面配信、入力、device tools、アクセシビリティ、権限・設定、SSH device host、agentアクセス |
| SnapShot | OS画面の選択、ショートカット、アプリのアクセシビリティ情報、添付とcontext化、OS権限と設定 |
| usage | 使用量・費用・期間・provider/model別集計、価格の上書きと対応付け、購読制限、複数アカウントの重複除去、hub、widget |
| 設定 | クライアント・PC・プロジェクトの所有範囲、継承・override・mixed、theme、motion、キー割当て、通知、background、省電力、診断、scheduled tasks、保存領域の整理 |
| 運用と更新 | Hostサービスの登録・停止・状態確認、provider更新、Host更新、クライアント更新、失敗復旧、版の表示、Nightly・stableの分離 |

T3でモバイルに提供されないデスクトップ機能は、モバイルに新規拡張する前提にしない。対象クライアントはT3の実装と文書で確定する。UIはネイティブの入力・選択・スクロール・アクセシビリティを使い、機能の結果と状態遷移に加えてT3の見た目を合わせる。

### 全クライアントの外観

T3のデスクトップ・iOS・Androidをそれぞれ参照し、各画面の配置、sidebarやpanelの幅、toolbar、composer、message、activity、dialog、menu、設定、色、文字とアイコンの大きさ、余白、角丸を台帳へ記録する。上流のthemeと各画面のsourceを基準に、同じtheme・viewport・fixture内容の画像で照合する。T3のモバイルを単にデスクトップの縮小表示として扱わない。

通常状態だけでなく、初回設定、空一覧、接続中、offline、実行中、承認・質問待ち、失敗、キュー編集、添付、長い履歴、各panelと設定、light/dark、狭い画面、iPadを比較する。実ユーザーの会話や認証情報を参照画像・test fixtureへ保存しない。見た目の差分はCIの画像と手動レビューで確認し、要件上の差か実装不足かを記録する。

## 目標の責任分担

以下は実装する設計であり、既存Bexの説明ではない。

| 所有者 | 所有するもの |
| --- | --- |
| Hostの会話runtime | アプリのthread・run・attempt、provider sessionとの対応、要求、キュー、イベント、受付記録、実行待ち処理、設定とproject |
| provider adapter | native protocol、認証とaccount instance、モデルとcapability、native sessionの作成・再開・終了、providerイベントの変換 |
| HostのPC機能 | Git・ファイル・PTY・ブラウザー・device・SnapShot・成果物の配信と認可 |
| Rust共通クライアント層 | PC別接続、購読と同期、キャッシュ、操作の結果、表示用データ、下書き、通知の対象判断 |
| ネイティブUI | 描画、入力、IME、選択、フォーカス、スクロール、OS権限とlifecycle、配布経路固有の更新 |
| irohの接続層 | identity、QRでの信頼確立、接続、経路変更、リレー、ストリーム、暗号化と相手の検証 |

会話のアプリIDをproviderのnative IDから独立させる。driverはproviderの種類、instanceはアカウントと設定の所有単位とする。新たなproviderを追加しても、ドメインやUIにproviderごとの分岐を増やさない。

純粋な判断関数は現在の状態、操作、時刻、割り当て済みIDなど必要な値だけを受け取り、イベントと実行する処理を返す。DB・process・ネットワークを参照しない。Hostが判断結果の確定と副作用を担当する。

### 会話と操作の永続化

T3の設計に合わせ、SQLiteにイベント、表示用の状態、操作IDごとの受付結果、実行待ち処理を同じトランザクションで保存する。確定後に購読者へ通知し、workerがprovider・Git・ファイルへの処理を実行し、その結果をイベントとして戻す。

同一操作IDの再送は同じ受付結果を返す。同じIDに違う内容が来た場合は拒否する。受付済みは実行完了を意味しない。ネットワークの切断で結果不明になった操作を、別IDで自動再送しない。

providerのターン完了、background workの終了、checkpointや差分の確定を別の状態として扱う。Host再起動後は失われたprovider processに属する処理を検査し、実行済みか不明な処理を無条件に再実行しない。未実行のキューは順序を保持して停止状態から明示的に再開する。

購読は一覧と会話詳細を分ける。会話詳細は必要なページだけ取得し、Snapshotと再開cursorを一緒に管理する。未閲覧会話の全履歴を全クライアントへ送らない。

### 初回の自動取り込み

初回にPCで設定されたCodex・Claudeの保存領域を走査し、projectと会話を取り込む。新しい会話IDとnative sessionの対応を保存して、取り込んだ会話を継続できるようにする。provider/accountの保存領域を跨いだ同名IDを混同しない。

会話単位で本文・進捗・元identityをトランザクションで確定する。中断後は確定済みの会話を重複取り込みせず、未完了のものを再開する。一部の壊れた会話や利用できない保存領域が他の取り込みを止めないようにし、スキップ理由と再試行を提供する。走査量・ページ量・ファイルサイズに上限を設け、一括で全メモリに載せない。

T3の既存importは直近30日・最大200メッセージなどの制限があり、tool activityと添付を省略する。今回の自動取り込みはユーザーの追加要件として、既存Bexで表示している履歴と継続に必要な情報を対象とし、履歴を黙って200件に切り詰めない。読み取れない本文や添付は欠落を表示し、成功扱いにしない。

取り込み完了後、Bexの履歴表示はアプリのDBを正本とする。providerの履歴を毎回読み直す旧経路と、新しいイベント管理を並列の正本にしない。元のproviderデータは保持し、provider再開に必要なnative sessionだけadapterが使う。

### irohとQR接続

既存の長期identityとペアリング済み端末を保護する。QRに含む情報、招待の期限・一回限りの消費、取消し、接続先の取り違え防止を維持する。旧クライアントと新Hostを混在させず、版が合わない場合は理由と更新先を表示する。

会話RPC、ライブイベント、成果物、端末・ブラウザー映像をirohのストリームで運ぶ。制御・大容量本文・映像の待ち合わせを分け、承認・停止・入力を大容量転送で待たせない。経路と購読の復旧責任を共通層に置き、ネイティブUIが独自の再接続ループを持たない。

## 実装順序と完了条件

| 段階 | 実装すること | その段階の完了条件 |
| --- | --- | --- |
| 0 | 参照版・機能台帳・新しい契約・速度比較の測定条件を固定する | 機能群と操作を担当箇所・対象クライアント・検証に対応付け、Webと接続方式の例外を明示する |
| 1 | 会話ID・driver/instance・thread/run/attempt・純粋な判断・SQLite確定・受付記録・実行待ちworker・初回取り込み | 再起動・重複操作・部分失敗の実経路を検証し、取り込み済み会話を新しい正本から読める |
| 2 | Codex・Claudeとirohで、新規作成から送信・ライブ表示・承認・停止・再接続・履歴再読込までを全ネイティブクライアントにつなぐ | 同じHost・同じ会話状態を3クライアントから使え、対象範囲の旧会話管理・読み直し・表示変換を削除する |
| 3 | 残りのprovider、instance・認証・モデル・権限・永続キュー・fork/merge/handoff・コンパクション・委任・checkpoint | capabilityとnative contractを確認し、provider変更・background完了・要求の復元・rollbackを検証する |
| 4 | Git・ソース管理サービス・editor・terminal・preview/browser・device・SnapShot・usage・scheduled tasks | 台帳のHost操作をUI・agent toolsから実行でき、所有権と切断・失敗後の継続を検証する |
| 5 | 一覧・検索・整理・snooze・入力・添付・通知・widget・設定・theme・キー割当てなど各クライアントの機能と外観の差を解消する | 対象となる全項目に実装と受け入れ結果が対応し、旧UI・旧state・重複処理を除去する |
| 6 | Nightly/stableの識別、署名・package・更新・自動配布、初回Nightlyを公開する | 同一コミットの全対象成果物を配布し、ダウンロード・署名・版・更新・TestFlightの利用可能状態を確認する |

配布基盤の構築は段階2以降に進められるが、全機能の再現完了と初回の完成Nightly配布は台帳の検証完了後に判定する。開発用の部分ビルドを全機能版として公開しない。

各段階は操作の入口からHost・共通状態・UIまでをつなぐ。未使用の新crateや型だけを先に増やさない。置き換えた責務の旧実装は、その段階で呼び出し側も移行して削除する。

## irohとT3の速度比較

irohは維持する。調査は速度優位を確認するためのものであり、結果に応じて勝手に通信方式を変更しない。[同一PCの転送比較](IROH_T3_TRANSPORT_COMPARISON.md)ではWebSocketが速かった。irohが常に速いという結論は出ていない。T3全体の実装やインターネット経路は別に検証する。

まず同じ機械・同じ暗号化条件・同じ処理・同じpayloadで、iroh/QUICとHTTP/WebSocketを比較する。RPCのエンコード方式の差と経路の差も分けて記録する。T3のEffect RPC実装を含む測定と、転送だけの測定を混同しない。

| 条件 | 測定するもの |
| --- | --- |
| 同一PC・LANの直接接続 | 初回接続、認証、再接続、warm RPC、stream更新の遅延 |
| インターネットの直接接続と中継接続 | irohのdirect/relayとT3 Connectのトンネルを経路別に比較。設定済みの到達可能な検証環境がある場合に測定 |
| 小さい制御要求 | 応答のp50/p95/p99、timeout、エラー率 |
| 本文・成果物・映像 | 同じサイズ・同じ内容の転送時間、throughput、CPU、memory |
| 制御と大容量転送の同時実行 | 承認・停止のp95、表示更新の停滞、転送間の干渉 |
| ネットワークの切替とbackground復帰 | 復旧時間、欠落と重複、結果不明な操作の扱い |

測定する版、端末、接続経路、暗号化、payload、並列数、warmup、試行数、生データを保存する。順序を入れ替えて複数回測定し、中央値と分布を比較する。localhostの結果を、外出先のiPhone接続やT3アプリ全体の性能へ一般化しない。

利用中Host、他セッション、OS全体のネットワーク設定には手を加えない。隔離された測定用Hostを使う。実際の外部経路が使えない条件は未測定と明示する。

## Nightlyのビルドと配布

参照T3のNightlyはスケジュールと手動実行に対応し、同一コミットの重複公開と公開順の逆転を防ぐ。自動実行は30分ごとに確認し、変更と前回公開から6時間以上の間隔を条件にする。これをBexの再現する既定動作とする。

- 今回はPRの確定コミットを一つ選び、Host・desktop・iOS・Android・CLIとmanifestを同じSHAから作る。必須CIの成功前には公開しない。PRをmainへマージしない。
- バージョンにchannel・UTC日付・単調増加するbuild番号を持たせ、公開後は上書きしない。Nightlyはstableの更新先へ混ぜない。
- デスクトップ・Host・CLIはGitHubのprereleaseで配布する。対象OSとarchitectureはT3の配布と現行Bexのbuild supportを突き合わせ、欠けた対象も台帳へ登録する。
- macOSは署名・notarization・stapleを行う。WindowsとAndroidも継続して更新できる署名identityを使う。ダウンロードにchecksumと署名検証を用意する。
- iOSは既存のBEX Dev (`com.ttizze.b-codex.dev`) のTestFlightへ配布する。Appleの`VALID`と内部グループの`IN_BETA_TESTING`を両方確認する。
- Androidはまず署名済みNightly APKを同じGitHub releaseで配布する。ストア経路を使う場合は設定済みのアプリ・トラックを確認して追加する。APKの公開とPlayへの公開を混同しない。
- デスクトップ・Hostの更新は署名とrevisionを検査する。利用中のprovider実行と未回答要求を確認し、更新のために勝手に作業を止めない。更新失敗で現行実行ファイルを失わないようにする。
- リリース処理を直列化し、build・署名・アップロードの途中失敗から同じreleaseを安全に再開する。最新版の案内は配布・処理が完了した対象だけ更新する。
- 秘密鍵・証明書・ストア認証は既存のKeychainやCI secretsを使い、ソース・ログ・test data・Nix storeへ保存しない。

GitHubのscheduleはdefault branch上のworkflowを実行するため、今回のPRの定期実行は利用者が将来マージした後に有効になる。mainを変更せずに定期実行を有効化したと報告しない。今回のNightlyは、PRのSHAで必須CIを実行し、そのSHAの成果物を手動起動の配布工程で公開する。

配布の完了には、workflow作成だけでなく、実際の公開URL・全成果物のrevision・署名・checksum・iOSグループ利用可能状態・更新の検証結果が必要である。利用できない署名identityやストア権限が判明した場合、その配布先を未完了として具体的に記録する。

## 検証と統合

通常のテストにはproptestを含める。純粋な状態判断と、DB確定・provider process・iroh購読の境界を区別して検証する。失敗後の状態、同時操作、再起動、同じ操作の再送、古い購読、遅れて届く結果を受け入れ条件に含める。

変更した分岐・状態遷移はcargo-mutantsで範囲を絞って監査する。Kaniは小さく重要な純粋関数に明示したboundsで適用する。fixture自身や新実装をそのまま写した二つ目の実装をテストしない。

ローカルでは各統合前に`scripts/dev-env.sh just unit-tests`を実行する。重いUI受け入れはPRのNative clients CIで実行し、現在のコミットの失敗を修正する。ローカルE2Eやbackground QAを自動追加しない。速度測定は依頼された調査として別に実行する。

既存の会話表示contractは、新しいT3準拠の要件と照合して置き換える。期待値変更にはT3の操作・表示に基づく理由を記録し、失敗したテストを通す目的でassertionを弱めない。

作業は現在のworktreeで行う。mainは変更せず、作業ブランチをpushしてPRを作成する。ユーザーの最新指示がAGENTS.mdの直接main統合より優先する。別セッションの変更を保護し、force push・squash・rebaseを使わない。稼働中Hostはビルドだけでは更新されない。実行ファイル・revision・active taskを特定し、置換と検証を別の工程として扱う。

各置換後に、変更したコードと全callerを繰り返しレビューする。古い実装、未使用定義、二重の状態、重複規則、不要なwrapperやforwardingを除去する。全レビューで不要なコードがなくなった後に最終動作を検証し、修正が必要ならcleanupから繰り返す。

## 全体の完了条件

1. 固定したNightlyの全対象機能に、実装・対象クライアント・受け入れ結果が対応している。未確認やstubが残っていない。全クライアントの外観をT3の対応画面と照合し、差を解消している。
2. irohとQR接続、登録端末の再接続・取消しが新しい会話runtimeで動作する。
3. 初回の既存履歴取り込みを、中断・再起動・部分失敗・再実行・継続送信まで検証する。
4. 置き換えた旧会話管理・旧RPC・旧UI・重複処理を削除している。
5. 必須ローカル単体テストと現在のPRコミットのCIが成功し、作業ツリーがcleanである。mainへのマージは行っていない。
6. 速度比較の結果と測定できていない条件を記録し、iroh優位を未測定の経路へ一般化していない。
7. 同一SHAのNightlyを実際に配布し、公開先・署名・更新・TestFlightの利用可能状態を検証している。

段階1と外観の共通部分を実装中である。Host所有の会話ID、SQLiteの履歴・イベント・送信受付記録、再起動時の実行と送信結果の扱い、Codex/Claude履歴の初回取り込みを実経路につないだ。段階1全体は未完了であり、provider instanceの独立化と永続実行キューは残っている。

一覧の読み込みは全履歴の取り込み完了を待たず、取り込みはページごとに確定する。ページ間では会話の操作ロックを解放する。元のproviderデータは保持する。取り込み後の本文はHostのDBから読む。

T3の既定配色をRust共通層へ置き、desktop・iOS・Androidへ接続した。モバイルにはT3と同じDM Sans 400/500/700を組み込み、本文の標準サイズを16へ合わせた。全画面の構成・操作・スクリーンショットの照合は未完了である。

ローカルの全単体テストは417件が成功した（3件は既存のskip）。これはネイティブ受け入れ・PRコミットのCI・配布の成功を示さない。保存失敗の通知抑止、取り込みの再開、履歴に含まれる実行とHost所有実行の区別、送信受付の再送と結果不明の扱い、送信元の切断後もHostが実行を所有すること、差分更新が該当itemだけを変更することを検証している。iOSのappとUI test bundleは実行せずにbuild-for-testingを通した。

履歴DBのfocused cargo-mutants監査は36件中32件をtestで検出し、4件はbuild不能だった。未検出・timeoutは0件。`import_page`、`read_page`、`apply_change`、受付の再送照合、native IDの境界、手動titleの保持を対象に、`--jobs 1`で実行した。先行した並列実行は絶対パスの`CARGO_TARGET_DIR`を共有したため、結果を採用していない。

irohとTLS WebSocketの隔離された比較ツールで測定を2回行い、[条件・結果・生データ](IROH_T3_TRANSPORT_COMPARISON.md)を記録した。T3のEffect RPC実装とインターネットのdirect/relay経路は別の測定対象であり、raw loopback測定で代用しない。Nightlyはまだ公開していない。

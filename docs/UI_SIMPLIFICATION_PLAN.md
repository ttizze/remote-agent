# クライアント簡素化計画

既存の操作・表示を維持し、重複した判断と不要な同期経路を取り除く。実施者はこの会話のエージェント。完了した項目は文書から除き、全体の完了時に削除する。

## 判断の基準

- 可変な事実の所有者を一つにする。共有する不変 Snapshot や導出済み presentation の参照は、それだけでは状態の二重管理ではない。
- 共通の判断は core、ウィジェット・フォーカス・IME・OS 操作・ネイティブ資源の寿命は各アプリが持つ。単にファイルや型を移して層を増やさない。
- 必要な値を明示して純粋な投影に渡す。設定の保存や接続などの副作用は資源の所有者が行う。
- Arc の同一性と `PartialEq` で導出結果を再利用する。フラグ・モデル・channel・フィールドの数を減らすこと自体を受け入れ条件にしない。
- テストが検証する製品の動作と、現在の実装方法を数える検査を区別する。後者を理由に既存構造を固定せず、期待する動作・性能を確かめる検査に置き換える。実装を通すためだけに期待値を緩めない。

## 先に維持すべき契約

- 入力・ストリーミング・アニメーションで無関係な画面を再描画しない。会話の長さに比例する投影を入力や無関係な通知のたびに実行しない。
- `view.rs::cache_tests` の描画回数・通知回数の検査を維持する。`projections` は現状では `set_model` / `sync` の呼び出し回数であり、実際の計算量ではない。その回数だけを性能要件として扱わない。
- native 入力の未保存 revision、IME の編集中テキスト、送信直前の内容を保持する。core の保存済み draft と入力ウィジェットの値が一時的に異なることは正常。
- 同じ draft key の Host 切替、再接続、ペインの閉鎖、遅れた完了通知でも、新しい入力・エラー・録音状態を古い結果で上書きしない。
- 会話行の順序、繰り返す native ID、完了時の操作ボタン、fork 先、時刻・進捗・最終行の更新を維持する。
- ドッキング表示の設定と一時的な details popover は別の事実として保持する。画面幅の変更で一方の設定を失わない。
- 接続前・接続失敗時にも設定と言語・エラー表示を利用できる。複数ウィンドウ・複数 Host の設定更新を失わない。

## 段階 5: desktop のウィンドウと会話ペインを分離する

段階 1・2 の共通 presentation を利用する。core の正規化とは別の変更として検証する。

- sidebar、接続一覧、アプリ更新と設定画面のワークフローはウィンドウが所有する。conversation、composer とその draft に依存するモデル選択・quick controls、workbench、selection、media、dictation、ファイル編集は会話ペインが所有する。現在の `Preferences` に混在する設定画面と composer の責務を分離する。
- ウィンドウの設定画面は明示的な Host / 設定対象に固定して bind し、会話ペインのフォーカス移動だけでは bind し直さない。アカウント操作の開始時の draft など必要な発生元を保持する。各 composer のモデル選択は自身のペインの presentation を読み、自身の draft に書く。native の `account.busy` による無効化は操作の発生元ペインに紐づく workflow を入力にし、主ペインの操作中という理由だけでサイドチャットを無効化する Host 全体の busy は追加しない。core の共有 account login 状態による無効化は既存どおり保持する。
- サイドチャットも同じ会話ペインを使い、2 個目の Desktop と不要な `side_chat_mode` 分岐を削除する。
- header のような選択中ペインに依存する表示は、その参照先を明示する。フォーカス、ショートカット、引用・説明、表示中ペインの切替を保持する。
- テスト fixture は `#[cfg(test)]` の専用コンストラクターから背景の Hosts / 更新監視を開始しない依存を注入する。side-chat mode を兼用して起動を抑止する構造は除く。型や `Option` をなくすために実際の任意性まで消さない。

受け入れ: サイドチャットの開閉、別ペイン間の入力・ナビゲーション・設定、非表示時の録音停止、閉鎖時の購読・資源解放と既存の描画キャッシュ検査が通る。worktree 保存中・account polling 中に主ペインとサイドチャットのフォーカスを切り替えても、未保存の入力と処理のフラグを失わず完了を受け取る。サイドチャットのモデル選択が主ペインの draft を読み書きしない。

## 段階 6: native の非同期完了を必要なものに絞る

段階 5 の所有者に揃え、GPUI の既存の task / entity 更新機構を優先する。

- Store の結果は presentation の購読で表示する。完了通知がその内容を再配信するだけなら削除する。
- 入力 revision の確認、録音、OS ダイアログ、接続・設定保存など、native の状態更新に必要な完了は所有者で扱う。
- enum の代わりに boxed closure を独自 channel に流す設計を前提にしない。型消去、Send 制約、別のイベントループを新たに増やさない。
- epoch を削除すること自体を目的にしない。Host / 接続 / ペイン / draft / operation generation / 入力 revision のうち必要な識別を保ち、別 Host の同じ draft key や再接続前の結果を拒否する。
- `Preferences` と `Hosts` はそれぞれの資源の寿命を持つ。すべてを一つの channel にまとめるための中継層を作らない。

受け入れ: 古い完了が新しい入力を変更しない検査と、snapshot のバーストをフレーム単位に集約する検査、接続失敗・保存失敗・閉鎖後の完了の検査が通る。

## 段階 7: iOS の会話ペインと Host 一覧の更新経路を整理する

対象: `AppViewModel.swift`、`AppViewConnection.swift`、`ConversationModel.swift`、`NshConversationView.swift`、`WorkspaceScreen.swift`、`ConversationSideChat.swift` と core の `client/session` API。段階 2 の行投影を利用し、Host 一覧と会話のそれぞれが必要な表示だけを更新する。

1. 主会話は `ClientSession` が持つ、ready 前の Intent の順序付き受付と、受け付けた操作を待つ `flush` の境界を保持する。サイドチャットは作成済みの pane の寿命を所有する。この違いのために必要な session / pane の経路を、重複という理由だけで削除しない。API を揃える場合も主会話の受付は ClientSession の既存の queue / operations 追跡を通す。主会話の画面を閉じても Host 一覧の ClientSession を終了せず、サイドチャットの終了ではその pane だけを retire する。
2. `AppViewModel` は Host 一覧・接続表示・アプリのナビゲーション、`ConversationModel` は会話ペインの表示と入力を所有する。不変 Snapshot / Presentation の参照や、別の表示に必要な設定・接続情報を読むことまで重複状態と扱わない。同じ `changes(previous:)` を何回呼ぶかは製品要件にせず、独立した書き手・重い重複投影・不要な再配信を取り除く。
3. pane の購読、Host 一覧の購読、操作受付直後と receipt 後の読み直しを用途別に確認する。core の受付状態、操作中の無効化、画面遷移、質問回答などが必要な時点で反映されることを確認してから、同じ状態を再配信するだけの経路を削除する。`BufferedTextInput` があることだけを根拠にすべての即時反映を削除しない。
4. ワークベンチの状態と browser / terminal / side-chat の native 資源はペインが所有する。draft → session の付け替えは既存の `Snapshot.submittedSession` と `Panels.rekey` を使う。desktop / iOS に残る共通判断があれば、必要な値だけを受け取る core の純粋な処理にまとめ、置き換えられた判断を両方から削除する。共通の規則があることを理由に native 資源や Panels の全状態を Store へ移さない。
5. `notice` は段階 1 と同じ発生元・scope の通知モデルに揃える。プロファイル / 端末設定の保存失敗はアプリ単位、接続 / Host 固有状態の保存失敗は Host 単位、会話操作は pane 単位とする。ClientSession の永続化失敗は Host 一覧の所有者が一度だけ観測して通知し、会話モデルで重ねて取り込まない。`threadLoadState` は一覧の読み込みと失敗を表す値から導き、無関係なローカル通知によって一覧の状態を変えない。接続前、Host 切替後、永続化失敗時にも通知を保持・dismiss できる所有者を明示する。
6. 保存する Host プロファイルと実行中の `ClientSessions` の map は別の事実として保持する。未接続の登録先を失わず、ticket の保存は現在のプラットフォームの保存契約を維持する。iOS / Android の接続処理は、既存の core が所有する再試行・接続有効化・active 状態の API を使う。共通判断が実際に重複している部分だけ core に移し、フォアグラウンド / ネットワークの OS 通知と UI の Task の寿命まで一律に移さない。

受け入れ:

- 受付 / pane API を変更する場合は、ready 前の入力を受け付け、起動時の ShowThreadList / ShowRecentChats の後に受付順で適用し、復元失敗では全 queued receipt を失敗させて保存ファイルを保持することを core のユニットテストで検証する。`flush` が受け付けた主会話の操作の完了を待って保存することも検証する。
- 変更した pane の寿命と、移した接続判断は core のユニットテストで境界・失敗・復旧を検証する。
- 既存の Synchronization / History / Navigation / Pagination の XCUITest が Native clients CI で通る。`items=` の元データ件数、履歴の読書位置と仮想化を保つ。
- 会話を開く・戻る・サイドチャット・フォアグラウンド復帰・再接続で、入力中の draft と質問回答が保持される。主会話を閉じても一覧の接続・永続化が続き、サイドチャットの閉鎖は他の pane に影響しない。
- 長い会話への再表示で不要な本文の再処理・主スレッドの停止を増やさず、古い非同期処理の結果が別の会話を上書きしない。
- 非選択 Host の一覧更新が、選択中の会話の重い投影や描画を不要に更新しない。操作の受付・完了と core の購読だけで必要な表示が欠けないことを確認する。
- 共通の接続判断を core に移した場合は Android の同じ利用側も同じ変更で切り替え、pairing のキャンセル、接続失敗からの再試行、背景移行後の復帰、未接続プロファイルの保持を検証する。

## この計画に含めないもの

- iOS の `TaskLiveActivities` の token 管理と ActivityKit 連携は OS 連携なので native に残す。

- `account_request` の `AccountCommand` / `AccountReply` は provider の業務境界なので維持する。RPC の Call / Body を provider に渡して変換数だけを減らさない。
- protocol の optional uniffi feature は追加しない。ネイティブバインディングに依存しない現行の規則を維持する。
- desktop / mobile のアイコン表は T3 の別カタログに由来する意図的な差異なので統一しない。
- Host 全体の操作分類は実行・認可・更新中の資源保護を含む。単一の CallClass を protocol に持ち込んだり、横断的な排他を個別資源へ動かしたりする変更は、UI 簡素化と分離する。
- CI・リリース補助ツールの言語統一と `dev-env.test.mjs` の削除は行わない。環境準備の契約を検証するテストは、使用言語を理由に削除しない。

## 実施と検証

- 現在の作業ツリーで、各段階を独立して検証・統合できる変更に分ける。他セッションの変更を保持する。
- 各段階で置き換えられた実装と全利用者を確認し、不要な状態・転送・重複を削除する。削除後に影響先を再確認し、不要なものがなくなるまで繰り返す。
- 変更中は `scripts/dev-env.sh cargo nextest run --locked -p <crate>` と `scripts/dev-env.sh just complexity` を実行する。Host の検証に必要な supervisor と SDK は既定のビルド手順で用意する。
- 分岐・境界・状態遷移の変更は対象を絞った cargo-mutants で検査する。無関係な mutant の追及や、mutant 用の回帰シード保存は行わない。
- 統合前に `scripts/dev-env.sh just unit-tests` を一度実行し、指定の統合前レビューを通す。共有 Rust の実行は設定済みビルダーを使う。
- main への統合・push はリポジトリの手順に従う。Apple のビルド・受け入れ確認は Native clients CI が担当し、ローカルの重い QA は追加しない。
- 完全な検証の報告には、対象コミットの Portable checks、対象コミットを含む main の Native clients、作業ツリーの状態を確認する。

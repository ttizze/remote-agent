# モバイルアプリ基盤リファクタ仕様

## ステータス

- 対象: 共通 Kotlin、Android、iOS
- 方針: Project 関連機能とは分離し、Project 以外の基盤から着手する
- 互換性: 既存のペアリング情報、Host Profile、Mobile Cache を維持する
- 挙動修正: 再接続競合と、未知イベントの永続化不一致をリファクタの受け入れ条件として修正する

## 着手条件

- 現在の Project 関連の未コミット差分は、リファクタ実装とは別に保存、レビュー、または統合し、同一コミットへ混在させない。
- リファクタ開始時点の共通 Kotlin テスト結果と、Android/iOS のビルド可否を基準として記録する。
- Android SDK の構成と iOS linker option error を解消し、各 platform を変更するコミットでは少なくとも対象 platform の compile と自動テストを実行できる状態にする。
- 永続化実装を変更する前に、実在する旧 Android/iOS 形式から秘密情報を除いた fixture を固定する。

## Problem Statement

モバイルアプリは、共通 Kotlin の状態管理を Android Compose と iOS SwiftUI から利用する構成になっている。一方で、現在は次の責務が少数の大きなモジュールへ集中、またはプラットフォーム間で重複している。

- アプリケーション制御が、接続、購読、再接続、Codex 操作、イベント順序制御、状態更新、永続化まで担当している。
- Android と iOS が、Codex リクエストの組み立て、ページング、レスポンス投影、作業ディレクトリの記憶などをそれぞれ実装している。
- Android と iOS の永続化方式が異なり、同じ Mobile Cache 契約を完全には保証できていない。
- 接続の購読 callback には世代確認があるが、一覧取得、Thread 読み込み、Turn 操作などの遅延応答には一貫した世代確認がない。
- 同一 Host への接続処理と native handle の交換が直列化されておらず、接続世代と実際の transport 所有権が食い違う可能性がある。
- Thread の作業ディレクトリが platform gateway 内部の可変 map にも保存され、共通状態と二重管理されている。
- 未知イベントの拡張フィールドを Android の永続化が欠落させる一方、iOS は保持するため、復元後の情報量が異なる。

この状態では、Codex API の変更や再接続処理の修正を両プラットフォームへ安全に反映しにくい。また、テスト可能な決定ロジックと、native transport やファイル I/O のような効果が同じ責務に混在しているため、競合や部分失敗の契約を検証しにくい。

## Goals

1. Codex リクエストとレスポンス投影を共通 Kotlin の一つの契約へ集約する。
2. Host ごとの接続、購読、世代、read barrier、native resource 所有権を一つの明示的なライフサイクルとして管理する。
3. アプリケーション制御を、ユーザー intent の調停、状態遷移、永続化の起動に集中させる。
4. Mobile Cache の符号化・復号を共通化し、Android と iOS で同じ情報保持契約を保証する。
5. 既存データを失わずに新しい永続化形式へ移行する。
6. 同一 Host の操作順序を安全に保ちながら、異なる Host の並行操作を阻害しない。
7. 各コミット後にビルド可能かつ既存のユーザー操作が成立する、小さな段階で移行する。

## Success Criteria

- 既存ユーザーのペアリング情報、Host Profile、選択状態、表示可能な Mobile Cache がアップデート後も読み込める。
- 移行中に旧データの読み込みまたは新形式の保存に失敗しても、既存ファイルと secure storage の鍵を削除しない。
- 接続解除または再接続より前に開始した非同期処理は、その後の状態、cache、notice を更新しない。
- 同一 Host では接続開始、handle 交換、購読交換、切断が直列化される。
- 異なる Host の接続と Thread 操作は互いを待たずに実行できる。
- Thread read 中に届いた同一 Thread のイベントは、snapshot 適用後に到着順で反映される。別 Thread または旧接続世代のイベントは混入しない。
- 後続 Turn の作業ディレクトリは共通状態の Thread 情報から明示的に渡され、platform gateway に隠れた複製を持たない。
- 未知通知、未知 Item、拡張フィールドは、受信、cache 制限、永続化、復元の全経路で Android/iOS とも同等に保持される。
- Android と iOS は同じ Codex request builder、response parser、pagination 規則を利用する。
- 既存の Project 表示、新規 Project Task、Project 順序、Thread の Project 所属には、本リファクタによるユーザー可視の変更がない。
- Compose と SwiftUI に公開している state、action、callback は、段階移行中も既存 UI が変更なしで利用できる。
- 新しい codec は現在モデルに含まれる Project 関連フィールドも欠落させずに往復させるが、その意味、生成、表示、並び順は変更しない。

## Solution

### 1. 共通 Codex クライアント

Codex のメソッド名、parameter の組み立て、response の検証、ページング、既知モデルへの投影を共通 Kotlin に移す。ここでは platform API や native handle を参照しない。

プラットフォーム実装は、接続済み transport に raw request を渡し、raw response を返す責務だけを持つ。QR、secure storage、mDNS、native library のロード、native handle の生成と破棄は引き続き platform adapter が所有する。

共通 Codex クライアントは次を明示する。

- ページごとの上限と全体上限
- cursor が進まない場合の停止条件
- ID 重複時の採用規則
- 必須フィールド欠落または型不一致時の失敗
- `thread/read`、`thread/resume`、`turn/start`、`turn/interrupt` の順序と parameter 契約
- raw notification、server request、未知フィールドを失わない境界

Project API の request、projection、画面用 grouping は初期段階では既存実装を維持する。共通化によって既存 Project 契約を変えない。

### 2. Host セッションのライフサイクル所有者

Host 単位で、次の可変資源を論理的に所有する小さなセッション調停モジュールを設ける。セッションは一つの抽象 transport の寿命を所有し、platform adapter はその transport 内部の native handle と close 実装を所有する。

- 現在の接続世代
- 抽象 transport instance の所有権
- raw notification の購読
- 同一 Host の接続直列化
- Thread ごとの read barrier と bounded event buffer
- 切断時の cancellation と後始末

このモジュールは、接続世代を単調増加する値として扱う。接続、一覧取得、Thread read、Turn 開始、interrupt など、状態を更新し得る非同期処理は開始時の世代を保持し、完了時に現在世代と一致する場合だけ結果を適用する。

切断は先に世代を無効化し、その後で購読と native resource を閉じる。すでに callback が実行中でも、状態適用前の世代確認によって無効化する。close と request/poll の排他は platform transport が保証し、セッション調停側は close 済み resource を再利用しない。

同じ Host の接続変更だけを直列化し、Host 間で共有する大域 lock は導入しない。

### 3. アプリケーション制御の縮小

アプリケーション制御は次に集中する。

- UI から受け取った intent の入力検証
- 共通 Codex クライアントと Host セッションへの操作依頼
- reducer へ渡す action の生成
- repository 保存の起動
- UI observer への新しい state の通知

Codex JSON の解析、pagination、native handle 管理、購読世代、read buffer の実装詳細は持たない。状態遷移そのものは既存の純粋 reducer を維持し、効果の実行と分離する。

状態更新と repository 保存は一つの順序で実行する。observer は保存方式や transport を知らず、同じ App State だけを受け取る。

### 4. 作業ディレクトリの明示的なデータフロー

後続 Turn の開始に必要な作業ディレクトリは、選択中 Thread の共通状態から取得して操作へ明示的に渡す。platform gateway 内部の Thread ID から作業ディレクトリへの map は廃止する。

Thread 情報が cache 制限、破損、または同期未完了によって存在しない場合は、暗黙の既定値で操作せず、再読込可能な失敗として扱う。

### 5. 共通 Mobile Cache codec

永続化する App State と Mobile Cache に、共通 Kotlin の versioned codec を設ける。platform repository は、byte sequence の atomic read/write と保存場所だけを担当する。

移行は次の順序で行う。

1. 現行 Android 形式と現行 iOS 形式を fixture として固定する。
2. 共通 codec は新形式に加えて両方の旧形式を読み込む。
3. 旧形式を読み込めた場合は、メモリ上で現在モデルへ変換する。
4. 次回の正常な保存で、新形式を一時ファイルへ完全に書き、atomic replace する。
5. decode、encode、fsync 相当、replace のいずれかが失敗した場合、旧ファイルを保持する。

ペアリング秘密鍵は codec の対象に含めず、OS secure storage に残す。pairing ticket、transport handle、実行中 request、購読、read buffer は永続化しない。

cache 上限は decode 後にも必ず適用する。破損、未知 version、過大入力は、クラッシュや無制限 allocation を起こさず、安全な初期状態または読み取り可能な既存部分へフォールバックする。フォールバック時も secure storage と既存ファイルを自動削除しない。

### 6. エラーと部分失敗

- transport error と Codex application error を区別し、ユーザー向け notice へ変換する場所を共通化する。
- cancellation と stale generation は通常の無効化として扱い、後から failure notice を表示しない。
- 接続成功後の初期同期に一部失敗しても、接続 resource の所有権と再試行可否を明確に保つ。
- native close は冪等にし、二重 close や close 後の poll/request が resource を再利用しない。
- 永続化失敗でメモリ上の正常な状態を巻き戻さない。ただし秘密鍵、pairing payload、message 本文を記録せずに診断できる error surface を設け、次の保存で再試行できるようにする。新しいユーザー向け notice は追加しない。

Project に依存する `thread/start` と初回 prompt の部分成功契約は、初期段階で挙動を変更しない。共通基盤の移行完了後、Project 機能側の作業として別途扱う。

## Decision Document

- 共通 Kotlin を、状態、決定ロジック、Codex protocol 契約、永続化 codec の所有者とする。
- Android/iOS は、UI、lifecycle、QR、secure storage、mDNS、atomic file I/O、native transport の adapter を所有する。
- Host セッションは stateful resource owner として認めるが、用途別の pass-through service や汎用 manager は増やさない。
- Host セッションは Host ごとに独立し、大域 singleton の接続 lock を持たない。
- すべての非同期 state 更新は Host identity と接続世代の両方で検証する。
- read barrier は Host identity、Thread ID、接続世代で識別し、件数と概算 byte 数の両方で上限を持つ。
- Codex が Thread、Turn、Item、履歴、status、working directory の authoritative source である。
- Mobile Cache は非 authoritative な表示用 copy であり、再接続時に Codex の fresh read で調停する。
- 未知 protocol data は、明確な cache 上限内で保持する。
- 既存の reducer と UI state の外部契約は段階移行中も維持する。
- 永続化形式は versioned envelope とし、旧 Android/iOS 形式を明示的に migration する。
- secure storage の鍵と pairing 情報は cache migration から独立させる。
- セッション調停は抽象 transport の寿命を所有し、native handle の同期と破棄方法は platform adapter に閉じ込める。
- Project API、Project grouping、Project Task UI は初期スコープに含めない。
- codec は Project 関連フィールドを opaque な既存データとして保持するが、その contract を再定義しない。

## Commits

各コミットは独立してレビュー可能で、テストが成功し、アプリが起動できる状態を保つ。Project 関連の進行中差分とは混ぜない。

1. 現行 Android/iOS の永続化データを匿名化 fixture として固定し、Profile、選択状態、Thread、未知イベント、拡張フィールドの期待値を記録する。
2. 共通 codec の round-trip 契約テストを追加する。現時点では既存 repository 実装を変更しない。
3. 共通 codec に現行 Android 形式の decoder を追加し、fixture が現在モデルへ復元されることを確認する。
4. 共通 codec に現行 iOS 形式の decoder を追加し、同じ現在モデルへ復元されることを確認する。
5. versioned envelope の encoder/decoder を追加し、未知 version、破損、過大入力の安全な失敗をテストする。
6. Android repository を共通 codec へ切り替え、atomic file の所有権だけを残す。旧形式からの読み込みと次回保存時の移行を検証する。
7. iOS repository を共通 codec へ切り替え、atomic file の所有権だけを残す。旧形式からの読み込みと次回保存時の移行を検証する。
8. 未知イベントと拡張フィールドの永続化契約を両 platform で一致させ、既知の Android 情報欠落を修正する。
9. 非 Project 系 Codex operation の request builder と response parser を共通 Kotlin に追加し、platform gateway からはまだ利用しない。
10. `thread/list` の pagination、cursor 停止、重複 ID、件数上限の契約テストを追加する。
11. Android gateway の非 Project 系 Codex operation を共通クライアントへ委譲し、native transport と platform facility だけを残す。
12. iOS gateway の非 Project 系 Codex operation を同じ共通クライアントへ委譲し、Android と同じ contract suite を通す。
13. 後続 Turn の working directory を共通 state から明示的に渡すようにし、Android の隠れた Thread-to-directory map を削除する。
14. 同じ変更を iOS に適用し、cache 復元後と再接続後の Turn 開始を検証する。
15. 接続世代の判定を純粋な状態遷移として固定する characterization test を追加する。disconnect 後、reconnect 後、別 Host の結果を網羅する。
16. Host 単位のセッション調停モジュールを追加し、既存の購読 generation と cancellation を挙動変更なしで移す。
17. 同一 Host の connect、handle 交換、subscription 交換、disconnect を直列化する。異なる Host が並行できるテストを追加する。
18. list と read の完了結果に接続世代を適用し、旧接続の成功・失敗が新しい state を更新しないようにする。
19. Turn 開始と interrupt の完了結果にも同じ世代契約を適用し、stale failure notice を抑止する。
20. read barrier と bounded event buffer を Host セッションへ移し、snapshot と event の順序契約を維持する。
21. native close、poll、request の lifecycle contract test を Android adapter に追加し、close を冪等にする。
22. 同じ lifecycle contract を iOS adapter に追加し、複数 Host の handle が独立することを検証する。
23. アプリケーション制御から Codex parsing、pagination、native lifecycle、read buffer の実装詳細を削除し、intent 調停と state 更新へ縮小する。
24. Android の canonical UI flow を emulator で確認し、ペアリング、再接続、Thread 一覧、read、後続 Turn、interrupt の回帰を修正する。
25. iOS の同じ canonical UI flow を simulator で確認し、foreground/background reconnect を含む回帰を修正する。
26. 物理端末で QR、secure storage、local-network discovery、Host restart、複数 Host を確認し、検証記録と残る制約を文書化する。

## Testing Decisions

### テスト原則

- private state、呼び出し回数、内部クラス構成ではなく、返り値、保存データ、状態遷移、protocol message、resource lifecycle、ユーザー可視結果を検証する。
- 純粋な protocol、codec、cache、reducer、世代判定は共通 Kotlin の決定論的テストで網羅する。
- platform boundary は実際の atomic file、secure storage sandbox、native test transport、simulator/emulator を優先する。
- 外部 Host や camera が必要な面だけを bounded fake または手動確認にし、共通ロジックのテストを platform ごとに重複させない。
- sleep に依存せず、競合テストは明示的な barrier、controllable completion、timeout を用いる。

### 共通 Kotlin

- 旧形式 migration と新形式 round-trip
- 破損、未知 version、過大 cache、未知フィールド
- pagination、cursor loop、重複 ID、必須フィールド欠落
- Host ごとの接続世代と stale result rejection
- reconnect、disconnect、subscription replacement
- Thread read と live event の順序、別 Thread/別 Host の分離
- cancellation と stale failure notice の抑止
- working directory 欠落時の明示的失敗

### Android

- AtomicFile の旧形式読み込み、新形式保存、失敗時の旧ファイル保持
- Keystore の既存鍵再利用と cache migration からの独立
- JNI handle の Host 分離、close 中の request/poll、二重 close
- lifecycle による disconnect/reconnect
- emulator 上の canonical UI flow

### iOS

- atomic write の旧形式読み込み、新形式保存、失敗時の旧ファイル保持
- Keychain の既存鍵再利用と cache migration からの独立
- C FFI handle の Host 分離、close 中の request/poll、二重 close
- foreground/background transition と再同期
- simulator 上の canonical UI flow

### 物理端末

- QR camera permission と pairing
- local-network permission、mDNS、IPv4/IPv6
- secure storage を維持したアップデート
- Host restart と mobile reconnect
- 複数 Host、可能なら複数端末からの同時操作

## Out of Scope

- Project API の追加・変更
- Project と Thread の grouping、検索、折りたたみ、表示順の変更
- Project root 選択または新規 Project Task UI の変更
- `thread/start` と最初の prompt 間の部分成功 UX の変更
- Host Daemon または Mobile RPC framing の再設計
- relay、クラウド同期、web client、offline queue の追加
- cache の authoritative store 化
- UI デザインの全面刷新
- 全リポジトリの package/directory 再編
- 新形式保存後に旧アプリへ戻す downgrade migration

## Rollout and Recovery

- 永続化 migration を最初に独立して完成させ、接続ライフサイクルの変更と同じコミットに含めない。
- Android と iOS は、それぞれ旧形式 fixture、platform repository test、起動確認が揃ってから切り替える。
- 新形式の保存は atomic replace を必須とし、失敗時に旧ファイルを回復不能にしない。
- 接続世代の適用は operation ごとに広げ、各段階で既存 flow を維持する。
- platform gateway の共通クライアント移行は Android、iOS の順に一つずつ行い、両方を一度に切り替えない。
- release 前に simulator/emulator と物理端末の検証結果を分けて記録し、未検証面を明示する。

## Further Notes

2026-08-25 時点で、共通 Kotlin テストは 27 件すべて成功している。一方、Android SDK が現在の開発環境に構成されておらず、Android test は未実行である。iOS native test は現在の Xcode で linker option error が発生し、XCUITest まで到達していない。リファクタ着手前に、これらを「テスト対象の失敗」ではなく「検証環境の未整備」として分け、環境修復を前提作業として扱う。

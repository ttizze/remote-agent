# モバイルアプリの責務と保持契約

## 責務の境界

Mac とモバイルで共通のエージェント処理は Rust に置く。Kotlin はモバイル固有のライフサイクルと永続化・表示への接続を担当し、Swift と Android は UI と OS API を担当する。

対応 OS は最新の正式メジャーのみとし、現在は iOS 26 と Android 17（API 37）を最低バージョンにする。最低 OS を引き上げたら、不要になった互換分岐を削除する。

| 所有者 | 責務 |
| --- | --- |
| Rust `agent-client` | リクエスト構築、レスポンス検証、ページング、履歴取得、送信・再開・steer・queue、承認・質問への応答、文字起こし、アカウント、worktree、ファイル、差分、fork、画像一覧 |
| Rust `conversation-presentation` | イベント分類、状態遷移、履歴の統合、送信判断、承認フォーム、エラー表示、モデル・推論量の選択規則 |
| Rust `mobile-client` | 暗号化通信、C/JNI の接続 ID、実行中呼び出しの寿命、順序付きイベントキュー |
| 共通 Kotlin | 接続世代、モバイルの再接続、画面遷移、読み込み中のイベント保持、通知購読、保存スケジュール、versioned codec |
| Swift・Android | 描画、入力、スクロール、IME、録音、QR、ファイル選択、secure storage、atomic file I/O、薄い C/JNI 呼び出し |

UI フレームワークのオブジェクトは、描画・操作・購読の所有者として残す。エージェントのルールを持つ可変 service、manager、controller は設けない。共有処理を変更するときは両クライアントを移行し、古い実装や互換用の別経路を残さない。

## 状態と副作用

`MobileApp` は App State、設定、読み込み世代、実行中処理の参照、購読集合を持つ不変の値である。`MobileEffects` は通信、保存キュー、scope、同期用ハンドルを明示する。状態遷移は値を受け取り、新しい値を返す。公開、キャンセル、通信、保存は遷移の外で実行する。

`HostSessions` は Host ごとの接続世代、購読、読み込みバリアを表す。不変の値を atomic に交換し、その後で退役した購読をキャンセルする。更新関数は競合時に再実行されるため、内部で I/O や callback を実行しない。同一 Host の接続変更には専用の Mutex を使い、別 Host の接続を待たせない。

`IosViewProjection` は前回の投影値と現在の App State から計算する。変更されていない Turn と Item の表示値を再利用し、過去の投影キャッシュを書き換えない。通知本文だけの変更でナビゲーションやタイトル一覧を作り直さない。

Rust の会話状態と要求 ID の対応表は、共有可変オブジェクトを経由せず、所有または排他的に借用した値に対して遷移を計算する。要求 ID の対応付けはチャネルへの送信から分離する。キュー、ロック、タスク、ファイルなどの I/O 資源は、その操作を行う境界で所有する。

## Codex とネイティブ境界

Mac は `AgentClient` を直接呼び、モバイルは型付き `AgentCommand` を C/JNI へ渡す。両方が同じ Rust 実装を使う。Kotlin や Swift で Codex のメソッド名、ページング規則、承認レスポンス、モデル選択規則を再実装しない。

通知と server request は `RawCodexMessage` として保持する。Rust がイベントを分類し、Kotlin は分類済みコードと必要なメタデータを使う。イベントを別の型階層に変換して再分類する経路は設けない。

モバイルの本文はネイティブ側で保持する。Rust の遷移・表示判断にはメタデータと source index を渡し、その判断を Kotlin の保存形式へ適用する。デルタごとに会話全体を JSON へ変換しない。未知 Item、raw metadata、拡張フィールドを保持し、既知フィールドの更新によって落とさない。

接続は Rust が発行する再利用しない整数 ID で識別する。C/JNI の各呼び出しは接続を保持し、close は ID を退役させる。close 後の新規呼び出しは失敗し、すでに実行中の呼び出しは完了まで有効である。二重 close は無害で、古い ID が新しい接続を指すことはない。Android/iOS 独自の貸出数や close フラグは持たない。

モバイルの通知ポーリングは共通 Kotlin に一つ置く。接続ごとに一つの poller が順序付きキューを読み、その接続の現在の購読者へ配信する。購読者がいない間はキューを読み捨てない。

## 接続・履歴・送信の契約

- 非同期処理は開始時の Host identity と接続世代を保持する。旧世代の成功・失敗は新しい state、cache、notice を更新しない。
- 切断・再接続は世代を先に無効化する。購読のキャンセルが同期 callback を起こしても旧世代は適用されない。
- Thread read 中の同一 Thread のイベントは snapshot の後に到着順で反映する。別 Thread、別 Host、旧世代を混ぜない。
- 読み込みバッファには 256 件と `min(cache 上限, 256 KiB)` の概算サイズ上限を適用する。超過時は不完全なイベント列を適用せず、再読み込み可能な失敗とする。
- バッファの追加は既存イベントを構造共有し、本文をコピーしない。読み込み完了時に一度だけ到着順のリストへ変換する。
- 履歴ページは opaque cursor、時系列、すでに読み込んだ Item、ライブ更新を保持する。古いナビゲーションに対する応答を適用しない。
- start、resume、steer、queue の判断と実行は Rust に置く。working directory は Thread 情報から明示し、platform gateway に複製した map を持たない。
- `thread/start` 成功後に初回 prompt が失敗した場合も、作成済み Thread を保持する。再送で別の Thread を作らない。
- Host が受理した入力を保持し、後から到着する同じ client ID の user-message echo と照合する。
- Project 表示、順序、所属、新規 Task、検索、折りたたみの契約を共通化のために変更しない。

## 永続化と設定

App State と Mobile Cache は共通 Kotlin の versioned codec を使う。現行 envelope は `format`、`version`、`kind`、`payload` を持つ。format、version、kind、入力サイズを検証し、decode 後も cache 上限を適用する。

Host Profile、選択状態、Project フィールド、表示可能な cache、モデル・推論量の選択を保持する。接続ハンドル、タスク、購読、読み込みバッファ、pairing ticket、秘密鍵は保存しない。鍵と relay 資格情報は OS secure storage に置く。

モデル一覧、選択、読み込み世代は共通 Kotlin の設定値に置き、利用可能なモデル・推論量の判断は Rust を呼ぶ。アカウントや接続が切り替わった後に古い一覧の応答を適用しない。iOS の旧モデル選択は versioned App State に取り込み、保存が成功してから旧キーを削除する。

platform repository は保存場所と atomic read/write を担当する。破損、未知 version、過大入力、保存失敗で、正常なメモリ状態、既存ファイル、secure storage の鍵を削除しない。保存失敗を理由に新しいユーザー向け notice を追加せず、診断には例外の種類だけを残す。

ストリーミング中のデルタはメモリで保持し、完了または明示的 flush で保存する。保存は一つの worker で順序を守り、遅い I/O 中の中間 checkpoint をまとめる。flush は対象の保存処理が終了するまで待つ。

## 検証

検証は観測可能な状態、保存内容、protocol message、順序、資源の寿命を対象にする。private field やファイル構成だけを検証するテストは追加しない。

- Rust: agent 操作、イベント・履歴・送信の共通 corpus、要求 ID の所有者、最初の応答だけの採用、未解決要求の再配信、C ハンドルの退役と実行中呼び出し。
- 共通 Kotlin: 接続世代、再接続、read barrier、保存、部分成功、通知順序、設定切替、codec の round-trip と失敗。
- Android: JNI を通した共通 corpus、AtomicFile、Keystore、Activity のライフサイクル、エミュレータ上の UI。
- iOS: C ABI と Kotlin/Native、投影値の再利用と解放、Keychain、atomic write、Simulator 上の SwiftUI 操作。
- 実際の暗号化経路: 新しい隔離 pairing fixture で relay、Host、モバイル接続を起動し、期待する結果を assertion で確認する。

リポジトリの Nix 環境から次を使う。

```sh
nix develop --command cargo test --workspace --locked
nix develop --command gradle :apps:mobile:testAndroidHostTest :apps:mobile:iosSimulatorArm64Test
nix develop --command cargo xtask quality
nix develop --command cargo xtask ios-e2e
```

モバイル障害は headless 検証から始め、通過後に Simulator/XCUITest へ進む。`xcresult` の passed、failed、skipped を確認し、skip を pass と数えない。物理端末は明示的なユーザー承認がある場合だけ使用する。ビルド、headless、Simulator、Android UI、物理端末の検証結果を区別し、未検証面を明示する。

## 変更対象外

Project API の追加、UI の全面刷新、relay や RPC framing の再設計、offline queue、cache の authoritative store 化、全パッケージの再配置は、この責務整理には含めない。既存のユーザー作業を保持し、秘密情報と一時 fixture は Git に含めない。

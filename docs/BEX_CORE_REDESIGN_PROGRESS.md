# コア再設計の実装記録

この文書は2026年9月の設計・実装記録。現在の移行方針と進捗は[T3 native rewrite plan](T3_NATIVE_REWRITE_PLAN.md)、現在の会話所有と参照形式は[Session runtime](SESSION_RUNTIME.md)に従う。

2026-09-16 / macOS arm64 / Nixの固定環境。計画の第1〜第3段階を実装した。稼働Host・実機の置換はしていない。

基準HEADは `a76284c`。開始時の `4def0f2` 以降に別セッションがコミットした初回履歴読取・quality変更は保持している。以下はPR作成前の実装・検証記録であり、commit単位の必須quality結果はPRに記録する。

## 第1段階：本文取得と制御更新

対象は `agent-core/src/store.rs`、`state/operations/threads.rs`、`client.rs` とその退行テスト。

- `ReadItem` の順序付きRPC応答だけを既存のwire順序へ参加させ、本文は順序待ちを持たない `ResolveItem` effectへ引き継ぐ。inline項目は直ちに適用する。
- dispatchのreceiptを本文適用まで引き継ぎ、成功の意味を維持。同じ対象の呼び出しはreceiptを共有する。
- ItemのArc、購読ID、既存epochを使い、古い本文を適用しない。新しい完全なItemは保持し、deferredのままdeltaが来た場合は進行中の取得後に一つだけ再取得する。削除した項目は再生成しない。
- 接続単位で同時取得4件、対象合計132件、同一対象の待機呼び出し128件、本文期限120秒。既存のsize・digest・ID検査と転送経路を使用する。
- 接続を閉じると、その接続のeffect・取得管理も破棄される。Hostの履歴revisionや永続キューは追加していない。

検証は実iroh転送をbarrierで止め、別会話のText、承認の表示と回答、停止が本文完了前に進むことを確認する。同一itemの重複取得、delta後の再取得、誤ったID、digest不一致も同じ実経路で確認する。120秒の実期限中も停止RPCを続け、timeout後の制御接続を確認する。Item置換・再open購読・削除・epoch変更は共通状態のテストで確認し、既存の再接続・storage scope変更テストも維持する。

第1段階は `ReadItem` の制御応答と本文effect、Storeのreceipt継続・取得上限、その退行テストの順でレビューできる。共通型の移動は第2段階で行っている。

## 第2段階：provider契約と振り分け

対象は `session.rs`、`models.rs`、`peer.rs`、共通RPC契約、Host router、Codex/Claude adapter。

- Host受信境界で保存済みの文字列IDを `SessionRef` へ変換し、router・adapter呼び出しにはproviderとnative IDを明示する。予約prefixで始まるCodex native IDも往復できる。
- Claudeの実行記録・履歴検証はnative IDを使用。Codex通知・要求もadapterで明示的なSessionRefへ変換する。
- 未登録request・notification・生のprovider responseを拒否。`host/session/answer` の所有権・claimを通す。従来の未知RPCのCodex転送と、ターミナルのCodex `process/spawn` 転送を削除した。
- capabilitiesは各adapterが実装済み機能から返し、Hostもfork・rename・steer・queueを検査する。共通層のprovider別能力表を削除。
- 共通のthread status、history read state、要求delivery/native request IDを名前付きの型へ移した。未知のnative情報は補足データとして保持する。
- `Delivery::NotSent / Unknown` を送信結果の判断に使用。表示文言による `contains("unknown")` 等を削除した。送信後の切断・event pump停止・未知のnativeエラーはUnknown。nativeエラーは `providerError` として保存し、同名のdeliveryフィールドを未送信の証拠にしない。
- Hostが使用するRPC引数は既存 `client` 契約へ置き、Storeの `Operation` 実装を状態更新側へ残した。アカウント・Workspaceの引数も移行した。既存desktop設定読取の保存形式や表示ロジックの全面分割は今回の対象外。

### 公開操作の確認範囲

| 所有者 | 維持した経路 |
|---|---|
| セッション | list / start / open / item read / request open / answer / close / resume |
| 入力 | turn start / steer / queue / interrupt、実行中input IDの重複防止 |
| provider固有 | Codex fork / rename / model list / account、Claudeの明示的な未対応拒否 |
| Workspace | file list/read/write、blob upload/download、Git review、worktree設定/list/remove、visualization |
| Host | PTY、dictation、既存の接続・pairing・管理操作 |

wire golden fixtureには追加したSessionRef・capabilitiesだけを反映し、元の本文と表示条件は維持した。アカウントfixtureの非公開 `fixture/account/refresh` は直接fixtureへ送る。認証情報非漏洩・復元のassertionは維持した。forkボタンのテストは期待表示を変えず、fixtureにHost提供のcapabilitiesを追加した。

## 第3段階：HostのPTYと共通プロセス管理

対象は `bex-process`、`host-daemon/src/terminals.rs`、service、dictation、ビルド・同梱設定。

- Codex crateの共通supervisorを `bex-process` へ移した。Codex・Claude・PTYが共通の寿命管理を使う。既存のsupervisor実行ファイル名・隣接配置は維持する。macOS同梱・CI・Android/iOS fixture・通常qualityで新しいpackageからビルドする。
- `portable-pty = 0.9.0` とロックファイルでPTYを実装。Hostが接続、cwd、handle、入力、取消し、cleanup完了を所有し、既存RPCへ入出力・resize・killを接続する。
- 最大32PTY、入力キュー32件、supervisor出力キュー16件。Codexから来た旧process出力・終了通知も遮断し、Hostが所有するPTYへ混入させない。起動予約後の切断・起動future取消しもcleanupへ進む。他接続のhandle操作は拒否する。
- killの成功はcleanupと所有記録の解放後に返す。worktree利用記録はcleanup中も残る。既存のworktree排他・削除保護を維持する。
- Unixはlifetime pipe EOFでPTYセッション全体の終了処理を開始し、別のjob-control groupも終了対象にする。session IDの再利用を避けるため、後始末までshellをreapしない。WindowsはHostがJob Objectを所有するコードに接続したが、この作業環境では未実行。
- 音声入力のbackend所有・利用可否判定をdictationへ移した。**責任境界の分離済み、認識backendのCodex依存は残存**。別のAPIや認証は導入していない。

Codex未導入fixtureで、Claude送信・履歴再読込、PTY入出力・resize・kill・他接続拒否、ファイル一覧・revision付き読書き、Git review、worktree設定、dictation失敗後の継続を確認する。Codex途中終了時はPTYをRunningのまま維持するという計画の新要件をassertionへ反映し、Claude承認の既存条件は変えていない。Host強制終了でshell・background jobが残らないことと、PTY起動途中の切断も追加検証した。

## 検証結果

以下のコマンドはすべて `nix develop . --command` で実行。SDKの既存linker warningはあるが、実行結果と静的検査のエラーは区別して記録する。

| コマンド・範囲 | 結果 |
|---|---|
| `cargo build --locked -p bex-process -p host-daemon -p host-fixture -p bex-desktop` | 成功。最終ソースで通常実行ファイルをビルド |
| `cargo test --locked --workspace` | 成功。Rust 303件成功、2件opt-in ignored |
| `cargo test --locked -p agent-core --test store item_transfer_releases_wire_order_and_preserves_newer_items -- --exact` | 成功。120.93秒、本文期限の後も接続と停止操作を維持 |
| `cargo clippy --locked --workspace --all-targets -- -D warnings` | 成功。最終Host追補も再検査成功 |
| `cargo test --locked -p host-daemon --lib` | 最終追補を含む68件成功 |
| `cargo fmt --all -- --check` / `git diff --check` | 成功 |
| `scripts/build-agent-ios.sh simulator` | 成功。下記E2Eでも同じソースから再ビルド |
| `./gradlew :apps:mobile:assembleDebug --console=plain` | 成功。arm64/x86_64 Rustライブラリ・Kotlin・APK |
| `just ios-e2e`（以下4件） | 4件成功、0件失敗・skipなし |

選択したSimulator受け入れテスト:

- `testSimulatorSwitchesCodexAccountsAndForksConversation`
- `testSimulatorShowsAcceptedAdditionalInputBeforeCodexProcessesIt`
- `testSimulatorOpensTasksBeforeHistoryReadFinishes`
- `testSimulatorReopensCompletedHistoryCollapsed`

Host、desktop、FFIは同じ作業ツリーからworkspaceビルド・テストする。Simulatorは専用fixture Hostを起動し、稼働Hostを使用しない。desktopの既存会話表示37テストを維持する。

## 実装時点の制約と未実施

- Windows/Linuxでの実行とWindows Job Object cleanup、Android実機・emulator操作、iOS実機、全Simulatorテスト集合は未実施。
- 実認証のprovider推論、5分間の実時間passive approval soakは既存のopt-inテストであり、合格に数えない。
- 音声認識・CodexアカウントはCodexが必要。provider活動を確認できないworktreeの削除は引き続き保守的に拒否する。
- 配達不明入力・承認の自動再送や、Host再起動を越えるexactly-once保証は追加していない。
- 稼働Host再起動・実機インストールはしていない。実装時の検証結果をcommit後のquality結果として流用しない。

## 変更規模

集計はHEAD `a76284c` との差分。移動前後を対応付け、ファイル移動そのものを削減に数えない。Rust内の `#[cfg(test)]` 以降とtestsディレクトリをテストとして分ける。ユーザー提供の計画書を新規登録する行数は、この実装差分の集計に含めない。

| 区分 | 追加 | 削除 |
|---|---:|---:|
| 本体 | 1675 | 532 |
| テスト | 760 | 51 |
| ビルド設定 | 32 | 10 |
| lockfile | 107 | 18 |
| 文書 | 110 | 6 |

本体の増加は、既存supervisorに加えたPTY backend、Host側のPTY所有・認可・終了処理、および本文取得の有限キューとreceipt継続が中心。削除対象はCodexによるPTY起動・管理、未知RPCの転送、文字列での配達判断、共通層のprovider別capabilities。生成bindings・APK・ビルド出力はソース差分に含めない。既存の未追跡Androidディレクトリ、画像、動画は変更していない。

## PR #21レビュー後の修正（`ccde07d`への指摘）

- 一つの項目の取得枠を、制御応答・本文転送・適用完了まで保持する。継続処理を新規項目より先に実行し、wire順序待ちは従来どおり制御応答で解除する。Hostの転送予約上限8件は変更していない。
- staleな制御応答も、Hostが発行したgrantを既存のバイナリ転送で消費してから破棄・再取得する。本文を適用する際には元のArc・購読・epochを再検査する。
- PTYの後始末はLinuxの`/proc`、macOSのprocess APIから同じsession IDの生存プロセスを列挙し、終了を確認するまで再走査する。shellのHUP転送に依存しない。Hostはsupervisorを3秒で強制終了する処理を削除し、cleanup失敗時はkillをエラーにしてworktreeの所有記録を保持する。

追加した回帰テストは、実際のStore・iroh接続とHostの`WorkspaceFiles`予約管理を使う。制御応答だけをfixtureで制御し、Hostの本物の8枠制限とsingle-use token消費を検査する。

- 大容量のメッセージ・画像12件。先頭4件の転送を止めても、新規予約が増えず、会話delta・承認回答・停止が進む。最後は12件すべて内容一致、未消費grantは0件。
- ReadItemの制御応答より前にdeltaが適用されたことをbarrierで確認し、12回反復する。各応答のgrantを消費し、最後に最新の完全な本文を取得する。
- PTY内でshellとは異なるPGIDのbackground jobを2件生成する。明示kill・接続切断の完了時点で全jobが終了し、完了まではworktree保護が残る。Host強制終了も実際のRust supervisorと隔離したHost代替プロセスで検査する。

Linux検証はDebian bookwormの隔離コンテナ、arm64、`/bin/sh -> dash`（0.5.12-2）、`SHELL=/bin/sh`。コンパイラはリポジトリのNix flake/lock、依存はCargo.lockを使用。`bex-process`の3テストとHostの71テストが成功した。macOSでもPTYの明示kill・接続切断・Host強制終了と転送2テストが成功。コミット後の全体qualityは別途PRへ記録する。

対照実験として、同じLinuxコンテナで`ccde07d`のRust supervisorに今回のPTY再現テストを適用した。旧実装ではbackground jobがHost強制終了後も生存して失敗し、修正後は成功する。既存テストの`sh -i -c`を、Hostと同じlogin shell起動とPTY入力へ変更し、別PGIDのjobが実際に生成されたことも検査している。

修正後のmacOS workspaceテストは307件成功、2件opt-in ignored。最終調整後の転送テストとHost・desktop・fixture・supervisorの同時ビルドも成功した。

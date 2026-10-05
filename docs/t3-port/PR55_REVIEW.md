# PR #55 M1 レビュー対応

対象: b52a2be1 のレビューを M2 中断時点 aacd3737 と照合する。M2/M3 の残りは再開しない。ID は /tmp/pr55-review/ の各文書の番号。未確認を解決済みとは扱わない。

| 指摘 | 対応・根拠 | 検証 |
| --- | --- | --- |
| O1 | thread 全体の単調 ordinal と実際に切り詰めた時だけ partial projector を使う。 | 該当 crate の回帰テスト（追加分を含む）。 |
| O2 | Claude に Allow once / Decline を返す。 | 該当 crate の回帰テスト（追加分を含む）。 |
| O3 | 実行中 Start が stop commit を購読し、初期化中の Claude を閉じる。Codex の遅い turn/started も interrupt する。 | stopping_an_in_progress_start 回帰テスト。 |
| O4 | 照合・対応中 | 未完了 |
| O5 | T3 の steered abort result を無視し、stop は失敗 result でも Interrupted にする。 | 該当 crate の回帰テスト（追加分を含む）。 |
| O6 | Claude の1ブロックごとの final frame に message ごとの block cursor を使う。 | 該当 crate の回帰テスト（追加分を含む）。 |
| O7 | Codex の lag では影響する turn を終端にして pump を継続する。 | 該当 crate の回帰テスト（追加分を含む）。 |
| O8 | effect の job panic/Store error/claim error を隔離し、worker の loop を継続する。 | 該当 crate の回帰テスト（追加分を含む）。 |
| O9 | 復旧時に未完了 item と streaming message も終端にする。 | 該当 crate の回帰テスト（追加分を含む）。 |
| O10 | UTC 正規化後の年を0..9999に制限し、保存後も RFC3339 再読込が可能にする。 | 該当 crate の回帰テスト（追加分を含む）。 |
| O11 | Capture を promoted Start より前に outbox へ置く。 | 該当 crate の回帰テスト（追加分を含む）。 |
| O12 | 照合・対応中 | 未完了 |
| O13 | parent_tool_use_id がある Claude frame は root timeline に流さない。subagent の実装は中断中 M2 の範囲。 | 該当 crate の回帰テスト（追加分を含む）。 |
| O14 | Codex userMessage echo は既に保存した user message と重複するため無視する。 | 該当 crate の回帰テスト（追加分を含む）。 |
| O15 | prepared failure で queue を進め、別の active run があれば retry を拒否する。 | 該当 crate の回帰テスト（追加分を含む）。 |
| O16 | T3 ProviderRuntimeRecoveryService.ts に合わせ、再実行可能な capture を持つ Waiting run を保持する。 | 該当 crate の回帰テスト（追加分を含む）。 |
| O17 | 最新の完了 run を選び、queued run による拒否を防ぐ。 | 該当 crate の回帰テスト（追加分を含む）。 |
| H1 | Bex を含む全 provider-thread の native ID を参照して重複を除外。固定 T3 の scratch/managed cwd 除外を移植。 | native_session_lookup と import_excludes 回帰テスト。 |
| H2 | 新規・既存プロジェクトとも register は canonical path ではなく保存した UUID ID を返す。 | projects::registration の ID 戻り値・再登録検証。 |
| H3 | 照合・対応中 | 未完了 |
| H4 | 照合・対応中 | 未完了 |
| H5 | private index は実 index のコピーで stat cache を保持する。index のない初回だけ HEAD/empty から作る。 | 既存 checkpoint の tracked/untracked、通常 index 保持、sparse、rollback テスト。 |
| H6 | 照合・対応中 | 未完了 |
| H7 | Interrupt/Steer は provider の既存 process owner へ直接渡し、workspace 準備と browser 設定を通さない。 | Host 単体テスト、最終全 unit-tests。 |
| H8 | provider ごとの初回取り込み完了を durable metadata に保存し、完了後は scan 自体を呼ばない。中断・保存失敗時は再実行する。 | import_completion_and_checkpoint_namespace 回帰テスト。 |
| H9 | transcript.cwd を canonicalize してから除外・登録・検索・保存に同じ値を使う。 | project symlink/idempotency と import のテスト。 |
| H10 | launch は Host 所有 task と mutex で最後まで実行し、再送を直列化。入力 admission を workspace 作成前に検証。create 後の失敗は delivery Unknown。 | Host RPC の receipt 再送テスト、入力 decider テスト。追加シナリオを最終検証。 |
| H11 | subscribe の開始失敗も型付き Failure frame を返す。 | Host RPC subscribe 回帰テストを追加する。 |
| H12 | 直接 thread.create の未登録 project を admission で拒否。既存の不明 project は worktree 一覧から除外し、他の一覧を継続。 | Host 単体テスト。 |
| H13 | cwd に基づく terminal ID を protocol に統一。別 thread が同じ cwd を使用中なら terminal を残す。 | terminal の所有権テスト、最終全 unit-tests。 |
| H14 | O8 と同じ worker の修正。 | 該当 crate の回帰テスト（追加分を含む）。 |
| H15 | root scope に永続 Store instance ID を含め、store reset/別 Host の ref を分離。保存済み scope はその store の値を使用する。 | instance ID の再起動保持と別 Store の相違を検証。 |
| H16 | Browser startup alias と bind_scope、削除済み runner 専用の xtask Android E2E helper/instrumentation test/依存を削除。 | Host/xtask の単体と integration を最終検証。 |
| C1 | 照合・対応中 | 未完了 |
| C2 | 照合・対応中 | 未完了 |
| C3 | 照合・対応中 | 未完了 |
| C4 | 照合・対応中 | 未完了 |
| C5 | 照合・対応中 | 未完了 |
| C6 | 照合・対応中 | 未完了 |
| C7 | 照合・対応中 | 未完了 |
| C8 | 照合・対応中 | 未完了 |
| C9 | 照合・対応中 | 未完了 |
| C10 | 照合・対応中 | 未完了 |
| C11 | 照合・対応中 | 未完了 |
| C12 | 照合・対応中 | 未完了 |
| C13 | 照合・対応中 | 未完了 |
| C14 | 照合・対応中 | 未完了 |
| C15 | 照合・対応中 | 未完了 |
| C16 | 照合・対応中 | 未完了 |
| D1 | 照合・対応中 | 未完了 |
| D2 | 照合・対応中 | 未完了 |
| D3 | 照合・対応中 | 未完了 |
| D4 | 照合・対応中 | 未完了 |
| D5 | 照合・対応中 | 未完了 |
| D6 | 照合・対応中 | 未完了 |
| D7 | 照合・対応中 | 未完了 |
| D8 | 照合・対応中 | 未完了 |
| D9 | 照合・対応中 | 未完了 |
| D10 | 照合・対応中 | 未完了 |
| D11 | 照合・対応中 | 未完了 |
| D12 | 照合・対応中 | 未完了 |
| D13 | 照合・対応中 | 未完了 |
| D14 | 照合・対応中 | 未完了 |
| D15 | 照合・対応中 | 未完了 |
| D16 | 照合・対応中 | 未完了 |
| D17 | 照合・対応中 | 未完了 |
| D18 | 照合・対応中 | 未完了 |
| D19 | 照合・対応中 | 未完了 |
| D20 | 照合・対応中 | 未完了 |
| M1 | 照合・対応中 | 未完了 |
| M2 | 照合・対応中 | 未完了 |
| M3 | 照合・対応中 | 未完了 |
| M4 | 照合・対応中 | 未完了 |
| M5 | 照合・対応中 | 未完了 |
| M6 | 照合・対応中 | 未完了 |
| M7 | 照合・対応中 | 未完了 |
| M8 | 照合・対応中 | 未完了 |
| M9 | 照合・対応中 | 未完了 |
| M10 | 照合・対応中 | 未完了 |
| M11 | 照合・対応中 | 未完了 |
| M12 | 照合・対応中 | 未完了 |
| M13 | 照合・対応中 | 未完了 |
| M14 | 照合・対応中 | 未完了 |
| M15 | 照合・対応中 | 未完了 |
| M16 | 照合・対応中 | 未完了 |
| M17 | 照合・対応中 | 未完了 |
| M18 | 照合・対応中 | 未完了 |

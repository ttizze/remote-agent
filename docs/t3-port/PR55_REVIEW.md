# PR #55 M1 レビュー対応

対象: b52a2be1 のレビューを M2 中断時点 aacd3737 と照合する。M2/M3 の残りは再開しない。ID は /tmp/pr55-review/ の各文書の番号。全87件を照合した。以下の「検証」は対応するテスト群とコード確認を示し、native UI の操作テストを実行したという意味ではない。

| 指摘 | 対応・根拠 | 検証 |
| --- | --- | --- |
| O1 | thread 全体の単調 ordinal と実際に切り詰めた時だけ partial projector を使う。 | `queued_message_does_not_hide_later_active_run_output_on_partial_clients`。 |
| O2 | Claude に Allow once / Decline を返す。 | `claude_approval_has_allow_and_decline_options`。 |
| O3 | 実行中 Start が stop commit を購読し、初期化中の Claude を閉じる。Codex の遅い turn/started も interrupt する。 | `stopping_an_in_progress_start_cancels_provider_input_and_holds_the_queue`。 |
| O4 | 完了済み turn の steer は同じ message ID の follow-up にする。Waiting を adapter error で Failed にせず、Claude も terminal を確認して新しい native turn を誤って開始しない。 | `completed_steer_becomes_one_idempotent_followup_on_the_selected_provider`。 |
| O5 | T3 の steered abort result を無視し、stop は失敗 result でも Interrupted にする。 | `claude_steering_abort_does_not_finish_and_stop_failure_is_interrupted`。 |
| O6 | Claude の1ブロックごとの final frame に message ごとの block cursor を使う。 | `claude_per_block_final_frames_preserve_reasoning_and_text_identity`。 |
| O7 | Codex の lag では影響する turn を終端にして pump を継続する。 | `notification_pump_recovers_and_records_causes_without_conversation_payloads`。 |
| O8 | effect の job panic/Store error/claim error を隔離し、worker の loop を継続する。 | `worker_survives_a_panicking_job_and_processes_another_thread`。 |
| O9 | 復旧時に未完了 item と streaming message も終端にする。 | `recovery_finishes_streaming_items_and_messages`。 |
| O10 | UTC 正規化後の年を0..9999に制限し、保存後も RFC3339 再読込が可能にする。 | orchestration/provider-adapters の unit/property tests。 |
| O11 | Capture を promoted Start より前に outbox へ置く。 | `stopping_a_scoped_run_enqueues_capture_before_the_next_start`。 |
| O12 | provider の連続 text/reasoning/plan/command output delta を50ms・64KiBでまとめてから正規化する。guarded delta は thread と現在 run だけを読み、shell は共通 cache を差分更新する。完全な lifecycle event は従来の projector を使う。 | `megabyte_burst_is_exact_and_does_not_persist_ten_thousand_snapshots`。 |
| O13 | parent_tool_use_id がある Claude frame は root timeline に流さない。subagent の実装は中断中 M2 の範囲。 | `child_frames_and_codex_user_echo_do_not_create_root_tool_or_message_items`。 |
| O14 | Codex userMessage echo は既に保存した user message と重複するため無視する。 | `child_frames_and_codex_user_echo_do_not_create_root_tool_or_message_items`。 |
| O15 | prepared failure で queue を進め、別の active run があれば retry を拒否する。 | `failed_preparation_promotes_queue_and_cannot_retry_over_an_active_run`。 |
| O16 | T3 ProviderRuntimeRecoveryService.ts に合わせ、再実行可能な capture を持つ Waiting run を保持する。 | `process_loss_retains_capture_without_resuming_queued_work`。 |
| O17 | 最新の完了 run を選び、queued run による拒否を防ぐ。 | `unread_uses_latest_completed_run_even_when_newer_run_is_queued`。 |
| H1 | Bex を含む全 provider-thread の native ID を参照して重複を除外。固定 T3 の scratch/managed cwd 除外を移植。 | `native_session_lookup_includes_bex_owned_threads_not_only_import_ids`。 |
| H2 | 新規・既存プロジェクトとも register は canonical path ではなく保存した UUID ID を返す。 | Host/transport/process/xtask の unit・integration tests。 |
| H3 | nextest の --lib --bins 制限を除き、transport/pairing/management/browser/diagnostics/process ownership の integration targets を戻した。旧 Host 会話ループの診断検証は現在の provider pump へ移し、原因記録と payload 非記録を検証する。Swiftformat/Swiftlint、detekt、native unit tests、clean-builds の dry-run を quality/CI に戻した。 | Host/transport/process/xtask の unit・integration tests。 |
| H4 | thread の shell projection を subscriber 間で共有し、連続した streaming event は最新 message と item count のみ更新する。metadata/lifecycle/fork の変化だけ complete projector を使う。replay cursor は元の event sequence を保持する。 | `delta_ingest_skips_unrelated_history_and_shell_subscribers_share_the_projection`。 |
| H5 | private index は実 index のコピーで stat cache を保持する。index のない初回だけ HEAD/empty から作る。 | Host/transport/process/xtask の unit・integration tests。 |
| H6 | orchestration の既定 feature は contracts/純粋な判断のみ。SQLite/Tokio worker は Host の runtime feature、adapter 境界は別 feature にした。 | Host/transport/process/xtask の unit・integration tests。 |
| H7 | Interrupt/Steer は provider の既存 process owner へ直接渡し、workspace 準備と browser 設定を通さない。 | `stop_and_steer_bypass_unavailable_workspace_state`。 |
| H8 | provider ごとの初回取り込み完了を durable metadata に保存し、完了後は scan 自体を呼ばない。中断・保存失敗時は再実行する。 | `import_completion_and_checkpoint_namespace_are_durable_and_store_scoped`。 |
| H9 | transcript.cwd を canonicalize してから除外・登録・検索・保存に同じ値を使う。 | Host/transport/process/xtask の unit・integration tests。 |
| H10 | launch は Host 所有 task と mutex で最後まで実行し、再送を直列化。入力 admission を workspace 作成前に検証。create 後の失敗は delivery Unknown。 | `launch_survives_delivery_cancellation_and_resends_share_one_worktree`。 |
| H11 | subscribe の開始失敗も型付き Failure frame を返す。 | Host/transport/process/xtask の unit・integration tests。 |
| H12 | 直接 thread.create の未登録 project を admission で拒否。既存の不明 project は worktree 一覧から除外し、他の一覧を継続。 | Host/transport/process/xtask の unit・integration tests。 |
| H13 | cwd に基づく terminal ID を protocol に統一。別 thread が同じ cwd を使用中なら terminal を残す。 | `deleting_threads_keeps_shared_cwd_terminal_until_the_last_thread`。 |
| H14 | O8 と同じ worker の修正。 | `worker_survives_a_panicking_job_and_processes_another_thread`。 |
| H15 | root scope に永続 Store instance ID を含め、store reset/別 Host の ref を分離。保存済み scope はその store の値を使用する。 | `import_completion_and_checkpoint_namespace_are_durable_and_store_scoped`。 |
| H16 | Browser startup alias と bind_scope、削除済み runner 専用の xtask Android E2E helper/instrumentation test/依存を削除。 | Host/transport/process/xtask の unit・integration tests。 |
| C1 | 検索は2..200文字・limit 50。空/短すぎる/offline は RPC を出さず、以前の検索結果を消す。 | `search_respects_server_limits_and_clear_remains_local`。 |
| C2 | 未読は最新の完了 watermark と lastVisited を比較し、never visited は false。表示中/launch 後も server watermark で visit する。 | `unread_is_a_completion_watermark_and_never_visited_is_not_unread`。 |
| C3 | 未受領の同じ draft/run/launch は composer を無効にし、owner が再送 Intent を no-op にする。 | `awaiting_message_cannot_be_submitted_twice`。 |
| C4 | queued edit は run ごとの draft key。Cancel/成功で通常 draft を保持し、queue から消えれば編集を終了。 | `queue_edit_cancel_and_receipt_preserve_the_main_composer_draft`。 |
| C5 | I/O は ConnectionClosed とし、NotSent が確定した Host rejection だけ durable pending を除去。 | `failed_terminal_start_is_terminal_and_transport_failures_keep_pending_commands`。 |
| C6 | 生きている接続の timeout は同じ job/command ID で再試行し、thread ごとの mutation 順序を維持。 | `healthy_resume_reuses_connection_and_timed_out_mutations_keep_id_and_order`。 |
| C7 | shell snapshot で消えた selected thread を外し、stream も停止。未確定 launch は保持。 | `reconnect_shell_snapshot_clears_a_deleted_selection`。 |
| C8 | terminal/file/review/file drafts を Arc 共有し、入力を損失しない専用 FIFO へ。旧 connection close は owner loop の外で行う。 | `a_burst_of_input_over_the_stream_channel_capacity_keeps_every_edit`。 |
| C9 | 最初の空でない trimmed line を100文字まで title にする。 | `launch_title_uses_the_first_nonempty_trimmed_line`。 |
| C10 | 固定 T3 の snoozed > settled > pinned に合わせる。 | `approval_wakes_snoozed_threads_without_changing_parked_shelf_precedence`。 |
| C11 | 古い snapshot と、その bounded window より古い history/page cursor を拒否する。 | `older_snapshots_and_history_pages_cannot_move_the_window_backwards`。 |
| C12 | terminal start の失敗は Failed にし、Starting 表示を終える。 | `failed_terminal_start_is_terminal_and_transport_failures_keep_pending_commands`。 |
| C13 | provider switch は Host thread/projection または shell の instance と比較する。 | `model_switch_is_compared_with_the_host_thread_selection`。 |
| C14 | endpoint と ticket が同じ healthy session の resume は既存接続を使い reused=true を返す。 | `healthy_resume_reuses_connection_and_timed_out_mutations_keep_id_and_order`。 |
| C15 | local/offline の編集を成功扱い。表示用 error formatter を使い、無関係な background 成功はエラーを消さない。 | agent-core の unit/property tests。C6/C14 は loopback transport fixture で検証。 |
| C16 | 変更しない。固定 T3 Sidebar.logic.ts と同様、Working shelf 有効時の Active は復帰時刻順。UI は ActiveReorder を提供していない。 | agent-core の unit/property tests。C6/C14 は loopback transport fixture で検証。 |
| D1 | Textarea の submit_on_enter(true) と PressEnter で通常送信。Shift+Enter は改行、Option+Enter は steer の操作を維持。 | desktop/core の unit tests、ソース照合、native build/clippy。 |
| D2 | revision が古い／同じ Snapshot を拒否し、Completed で得た最新状態を巻き戻さない。 | `stale_or_duplicate_snapshots_are_rejected`。native UI の配線はソース照合・compile。 |
| D3 | file editor に専用 receipt revision を持たせ、最新の編集応答まで native buffer を守る。古い read/save は別の editor context を開かず、canonical path の reply は要求元で照合する。 | `late_file_reads_do_not_switch_the_editor_and_canonical_paths_are_accepted`。native UI の配線はソース照合・compile。 |
| D4 | core の未受領 draft 判定で Send を無効化し、owner でも同じ draft の二度目の Intent を no-op にする。 | `awaiting_message_cannot_be_submitted_twice`。native UI の配線はソース照合・compile。 |
| D5 | session がない間は draft receipt を待つ状態にしない。core の can_edit を表示に使用する。 | desktop/core の unit tests、ソース照合、native build/clippy。 |
| D6 | thread/cwd/Host の切替で rename 状態を解除する。 | desktop/core の unit tests、ソース照合、native build/clippy。 |
| D7 | Settings／会話／panel の状態が変わる時に native browser の visibility を更新する。 | desktop/core の unit tests、ソース照合、native build/clippy。 |
| D8 | ConversationView を一度だけ保存し、Tick は再計算しない。GPUI ListState で可変高 timeline を仮想化し、history prepend は splice で anchor を維持。Terminal はメイン Store を共有する。Hosts の独立接続はローカル Host の管理・QR pairing を remote conversation と独立して行うため維持する。 | desktop/core の unit tests、ソース照合、native build/clippy。 |
| D9 | thread/cwd 切替で editor/terminal/browser を解除し、開いている Diff/Files/Terminal は新しい context を要求する。Host 切替でも panel の古い path を保存しない。 | desktop/core の unit tests、ソース照合、native build/clippy。 |
| D10 | 固定 T3 の confirmThreadDelete=true に合わせ、ID を捕捉した native confirmation を追加した。 | desktop/core の unit tests、ソース照合、native build/clippy。 |
| D11 | macOS の sidebar header 左余白を T3 指定の90pxにする。 | desktop/core の unit tests、ソース照合、native build/clippy。 |
| D12 | 音声を送った後も Transcribing 表示を保持する。core が元の draft key へ追記し、base_text によって遅い native edit が transcript の追記を消さない。 | `delayed_native_edit_preserves_transcription_append`。native UI の配線はソース照合・compile。 |
| D13 | 破損した local snapshot/model preferences は現行形式の空状態から接続する。旧形式 migration は追加しない。 | desktop/core の unit tests、ソース照合、native build/clippy。 |
| D14 | 接続済みの同じ Host ID/ticket を選んでも reconnect しない。 | desktop/core の unit tests、ソース照合、native build/clippy。 |
| D15 | core の ModelChoice/provider_kind を使い、instance ID と selected model の判定を UI から除去する。 | desktop/core の unit tests、ソース照合、native build/clippy。 |
| D16 | core の placeholder/can_edit を使い、live approval 中の入力を止める。 | desktop/core の unit tests、ソース照合、native build/clippy。 |
| D17 | 選択状態と送信値を core の question helpers に統一する。custom single answer の入力時は元 option の表示も外れる。 | `custom_single_answer_deselects_option_but_multi_preserves_it`。native UI の配線はソース照合・compile。 |
| D18 | sidebar の Settle/Unsettle、Wake/Unsnooze は row の実際の settled/snoozed 値を使う。 | desktop/core の unit tests、ソース照合、native build/clippy。 |
| D19 | 残存 store_session の3テストを新 Store に移植。初回 model preferences 保存漏れも修正した。timeline splice/anchor の unit tests を追加した。 | `closing_flushes_the_latest_draft_and_closes_the_store`。native UI の配線はソース照合・compile。 |
| D20 | 固定 T3 に合わせ mode labels、project / title breadcrumb、pending request drawer、新規 hero、offline search を修正。空棚をすべて隠す指摘は採用しない。固定 Sidebar.tsx は Pinned/Active/Settled を残し、空の Working/Snoozed だけ省く。 | desktop/core の unit tests、ソース照合、native build/clippy。 |
| M1 | terminal coordinator の最終描画 sequence 以降だけ FFI で取得する。status は output を複製しない cursor を使い、iOS は入力可否も cache する。Host/cwd の変更時に native emulator を作り直す。 | core/native unit tests、Swift/Kotlin lint、iOS/Android compile。UI 操作テストは対象外。 |
| M2 | ConversationView は1個だけ保持し、16msでまとめて background projection する。連続入力で計算が終わらない debounce を避け、一つの task が最新状態まで追いつく。SwiftUI row は値で比較し、markdown parse も source ごとに background cache する。 | core/native unit tests、Swift/Kotlin lint、iOS/Android compile。UI 操作テストは対象外。 |
| M3 | 保存は owner.snapshot がある時だけ行う。起動中／初期化失敗時に empty snapshot を保存する fallback を除去した。 | `AndroidAppModelTest.failedStartupDoesNotOverwriteSavedStateOrSharedModelPreferences`。native UI の配線はソース照合・compile。 |
| M4 | C4 と同じ per-run draft key を使う。native buffer の receipt guard も core の draft context key で切替する。 | `DraftRevisionTests/DraftRevisionTest の新旧 receipt・Host 切替`。native UI の配線はソース照合・compile。 |
| M5 | search query を core snapshot から復元し、保存されている project filter を表示して解除操作を設けた。 | core/native unit tests、Swift/Kotlin lint、iOS/Android compile。UI 操作テストは対象外。 |
| M6 | Diff row は collapsed の native disclosure を使い、開いた時だけ diff を表示する。 | core/native unit tests、Swift/Kotlin lint、iOS/Android compile。UI 操作テストは対象外。 |
| M7 | Android は返信 row の先頭ではなく、timeline の footer anchor へ追従する。履歴閲覧中は追従しない。 | core/native unit tests、Swift/Kotlin lint、iOS/Android compile。UI 操作テストは対象外。 |
| M8 | iOS/Android に ID を捕捉した Delete confirmation を追加し、削除した会話が表示中なら一覧へ戻す。 | core/native unit tests、Swift/Kotlin lint、iOS/Android compile。UI 操作テストは対象外。 |
| M9 | shelf から settled を逆算せず core row の状態を使い、Unsettle/Wake を提供する。 | core/native unit tests、Swift/Kotlin lint、iOS/Android compile。UI 操作テストは対象外。 |
| M10 | workspace/turn review の採用済み結果に stable generation を付け、Android の diffFiles をその key で cache する。 | core/native unit tests、Swift/Kotlin lint、iOS/Android compile。UI 操作テストは対象外。 |
| M11 | D17 と同じ core question helpers で選択表示と回答を一致させる。 | `custom_single_answer_deselects_option_but_multi_preserves_it`。native UI の配線はソース照合・compile。 |
| M12 | global error を composer.notice へ複製しない。上の banner に一度だけ表示する。 | core/native unit tests、Swift/Kotlin lint、iOS/Android compile。UI 操作テストは対象外。 |
| M13 | 固定 T3 に合わせ pending approval/question を composer の上へ置き、resolved request は timeline の work log に戻す。 | `only_pending_requests_use_the_composer_drawer_and_resolved_requests_join_work_log`。native UI の配線はソース照合・compile。 |
| M14 | model/instance/current selection/runtime choices の判断と mode labels を core へ統一する。 | core/native unit tests、Swift/Kotlin lint、iOS/Android compile。UI 操作テストは対象外。 |
| M15 | bex:chats の cwd が空という推測は不採用。Host は実在する chat_directory を project root として返す。ただし未取得／offline の terminal 起動を防ぐ core.can_open_terminal は両モバイルへ適用した。 | core/native unit tests、Swift/Kotlin lint、iOS/Android compile。UI 操作テストは対象外。 |
| M16 | Android/iOS の markdown link の scheme 判定を core.safe_markdown_url に統一する。 | `unsafe_markdown_links_are_rejected`。native UI の配線はソース照合・compile。 |
| M17 | iOS camera は QR pairing、microphone は音声入力の permission text に修正。使わない photo-library-add permission を削除した。 | core/native unit tests、Swift/Kotlin lint、iOS/Android compile。UI 操作テストは対象外。 |
| M18 | Swift/Kotlin に draft receipt/context 切替の native unit tests、Android に初期化失敗時の実ファイル保存の JVM regression を追加。旧 instrumentation runner は削除を維持し、Simulator UI/E2E は実行しない。 | `Swift 2件・Kotlin 3件の native unit tests`。native UI の配線はソース照合・compile。 |

追加の判断・制限:

- C16 は固定 T3 と同じ順序のため変更しない。D8 の Hosts はローカル管理用の独立接続を保持する。D20 の「全空棚を隠す」は固定 T3 と一致しないため採用しない。M15 の「bex:chats は cwd が空」は現行 Host と一致しない。いずれも対応表に理由を記録した。
- O13 は root timeline への混入を直した。subagent の委任・表示・nested scope の完全な移植は、ユーザー指示で中断中の M2 として残す。
- 全 target の初回テストに、harness=false の手動 WebKit 単体 probe が混入して一度実行された。通常 nextest では ignored として列挙するよう修正し、最終実行では skip。実 provider E2E と Simulator UI テストは実施していない。
- browser bridge は macOS のローダー内で実行開始が遅れることを確認し、初回 initialize だけ30秒の起動枠を設けた。起動後の RPC/cancel は3〜5秒の検証期限を維持する。

## 最終検証

実装・テスト commit: `08b882ec`。この後の変更は検証記録の文書のみ。

- **`scripts/dev-env.sh just unit-tests` 通過**: workspace **306件通過、5件 skip**。standalone agent-peer の protocols / failures / paths and validation / cancellation / installation の5テスト群も通過。
- **workspace と agent-peer の全 target Clippy `-D warnings`、両方の fmt、actionlint、`git diff --check` 通過**。
- **Swift**: swiftformat / Swiftlint strict、native unit tests **2件通過**、Rust/Swift bindings と generic iOS Simulator destination の **build 成功**（Simulator の起動・UI 操作はしていない）。
- **Android**: ktfmt / detekt、JVM unit tests **3件通過**（実ファイル保存の失敗時保護を含む）、arm64-v8a/x86_64 の **assembleDebug 成功**。
- transport、pairing、management、process ownership、browser bridge、診断、依存境界、build cleanup の現行 integration tests を全体テストに含めた。5件の skip は microphone/Chrome/Codex/public relay/手動 WebKit に依存する明示的な外部・手動検証。

## 2回目のレビュー（224eeb3d への指摘）

前回の O8 の「job を隔離すれば十分」という判断は誤りだった。loop の生存に加え、同じ thread の outbox を解放する必要がある。O3 は Restart、O4 は steer の終了競合、O7 は lag 時の native interrupt、O12 は長時間の連続 stream、C3/C4/C8/C14 と D8/D20 は追加修正が必要として扱う。2回目の対応を先に完了し、その後に M2 の残りを実装する。M3 と main の取り込みは行わない。

| R2 指摘 | 現在の対応 | 回帰検証 |
|---|---|---|
| O1 / 前回 O8 | error/panic/期限切れ lease の再試行を5回で終端にし、後続 effect を解放。rejected steer follow-up も境界で終端化 | worker の同じ thread の後続 run・繰り返し panic・期限切れ lease |
| O3, O11 | rollback の重複要求を拒否。完了は現在の thread の rollback フィールドだけを更新 | rollback request と並行 rename/pin/delete |
| O2, O5, O6, O16 | Claude の成功時に resume cursor を消費。初期化の timeout/reject は process/capacity を解放し、fallback 前に Failed イベントを出さない。ack 中の process 終了は失敗として扱う | normalize cursor、初期化 timeout/reject と容量解放 |
| O4 / H4 | native ID の絶対的な revert 境界を使い、境界が既に消えていれば再度 revert しない。native input 前の失敗を数えない。ファイル復元を provider 操作前に検証・退避し、provider 失敗は元に戻す | native boundary、補償と journal recovery |
| H1, H5, H6, H7 | import の worktreePath は null。Git checkout の identity で隔離を確認。capture/diff/restore は同じ cwd の範囲。衝突/submodule を変更前に拒否 | import、nested cwd/sibling、directory collision、原 checkout/linked worktree |
| H3 / C4 | before-run checkpoint と parent ID で関連付け。stale refs を削除して再利用を防ぐ。diff は番号の欠番を許可 | 欠番・parent link・stale ref 削除と再 capture |
| O8, O9 / 前回 O3 | queued item は promote 時に作成。Start/Restart 両方を stop でキャンセル | queued output 順序・Restart の開始中 stop |
| H2, O13, O14, O15, O17 | handoff は実際の直前 run を基準にし、compact は未送信 transfer を保持。fork history を固定。merge の run 判定・重複/supersede を共通化。非 active provider の巻き戻された native context は無効化 | fork の親 rollback・merge run 判定・queue/checkpoint tests |
| O18（rate limit / unknown block） | 正常な rate limit status と未対応の content block は空の UI 行を作らない | normalize の status/block 回帰テスト |
| H9, H10, H11 | explicit worktree の launch も project 登録を検証。launch の lock は command ごと。snapshot の frame size を送信前に検証し型付き failure を返す | explicit workspace、lock identity、oversized subscription |

残る R2 指摘（Claude の echo/steer 所有権、transfer 再試行、連続 stream の差分化、core の下書き/送信/再開、desktop と mobile の更新・操作・保存回復）は引き続き修正中。最終検証と対応理由の全表は完了時に更新する。

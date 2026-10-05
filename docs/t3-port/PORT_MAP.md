# T3 の翻訳対応表

参照を `4ee6bfd50ef4a089440d5c3662db2298da9cc50e` に固定する。旧実装に対する M1/M2 の完了記録は、関数単位の翻訳完了・T3 のテスト通過を意味しない。この表で照合を終えた範囲だけを翻訳済みとする。

ユーザーの新しい指示に従い、順序は未コミット変更の採否 → この対応表 → orchestration core → Codex/Claude と SDK → 履歴取り込み → client-runtime と3クライアント → R3 全件再照合とする。M3、main のマージ、実 Host の起動・再起動、他 worktree の変更は行わない。

## 記録と検証

- `PORT_MAP.json` は全ファイルの source SHA-256、行数、対応先、関数・定数、T3 テストケース、照合した行範囲を保持する。初期の symbol/case リストは検索用の索引であり、匿名関数・parameterized case の完全性の証明には使わない。翻訳時に該当ファイル全体を読み、関数と行範囲を補う。
- `scripts/t3-port/inventory.py --check` は4対象ディレクトリの欠落・余分なファイル・参照 hash の変更を検出する。通常実行は照合記録を保存したまま下表を更新する。対象外のファイルと fixture も省略しない。
- 未翻訳／未移植の行は、既存の短縮された実装や独自テストがあっても完了扱いにしない。T3 の期待値・fixture は変更しない。production の呼出し経路につながっていない翻訳を完了扱いにしない。
- Effect の Service/Layer/Ref/Deferred/Scope は Rust の構造体・owner・future・task に置き換える。決定順序、トランザクション境界、cancel の勝ち方、ID、エラー分類、再送・再取得の意味は維持する。
- 該当 source の外にある persistence・shared helper・Claude SDK も、使用する関数を後述の追加依存対応に記録して翻訳する。

## 中断時の12ファイルの採否

中断時 HEAD は `e1d359c7`。未コミット差分をすべて読み、暫定差分をこの worktree の ignored `target/t3-translation-pre-audit.patch` に保存した。いずれも関数全体・全呼出し経路の翻訳としては採用せず、HEAD に戻した。T3 と一致する目的は、該当モジュールを翻訳する際の元テストで検証する。

| 未コミットのファイル／変更 | T3 との照合と採否 |
|---|---|
| `orchestration/context.rs`：fork の target 履歴を使う | 固定履歴は必要だが T3 の ContextHandoffService/ProjectionStore の構築・範囲選択・文面を翻訳していない。短縮 handoff の差分は置き換える。 |
| `orchestration/delegation.rs`：ack を空 Decision にする、Cancelled の task を disposed にする | Orchestrator 1893–1909 は ack 済みの同じ task row を再発行する。7310–7343 の queue cancel は cohort 全体を dispose する。どちらの暫定変更も不一致のため不採用。 |
| `orchestration/store.rs`：project 完了印、commit 購読、delta の欠落時 no-op | AgentSessionImporter は transcript ごとの imported source を記録する。独自の project 完了印は不採用。EventSink/ProjectionStore に従って購読と delta を翻訳する。 |
| `orchestration/worker.rs`：4件上限削除、generic cancel-before-fail | EffectWorker 755–756 の既定並列数は4。上限削除は不採用。失敗処理は各 Service と worker の willRetry/settlement 区分へ翻訳する。 |
| `provider-adapters/claude.rs`：cursor 欠落を常に null にする | ClaudeAdapterV2 1167–1211 は後続 terminal turn がない場合だけ null。後続がある場合は error。暫定変更はその分岐を失うため不採用。 |
| `provider-adapters/codex.rs`：detach 前の stop、root の停止待ち、曖昧な Start 失敗の保持 | 独自 TurnState/通知 pump を基準にした修正。CodexAdapterV2 の同じ関数、ProviderTurnControlService/StartService と元 replay を翻訳する。 |
| `host_rpc/import.rs`：exact cwd、project 完了印 | exact cwd の目的は一致。scanner の skip/duplicate/already-imported、ファイルの fingerprint、instance、event ID・既存 thread 判断は未翻訳なのでファイル全体を置き換える。 |
| `host_rpc/service.rs`：project 追加後に走査、独自 mutex | T3 の importer 呼出し・project 検証と連動させる。暫定 project 完了印を使う経路は不採用。 |
| `host_rpc/agent_tools.rs`：購読で wait、固定 ack ID、6 MiB error | OrchestratorMcpService の waitForTask は poll、ack の request key は呼出しごと。購読 wait と固定 ack ID は不採用。framing 上限への変換は通信境界として、元 MCP の返り値と比較して実装する。 |
| `projects.rs` / `projects/state.rs`：exact root helper、worktree mapping 削除 | 元 importer の project.workspaceRoot 比較を翻訳する時に接続し直す。移植前の既存 importer を壊さないよう、今は HEAD に戻す。 |
| `worktrees.rs`：未使用 reader 削除 | importer の翻訳によって不要となった時点で呼出し側とともに削除する。今回の段階では戻す。 |

R3 の O22（4並列）については、上限自体を T3 と違うものとして削除しない。なお現在の短縮 worker と T3 の worker が同じ、という判断ではない。claim/cancel/settlement/通知・再試行・回復を別途照合する。

## 差分

許される境界を明示する。これ以外の挙動差は、該当行へ理由を記載できる場合だけ残す。

| 境界 | Bex の置換 | 保持する意味／検証 |
|---|---|---|
| HTTP / WebSocket | iroh、既存の Postcard framing、QR と端末鍵 | RPC の引数・結果、snapshot/replay/synchronized、afterSequence、順序・fallback 条件。QUIC の handshake/stream decode 保護は通信 owner に置く。 |
| Claude Agent SDK | pnpm-lock の `@anthropic-ai/claude-agent-sdk@0.3.276` の CLI 制御を Rust へ翻訳 | control_request/control_response、初期化、canUseTool、interrupt、model/permission 変更、resume/fork、stream の順序。その上で ClaudeAdapterV2 を翻訳する。SDK 不在を独自 gate/buffer の理由にしない。 |
| browser/ReactNative の描画 | GPUI / SwiftUI / Compose | client-runtime の結果を表示する。UI の条件・ラベル・操作は元コンポーネントに合わせ、Web 専用機能を追加しない。 |
| T3 Connect / 外部サービス | 対象外 | T3 Connect の HTTP 認証/relay は作らない。iroh の既存接続機能を保持する。 |
| V1 migration | 対象外 | ユーザー指示により互換性・旧形式移行は不要。 |

本体に置いた独自 gate、wake の frame/byte 上限、手作りの ID、短縮した policy、独自 retry 条件は、T3 に対応する実装がなければ削除する。既存の PLAN 判断の記録にあるこれらの記述も翻訳の根拠としては使用しない。

## 追加依存対応

SDK の取得記録（tarball の integrity/hash と制御関数）、persistence の各 repository/SQL、shared helper は該当モジュールの翻訳時に追記する。

<!-- generated inventory -->

## ファイル対応表

予定の Rust パスは未翻訳行にも明記する。翻訳済みは production の呼出し経路・関数・移植テストを照合してから付ける。

### `apps/server/src/orchestration-v2`

本体 94 ファイル / 81,238 行。テスト 106 ファイル / 104,071 行。testkit/fixture は別行で全件追跡する。

| T3 のファイル（行数） | Rust のモジュール／テスト | 移植した T3 テスト | 状態・理由 |
|---|---|---|---|
| `apps/server/src/orchestration-v2/AcpRegistryOrchestratorV2.live.test.ts` (255) | — | — | 対象外：Bex の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/Adapters/AcpAdapterV2.test.ts` (14296) | — | — | 対象外：Bex の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/Adapters/AcpAdapterV2.testkit.ts` (246) | — | — | 対象外：Bex の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/Adapters/AcpAdapterV2.ts` (7969) | — | — | 対象外：Bex の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/Adapters/AcpRegistryAdapterV2.test.ts` (486) | — | — | 対象外：Bex の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/Adapters/AcpRegistryAdapterV2.testkit.ts` (109) | — | — | 対象外：Bex の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/Adapters/AcpRegistryAdapterV2.ts` (328) | — | — | 対象外：Bex の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/Adapters/AntigravityAdapterV2.test.ts` (537) | — | — | 対象外：Bex の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/Adapters/AntigravityAdapterV2.ts` (241) | — | — | 対象外：Bex の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/Adapters/ClaudeAdapterV2.test.ts` (7963) | `crates/provider-adapters/src/claude_adapter_v2.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/Adapters/ClaudeAdapterV2.testkit.ts` (2844) | `crates/provider-adapters/src/claude_adapter_v2kit.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/Adapters/ClaudeAdapterV2.ts` (7859) | `crates/provider-adapters/src/claude_adapter_v2.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/Adapters/CodexAdapterV2.test.ts` (7275) | `crates/provider-adapters/src/codex_adapter_v2.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/Adapters/CodexAdapterV2.testkit.ts` (276) | `crates/provider-adapters/src/codex_adapter_v2kit.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/Adapters/CodexAdapterV2.ts` (6317) | `crates/provider-adapters/src/codex_adapter_v2.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/Adapters/CursorAdapterV2.test.ts` (895) | — | — | 対象外：Bex の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/Adapters/CursorAdapterV2.testkit.ts` (1022) | — | — | 対象外：Bex の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/Adapters/CursorAdapterV2.ts` (2677) | — | — | 対象外：Bex の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/Adapters/CursorAgentSdk.test.ts` (304) | — | — | 対象外：Bex の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/Adapters/CursorAgentSdk.ts` (612) | — | — | 対象外：Bex の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/Adapters/DevinAcp.ts` (57) | — | — | 対象外：Bex の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/Adapters/GrokAdapterV2.test.ts` (468) | — | — | 対象外：Bex の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/Adapters/GrokAdapterV2.testkit.ts` (122) | — | — | 対象外：Bex の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/Adapters/GrokAdapterV2.ts` (455) | — | — | 対象外：Bex の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/Adapters/OpenCode2AdapterV2.test.ts` (3921) | — | — | 対象外：Bex の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/Adapters/OpenCode2AdapterV2.testkit.ts` (344) | — | — | 対象外：Bex の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/Adapters/OpenCode2AdapterV2.ts` (4266) | — | — | 対象外：Bex の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/Adapters/OpenCodeAdapterV2.test.ts` (2445) | — | — | 対象外：Bex の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/Adapters/OpenCodeAdapterV2.testkit.ts` (511) | — | — | 対象外：Bex の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/Adapters/OpenCodeAdapterV2.ts` (3758) | — | — | 対象外：Bex の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/Adapters/OpenCodeToolItems.ts` (170) | — | — | 対象外：Bex の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/Adapters/PiAdapterV2.test.ts` (2489) | — | — | 対象外：Bex の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/Adapters/PiAdapterV2.testkit.ts` (524) | — | — | 対象外：Bex の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/Adapters/PiAdapterV2.ts` (3015) | — | — | 対象外：Bex の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/Adapters/PiRpc.ts` (484) | — | — | 対象外：Bex の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/Adapters/ProviderTextDeltaCoalescer.ts` (156) | `crates/provider-adapters/src/provider_text_delta_coalescer.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/Adapters/piT3McpExtensionSource.test.ts` (59) | — | — | 対象外：Bex の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/Adapters/piT3McpExtensionSource.ts` (328) | — | — | 対象外：Bex の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/Adapters/piT3McpInjection.test.ts` (167) | — | — | 対象外：Bex の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/Adapters/piT3McpInjection.ts` (316) | — | — | 対象外：Bex の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/AttachmentClaims.test.ts` (316) | `crates/orchestration/src/attachment_claims.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/AttachmentClaims.ts` (149) | `crates/orchestration/src/attachment_claims.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/AttachmentPrompt.test.ts` (150) | `crates/orchestration/src/attachment_prompt.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/AttachmentPrompt.ts` (222) | `crates/orchestration/src/attachment_prompt.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/BackgroundWorkStop.integration.test.ts` (446) | `crates/orchestration/src/background_work_stop.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/CheckpointCaptureService.test.ts` (737) | `crates/orchestration/src/checkpoint_capture_service.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/CheckpointCaptureService.ts` (285) | `crates/orchestration/src/checkpoint_capture_service.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/CheckpointRestoreSafety.test.ts` (198) | `crates/orchestration/src/checkpoint_restore_safety.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/CheckpointRestoreSafety.ts` (88) | `crates/orchestration/src/checkpoint_restore_safety.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/CheckpointRollbackService.test.ts` (509) | `crates/orchestration/src/checkpoint_rollback_service.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/CheckpointRollbackService.ts` (362) | `crates/orchestration/src/checkpoint_rollback_service.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/CheckpointScopeOwnership.test.ts` (237) | `crates/orchestration/src/checkpoint_scope_ownership.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/CheckpointService.test.ts` (103) | `crates/orchestration/src/checkpoint_service.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/CheckpointService.ts` (585) | `crates/orchestration/src/checkpoint_service.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/CommandPolicy.test.ts` (631) | `crates/orchestration/src/command_policy.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/CommandPolicy.ts` (491) | `crates/orchestration/src/command_policy.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/CommandReceiptStore.ts` (214) | `crates/orchestration/src/command_receipt_store.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ContextHandoffBudget.test.ts` (691) | `crates/orchestration/src/context_handoff_budget.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ContextHandoffBudget.ts` (282) | `crates/orchestration/src/context_handoff_budget.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ContextHandoffDelivery.ts` (159) | `crates/orchestration/src/context_handoff_delivery.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ContextHandoffService.test.ts` (164) | `crates/orchestration/src/context_handoff_service.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ContextHandoffService.ts` (498) | `crates/orchestration/src/context_handoff_service.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/CursorOrchestratorV2.live.test.ts` (378) | — | — | 対象外：Bex の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/DelegatedCompletionDelivery.test.ts` (1278) | `crates/orchestration/src/delegated_completion_delivery.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/EffectOutbox.ts` (636) | `crates/orchestration/src/effect_outbox.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/EffectWorker.test.ts` (802) | `crates/orchestration/src/effect_worker.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/EffectWorker.ts` (834) | `crates/orchestration/src/effect_worker.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/EventSink.ts` (891) | `crates/orchestration/src/event_sink.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/EventStore.ts` (156) | `crates/orchestration/src/event_store.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/FoundationPersistence.test.ts` (3383) | `crates/orchestration/src/foundation_persistence.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/GrokOrchestratorV2.live.test.ts` (236) | — | — | 対象外：Bex の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/IdAllocator.ts` (435) | `crates/orchestration/src/id_allocator.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/KeyedSerialExecutor.test.ts` (67) | `crates/orchestration/src/keyed_serial_executor.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/KeyedSerialExecutor.ts` (55) | `crates/orchestration/src/keyed_serial_executor.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/LiveStreamBudget.test.ts` (278) | `crates/orchestration/src/live_stream_budget.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/LiveStreamBudget.ts` (342) | `crates/orchestration/src/live_stream_budget.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/Notification.test.ts` (150) | `crates/orchestration/src/notification.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/Notification.ts` (235) | `crates/orchestration/src/notification.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/NotificationMailbox.ts` (23) | `crates/orchestration/src/notification_mailbox.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/OpenCode2OrchestratorV2.integration.test.ts` (1181) | — | — | 対象外：Bex の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/OpenCode2OrchestratorV2.live.test.ts` (1181) | — | — | 対象外：Bex の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/Orchestrator.control-reads.test.ts` (536) | `crates/orchestration/src/orchestrator.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/Orchestrator.migration.test.ts` (100) | — | — | 対象外：製品は未公開。ユーザー指示により旧 V1 の互換性・移行を作らない。 |
| `apps/server/src/orchestration-v2/Orchestrator.ts` (10272) | `crates/orchestration/src/orchestrator.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ProjectCommands.test.ts` (255) | `crates/orchestration/src/project_commands.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ProjectCommands.ts` (241) | `crates/orchestration/src/project_commands.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ProjectSettingsUpgrade.integration.test.ts` (195) | `crates/orchestration/src/project_settings_upgrade.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ProjectStore.test.ts` (56) | `crates/orchestration/src/project_store.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ProjectStore.ts` (278) | `crates/orchestration/src/project_store.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ProjectionControlReads.test.ts` (354) | `crates/orchestration/src/projection_control_reads.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ProjectionMaintenance.ts` (375) | `crates/orchestration/src/projection_maintenance.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ProjectionRecovery.test.ts` (517) | `crates/orchestration/src/projection_recovery.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ProjectionSettlement.test.ts` (525) | `crates/orchestration/src/projection_settlement.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ProjectionStore.test.ts` (4470) | `crates/orchestration/src/projection_store.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ProjectionStore.ts` (6176) | `crates/orchestration/src/projection_store.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ProviderAdapter.ts` (598) | `crates/orchestration/src/provider_adapter.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ProviderAdapterDriver.ts` (44) | `crates/orchestration/src/provider_adapter_driver.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ProviderAdapterRegistry.test.ts` (330) | `crates/orchestration/src/provider_adapter_registry.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ProviderAdapterRegistry.ts` (369) | `crates/orchestration/src/provider_adapter_registry.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ProviderContinuationRequests.ts` (76) | `crates/orchestration/src/provider_continuation_requests.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ProviderContinuationService.test.ts` (820) | `crates/orchestration/src/provider_continuation_service.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ProviderContinuationService.ts` (233) | `crates/orchestration/src/provider_continuation_service.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ProviderEventIngestor.test.ts` (1341) | `crates/orchestration/src/provider_event_ingestor.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ProviderEventIngestor.ts` (641) | `crates/orchestration/src/provider_event_ingestor.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ProviderFailure.test.ts` (209) | `crates/orchestration/src/provider_failure.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ProviderFailure.ts` (251) | `crates/orchestration/src/provider_failure.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ProviderRuntimeRecoveryPerformance.test.ts` (510) | `crates/orchestration/src/provider_runtime_recovery_performance.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ProviderRuntimeRecoveryService.regression.test.ts` (82) | `crates/orchestration/src/provider_runtime_recovery_service.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ProviderRuntimeRecoveryService.test.ts` (1422) | `crates/orchestration/src/provider_runtime_recovery_service.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ProviderRuntimeRecoveryService.ts` (815) | `crates/orchestration/src/provider_runtime_recovery_service.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ProviderSelectionTransition.test.ts` (55) | `crates/orchestration/src/provider_selection_transition.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ProviderSelectionTransition.ts` (37) | `crates/orchestration/src/provider_selection_transition.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ProviderSessionManager.test.ts` (3480) | `crates/orchestration/src/provider_session_manager.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ProviderSessionManager.ts` (2078) | `crates/orchestration/src/provider_session_manager.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ProviderSessionTransitionPolicy.test.ts` (176) | `crates/orchestration/src/provider_session_transition_policy.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ProviderSessionTransitionPolicy.ts` (96) | `crates/orchestration/src/provider_session_transition_policy.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ProviderSwitchService.test.ts` (343) | `crates/orchestration/src/provider_switch_service.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ProviderSwitchService.ts` (219) | `crates/orchestration/src/provider_switch_service.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ProviderTurnControlService.test.ts` (325) | `crates/orchestration/src/provider_turn_control_service.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ProviderTurnControlService.ts` (350) | `crates/orchestration/src/provider_turn_control_service.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ProviderTurnStartService.memory.test.ts` (25) | `crates/orchestration/src/provider_turn_start_service.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ProviderTurnStartService.test.ts` (857) | `crates/orchestration/src/provider_turn_start_service.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ProviderTurnStartService.testkit.ts` (16) | `crates/orchestration/src/provider_turn_start_servicekit.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ProviderTurnStartService.ts` (1266) | `crates/orchestration/src/provider_turn_start_service.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ProviderTurnTokenUsage.test.ts` (43) | `crates/orchestration/src/provider_turn_token_usage.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/PullRequestSyncReactor.test.ts` (966) | — | — | 対象外：今回の指示では M3（PR・workflow 周辺機能）を実装しない。 |
| `apps/server/src/orchestration-v2/PullRequestSyncReactor.ts` (411) | — | — | 対象外：今回の指示では M3（PR・workflow 周辺機能）を実装しない。 |
| `apps/server/src/orchestration-v2/PullRequestWatchReactor.ts` (280) | — | — | 対象外：今回の指示では M3（PR・workflow 周辺機能）を実装しない。 |
| `apps/server/src/orchestration-v2/QueuedRunOrder.test.ts` (51) | `crates/orchestration/src/queued_run_order.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/QueuedRunOrder.ts` (32) | `crates/orchestration/src/queued_run_order.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/RandomUuid.ts` (13) | `crates/orchestration/src/random_uuid.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ResourceCleanupService.ts` (71) | `crates/orchestration/src/resource_cleanup_service.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/RestartBackgroundNote.test.ts` (237) | `crates/orchestration/src/restart_background_note.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/RestartBackgroundNote.ts` (276) | `crates/orchestration/src/restart_background_note.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/RestartContinuation.test.ts` (809) | `crates/orchestration/src/restart_continuation.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/RestartContinuation.ts` (186) | `crates/orchestration/src/restart_continuation.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/RunCompletionReads.test.ts` (162) | `crates/orchestration/src/run_completion_reads.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/RunExecutionService.test.ts` (4018) | `crates/orchestration/src/run_execution_service.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/RunExecutionService.ts` (1467) | `crates/orchestration/src/run_execution_service.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/RunFinalizationService.test.ts` (131) | `crates/orchestration/src/run_finalization_service.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/RunFinalizationService.ts` (121) | `crates/orchestration/src/run_finalization_service.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/RuntimePolicy.test.ts` (156) | `crates/orchestration/src/runtime_policy.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/RuntimePolicy.ts` (170) | `crates/orchestration/src/runtime_policy.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/RuntimeRequestService.test.ts` (293) | `crates/orchestration/src/runtime_request_service.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/RuntimeRequestService.ts` (138) | `crates/orchestration/src/runtime_request_service.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/SelectionRestart.integration.test.ts` (996) | `crates/orchestration/src/selection_restart.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ShellStream.test.ts` (495) | `crates/orchestration/src/shell_stream.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ShellStream.ts` (260) | `crates/orchestration/src/shell_stream.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/SteeringCompletion.integration.test.ts` (704) | `crates/orchestration/src/steering_completion.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/SubagentProjection.test.ts` (308) | `crates/orchestration/src/subagent_projection.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/SubagentProjection.ts` (253) | `crates/orchestration/src/subagent_projection.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ThreadCommandExecutor.ts` (13) | `crates/orchestration/src/thread_command_executor.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ThreadDeletion.test.ts` (298) | `crates/orchestration/src/thread_deletion.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ThreadDeletion.ts` (228) | `crates/orchestration/src/thread_deletion.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ThreadFork.execution.test.ts` (251) | `crates/orchestration/src/thread_fork.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ThreadForkService.test.ts` (193) | `crates/orchestration/src/thread_fork_service.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ThreadForkService.ts` (139) | `crates/orchestration/src/thread_fork_service.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ThreadLaunchService.test.ts` (2218) | `crates/orchestration/src/thread_launch_service.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ThreadLaunchService.ts` (966) | `crates/orchestration/src/thread_launch_service.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ThreadLifecycleService.test.ts` (60) | `crates/orchestration/src/thread_lifecycle_service.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ThreadLifecycleService.ts` (142) | `crates/orchestration/src/thread_lifecycle_service.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ThreadLiveEventCoalescer.test.ts` (266) | `crates/orchestration/src/thread_live_event_coalescer.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ThreadLiveEventCoalescer.ts` (255) | `crates/orchestration/src/thread_live_event_coalescer.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ThreadManagementService.test.ts` (421) | `crates/orchestration/src/thread_management_service.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ThreadManagementService.ts` (782) | `crates/orchestration/src/thread_management_service.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ThreadMessageIntake.test.ts` (766) | `crates/orchestration/src/thread_message_intake.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ThreadMessageIntake.ts` (239) | `crates/orchestration/src/thread_message_intake.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ThreadPullRequestService.test.ts` (322) | — | — | 対象外：今回の指示では M3（PR・workflow 周辺機能）を実装しない。 |
| `apps/server/src/orchestration-v2/ThreadPullRequestService.ts` (408) | — | — | 対象外：今回の指示では M3（PR・workflow 周辺機能）を実装しない。 |
| `apps/server/src/orchestration-v2/ThreadSearch.test.ts` (201) | `crates/orchestration/src/thread_search.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ThreadSearch.ts` (168) | `crates/orchestration/src/thread_search.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ThreadSettlementService.test.ts` (1124) | `crates/orchestration/src/thread_settlement_service.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ThreadSettlementService.ts` (597) | `crates/orchestration/src/thread_settlement_service.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ThreadStream.test.ts` (264) | `crates/orchestration/src/thread_stream.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ThreadStream.ts` (96) | `crates/orchestration/src/thread_stream.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ThreadTitleRegenerationService.test.ts` (506) | `crates/orchestration/src/thread_title_regeneration_service.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ThreadTitleRegenerationService.ts` (150) | `crates/orchestration/src/thread_title_regeneration_service.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ThreadTransportPerformance.test.ts` (254) | `crates/orchestration/src/thread_transport_performance.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/TurnItemPositionStore.ts` (107) | `crates/orchestration/src/turn_item_position_store.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/TurnStartReads.test.ts` (369) | `crates/orchestration/src/turn_start_reads.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/UsageLimitRecoveryWorker.ts` (115) | `crates/orchestration/src/usage_limit_recovery_worker.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/UserFacingErrors.test.ts` (68) | `crates/orchestration/src/user_facing_errors.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/UserFacingErrors.ts` (110) | `crates/orchestration/src/user_facing_errors.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/V1ImportBoundary.test.ts` (91) | `crates/orchestration/src/v1_import_boundary.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/WireProjection.test.ts` (351) | `crates/orchestration/src/wire_projection.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/WireProjection.ts` (217) | `crates/orchestration/src/wire_projection.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/assistantStreaming.test.ts` (150) | `crates/orchestration/src/assistant_streaming.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/assistantStreaming.ts` (129) | `crates/orchestration/src/assistant_streaming.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/builtInProviderAdapterDrivers.ts` (47) | `crates/orchestration/src/built_in_provider_adapter_drivers.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/http.ts` (261) | `crates/orchestration/src/http.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/legacy/LegacyV1Cutover.integration.test.ts` (1045) | — | — | 対象外：製品は未公開。ユーザー指示により旧 V1 の互換性・移行を作らない。 |
| `apps/server/src/orchestration-v2/legacy/LegacyV1ThreadImporter.test.ts` (532) | — | — | 対象外：製品は未公開。ユーザー指示により旧 V1 の互換性・移行を作らない。 |
| `apps/server/src/orchestration-v2/legacy/LegacyV1ThreadImporter.ts` (833) | — | — | 対象外：製品は未公開。ユーザー指示により旧 V1 の互換性・移行を作らない。 |
| `apps/server/src/orchestration-v2/pullRequestWatch.test.ts` (217) | — | — | 対象外：今回の指示では M3（PR・workflow 周辺機能）を実装しない。 |
| `apps/server/src/orchestration-v2/pullRequestWatch.ts` (209) | — | — | 対象外：今回の指示では M3（PR・workflow 周辺機能）を実装しない。 |
| `apps/server/src/orchestration-v2/runtimeLayer.test.ts` (4758) | `crates/orchestration/src/runtime_layer.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/runtimeLayer.ts` (325) | `crates/orchestration/src/runtime_layer.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/ClaudeReplayFixtures.integration.test.ts` (382) | `crates/orchestration/src/testkit/claude_replay_fixtures_integration_test.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/CodexReplayFixtures.integration.test.ts` (773) | `crates/orchestration/src/testkit/codex_replay_fixtures_integration_test.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/DeterministicRuntime.ts` (13) | `crates/orchestration/src/testkit/deterministic_runtime.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/OrchestratorReplayFixtures.contract.test.ts` (158) | `crates/orchestration/src/testkit/orchestrator_replay_fixtures_contract_test.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/OrchestratorReplayFixtures.integration.test.ts` (296) | `crates/orchestration/src/testkit/orchestrator_replay_fixtures_integration_test.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/OrchestratorReplayRecovery.integration.test.ts` (424) | `crates/orchestration/src/testkit/orchestrator_replay_recovery_integration_test.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/OrchestratorReplayRestartBackgroundNote.integration.test.ts` (262) | `crates/orchestration/src/testkit/orchestrator_replay_restart_background_note_integration_test.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/OrchestratorScenario.ts` (739) | `crates/orchestration/src/testkit/orchestrator_scenario.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/ProviderReplayGate.testkit.ts` (119) | `crates/orchestration/src/testkit/provider_replay_gate_testkit.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/ProviderReplayHarness.ts` (508) | `crates/orchestration/src/testkit/provider_replay_harness.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/ProviderSwitch.integration.test.ts` (3315) | `crates/orchestration/src/testkit/provider_switch_integration_test.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/ReplayFixtureWorkspace.ts` (86) | `crates/orchestration/src/testkit/replay_fixture_workspace.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/ReplayTranscriptNdjson.ts` (248) | `crates/orchestration/src/testkit/replay_transcript_ndjson.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/ThreadFork.integration.test.ts` (1200) | `crates/orchestration/src/testkit/thread_fork_integration_test.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/ThreadMergeBack.integration.test.ts` (732) | `crates/orchestration/src/testkit/thread_merge_back_integration_test.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/acp_elicitation/registry_transcript.ndjson` (11) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_background_monitor_wake/claude_transcript.ndjson` (87) | `crates/provider-adapters/src/testkit/fixtures/claude_background_monitor_wake/claude_transcript.ndjson` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_background_monitor_wake/input.ts` (29) | `crates/orchestration/src/testkit/fixtures/claude_background_monitor_wake/input.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_background_monitor_wake/output.ts` (67) | `crates/orchestration/src/testkit/fixtures/claude_background_monitor_wake/output.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_background_subagent_after_root/claude_transcript.ndjson` (137) | `crates/provider-adapters/src/testkit/fixtures/claude_background_subagent_after_root/claude_transcript.ndjson` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_background_subagent_after_root/input.ts` (31) | `crates/orchestration/src/testkit/fixtures/claude_background_subagent_after_root/input.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_background_subagent_after_root/output.ts` (100) | `crates/orchestration/src/testkit/fixtures/claude_background_subagent_after_root/output.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_background_subagent_lifecycle/claude_transcript.ndjson` (334) | `crates/provider-adapters/src/testkit/fixtures/claude_background_subagent_lifecycle/claude_transcript.ndjson` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_background_subagent_lifecycle/input.ts` (38) | `crates/orchestration/src/testkit/fixtures/claude_background_subagent_lifecycle/input.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_background_subagent_lifecycle/output.ts` (251) | `crates/orchestration/src/testkit/fixtures/claude_background_subagent_lifecycle/output.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_background_task_after_root/claude_transcript.ndjson` (13) | `crates/provider-adapters/src/testkit/fixtures/claude_background_task_after_root/claude_transcript.ndjson` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_background_task_after_root/input.ts` (26) | `crates/orchestration/src/testkit/fixtures/claude_background_task_after_root/input.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_background_task_after_root/output.ts` (96) | `crates/orchestration/src/testkit/fixtures/claude_background_task_after_root/output.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_background_task_interrupt/claude_transcript.ndjson` (103) | `crates/provider-adapters/src/testkit/fixtures/claude_background_task_interrupt/claude_transcript.ndjson` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_background_task_interrupt/input.ts` (21) | `crates/orchestration/src/testkit/fixtures/claude_background_task_interrupt/input.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_background_task_interrupt/output.ts` (50) | `crates/orchestration/src/testkit/fixtures/claude_background_task_interrupt/output.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_background_task_wake/claude_transcript.ndjson` (111) | `crates/provider-adapters/src/testkit/fixtures/claude_background_task_wake/claude_transcript.ndjson` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_background_task_wake/input.ts` (33) | `crates/orchestration/src/testkit/fixtures/claude_background_task_wake/input.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_background_task_wake/output.ts` (102) | `crates/orchestration/src/testkit/fixtures/claude_background_task_wake/output.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_background_wake_before_queued_prompt/claude_transcript.ndjson` (338) | `crates/provider-adapters/src/testkit/fixtures/claude_background_wake_before_queued_prompt/claude_transcript.ndjson` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_background_wake_before_queued_prompt/input.ts` (34) | `crates/orchestration/src/testkit/fixtures/claude_background_wake_before_queued_prompt/input.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_background_wake_before_queued_prompt/output.ts` (96) | `crates/orchestration/src/testkit/fixtures/claude_background_wake_before_queued_prompt/output.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_background_wake_before_queued_prompt_no_echo/claude_transcript.ndjson` (262) | `crates/provider-adapters/src/testkit/fixtures/claude_background_wake_before_queued_prompt_no_echo/claude_transcript.ndjson` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_background_wake_before_queued_prompt_no_echo/input.ts` (23) | `crates/orchestration/src/testkit/fixtures/claude_background_wake_before_queued_prompt_no_echo/input.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_background_wake_before_queued_prompt_no_echo/output.ts` (36) | `crates/orchestration/src/testkit/fixtures/claude_background_wake_before_queued_prompt_no_echo/output.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_compact_after_peer_turn/claude_transcript.ndjson` (38) | `crates/provider-adapters/src/testkit/fixtures/claude_compact_after_peer_turn/claude_transcript.ndjson` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_compact_after_peer_turn/input.ts` (33) | `crates/orchestration/src/testkit/fixtures/claude_compact_after_peer_turn/input.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_compact_after_peer_turn/output.ts` (58) | `crates/orchestration/src/testkit/fixtures/claude_compact_after_peer_turn/output.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_compact_after_peer_turn_no_echo/claude_transcript.ndjson` (25) | `crates/provider-adapters/src/testkit/fixtures/claude_compact_after_peer_turn_no_echo/claude_transcript.ndjson` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_compact_after_peer_turn_no_echo/output.ts` (35) | `crates/orchestration/src/testkit/fixtures/claude_compact_after_peer_turn_no_echo/output.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_compact_after_resume_wake/claude_transcript.ndjson` (43) | `crates/provider-adapters/src/testkit/fixtures/claude_compact_after_resume_wake/claude_transcript.ndjson` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_compact_after_resume_wake/input.ts` (16) | `crates/orchestration/src/testkit/fixtures/claude_compact_after_resume_wake/input.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_compact_after_resume_wake/output.ts` (36) | `crates/orchestration/src/testkit/fixtures/claude_compact_after_resume_wake/output.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_idle_resume/claude_transcript.ndjson` (19) | `crates/provider-adapters/src/testkit/fixtures/claude_idle_resume/claude_transcript.ndjson` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_idle_resume/input.ts` (25) | `crates/orchestration/src/testkit/fixtures/claude_idle_resume/input.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_idle_resume/output.ts` (39) | `crates/orchestration/src/testkit/fixtures/claude_idle_resume/output.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_local_bash_task/claude_transcript.ndjson` (12) | `crates/provider-adapters/src/testkit/fixtures/claude_local_bash_task/claude_transcript.ndjson` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_local_bash_task/input.ts` (7) | `crates/orchestration/src/testkit/fixtures/claude_local_bash_task/input.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_local_bash_task/output.ts` (58) | `crates/orchestration/src/testkit/fixtures/claude_local_bash_task/output.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_mcp_tool_presentation/claude_transcript.ndjson` (94) | `crates/provider-adapters/src/testkit/fixtures/claude_mcp_tool_presentation/claude_transcript.ndjson` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_mcp_tool_presentation/input.ts` (14) | `crates/orchestration/src/testkit/fixtures/claude_mcp_tool_presentation/input.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_mcp_tool_presentation/output.ts` (51) | `crates/orchestration/src/testkit/fixtures/claude_mcp_tool_presentation/output.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_nested_background_subagent_wake/claude_transcript.ndjson` (163) | `crates/provider-adapters/src/testkit/fixtures/claude_nested_background_subagent_wake/claude_transcript.ndjson` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_nested_background_subagent_wake/input.ts` (31) | `crates/orchestration/src/testkit/fixtures/claude_nested_background_subagent_wake/input.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_nested_background_subagent_wake/output.ts` (58) | `crates/orchestration/src/testkit/fixtures/claude_nested_background_subagent_wake/output.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_nested_subagent_model/claude_transcript.ndjson` (105) | `crates/provider-adapters/src/testkit/fixtures/claude_nested_subagent_model/claude_transcript.ndjson` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_nested_subagent_model/input.ts` (17) | `crates/orchestration/src/testkit/fixtures/claude_nested_subagent_model/input.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_nested_subagent_model/output.ts` (60) | `crates/orchestration/src/testkit/fixtures/claude_nested_subagent_model/output.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_result_is_error/claude_transcript.ndjson` (10) | `crates/provider-adapters/src/testkit/fixtures/claude_result_is_error/claude_transcript.ndjson` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_result_is_error/input.ts` (22) | `crates/orchestration/src/testkit/fixtures/claude_result_is_error/input.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_result_is_error/output.ts` (73) | `crates/orchestration/src/testkit/fixtures/claude_result_is_error/output.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_subagent_resume_after_restart/claude_transcript.ndjson` (61) | `crates/provider-adapters/src/testkit/fixtures/claude_subagent_resume_after_restart/claude_transcript.ndjson` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/delegated_task_status/codex_transcript.ndjson` (41) | `crates/provider-adapters/src/testkit/fixtures/delegated_task_status/codex_transcript.ndjson` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/grok_auto_blocked_command/grok_transcript.ndjson` (136) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/grok_auto_blocked_command/input.ts` (30) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/grok_auto_blocked_command/output.ts` (97) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/grok_background_bash/grok_transcript.ndjson` (241) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/grok_background_bash/input.ts` (24) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/grok_background_bash/output.ts` (100) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/grok_background_bash_fast_wake/grok_transcript.ndjson` (258) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/grok_background_bash_fast_wake/input.ts` (18) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/grok_background_bash_fast_wake/output.ts` (11) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/grok_background_subagent/grok_transcript.ndjson` (353) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/grok_background_subagent/input.ts` (23) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/grok_background_subagent/output.ts` (116) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/grok_monitor/grok_transcript.ndjson` (361) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/grok_monitor/input.ts` (25) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/grok_monitor/output.ts` (112) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/grok_prompt_error/grok_transcript.ndjson` (74) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/grok_prompt_error/input.ts` (20) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/grok_prompt_error/output.ts` (45) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/grok_subagent_lineage/grok_transcript.ndjson` (17) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/grok_subagent_lineage/input.ts` (9) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/grok_subagent_lineage/output.ts` (98) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/index.ts` (1633) | `crates/orchestration/src/testkit/fixtures/index.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/message_steering/claude_output.ts` (43) | `crates/orchestration/src/testkit/fixtures/message_steering/claude_output.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/message_steering/claude_transcript.ndjson` (13) | `crates/provider-adapters/src/testkit/fixtures/message_steering/claude_transcript.ndjson` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/message_steering/codex_output.ts` (43) | `crates/orchestration/src/testkit/fixtures/message_steering/codex_output.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/message_steering/codex_transcript.ndjson` (43) | `crates/provider-adapters/src/testkit/fixtures/message_steering/codex_transcript.ndjson` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/message_steering/cursor_output.ts` (56) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/message_steering/cursor_transcript.ndjson` (25) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/message_steering/grok_output.ts` (55) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/message_steering/grok_transcript.ndjson` (67) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/message_steering/input.ts` (31) | `crates/orchestration/src/testkit/fixtures/message_steering/input.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/message_steering/pi_output.ts` (62) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/message_steering/pi_transcript.ndjson` (65) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/message_steering/registry_transcript.ndjson` (14) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/multi_turn/claude_output.ts` (53) | `crates/orchestration/src/testkit/fixtures/multi_turn/claude_output.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/multi_turn/claude_transcript.ndjson` (14) | `crates/provider-adapters/src/testkit/fixtures/multi_turn/claude_transcript.ndjson` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/multi_turn/codex_output.ts` (40) | `crates/orchestration/src/testkit/fixtures/multi_turn/codex_output.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/multi_turn/codex_transcript.ndjson` (46) | `crates/provider-adapters/src/testkit/fixtures/multi_turn/codex_transcript.ndjson` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/multi_turn/cursor_transcript.ndjson` (44) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/multi_turn/grok_transcript.ndjson` (91) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/multi_turn/input.ts` (14) | `crates/orchestration/src/testkit/fixtures/multi_turn/input.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/multi_turn/pi_output.ts` (17) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/multi_turn/pi_transcript.ndjson` (72) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/multi_turn/registry_transcript.ndjson` (12) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/multi_turn_restart/claude_transcript.ndjson` (19) | `crates/provider-adapters/src/testkit/fixtures/multi_turn_restart/claude_transcript.ndjson` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_background/input.ts` (15) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_background/opencode_transcript.ndjson` (69) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_background/output.ts` (114) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_command/input.ts` (5) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_command/opencode_transcript.ndjson` (34) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_command/output.ts` (34) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_compaction/input.ts` (15) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_compaction/opencode_transcript.ndjson` (66) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_compaction/output.ts` (61) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_fork/opencode_transcript.ndjson` (69) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_inbox/input.ts` (24) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_inbox/opencode_transcript.ndjson` (53) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_inbox/output.ts` (72) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_interrupt/input.ts` (11) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_interrupt/opencode_transcript.ndjson` (36) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_interrupt/output.ts` (47) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_nested_background/input.ts` (15) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_nested_background/opencode_transcript.ndjson` (131) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_nested_background/output.ts` (77) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_permission/input.ts` (15) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_permission/opencode_transcript.ndjson` (117) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_permission/output.ts` (81) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_question/input.ts` (11) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_question/opencode_transcript.ndjson` (42) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_question/output.ts` (59) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_resume_after_restart/input.ts` (14) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_resume_after_restart/opencode_transcript.ndjson` (54) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_resume_after_restart/output.ts` (53) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_revert/input.ts` (17) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_revert/opencode_transcript.ndjson` (68) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_revert/output.ts` (39) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_simple/input.ts` (5) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_simple/opencode_transcript.ndjson` (53) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_simple/output.ts` (51) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_skill/input.ts` (5) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_skill/opencode_transcript.ndjson` (32) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_skill/output.ts` (34) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_subagent/input.ts` (5) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_subagent/opencode_transcript.ndjson` (83) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_subagent/output.ts` (74) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_switch/opencode_transcript.ndjson` (88) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_tool_call/input.ts` (5) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_tool_call/opencode_transcript.ndjson` (44) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_tool_call/output.ts` (56) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode_child_approval/input.ts` (10) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode_child_approval/opencode_transcript.ndjson` (40) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode_child_approval/output.ts` (43) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode_running_child_approval/input.ts` (16) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode_running_child_approval/opencode_transcript.ndjson` (41) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode_running_child_approval/output.ts` (88) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode_subagent/input.ts` (7) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode_subagent/opencode_transcript.ndjson` (34) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode_subagent/output.ts` (71) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/pi_compaction/input.ts` (30) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/pi_compaction/output.ts` (93) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/pi_compaction/pi_transcript.ndjson` (160) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/plan_questions/codex_output.ts` (67) | `crates/orchestration/src/testkit/fixtures/plan_questions/codex_output.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/plan_questions/codex_transcript.ndjson` (39) | `crates/provider-adapters/src/testkit/fixtures/plan_questions/codex_transcript.ndjson` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/plan_questions/grok_transcript.ndjson` (11) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/plan_questions/input.ts` (18) | `crates/orchestration/src/testkit/fixtures/plan_questions/input.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/plan_questions/opencode_output.ts` (26) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/plan_questions/opencode_transcript.ndjson` (27) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/proposed_plan/codex_output.ts` (43) | `crates/orchestration/src/testkit/fixtures/proposed_plan/codex_output.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/proposed_plan/codex_transcript.ndjson` (278) | `crates/provider-adapters/src/testkit/fixtures/proposed_plan/codex_transcript.ndjson` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/proposed_plan/cursor_output.ts` (39) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/proposed_plan/cursor_transcript.ndjson` (132) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/proposed_plan/input.ts` (8) | `crates/orchestration/src/testkit/fixtures/proposed_plan/input.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/provider_thread_resume/codex_transcript.ndjson` (79) | `crates/provider-adapters/src/testkit/fixtures/provider_thread_resume/codex_transcript.ndjson` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/provider_thread_resume/cursor_transcript.ndjson` (67) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/provider_thread_resume/input.ts` (21) | `crates/orchestration/src/testkit/fixtures/provider_thread_resume/input.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/provider_thread_resume/pi_output.ts` (68) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/provider_thread_resume/pi_transcript.ndjson` (124) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/queued_cancelled_while_active/codex_output.ts` (46) | `crates/orchestration/src/testkit/fixtures/queued_cancelled_while_active/codex_output.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/queued_cancelled_while_active/input.ts` (25) | `crates/orchestration/src/testkit/fixtures/queued_cancelled_while_active/input.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/queued_turn/claude_transcript.ndjson` (14) | `crates/provider-adapters/src/testkit/fixtures/queued_turn/claude_transcript.ndjson` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/queued_turn/codex_output.ts` (49) | `crates/orchestration/src/testkit/fixtures/queued_turn/codex_output.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/queued_turn/codex_transcript.ndjson` (46) | `crates/provider-adapters/src/testkit/fixtures/queued_turn/codex_transcript.ndjson` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/queued_turn/cursor_transcript.ndjson` (36) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/queued_turn/grok_transcript.ndjson` (88) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/queued_turn/input.ts` (14) | `crates/orchestration/src/testkit/fixtures/queued_turn/input.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/queued_turn/registry_transcript.ndjson` (12) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/shared.ts` (1485) | `crates/orchestration/src/testkit/fixtures/shared.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/simple/claude_output.ts` (33) | `crates/orchestration/src/testkit/fixtures/simple/claude_output.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/simple/claude_transcript.ndjson` (10) | `crates/provider-adapters/src/testkit/fixtures/simple/claude_transcript.ndjson` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/simple/codex_output.ts` (33) | `crates/orchestration/src/testkit/fixtures/simple/codex_output.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/simple/codex_transcript.ndjson` (30) | `crates/provider-adapters/src/testkit/fixtures/simple/codex_transcript.ndjson` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/simple/cursor_transcript.ndjson` (20) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/simple/grok_transcript.ndjson` (76) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/simple/input.ts` (7) | `crates/orchestration/src/testkit/fixtures/simple/input.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/simple/opencode_transcript.ndjson` (23) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/simple/pi_output.ts` (117) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/simple/pi_transcript.ndjson` (46) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/simple/registry_transcript.ndjson` (12) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/skill_invocation/cursor_output.ts` (39) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/skill_invocation/cursor_transcript.ndjson` (64) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/skill_invocation/input.ts` (19) | `crates/orchestration/src/testkit/fixtures/skill_invocation/input.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/stop_background_work_after_failed_turn/input.ts` (33) | `crates/orchestration/src/testkit/fixtures/stop_background_work_after_failed_turn/input.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/stop_background_work_after_failed_turn/output.ts` (44) | `crates/orchestration/src/testkit/fixtures/stop_background_work_after_failed_turn/output.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/stop_background_work_after_failed_turn/registry_transcript.ndjson` (10) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/subagent/claude_output.ts` (123) | `crates/orchestration/src/testkit/fixtures/subagent/claude_output.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/subagent/claude_transcript.ndjson` (29) | `crates/provider-adapters/src/testkit/fixtures/subagent/claude_transcript.ndjson` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/subagent/codex_output.ts` (124) | `crates/orchestration/src/testkit/fixtures/subagent/codex_output.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/subagent/codex_transcript.ndjson` (628) | `crates/provider-adapters/src/testkit/fixtures/subagent/codex_transcript.ndjson` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/subagent/cursor_output.ts` (96) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/subagent/cursor_transcript.ndjson` (353) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/subagent/input.ts` (7) | `crates/orchestration/src/testkit/fixtures/subagent/input.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/subagent_continue/codex_output.ts` (60) | `crates/orchestration/src/testkit/fixtures/subagent_continue/codex_output.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/subagent_continue/codex_transcript.ndjson` (138) | `crates/provider-adapters/src/testkit/fixtures/subagent_continue/codex_transcript.ndjson` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/subagent_continue/input.ts` (14) | `crates/orchestration/src/testkit/fixtures/subagent_continue/input.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/subagent_v2/codex_output.ts` (86) | `crates/orchestration/src/testkit/fixtures/subagent_v2/codex_output.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/subagent_v2/codex_transcript.ndjson` (49) | `crates/provider-adapters/src/testkit/fixtures/subagent_v2/codex_transcript.ndjson` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/subagent_v2/input.ts` (17) | `crates/orchestration/src/testkit/fixtures/subagent_v2/input.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/subagent_v2_approval/codex_output.ts` (87) | `crates/orchestration/src/testkit/fixtures/subagent_v2_approval/codex_output.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/subagent_v2_approval/codex_transcript.ndjson` (68) | `crates/provider-adapters/src/testkit/fixtures/subagent_v2_approval/codex_transcript.ndjson` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/subagent_v2_approval/input.ts` (21) | `crates/orchestration/src/testkit/fixtures/subagent_v2_approval/input.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/subagent_v2_nested/codex_output.ts` (135) | `crates/orchestration/src/testkit/fixtures/subagent_v2_nested/codex_output.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/subagent_v2_nested/codex_transcript.ndjson` (98) | `crates/provider-adapters/src/testkit/fixtures/subagent_v2_nested/codex_transcript.ndjson` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/subagent_v2_nested_approval/codex_output.ts` (105) | `crates/orchestration/src/testkit/fixtures/subagent_v2_nested_approval/codex_output.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/subagent_v2_nested_approval/codex_transcript.ndjson` (84) | `crates/provider-adapters/src/testkit/fixtures/subagent_v2_nested_approval/codex_transcript.ndjson` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/subagent_v2_nested_approval/input.ts` (15) | `crates/orchestration/src/testkit/fixtures/subagent_v2_nested_approval/input.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/thread_fork_native/claude_transcript.ndjson` (21) | `crates/provider-adapters/src/testkit/fixtures/thread_fork_native/claude_transcript.ndjson` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/thread_fork_native/codex_transcript.ndjson` (21) | `crates/provider-adapters/src/testkit/fixtures/thread_fork_native/codex_transcript.ndjson` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/thread_fork_native_continue/claude_transcript.ndjson` (25) | `crates/provider-adapters/src/testkit/fixtures/thread_fork_native_continue/claude_transcript.ndjson` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/thread_fork_native_continue/codex_transcript.ndjson` (82) | `crates/provider-adapters/src/testkit/fixtures/thread_fork_native_continue/codex_transcript.ndjson` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/thread_fork_native_fork_local_rollback/claude_transcript.ndjson` (35) | `crates/provider-adapters/src/testkit/fixtures/thread_fork_native_fork_local_rollback/claude_transcript.ndjson` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/thread_fork_native_prior_turn/claude_transcript.ndjson` (26) | `crates/provider-adapters/src/testkit/fixtures/thread_fork_native_prior_turn/claude_transcript.ndjson` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/thread_fork_native_prior_turn/codex_transcript.ndjson` (928) | `crates/provider-adapters/src/testkit/fixtures/thread_fork_native_prior_turn/codex_transcript.ndjson` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/thread_fork_native_siblings/claude_transcript.ndjson` (32) | `crates/provider-adapters/src/testkit/fixtures/thread_fork_native_siblings/claude_transcript.ndjson` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/thread_fork_native_siblings/codex_transcript.ndjson` (107) | `crates/provider-adapters/src/testkit/fixtures/thread_fork_native_siblings/codex_transcript.ndjson` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/thread_merge_back_continue/claude_transcript.ndjson` (34) | `crates/provider-adapters/src/testkit/fixtures/thread_merge_back_continue/claude_transcript.ndjson` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/thread_merge_back_continue/codex_transcript.ndjson` (95) | `crates/provider-adapters/src/testkit/fixtures/thread_merge_back_continue/codex_transcript.ndjson` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/thread_merge_back_siblings/claude_transcript.ndjson` (49) | `crates/provider-adapters/src/testkit/fixtures/thread_merge_back_siblings/claude_transcript.ndjson` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/thread_merge_back_siblings/codex_transcript.ndjson` (155) | `crates/provider-adapters/src/testkit/fixtures/thread_merge_back_siblings/codex_transcript.ndjson` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/thread_rollback/claude_output.ts` (71) | `crates/orchestration/src/testkit/fixtures/thread_rollback/claude_output.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/thread_rollback/claude_transcript.ndjson` (24) | `crates/provider-adapters/src/testkit/fixtures/thread_rollback/claude_transcript.ndjson` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/thread_rollback/codex_output.ts` (72) | `crates/orchestration/src/testkit/fixtures/thread_rollback/codex_output.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/thread_rollback/codex_transcript.ndjson` (99) | `crates/provider-adapters/src/testkit/fixtures/thread_rollback/codex_transcript.ndjson` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/thread_rollback/input.ts` (21) | `crates/orchestration/src/testkit/fixtures/thread_rollback/input.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/thread_rollback/pi_output.ts` (83) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/thread_rollback/pi_transcript.ndjson` (163) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/thread_rollback_after_restart/codex_output.ts` (31) | `crates/orchestration/src/testkit/fixtures/thread_rollback_after_restart/codex_output.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/thread_rollback_after_restart/codex_transcript.ndjson` (107) | `crates/provider-adapters/src/testkit/fixtures/thread_rollback_after_restart/codex_transcript.ndjson` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/thread_rollback_after_restart/input.ts` (27) | `crates/orchestration/src/testkit/fixtures/thread_rollback_after_restart/input.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/thread_rollback_after_stop/input.ts` (29) | `crates/orchestration/src/testkit/fixtures/thread_rollback_after_stop/input.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/thread_rollback_after_stop/pi_output.ts` (83) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/thread_rollback_after_stop/pi_transcript.ndjson` (274) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/thread_rollback_to_stopped_turn/codex_output.ts` (66) | `crates/orchestration/src/testkit/fixtures/thread_rollback_to_stopped_turn/codex_output.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/thread_rollback_to_stopped_turn/codex_transcript.ndjson` (279) | `crates/provider-adapters/src/testkit/fixtures/thread_rollback_to_stopped_turn/codex_transcript.ndjson` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/thread_rollback_to_stopped_turn/input.ts` (28) | `crates/orchestration/src/testkit/fixtures/thread_rollback_to_stopped_turn/input.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/todo_list/codex_output.ts` (39) | `crates/orchestration/src/testkit/fixtures/todo_list/codex_output.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/todo_list/codex_transcript.ndjson` (37) | `crates/provider-adapters/src/testkit/fixtures/todo_list/codex_transcript.ndjson` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/todo_list/cursor_output.ts` (72) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/todo_list/cursor_transcript.ndjson` (210) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/todo_list/grok_output.ts` (52) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/todo_list/grok_transcript.ndjson` (329) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/todo_list/input.ts` (7) | `crates/orchestration/src/testkit/fixtures/todo_list/input.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/todo_list/registry_transcript.ndjson` (16) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/tool_call_denied_write/claude_output.ts` (44) | `crates/orchestration/src/testkit/fixtures/tool_call_denied_write/claude_output.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/tool_call_denied_write/claude_transcript.ndjson` (89) | `crates/provider-adapters/src/testkit/fixtures/tool_call_denied_write/claude_transcript.ndjson` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/tool_call_denied_write/input.ts` (24) | `crates/orchestration/src/testkit/fixtures/tool_call_denied_write/input.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/tool_call_read_only/claude_output.ts` (62) | `crates/orchestration/src/testkit/fixtures/tool_call_read_only/claude_output.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/tool_call_read_only/claude_transcript.ndjson` (16) | `crates/provider-adapters/src/testkit/fixtures/tool_call_read_only/claude_transcript.ndjson` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/tool_call_read_only/cursor_output.ts` (58) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/tool_call_read_only/cursor_transcript.ndjson` (49) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/tool_call_read_only/grok_transcript.ndjson` (15) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/tool_call_read_only/input.ts` (7) | `crates/orchestration/src/testkit/fixtures/tool_call_read_only/input.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/tool_call_read_only/registry_transcript.ndjson` (15) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/tool_call_read_only_on_request/claude_transcript.ndjson` (15) | `crates/provider-adapters/src/testkit/fixtures/tool_call_read_only_on_request/claude_transcript.ndjson` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/tool_call_read_only_on_request/codex_transcript.ndjson` (81) | `crates/provider-adapters/src/testkit/fixtures/tool_call_read_only_on_request/codex_transcript.ndjson` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/tool_call_read_only_on_request/grok_transcript.ndjson` (181) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/tool_call_read_only_on_request/input.ts` (10) | `crates/orchestration/src/testkit/fixtures/tool_call_read_only_on_request/input.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/tool_call_read_only_on_request/output.ts` (108) | `crates/orchestration/src/testkit/fixtures/tool_call_read_only_on_request/output.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/tool_call_read_only_on_request/registry_transcript.ndjson` (13) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/tool_call_restricted_granular/claude_output.ts` (41) | `crates/orchestration/src/testkit/fixtures/tool_call_restricted_granular/claude_output.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/tool_call_restricted_granular/claude_transcript.ndjson` (15) | `crates/provider-adapters/src/testkit/fixtures/tool_call_restricted_granular/claude_transcript.ndjson` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/tool_call_restricted_granular/codex_output.ts` (36) | `crates/orchestration/src/testkit/fixtures/tool_call_restricted_granular/codex_output.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/tool_call_restricted_granular/codex_transcript.ndjson` (80) | `crates/provider-adapters/src/testkit/fixtures/tool_call_restricted_granular/codex_transcript.ndjson` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/tool_call_restricted_granular/input.ts` (10) | `crates/orchestration/src/testkit/fixtures/tool_call_restricted_granular/input.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/tool_call_workspace_never/claude_output.ts` (34) | `crates/orchestration/src/testkit/fixtures/tool_call_workspace_never/claude_output.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/tool_call_workspace_never/claude_transcript.ndjson` (13) | `crates/provider-adapters/src/testkit/fixtures/tool_call_workspace_never/claude_transcript.ndjson` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/tool_call_workspace_never/codex_output.ts` (29) | `crates/orchestration/src/testkit/fixtures/tool_call_workspace_never/codex_output.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/tool_call_workspace_never/codex_transcript.ndjson` (68) | `crates/provider-adapters/src/testkit/fixtures/tool_call_workspace_never/codex_transcript.ndjson` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/tool_call_workspace_never/input.ts` (7) | `crates/orchestration/src/testkit/fixtures/tool_call_workspace_never/input.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/turn_interrupt/claude_output.ts` (51) | `crates/orchestration/src/testkit/fixtures/turn_interrupt/claude_output.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/turn_interrupt/claude_transcript.ndjson` (7) | `crates/provider-adapters/src/testkit/fixtures/turn_interrupt/claude_transcript.ndjson` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/turn_interrupt/codex_output.ts` (49) | `crates/orchestration/src/testkit/fixtures/turn_interrupt/codex_output.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/turn_interrupt/codex_transcript.ndjson` (25) | `crates/provider-adapters/src/testkit/fixtures/turn_interrupt/codex_transcript.ndjson` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/turn_interrupt/grok_transcript.ndjson` (28) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/turn_interrupt/input.ts` (10) | `crates/orchestration/src/testkit/fixtures/turn_interrupt/input.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/turn_interrupt/opencode_transcript.ndjson` (20) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/turn_interrupt/registry_transcript.ndjson` (9) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/turn_interrupt_mid_tool/claude_output.ts` (95) | `crates/orchestration/src/testkit/fixtures/turn_interrupt_mid_tool/claude_output.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/turn_interrupt_mid_tool/claude_transcript.ndjson` (11) | `crates/provider-adapters/src/testkit/fixtures/turn_interrupt_mid_tool/claude_transcript.ndjson` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/turn_interrupt_mid_tool/codex_output.ts` (150) | `crates/orchestration/src/testkit/fixtures/turn_interrupt_mid_tool/codex_output.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/turn_interrupt_mid_tool/codex_transcript.ndjson` (35) | `crates/provider-adapters/src/testkit/fixtures/turn_interrupt_mid_tool/codex_transcript.ndjson` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/turn_interrupt_mid_tool/cursor_output.ts` (76) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/turn_interrupt_mid_tool/cursor_transcript.ndjson` (33) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/turn_interrupt_mid_tool/input.ts` (10) | `crates/orchestration/src/testkit/fixtures/turn_interrupt_mid_tool/input.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/turn_interrupt_mid_tool/pi_output.ts` (67) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/turn_interrupt_mid_tool/pi_transcript.ndjson` (59) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/turn_interrupt_restart/claude_output.ts` (129) | `crates/orchestration/src/testkit/fixtures/turn_interrupt_restart/claude_output.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/turn_interrupt_restart/claude_transcript.ndjson` (20) | `crates/provider-adapters/src/testkit/fixtures/turn_interrupt_restart/claude_transcript.ndjson` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/turn_interrupt_restart/input.ts` (15) | `crates/orchestration/src/testkit/fixtures/turn_interrupt_restart/input.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/web_search/claude_output.ts` (58) | `crates/orchestration/src/testkit/fixtures/web_search/claude_output.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/web_search/claude_transcript.ndjson` (16) | `crates/provider-adapters/src/testkit/fixtures/web_search/claude_transcript.ndjson` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/web_search/codex_output.ts` (42) | `crates/orchestration/src/testkit/fixtures/web_search/codex_output.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/web_search/codex_transcript.ndjson` (54) | `crates/provider-adapters/src/testkit/fixtures/web_search/codex_transcript.ndjson` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/web_search/input.ts` (7) | `crates/orchestration/src/testkit/fixtures/web_search/input.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/pullRequestFixtures.ts` (70) | `crates/orchestration/src/testkit/pull_request_fixtures.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/threadHistoryPaging.test.ts` (702) | `crates/orchestration/src/thread_history_paging.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/threadHistoryPaging.ts` (532) | `crates/orchestration/src/thread_history_paging.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/workflowScriptQuery.test.ts` (74) | — | — | 対象外：今回の指示では M3（PR・workflow 周辺機能）を実装しない。 |
| `apps/server/src/orchestration-v2/workflowScriptQuery.ts` (127) | — | — | 対象外：今回の指示では M3（PR・workflow 周辺機能）を実装しない。 |

### `apps/server/src/project`

本体 15 ファイル / 5,595 行。テスト 15 ファイル / 7,354 行。testkit/fixture は別行で全件追跡する。

| T3 のファイル（行数） | Rust のモジュール／テスト | 移植した T3 テスト | 状態・理由 |
|---|---|---|---|
| `apps/server/src/project/AgentSessionImporter.test.ts` (147) | `crates/host-daemon/src/project/agent_session_importer.rs` | — | 未翻訳 |
| `apps/server/src/project/AgentSessionImporter.ts` (409) | `crates/host-daemon/src/project/agent_session_importer.rs` | — | 未翻訳 |
| `apps/server/src/project/AgentSessionJson.test.ts` (60) | `crates/host-daemon/src/project/agent_session_json.rs` | — | 未翻訳 |
| `apps/server/src/project/AgentSessionJson.ts` (201) | `crates/host-daemon/src/project/agent_session_json.rs` | — | 未翻訳 |
| `apps/server/src/project/AgentSessionScanner.test.ts` (3196) | `crates/host-daemon/src/project/agent_session_scanner.rs` | — | 未翻訳 |
| `apps/server/src/project/AgentSessionScanner.ts` (1495) | `crates/host-daemon/src/project/agent_session_scanner.rs` | — | 未翻訳 |
| `apps/server/src/project/ManagedProjectFolders.test.ts` (572) | — | — | 対象外：履歴取り込み以外の project 機能は今回の翻訳範囲外。残す Host のプロジェクト機能との接続点は importer に記録。 |
| `apps/server/src/project/ManagedProjectFolders.ts` (510) | — | — | 対象外：履歴取り込み以外の project 機能は今回の翻訳範囲外。残す Host のプロジェクト機能との接続点は importer に記録。 |
| `apps/server/src/project/ProjectCloneTracker.test.ts` (303) | — | — | 対象外：履歴取り込み以外の project 機能は今回の翻訳範囲外。残す Host のプロジェクト機能との接続点は importer に記録。 |
| `apps/server/src/project/ProjectCloneTracker.ts` (479) | — | — | 対象外：履歴取り込み以外の project 機能は今回の翻訳範囲外。残す Host のプロジェクト機能との接続点は importer に記録。 |
| `apps/server/src/project/ProjectEnrichmentService.test.ts` (343) | — | — | 対象外：履歴取り込み以外の project 機能は今回の翻訳範囲外。残す Host のプロジェクト機能との接続点は importer に記録。 |
| `apps/server/src/project/ProjectEnrichmentService.ts` (297) | — | — | 対象外：履歴取り込み以外の project 機能は今回の翻訳範囲外。残す Host のプロジェクト機能との接続点は importer に記録。 |
| `apps/server/src/project/ProjectFaviconResolver.test.ts` (468) | — | — | 対象外：履歴取り込み以外の project 機能は今回の翻訳範囲外。残す Host のプロジェクト機能との接続点は importer に記録。 |
| `apps/server/src/project/ProjectFaviconResolver.ts` (347) | — | — | 対象外：履歴取り込み以外の project 機能は今回の翻訳範囲外。残す Host のプロジェクト機能との接続点は importer に記録。 |
| `apps/server/src/project/ProjectMutation.test.ts` (93) | — | — | 対象外：履歴取り込み以外の project 機能は今回の翻訳範囲外。残す Host のプロジェクト機能との接続点は importer に記録。 |
| `apps/server/src/project/ProjectMutation.ts` (53) | — | — | 対象外：履歴取り込み以外の project 機能は今回の翻訳範囲外。残す Host のプロジェクト機能との接続点は importer に記録。 |
| `apps/server/src/project/ProjectService.deletion.test.ts` (486) | — | — | 対象外：履歴取り込み以外の project 機能は今回の翻訳範囲外。残す Host のプロジェクト機能との接続点は importer に記録。 |
| `apps/server/src/project/ProjectService.test.ts` (805) | — | — | 対象外：履歴取り込み以外の project 機能は今回の翻訳範囲外。残す Host のプロジェクト機能との接続点は importer に記録。 |
| `apps/server/src/project/ProjectService.ts` (572) | — | — | 対象外：履歴取り込み以外の project 機能は今回の翻訳範囲外。残す Host のプロジェクト機能との接続点は importer に記録。 |
| `apps/server/src/project/ProjectSetupScriptRunner.test.ts` (119) | — | — | 対象外：履歴取り込み以外の project 機能は今回の翻訳範囲外。残す Host のプロジェクト機能との接続点は importer に記録。 |
| `apps/server/src/project/ProjectSetupScriptRunner.ts` (456) | — | — | 対象外：履歴取り込み以外の project 機能は今回の翻訳範囲外。残す Host のプロジェクト機能との接続点は importer に記録。 |
| `apps/server/src/project/RepositoryIdentityResolver.test.ts` (393) | — | — | 対象外：履歴取り込み以外の project 機能は今回の翻訳範囲外。残す Host のプロジェクト機能との接続点は importer に記録。 |
| `apps/server/src/project/RepositoryIdentityResolver.ts` (197) | — | — | 対象外：履歴取り込み以外の project 機能は今回の翻訳範囲外。残す Host のプロジェクト機能との接続点は importer に記録。 |
| `apps/server/src/project/T3ProjectFileLoader.test.ts` (89) | — | — | 対象外：履歴取り込み以外の project 機能は今回の翻訳範囲外。残す Host のプロジェクト機能との接続点は importer に記録。 |
| `apps/server/src/project/T3ProjectFileLoader.ts` (109) | — | — | 対象外：履歴取り込み以外の project 機能は今回の翻訳範囲外。残す Host のプロジェクト機能との接続点は importer に記録。 |
| `apps/server/src/project/WorktreeSetupTracker.test.ts` (226) | — | — | 対象外：履歴取り込み以外の project 機能は今回の翻訳範囲外。残す Host のプロジェクト機能との接続点は importer に記録。 |
| `apps/server/src/project/WorktreeSetupTracker.ts` (366) | — | — | 対象外：履歴取り込み以外の project 機能は今回の翻訳範囲外。残す Host のプロジェクト機能との接続点は importer に記録。 |
| `apps/server/src/project/gitCloneProgress.ts` (44) | — | — | 対象外：履歴取り込み以外の project 機能は今回の翻訳範囲外。残す Host のプロジェクト機能との接続点は importer に記録。 |
| `apps/server/src/project/http.test.ts` (54) | — | — | 対象外：履歴取り込み以外の project 機能は今回の翻訳範囲外。残す Host のプロジェクト機能との接続点は importer に記録。 |
| `apps/server/src/project/http.ts` (60) | — | — | 対象外：履歴取り込み以外の project 機能は今回の翻訳範囲外。残す Host のプロジェクト機能との接続点は importer に記録。 |

### `packages/client-runtime`

本体 183 ファイル / 35,146 行。テスト 120 ファイル / 40,301 行。testkit/fixture は別行で全件追跡する。

| T3 のファイル（行数） | Rust のモジュール／テスト | 移植した T3 テスト | 状態・理由 |
|---|---|---|---|
| `packages/client-runtime/package.json` (400) | `crates/agent-core/src/package_json.rs` | — | 未翻訳 |
| `packages/client-runtime/src/authorization/index.ts` (6) | — | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/authorization/layer.test.ts` (937) | — | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/authorization/remote.test.ts` (488) | — | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/authorization/remote.ts` (250) | — | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/authorization/service.ts` (508) | — | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/authorization/tokenStore.ts` (38) | — | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/codexArtifactTemplates.test.ts` (119) | — | — | 対象外：今回の指示では M3 の Git/PR/usage/全設定・更新配布 UI は実装しない。会話に必要な契約・回復経路は除外しない。 |
| `packages/client-runtime/src/codexArtifactTemplates.ts` (136) | — | — | 対象外：今回の指示では M3 の Git/PR/usage/全設定・更新配布 UI は実装しない。会話に必要な契約・回復経路は除外しない。 |
| `packages/client-runtime/src/codexFileCitations.test.ts` (77) | `crates/agent-core/src/codex_file_citations_test.rs` | — | 未翻訳 |
| `packages/client-runtime/src/codexFileCitations.ts` (56) | `crates/agent-core/src/codex_file_citations.rs` | — | 未翻訳 |
| `packages/client-runtime/src/codexMarkdownDirectives.test.ts` (175) | `crates/agent-core/src/codex_markdown_directives_test.rs` | — | 未翻訳 |
| `packages/client-runtime/src/codexMarkdownDirectives.ts` (388) | `crates/agent-core/src/codex_markdown_directives.rs` | — | 未翻訳 |
| `packages/client-runtime/src/composerThreadItems.test.ts` (50) | `crates/agent-core/src/composer_thread_items_test.rs` | — | 未翻訳 |
| `packages/client-runtime/src/composerThreadItems.ts` (51) | `crates/agent-core/src/composer_thread_items.rs` | — | 未翻訳 |
| `packages/client-runtime/src/connection/catalog.ts` (132) | `crates/agent-core/src/connection/catalog.rs` | — | 未翻訳 |
| `packages/client-runtime/src/connection/compatibility.test.ts` (78) | `crates/agent-core/src/connection/compatibility_test.rs` | — | 未翻訳 |
| `packages/client-runtime/src/connection/compatibility.ts` (42) | `crates/agent-core/src/connection/compatibility.rs` | — | 未翻訳 |
| `packages/client-runtime/src/connection/connectivity.ts` (19) | `crates/agent-core/src/connection/connectivity.rs` | — | 未翻訳 |
| `packages/client-runtime/src/connection/credentialStore.ts` (23) | `crates/agent-core/src/connection/credential_store.rs` | — | 未翻訳 |
| `packages/client-runtime/src/connection/driver.ts` (65) | `crates/agent-core/src/connection/driver.rs` | — | 未翻訳 |
| `packages/client-runtime/src/connection/errors.test.ts` (131) | `crates/agent-core/src/connection/errors_test.rs` | — | 未翻訳 |
| `packages/client-runtime/src/connection/errors.ts` (192) | `crates/agent-core/src/connection/errors.rs` | — | 未翻訳 |
| `packages/client-runtime/src/connection/githubRoutingPermissions.ts` (147) | `crates/agent-core/src/connection/github_routing_permissions.rs` | — | 未翻訳 |
| `packages/client-runtime/src/connection/index.ts` (20) | `crates/agent-core/src/connection/index.rs` | — | 未翻訳 |
| `packages/client-runtime/src/connection/layer.ts` (99) | `crates/agent-core/src/connection/layer.rs` | — | 未翻訳 |
| `packages/client-runtime/src/connection/model.ts` (178) | `crates/agent-core/src/connection/model.rs` | — | 未翻訳 |
| `packages/client-runtime/src/connection/onboarding.test.ts` (335) | `crates/agent-core/src/connection/onboarding_test.rs` | — | 未翻訳 |
| `packages/client-runtime/src/connection/onboarding.ts` (279) | `crates/agent-core/src/connection/onboarding.rs` | — | 未翻訳 |
| `packages/client-runtime/src/connection/outdatedHostUpdate.test.ts` (278) | — | — | 対象外：今回の指示では M3 の Git/PR/usage/全設定・更新配布 UI は実装しない。会話に必要な契約・回復経路は除外しない。 |
| `packages/client-runtime/src/connection/outdatedHostUpdate.ts` (190) | — | — | 対象外：今回の指示では M3 の Git/PR/usage/全設定・更新配布 UI は実装しない。会話に必要な契約・回復経路は除外しない。 |
| `packages/client-runtime/src/connection/presentation.test.ts` (199) | `crates/agent-core/src/connection/presentation_test.rs` | — | 未翻訳 |
| `packages/client-runtime/src/connection/presentation.ts` (110) | `crates/agent-core/src/connection/presentation.rs` | — | 未翻訳 |
| `packages/client-runtime/src/connection/profileStore.ts` (20) | `crates/agent-core/src/connection/profile_store.rs` | — | 未翻訳 |
| `packages/client-runtime/src/connection/registry.test.ts` (1584) | `crates/agent-core/src/connection/registry_test.rs` | — | 未翻訳 |
| `packages/client-runtime/src/connection/registry.ts` (919) | `crates/agent-core/src/connection/registry.rs` | — | 未翻訳 |
| `packages/client-runtime/src/connection/resolver.test.ts` (563) | `crates/agent-core/src/connection/resolver_test.rs` | — | 未翻訳 |
| `packages/client-runtime/src/connection/resolver.ts` (314) | `crates/agent-core/src/connection/resolver.rs` | — | 未翻訳 |
| `packages/client-runtime/src/connection/supervisor.test.ts` (1635) | `crates/agent-core/src/connection/supervisor_test.rs` | — | 未翻訳 |
| `packages/client-runtime/src/connection/supervisor.ts` (847) | `crates/agent-core/src/connection/supervisor.rs` | — | 未翻訳 |
| `packages/client-runtime/src/connection/wakeups.ts` (34) | `crates/agent-core/src/connection/wakeups.rs` | — | 未翻訳 |
| `packages/client-runtime/src/delayedStatus.test.ts` (65) | `crates/agent-core/src/delayed_status_test.rs` | — | 未翻訳 |
| `packages/client-runtime/src/delayedStatus.ts` (86) | `crates/agent-core/src/delayed_status.rs` | — | 未翻訳 |
| `packages/client-runtime/src/device/androidFoldScene.test.ts` (78) | — | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/device/androidFoldScene.ts` (420) | — | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/device/deviceFraming.test.ts` (53) | — | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/device/deviceFraming.ts` (82) | — | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/device/deviceMotion.test.ts` (175) | — | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/device/deviceMotion.ts` (279) | — | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/device/deviceViewSnap.ts` (26) | — | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/device/duoControl.test.ts` (109) | — | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/device/duoControl.ts` (113) | — | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/device/duoScene.test.ts` (108) | — | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/device/duoScene.ts` (244) | — | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/device/duoSnap.test.ts` (54) | — | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/device/duoSnap.ts` (86) | — | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/device/duoStream.test.ts` (285) | — | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/device/duoViewer.test.ts` (703) | — | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/device/duoViewer.ts` (566) | — | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/device/frame.test.ts` (32) | — | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/device/frame.ts` (24) | — | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/device/hubAccess.ts` (18) | — | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/device/model.test.ts` (126) | — | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/device/model.ts` (88) | — | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/device/modelScene.test.ts` (121) | — | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/device/modelScene.ts` (118) | — | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/device/phoneInteraction.test.ts` (105) | — | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/device/phoneInteraction.ts` (83) | — | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/device/phoneScene.test.ts` (150) | — | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/device/phoneScene.ts` (302) | — | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/device/phoneViewer.test.ts` (530) | — | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/device/phoneViewer.ts` (444) | — | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/device/renderScheduler.test.ts` (27) | — | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/device/renderScheduler.ts` (23) | — | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/device/screenshot.test.ts` (50) | — | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/device/screenshot.ts` (30) | — | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/device/shapeProfile.test.ts` (27) | — | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/device/shapeProfile.ts` (124) | — | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/device/stream.test.ts` (738) | — | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/device/stream.ts` (1091) | — | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/device/streamFrames.test.ts` (97) | — | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/environment/descriptor.ts` (17) | `crates/agent-core/src/environment/descriptor.rs` | — | 未翻訳 |
| `packages/client-runtime/src/environment/endpoint.test.ts` (61) | `crates/agent-core/src/environment/endpoint_test.rs` | — | 未翻訳 |
| `packages/client-runtime/src/environment/endpoint.ts` (9) | `crates/agent-core/src/environment/endpoint.rs` | — | 未翻訳 |
| `packages/client-runtime/src/environment/index.ts` (4) | `crates/agent-core/src/environment/index.rs` | — | 未翻訳 |
| `packages/client-runtime/src/environment/knownEnvironment.test.ts` (63) | `crates/agent-core/src/environment/known_environment_test.rs` | — | 未翻訳 |
| `packages/client-runtime/src/environment/knownEnvironment.ts` (41) | `crates/agent-core/src/environment/known_environment.rs` | — | 未翻訳 |
| `packages/client-runtime/src/environment/scoped.ts` (69) | `crates/agent-core/src/environment/scoped.rs` | — | 未翻訳 |
| `packages/client-runtime/src/errors/errorTrace.test.ts` (44) | `crates/agent-core/src/errors/error_trace_test.rs` | — | 未翻訳 |
| `packages/client-runtime/src/errors/errorTrace.ts` (50) | `crates/agent-core/src/errors/error_trace.rs` | — | 未翻訳 |
| `packages/client-runtime/src/errors/index.ts` (4) | `crates/agent-core/src/errors/index.rs` | — | 未翻訳 |
| `packages/client-runtime/src/errors/network.ts` (4) | `crates/agent-core/src/errors/network.rs` | — | 未翻訳 |
| `packages/client-runtime/src/errors/orchestration.test.ts` (50) | `crates/agent-core/src/errors/orchestration_test.rs` | — | 未翻訳 |
| `packages/client-runtime/src/errors/orchestration.ts` (16) | `crates/agent-core/src/errors/orchestration.rs` | — | 未翻訳 |
| `packages/client-runtime/src/errors/safeLog.test.ts` (55) | `crates/agent-core/src/errors/safe_log_test.rs` | — | 未翻訳 |
| `packages/client-runtime/src/errors/safeLog.ts` (107) | `crates/agent-core/src/errors/safe_log.rs` | — | 未翻訳 |
| `packages/client-runtime/src/errors/transport.test.ts` (87) | `crates/agent-core/src/errors/transport_test.rs` | — | 未翻訳 |
| `packages/client-runtime/src/errors/transport.ts` (46) | `crates/agent-core/src/errors/transport.rs` | — | 未翻訳 |
| `packages/client-runtime/src/filePreview.test.ts` (67) | `crates/agent-core/src/file_preview_test.rs` | — | 未翻訳 |
| `packages/client-runtime/src/filePreview.ts` (44) | `crates/agent-core/src/file_preview.rs` | — | 未翻訳 |
| `packages/client-runtime/src/handoff.test.ts` (65) | `crates/agent-core/src/handoff_test.rs` | — | 未翻訳 |
| `packages/client-runtime/src/handoff.ts` (64) | `crates/agent-core/src/handoff.rs` | — | 未翻訳 |
| `packages/client-runtime/src/load-balancing.ts` (40) | `crates/agent-core/src/load_balancing.rs` | — | 未翻訳 |
| `packages/client-runtime/src/markdownImages.test.ts` (71) | `crates/agent-core/src/markdown_images_test.rs` | — | 未翻訳 |
| `packages/client-runtime/src/markdownImages.ts` (72) | `crates/agent-core/src/markdown_images.rs` | — | 未翻訳 |
| `packages/client-runtime/src/markdownLinks.test.ts` (320) | `crates/agent-core/src/markdown_links_test.rs` | — | 未翻訳 |
| `packages/client-runtime/src/markdownLinks.ts` (369) | `crates/agent-core/src/markdown_links.rs` | — | 未翻訳 |
| `packages/client-runtime/src/mediaActions.ts` (8) | `crates/agent-core/src/media_actions.rs` | — | 未翻訳 |
| `packages/client-runtime/src/mediaReference.test.ts` (39) | `crates/agent-core/src/media_reference_test.rs` | — | 未翻訳 |
| `packages/client-runtime/src/mediaReference.ts` (92) | `crates/agent-core/src/media_reference.rs` | — | 未翻訳 |
| `packages/client-runtime/src/mediaSource.test.ts` (141) | `crates/agent-core/src/media_source_test.rs` | — | 未翻訳 |
| `packages/client-runtime/src/mediaSource.ts` (104) | `crates/agent-core/src/media_source.rs` | — | 未翻訳 |
| `packages/client-runtime/src/operations/commands.performance.test.ts` (285) | `crates/agent-core/src/operations/commands_performance_test.rs` | — | 未翻訳 |
| `packages/client-runtime/src/operations/commands.test.ts` (972) | `crates/agent-core/src/operations/commands_test.rs` | — | 未翻訳 |
| `packages/client-runtime/src/operations/commands.ts` (1067) | `crates/agent-core/src/operations/commands.rs` | — | 未翻訳 |
| `packages/client-runtime/src/operations/index.ts` (3) | `crates/agent-core/src/operations/index.rs` | — | 未翻訳 |
| `packages/client-runtime/src/operations/projects.test.ts` (277) | `crates/agent-core/src/operations/projects_test.rs` | — | 未翻訳 |
| `packages/client-runtime/src/operations/projects.ts` (367) | `crates/agent-core/src/operations/projects.rs` | — | 未翻訳 |
| `packages/client-runtime/src/operations/threadTitle.test.ts` (47) | `crates/agent-core/src/operations/thread_title_test.rs` | — | 未翻訳 |
| `packages/client-runtime/src/operations/threadTitle.ts` (33) | `crates/agent-core/src/operations/thread_title.rs` | — | 未翻訳 |
| `packages/client-runtime/src/platform/capabilities.ts` (74) | `crates/agent-core/src/platform/capabilities.rs` | — | 未翻訳 |
| `packages/client-runtime/src/platform/index.ts` (7) | `crates/agent-core/src/platform/index.rs` | — | 未翻訳 |
| `packages/client-runtime/src/platform/orchestrationCache.test.ts` (186) | `crates/agent-core/src/platform/orchestration_cache_test.rs` | — | 未翻訳 |
| `packages/client-runtime/src/platform/orchestrationCache.ts` (36) | `crates/agent-core/src/platform/orchestration_cache.rs` | — | 未翻訳 |
| `packages/client-runtime/src/platform/persistence.ts` (139) | `crates/agent-core/src/platform/persistence.rs` | — | 未翻訳 |
| `packages/client-runtime/src/platform/source.ts` (15) | `crates/agent-core/src/platform/source.rs` | — | 未翻訳 |
| `packages/client-runtime/src/platform/storageDocument.test.ts` (414) | `crates/agent-core/src/platform/storage_document_test.rs` | — | 未翻訳 |
| `packages/client-runtime/src/platform/storageDocument.ts` (206) | `crates/agent-core/src/platform/storage_document.rs` | — | 未翻訳 |
| `packages/client-runtime/src/projectFaviconCache.test.ts` (323) | — | — | 対象外：今回の指示では M3 の Git/PR/usage/全設定・更新配布 UI は実装しない。会話に必要な契約・回復経路は除外しない。 |
| `packages/client-runtime/src/projectFaviconCache.ts` (263) | — | — | 対象外：今回の指示では M3 の Git/PR/usage/全設定・更新配布 UI は実装しない。会話に必要な契約・回復経路は除外しない。 |
| `packages/client-runtime/src/providerSkills.test.ts` (277) | `crates/agent-core/src/provider_skills_test.rs` | — | 未翻訳 |
| `packages/client-runtime/src/providerSkills.ts` (134) | `crates/agent-core/src/provider_skills.rs` | — | 未翻訳 |
| `packages/client-runtime/src/relay/discovery.test.ts` (432) | — | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/relay/discovery.ts` (350) | — | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/relay/errorPresentation.test.ts` (57) | — | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/relay/errorPresentation.ts` (66) | — | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/relay/index.ts` (4) | — | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/relay/managedRelay.test.ts` (708) | — | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/relay/managedRelay.ts` (940) | — | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/relay/managedRelayState.test.ts` (449) | — | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/relay/managedRelayState.ts` (445) | — | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/remotePerformance.bench.ts` (135) | — | — | 対象外：Web/relay の benchmark。Rust の意味論テストには該当しない。 |
| `packages/client-runtime/src/repairMarkdownFileLinks.ts` (84) | `crates/agent-core/src/repair_markdown_file_links.rs` | — | 未翻訳 |
| `packages/client-runtime/src/rpc/client.test.ts` (838) | `crates/agent-core/src/rpc/client_test.rs` | — | 未翻訳 |
| `packages/client-runtime/src/rpc/client.ts` (404) | `crates/agent-core/src/rpc/client.rs` | — | 未翻訳 |
| `packages/client-runtime/src/rpc/http.ts` (176) | `crates/agent-core/src/rpc/http.rs` | — | 未翻訳 |
| `packages/client-runtime/src/rpc/index.ts` (4) | `crates/agent-core/src/rpc/index.rs` | — | 未翻訳 |
| `packages/client-runtime/src/rpc/protocol.ts` (8) | `crates/agent-core/src/rpc/protocol.rs` | — | 未翻訳 |
| `packages/client-runtime/src/rpc/session.test.ts` (1268) | `crates/agent-core/src/rpc/session_test.rs` | — | 未翻訳 |
| `packages/client-runtime/src/rpc/session.ts` (398) | `crates/agent-core/src/rpc/session.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/archivedThreads.test.ts` (40) | `crates/agent-core/src/state/archived_threads_test.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/archivedThreads.ts` (69) | `crates/agent-core/src/state/archived_threads.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/assets.test.ts` (513) | `crates/agent-core/src/state/assets_test.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/assets.ts` (239) | `crates/agent-core/src/state/assets.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/attachments.test.ts` (202) | `crates/agent-core/src/state/attachments_test.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/attachments.ts` (236) | `crates/agent-core/src/state/attachments.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/auth.test.ts` (77) | `crates/agent-core/src/state/auth_test.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/auth.ts` (90) | `crates/agent-core/src/state/auth.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/boundedThreadSnapshotHttp.test.ts` (214) | `crates/agent-core/src/state/bounded_thread_snapshot_http_test.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/boundedThreadSnapshotHttp.ts` (162) | `crates/agent-core/src/state/bounded_thread_snapshot_http.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/cachePersistence.ts` (19) | `crates/agent-core/src/state/cache_persistence.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/checkpointDiff.ts` (25) | `crates/agent-core/src/state/checkpoint_diff.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/composerDispatch.test.ts` (72) | `crates/agent-core/src/state/composer_dispatch_test.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/composerDispatch.ts` (32) | `crates/agent-core/src/state/composer_dispatch.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/composerPathSearch.ts` (19) | `crates/agent-core/src/state/composer_path_search.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/connections.ts` (188) | `crates/agent-core/src/state/connections.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/customSnooze.test.ts` (66) | `crates/agent-core/src/state/custom_snooze_test.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/device.ts` (99) | `crates/agent-core/src/state/device.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/deviceHubAccess.ts` (60) | `crates/agent-core/src/state/device_hub_access.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/entities.test.ts` (734) | `crates/agent-core/src/state/entities_test.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/entities.ts` (125) | `crates/agent-core/src/state/entities.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/environmentHttpAuth.test.ts` (670) | `crates/agent-core/src/state/environment_http_auth_test.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/environmentHttpAuth.ts` (208) | `crates/agent-core/src/state/environment_http_auth.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/filesystem.test.ts` (70) | `crates/agent-core/src/state/filesystem_test.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/filesystem.ts` (83) | `crates/agent-core/src/state/filesystem.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/git.ts` (23) | — | — | 対象外：今回の指示では M3 の Git/PR/usage/全設定・更新配布 UI は実装しない。会話に必要な契約・回復経路は除外しない。 |
| `packages/client-runtime/src/state/gitActions.ts` (358) | — | — | 対象外：今回の指示では M3 の Git/PR/usage/全設定・更新配布 UI は実装しない。会話に必要な契約・回復経路は除外しない。 |
| `packages/client-runtime/src/state/itemSupport.test.ts` (205) | `crates/agent-core/src/state/item_support_test.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/itemSupport.ts` (138) | `crates/agent-core/src/state/item_support.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/models.ts` (329) | `crates/agent-core/src/state/models.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/orchestration.ts` (69) | `crates/agent-core/src/state/orchestration.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/orchestrationV2Projection.test.ts` (351) | `crates/agent-core/src/state/orchestration_v2_projection_test.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/orchestrationV2Projection.ts` (286) | `crates/agent-core/src/state/orchestration_v2_projection.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/orchestrationV2TestFixtures.ts` (110) | `crates/agent-core/src/state/orchestration_v2_test_fixtures.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/outdatedServerUpdate.ts` (78) | — | — | 対象外：今回の指示では M3 の Git/PR/usage/全設定・更新配布 UI は実装しない。会話に必要な契約・回復経路は除外しない。 |
| `packages/client-runtime/src/state/presentation.test.ts` (200) | `crates/agent-core/src/state/presentation_test.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/presentation.ts` (221) | `crates/agent-core/src/state/presentation.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/preview.test.ts` (18) | `crates/agent-core/src/state/preview_test.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/preview.ts` (116) | `crates/agent-core/src/state/preview.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/projectCommands.test.ts` (77) | `crates/agent-core/src/state/project_commands_test.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/projectCommands.ts` (168) | `crates/agent-core/src/state/project_commands.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/projectEntities.ts` (105) | `crates/agent-core/src/state/project_entities.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/projectGrouping.test.ts` (281) | `crates/agent-core/src/state/project_grouping_test.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/projectGrouping.ts` (322) | `crates/agent-core/src/state/project_grouping.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/projects.ts` (208) | `crates/agent-core/src/state/projects.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/providerInstanceDisplay.test.ts` (136) | `crates/agent-core/src/state/provider_instance_display_test.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/providerInstanceDisplay.ts` (103) | `crates/agent-core/src/state/provider_instance_display.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/pullRequestDiffHttp.test.ts` (120) | — | — | 対象外：今回の指示では M3 の Git/PR/usage/全設定・更新配布 UI は実装しない。会話に必要な契約・回復経路は除外しない。 |
| `packages/client-runtime/src/state/pullRequestDiffHttp.ts` (102) | — | — | 対象外：今回の指示では M3 の Git/PR/usage/全設定・更新配布 UI は実装しない。会話に必要な契約・回復経路は除外しない。 |
| `packages/client-runtime/src/state/pullRequestRouting.ts` (423) | — | — | 対象外：今回の指示では M3 の Git/PR/usage/全設定・更新配布 UI は実装しない。会話に必要な契約・回復経路は除外しない。 |
| `packages/client-runtime/src/state/pullRequests.test.ts` (1568) | — | — | 対象外：今回の指示では M3 の Git/PR/usage/全設定・更新配布 UI は実装しない。会話に必要な契約・回復経路は除外しない。 |
| `packages/client-runtime/src/state/pullRequests.ts` (525) | — | — | 対象外：今回の指示では M3 の Git/PR/usage/全設定・更新配布 UI は実装しない。会話に必要な契約・回復経路は除外しない。 |
| `packages/client-runtime/src/state/relayDiscovery.ts` (41) | `crates/agent-core/src/state/relay_discovery.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/review.ts` (75) | `crates/agent-core/src/state/review.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/runtime.test.ts` (954) | `crates/agent-core/src/state/runtime_test.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/runtime.ts` (756) | `crates/agent-core/src/state/runtime.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/server.test.ts` (928) | `crates/agent-core/src/state/server_test.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/server.ts` (1305) | `crates/agent-core/src/state/server.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/serverConfigProjection.ts` (99) | `crates/agent-core/src/state/server_config_projection.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/serverUsage.test.ts` (263) | `crates/agent-core/src/state/server_usage_test.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/session.ts` (165) | `crates/agent-core/src/state/session.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/sharedSettings.test.ts` (404) | — | — | 対象外：今回の指示では M3 の Git/PR/usage/全設定・更新配布 UI は実装しない。会話に必要な契約・回復経路は除外しない。 |
| `packages/client-runtime/src/state/sharedSettings.ts` (178) | — | — | 対象外：今回の指示では M3 の Git/PR/usage/全設定・更新配布 UI は実装しない。会話に必要な契約・回復経路は除外しない。 |
| `packages/client-runtime/src/state/shell-sync.test.ts` (956) | `crates/agent-core/src/state/shell_sync_test.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/shell.test.ts` (234) | `crates/agent-core/src/state/shell_test.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/shell.ts` (453) | `crates/agent-core/src/state/shell.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/shellCommands.ts` (16) | `crates/agent-core/src/state/shell_commands.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/shellReducer.test.ts` (450) | `crates/agent-core/src/state/shell_reducer_test.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/shellReducer.ts` (151) | `crates/agent-core/src/state/shell_reducer.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/shellSnapshotHttp.ts` (94) | `crates/agent-core/src/state/shell_snapshot_http.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/snapshots.ts` (20) | `crates/agent-core/src/state/snapshots.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/sourceControl.test.ts` (170) | `crates/agent-core/src/state/source_control_test.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/sourceControl.ts` (86) | `crates/agent-core/src/state/source_control.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/subagentDisplay.test.ts` (166) | `crates/agent-core/src/state/subagent_display_test.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/subagentDisplay.ts` (132) | `crates/agent-core/src/state/subagent_display.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/subagentRuntime.ts` (147) | `crates/agent-core/src/state/subagent_runtime.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/terminal.ts` (97) | `crates/agent-core/src/state/terminal.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/terminalOutput.ts` (320) | `crates/agent-core/src/state/terminal_output.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/terminalSession.test.ts` (494) | `crates/agent-core/src/state/terminal_session_test.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/terminalSession.ts` (213) | `crates/agent-core/src/state/terminal_session.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/threadCheckpoints.ts` (52) | `crates/agent-core/src/state/thread_checkpoints.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/threadCommands.test.ts` (377) | `crates/agent-core/src/state/thread_commands_test.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/threadCommands.ts` (487) | `crates/agent-core/src/state/thread_commands.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/threadDetail.test.ts` (328) | `crates/agent-core/src/state/thread_detail_test.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/threadDetail.ts` (199) | `crates/agent-core/src/state/thread_detail.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/threadExecution.test.ts` (679) | `crates/agent-core/src/state/thread_execution_test.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/threadExecution.ts` (388) | `crates/agent-core/src/state/thread_execution.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/threadFeedback.test.ts` (176) | `crates/agent-core/src/state/thread_feedback_test.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/threadFeedback.ts` (119) | `crates/agent-core/src/state/thread_feedback.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/threadHistoryController.test.ts` (63) | `crates/agent-core/src/state/thread_history_controller_test.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/threadHistoryController.ts` (93) | `crates/agent-core/src/state/thread_history_controller.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/threadHistoryHttp.ts` (40) | `crates/agent-core/src/state/thread_history_http.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/threadHistoryMerge.test.ts` (416) | `crates/agent-core/src/state/thread_history_merge_test.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/threadHistoryMerge.ts` (156) | `crates/agent-core/src/state/thread_history_merge.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/threadInbox.test.ts` (86) | `crates/agent-core/src/state/thread_inbox_test.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/threadInbox.ts` (145) | `crates/agent-core/src/state/thread_inbox.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/threadLifecycle.ts` (102) | `crates/agent-core/src/state/thread_lifecycle.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/threadRelationships.test.ts` (505) | `crates/agent-core/src/state/thread_relationships_test.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/threadRelationships.ts` (251) | `crates/agent-core/src/state/thread_relationships.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/threadRequests.test.ts` (153) | `crates/agent-core/src/state/thread_requests_test.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/threadRequests.ts` (152) | `crates/agent-core/src/state/thread_requests.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/threadRetention.ts` (3) | `crates/agent-core/src/state/thread_retention.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/threadSearch.test.ts` (130) | `crates/agent-core/src/state/thread_search_test.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/threadSearch.ts` (85) | `crates/agent-core/src/state/thread_search.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/threadSettled.ts` (354) | `crates/agent-core/src/state/thread_settled.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/threadShell.test.ts` (201) | `crates/agent-core/src/state/thread_shell_test.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/threadShell.ts` (229) | `crates/agent-core/src/state/thread_shell.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/threadSnapshotHttp.ts` (97) | `crates/agent-core/src/state/thread_snapshot_http.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/threadSnoozed.test.ts` (369) | `crates/agent-core/src/state/thread_snoozed_test.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/threadSort.test.ts` (513) | `crates/agent-core/src/state/thread_sort_test.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/threadSort.ts` (400) | `crates/agent-core/src/state/thread_sort.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/threadState.ts` (25) | `crates/agent-core/src/state/thread_state.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/threadSubagents.test.ts` (151) | `crates/agent-core/src/state/thread_subagents_test.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/threadSubagents.ts` (90) | `crates/agent-core/src/state/thread_subagents.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/threadWorkflows.test.ts` (424) | `crates/agent-core/src/state/thread_workflows_test.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/threadWorkflows.ts` (190) | `crates/agent-core/src/state/thread_workflows.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/threads-atoms.test.ts` (598) | `crates/agent-core/src/state/threads_atoms_test.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/threads-sync.test.ts` (1915) | `crates/agent-core/src/state/threads_sync_test.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/threads.ts` (1009) | `crates/agent-core/src/state/threads.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/turnItemPresentation.test.ts` (104) | `crates/agent-core/src/state/turn_item_presentation_test.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/turnItemPresentation.ts` (49) | `crates/agent-core/src/state/turn_item_presentation.rs` | — | 未翻訳 |
| `packages/client-runtime/src/state/usage.test.ts` (323) | — | — | 対象外：今回の指示では M3 の Git/PR/usage/全設定・更新配布 UI は実装しない。会話に必要な契約・回復経路は除外しない。 |
| `packages/client-runtime/src/state/usage.ts` (135) | — | — | 対象外：今回の指示では M3 の Git/PR/usage/全設定・更新配布 UI は実装しない。会話に必要な契約・回復経路は除外しない。 |
| `packages/client-runtime/src/state/vcs.test.ts` (645) | — | — | 対象外：今回の指示では M3 の Git/PR/usage/全設定・更新配布 UI は実装しない。会話に必要な契約・回復経路は除外しない。 |
| `packages/client-runtime/src/state/vcs.ts` (363) | — | — | 対象外：今回の指示では M3 の Git/PR/usage/全設定・更新配布 UI は実装しない。会話に必要な契約・回復経路は除外しない。 |
| `packages/client-runtime/src/state/vcsAction.test.ts` (720) | — | — | 対象外：今回の指示では M3 の Git/PR/usage/全設定・更新配布 UI は実装しない。会話に必要な契約・回復経路は除外しない。 |
| `packages/client-runtime/src/state/vcsAction.ts` (595) | — | — | 対象外：今回の指示では M3 の Git/PR/usage/全設定・更新配布 UI は実装しない。会話に必要な契約・回復経路は除外しない。 |
| `packages/client-runtime/src/state/vcsCommandScheduler.ts` (13) | — | — | 対象外：今回の指示では M3 の Git/PR/usage/全設定・更新配布 UI は実装しない。会話に必要な契約・回復経路は除外しない。 |
| `packages/client-runtime/src/state/vcsRef.ts` (9) | — | — | 対象外：今回の指示では M3 の Git/PR/usage/全設定・更新配布 UI は実装しない。会話に必要な契約・回復経路は除外しない。 |
| `packages/client-runtime/src/state/vcsRefInvalidation.ts` (83) | — | — | 対象外：今回の指示では M3 の Git/PR/usage/全設定・更新配布 UI は実装しない。会話に必要な契約・回復経路は除外しない。 |
| `packages/client-runtime/src/t3ToolSummary.test.ts` (251) | `crates/agent-core/src/t3_tool_summary_test.rs` | — | 未翻訳 |
| `packages/client-runtime/src/t3ToolSummary.ts` (403) | `crates/agent-core/src/t3_tool_summary.rs` | — | 未翻訳 |
| `packages/client-runtime/src/textPaste.test.ts` (156) | `crates/agent-core/src/text_paste_test.rs` | — | 未翻訳 |
| `packages/client-runtime/src/textPaste.ts` (76) | `crates/agent-core/src/text_paste.rs` | — | 未翻訳 |
| `packages/client-runtime/src/threadPullRequestCompatibility.test.ts` (133) | — | — | 対象外：今回の指示では M3 の Git/PR/usage/全設定・更新配布 UI は実装しない。会話に必要な契約・回復経路は除外しない。 |
| `packages/client-runtime/src/threadPullRequestCompatibility.ts` (75) | — | — | 対象外：今回の指示では M3 の Git/PR/usage/全設定・更新配布 UI は実装しない。会話に必要な契約・回復経路は除外しない。 |
| `packages/client-runtime/src/userMessage.test.ts` (70) | `crates/agent-core/src/user_message_test.rs` | — | 未翻訳 |
| `packages/client-runtime/src/userMessage.ts` (31) | `crates/agent-core/src/user_message.rs` | — | 未翻訳 |
| `packages/client-runtime/src/voice-input/controller.test.ts` (640) | `crates/agent-core/src/voice_input/controller_test.rs` | — | 未翻訳 |
| `packages/client-runtime/src/voice-input/controller.ts` (515) | `crates/agent-core/src/voice_input/controller.rs` | — | 未翻訳 |
| `packages/client-runtime/src/voice-input/index.ts` (20) | `crates/agent-core/src/voice_input/index.rs` | — | 未翻訳 |
| `packages/client-runtime/src/voice-input/transcription.ts` (37) | `crates/agent-core/src/voice_input/transcription.rs` | — | 未翻訳 |
| `packages/client-runtime/src/work-log/commandLabel.test.ts` (495) | `crates/agent-core/src/work_log/command_label_test.rs` | — | 未翻訳 |
| `packages/client-runtime/src/work-log/commandLabel.ts` (1386) | `crates/agent-core/src/work_log/command_label.rs` | — | 未翻訳 |
| `packages/client-runtime/src/work-log/itemDetail.ts` (248) | `crates/agent-core/src/work_log/item_detail.rs` | — | 未翻訳 |
| `packages/client-runtime/src/work-log/presentation.test.ts` (962) | `crates/agent-core/src/work_log/presentation_test.rs` | — | 未翻訳 |
| `packages/client-runtime/src/work-log/presentation.ts` (756) | `crates/agent-core/src/work_log/presentation.rs` | — | 未翻訳 |
| `packages/client-runtime/src/work-log/scrollAnchor.test.ts` (63) | `crates/agent-core/src/work_log/scroll_anchor_test.rs` | — | 未翻訳 |
| `packages/client-runtime/src/work-log/scrollAnchor.ts` (33) | `crates/agent-core/src/work_log/scroll_anchor.rs` | — | 未翻訳 |
| `packages/client-runtime/src/work-log/toolPresentation.test.ts` (107) | `crates/agent-core/src/work_log/tool_presentation_test.rs` | — | 未翻訳 |
| `packages/client-runtime/src/work-log/toolPresentation.ts` (131) | `crates/agent-core/src/work_log/tool_presentation.rs` | — | 未翻訳 |
| `packages/client-runtime/src/work-log/userInput.test.ts` (37) | `crates/agent-core/src/work_log/user_input_test.rs` | — | 未翻訳 |
| `packages/client-runtime/src/work-log/userInput.ts` (44) | `crates/agent-core/src/work_log/user_input.rs` | — | 未翻訳 |
| `packages/client-runtime/src/worktreeSetup.ts` (64) | — | — | 対象外：今回の指示では M3 の Git/PR/usage/全設定・更新配布 UI は実装しない。会話に必要な契約・回復経路は除外しない。 |
| `packages/client-runtime/tsconfig.json` (4) | `crates/agent-core/src/tsconfig_json.rs` | — | 未翻訳 |
| `packages/client-runtime/vite.config.ts` (10) | `crates/agent-core/src/vite_config.rs` | — | 未翻訳 |

### `packages/contracts`

本体 63 ファイル / 24,888 行。テスト 36 ファイル / 7,142 行。testkit/fixture は別行で全件追跡する。

| T3 のファイル（行数） | Rust のモジュール／テスト | 移植した T3 テスト | 状態・理由 |
|---|---|---|---|
| `packages/contracts/package.json` (34) | `crates/orchestration/src/contracts/package.rs` | — | 未翻訳 |
| `packages/contracts/src/acpRegistry.test.ts` (180) | — | — | 対象外：T3 Connect/ブラウザ・Web 専用/外部サービスの契約。Bex の iroh/ネイティブ境界に該当。 |
| `packages/contracts/src/acpRegistry.ts` (347) | — | — | 対象外：T3 Connect/ブラウザ・Web 専用/外部サービスの契約。Bex の iroh/ネイティブ境界に該当。 |
| `packages/contracts/src/agentSessions.test.ts` (36) | `crates/orchestration/src/contracts/agent_sessions.rs` | — | 未翻訳 |
| `packages/contracts/src/agentSessions.ts` (111) | `crates/orchestration/src/contracts/agent_sessions.rs` | — | 未翻訳 |
| `packages/contracts/src/applicationEvent.test.ts` (63) | `crates/orchestration/src/contracts/application_event.rs` | — | 未翻訳 |
| `packages/contracts/src/applicationEvent.ts` (124) | `crates/orchestration/src/contracts/application_event.rs` | — | 未翻訳 |
| `packages/contracts/src/assets.test.ts` (60) | `crates/orchestration/src/contracts/assets.rs` | — | 未翻訳 |
| `packages/contracts/src/assets.ts` (320) | `crates/orchestration/src/contracts/assets.rs` | — | 未翻訳 |
| `packages/contracts/src/assistantCitations.ts` (31) | `crates/orchestration/src/contracts/assistant_citations.rs` | — | 未翻訳 |
| `packages/contracts/src/auth.ts` (355) | — | — | 対象外：T3 Connect/ブラウザ・Web 専用/外部サービスの契約。Bex の iroh/ネイティブ境界に該当。 |
| `packages/contracts/src/background.test.ts` (18) | `crates/orchestration/src/contracts/background.rs` | — | 未翻訳 |
| `packages/contracts/src/background.ts` (110) | `crates/orchestration/src/contracts/background.rs` | — | 未翻訳 |
| `packages/contracts/src/baseSchemas.ts` (223) | `crates/orchestration/src/contracts/base_schemas.rs` | — | 未翻訳 |
| `packages/contracts/src/browserImport.ts` (165) | — | — | 対象外：T3 Connect/ブラウザ・Web 専用/外部サービスの契約。Bex の iroh/ネイティブ境界に該当。 |
| `packages/contracts/src/browserProfile.test.ts` (111) | — | — | 対象外：T3 Connect/ブラウザ・Web 専用/外部サービスの契約。Bex の iroh/ネイティブ境界に該当。 |
| `packages/contracts/src/browserProfile.ts` (99) | — | — | 対象外：T3 Connect/ブラウザ・Web 専用/外部サービスの契約。Bex の iroh/ネイティブ境界に該当。 |
| `packages/contracts/src/chatAttachment.test.ts` (82) | `crates/orchestration/src/contracts/chat_attachment.rs` | — | 未翻訳 |
| `packages/contracts/src/chatAttachment.ts` (260) | `crates/orchestration/src/contracts/chat_attachment.rs` | — | 未翻訳 |
| `packages/contracts/src/checkpointDiff.test.ts` (75) | `crates/orchestration/src/contracts/checkpoint_diff.rs` | — | 未翻訳 |
| `packages/contracts/src/checkpointDiff.ts` (65) | `crates/orchestration/src/contracts/checkpoint_diff.rs` | — | 未翻訳 |
| `packages/contracts/src/composerContext.test.ts` (271) | `crates/orchestration/src/contracts/composer_context.rs` | — | 未翻訳 |
| `packages/contracts/src/composerContext.ts` (298) | `crates/orchestration/src/contracts/composer_context.rs` | — | 未翻訳 |
| `packages/contracts/src/composerContextClipboard.ts` (27) | `crates/orchestration/src/contracts/composer_context_clipboard.rs` | — | 未翻訳 |
| `packages/contracts/src/desktopAppActivation.ts` (53) | — | — | 対象外：T3 Connect/ブラウザ・Web 専用/外部サービスの契約。Bex の iroh/ネイティブ境界に該当。 |
| `packages/contracts/src/desktopBootstrap.ts` (31) | — | — | 対象外：T3 Connect/ブラウザ・Web 専用/外部サービスの契約。Bex の iroh/ネイティブ境界に該当。 |
| `packages/contracts/src/device.test.ts` (21) | — | — | 対象外：T3 Connect/ブラウザ・Web 専用/外部サービスの契約。Bex の iroh/ネイティブ境界に該当。 |
| `packages/contracts/src/device.ts` (583) | — | — | 対象外：T3 Connect/ブラウザ・Web 専用/外部サービスの契約。Bex の iroh/ネイティブ境界に該当。 |
| `packages/contracts/src/editor.ts` (232) | — | — | 対象外：T3 Connect/ブラウザ・Web 専用/外部サービスの契約。Bex の iroh/ネイティブ境界に該当。 |
| `packages/contracts/src/environment.test.ts` (99) | `crates/orchestration/src/contracts/environment.rs` | — | 未翻訳 |
| `packages/contracts/src/environment.ts` (253) | `crates/orchestration/src/contracts/environment.rs` | — | 未翻訳 |
| `packages/contracts/src/environmentHttp.test.ts` (62) | — | — | 対象外：T3 Connect/ブラウザ・Web 専用/外部サービスの契約。Bex の iroh/ネイティブ境界に該当。 |
| `packages/contracts/src/environmentHttp.ts` (660) | — | — | 対象外：T3 Connect/ブラウザ・Web 専用/外部サービスの契約。Bex の iroh/ネイティブ境界に該当。 |
| `packages/contracts/src/filesystem.test.ts` (33) | `crates/orchestration/src/contracts/filesystem.rs` | — | 未翻訳 |
| `packages/contracts/src/filesystem.ts` (67) | `crates/orchestration/src/contracts/filesystem.rs` | — | 未翻訳 |
| `packages/contracts/src/git.test.ts` (169) | — | — | 対象外：M3 の独立機能・契約。今回保留（会話から使う関連型は該当ファイルで個別に記録）。 |
| `packages/contracts/src/git.ts` (482) | — | — | 対象外：M3 の独立機能・契約。今回保留（会話から使う関連型は該当ファイルで個別に記録）。 |
| `packages/contracts/src/index.ts` (62) | `crates/orchestration/src/contracts/index.rs` | — | 未翻訳 |
| `packages/contracts/src/ipc.test.ts` (38) | `crates/orchestration/src/contracts/ipc.rs` | — | 未翻訳 |
| `packages/contracts/src/ipc.ts` (1391) | `crates/orchestration/src/contracts/ipc.rs` | — | 未翻訳 |
| `packages/contracts/src/keybindings.test.ts` (322) | — | — | 対象外：T3 Connect/ブラウザ・Web 専用/外部サービスの契約。Bex の iroh/ネイティブ境界に該当。 |
| `packages/contracts/src/keybindings.ts` (218) | — | — | 対象外：T3 Connect/ブラウザ・Web 専用/外部サービスの契約。Bex の iroh/ネイティブ境界に該当。 |
| `packages/contracts/src/model.ts` (232) | `crates/orchestration/src/contracts/model.rs` | — | 未翻訳 |
| `packages/contracts/src/modelSelection.test.ts` (124) | `crates/orchestration/src/contracts/model_selection.rs` | — | 未翻訳 |
| `packages/contracts/src/modelSelection.ts` (63) | `crates/orchestration/src/contracts/model_selection.rs` | — | 未翻訳 |
| `packages/contracts/src/orchestrationDispatch.test.ts` (31) | `crates/orchestration/src/contracts/orchestration_dispatch.rs` | — | 未翻訳 |
| `packages/contracts/src/orchestrationDispatch.ts` (16) | `crates/orchestration/src/contracts/orchestration_dispatch.rs` | — | 未翻訳 |
| `packages/contracts/src/orchestrationProject.ts` (28) | `crates/orchestration/src/contracts/orchestration_project.rs` | — | 未翻訳 |
| `packages/contracts/src/orchestrationV2.test.ts` (1250) | `crates/orchestration/src/contracts/orchestration_v2.rs` | — | 未翻訳 |
| `packages/contracts/src/orchestrationV2.ts` (3405) | `crates/orchestration/src/contracts/orchestration_v2.rs` | — | 未翻訳 |
| `packages/contracts/src/orchestratorMcp.test.ts` (166) | `crates/orchestration/src/contracts/orchestrator_mcp.rs` | — | 未翻訳 |
| `packages/contracts/src/orchestratorMcp.ts` (589) | `crates/orchestration/src/contracts/orchestrator_mcp.rs` | — | 未翻訳 |
| `packages/contracts/src/preview.test.ts` (364) | — | — | 対象外：T3 Connect/ブラウザ・Web 専用/外部サービスの契約。Bex の iroh/ネイティブ境界に該当。 |
| `packages/contracts/src/preview.ts` (354) | — | — | 対象外：T3 Connect/ブラウザ・Web 専用/外部サービスの契約。Bex の iroh/ネイティブ境界に該当。 |
| `packages/contracts/src/previewAutomation.ts` (953) | — | — | 対象外：T3 Connect/ブラウザ・Web 専用/外部サービスの契約。Bex の iroh/ネイティブ境界に該当。 |
| `packages/contracts/src/project.test.ts` (288) | `crates/orchestration/src/contracts/project.rs` | — | 未翻訳 |
| `packages/contracts/src/project.ts` (536) | `crates/orchestration/src/contracts/project.rs` | — | 未翻訳 |
| `packages/contracts/src/projectClone.ts` (122) | — | — | 対象外：M3 の独立機能・契約。今回保留（会話から使う関連型は該当ファイルで個別に記録）。 |
| `packages/contracts/src/provider.test.ts` (318) | `crates/orchestration/src/contracts/provider.rs` | — | 未翻訳 |
| `packages/contracts/src/provider.ts` (164) | `crates/orchestration/src/contracts/provider.rs` | — | 未翻訳 |
| `packages/contracts/src/providerInstance.test.ts` (206) | `crates/orchestration/src/contracts/provider_instance.rs` | — | 未翻訳 |
| `packages/contracts/src/providerInstance.ts` (168) | `crates/orchestration/src/contracts/provider_instance.rs` | — | 未翻訳 |
| `packages/contracts/src/providerPolicy.ts` (84) | `crates/orchestration/src/contracts/provider_policy.rs` | — | 未翻訳 |
| `packages/contracts/src/providerRuntime.test.ts` (230) | `crates/orchestration/src/contracts/provider_runtime.rs` | — | 未翻訳 |
| `packages/contracts/src/providerRuntime.ts` (1204) | `crates/orchestration/src/contracts/provider_runtime.rs` | — | 未翻訳 |
| `packages/contracts/src/providerSetup.test.ts` (53) | `crates/orchestration/src/contracts/provider_setup.rs` | — | 未翻訳 |
| `packages/contracts/src/providerSetup.ts` (255) | `crates/orchestration/src/contracts/provider_setup.rs` | — | 未翻訳 |
| `packages/contracts/src/providerUsageLimits.ts` (176) | `crates/orchestration/src/contracts/provider_usage_limits.rs` | — | 未翻訳 |
| `packages/contracts/src/pullRequest.test.ts` (340) | — | — | 対象外：M3 の独立機能・契約。今回保留（会話から使う関連型は該当ファイルで個別に記録）。 |
| `packages/contracts/src/pullRequest.ts` (1424) | — | — | 対象外：M3 の独立機能・契約。今回保留（会話から使う関連型は該当ファイルで個別に記録）。 |
| `packages/contracts/src/relay.test.ts` (61) | — | — | 対象外：T3 Connect/ブラウザ・Web 専用/外部サービスの契約。Bex の iroh/ネイティブ境界に該当。 |
| `packages/contracts/src/relay.ts` (1194) | — | — | 対象外：T3 Connect/ブラウザ・Web 専用/外部サービスの契約。Bex の iroh/ネイティブ境界に該当。 |
| `packages/contracts/src/relayClient.ts` (63) | — | — | 対象外：T3 Connect/ブラウザ・Web 専用/外部サービスの契約。Bex の iroh/ネイティブ境界に該当。 |
| `packages/contracts/src/remoteAccess.ts` (68) | — | — | 対象外：T3 Connect/ブラウザ・Web 専用/外部サービスの契約。Bex の iroh/ネイティブ境界に該当。 |
| `packages/contracts/src/resourceTelemetry.ts` (522) | — | — | 対象外：T3 Connect/ブラウザ・Web 専用/外部サービスの契約。Bex の iroh/ネイティブ境界に該当。 |
| `packages/contracts/src/review.ts` (70) | `crates/orchestration/src/contracts/review.rs` | — | 未翻訳 |
| `packages/contracts/src/rpc.test.ts` (70) | `crates/orchestration/src/contracts/rpc.rs` | — | 未翻訳 |
| `packages/contracts/src/rpc.ts` (1877) | `crates/orchestration/src/contracts/rpc.rs` | — | 未翻訳 |
| `packages/contracts/src/scheduledTask.test.ts` (52) | — | — | 対象外：M3 の独立機能・契約。今回保留（会話から使う関連型は該当ファイルで個別に記録）。 |
| `packages/contracts/src/scheduledTask.ts` (185) | — | — | 対象外：M3 の独立機能・契約。今回保留（会話から使う関連型は該当ファイルで個別に記録）。 |
| `packages/contracts/src/server.test.ts` (268) | `crates/orchestration/src/contracts/server.rs` | — | 未翻訳 |
| `packages/contracts/src/server.ts` (955) | `crates/orchestration/src/contracts/server.rs` | — | 未翻訳 |
| `packages/contracts/src/settings.test.ts` (1107) | — | — | 対象外：M3 の独立機能・契約。今回保留（会話から使う関連型は該当ファイルで個別に記録）。 |
| `packages/contracts/src/settings.ts` (1807) | — | — | 対象外：M3 の独立機能・契約。今回保留（会話から使う関連型は該当ファイルで個別に記録）。 |
| `packages/contracts/src/sourceControl.ts` (188) | — | — | 対象外：M3 の独立機能・契約。今回保留（会話から使う関連型は該当ファイルで個別に記録）。 |
| `packages/contracts/src/t3ProjectFile.test.ts` (71) | `crates/orchestration/src/contracts/t3_project_file.rs` | — | 未翻訳 |
| `packages/contracts/src/t3ProjectFile.ts` (143) | `crates/orchestration/src/contracts/t3_project_file.rs` | — | 未翻訳 |
| `packages/contracts/src/terminal.test.ts` (347) | `crates/orchestration/src/contracts/terminal.rs` | — | 未翻訳 |
| `packages/contracts/src/terminal.ts` (381) | `crates/orchestration/src/contracts/terminal.rs` | — | 未翻訳 |
| `packages/contracts/src/threadMetadataMcp.test.ts` (100) | `crates/orchestration/src/contracts/thread_metadata_mcp.rs` | — | 未翻訳 |
| `packages/contracts/src/threadMetadataMcp.ts` (122) | `crates/orchestration/src/contracts/thread_metadata_mcp.rs` | — | 未翻訳 |
| `packages/contracts/src/threadPullRequest.test.ts` (56) | — | — | 対象外：M3 の独立機能・契約。今回保留（会話から使う関連型は該当ファイルで個別に記録）。 |
| `packages/contracts/src/threadPullRequest.ts` (128) | — | — | 対象外：M3 の独立機能・契約。今回保留（会話から使う関連型は該当ファイルで個別に記録）。 |
| `packages/contracts/src/threadSearch.ts` (42) | `crates/orchestration/src/contracts/thread_search.rs` | — | 未翻訳 |
| `packages/contracts/src/threadTitle.ts` (10) | `crates/orchestration/src/contracts/thread_title.rs` | — | 未翻訳 |
| `packages/contracts/src/usage.ts` (246) | — | — | 対象外：M3 の独立機能・契約。今回保留（会話から使う関連型は該当ファイルで個別に記録）。 |
| `packages/contracts/src/usageLimitSourceId.ts` (9) | — | — | 対象外：M3 の独立機能・契約。今回保留（会話から使う関連型は該当ファイルで個別に記録）。 |
| `packages/contracts/src/vcs.ts` (287) | — | — | 対象外：M3 の独立機能・契約。今回保留（会話から使う関連型は該当ファイルで個別に記録）。 |
| `packages/contracts/src/worktreeMcp.ts` (127) | — | — | 対象外：M3 の独立機能・契約。今回保留（会話から使う関連型は該当ファイルで個別に記録）。 |
| `packages/contracts/src/worktreeSetup.ts` (124) | — | — | 対象外：M3 の独立機能・契約。今回保留（会話から使う関連型は該当ファイルで個別に記録）。 |
| `packages/contracts/tsconfig.json` (5) | `crates/orchestration/src/contracts/tsconfig.rs` | — | 未翻訳 |

## 新設計の挙動テスト対応（段階 1・2）

固定版 `4ee6bfd` を参照する。旧ファイル表の逐語翻訳先より、この新設計の責務を優先する。Host の永続化・購読・履歴取り込みは段階 3、接続 reducer と表示は段階 4 で接続する。

| T3 の挙動／テスト | 新しい実装と検証 | 判定 |
|---|---|---|
| `QueuedRunOrder.test.ts` の automatic / visible-first / visible-second | `agent-domain::State::queued_runs`、`automatic_completion_delivery_precedes_visible_queued_messages`（ordinal 4,2,3） | 優先順位を維持 |
| `CommandPolicy.test.ts` の automatic / preparing / starting / steer / restart | `agent-domain::resolve_dispatch`、`automatic_delivery_obeys_negotiated_turn_capabilities` | 入力を明示した純粋な判断 |
| `fixtures/queued_turn/*_output.ts` | `queued_turn_starts_after_active_turn_and_only_then_enters_the_timeline` | queued → starting、ordinal 1,2、queued_turn を維持 |
| `fixtures/message_steering/claude_output.ts` | `steering_preserves_run_and_attempt_and_records_the_input_intent` | 1 run / 1 attempt、steer 発言を維持 |
| `ProviderRuntimeRecoveryService.test.ts` | recovery / waiting_capture / prepared_failure のテスト | cancelled、queueHeld、async question、replayable capture を維持 |
| `RuntimeRequestService.test.ts` | `approvals_resolve_once_and_questions_keep_attachment_answers` | 二重応答・期限切れ応答を拒否 |
| `client-runtime/state/orchestrationV2Projection.test.ts` | usage / authoritative ordinal / inherited row の fold テスト | token usage を終端で保持、rollback で local だけ隠す |
| reducer の JavaScript object identity assertions | 同じ項目 ID、内容、表示順序を fold で検証 | 内部の参照同一性は対象外：Rust の projection と facts に再構成 |
| SQLite の control-read 回数、Effect service の mock 呼出し構造 | domain の入力→事実→projection、Host の境界テストへ分離 | T3 の内部 service 配線は対象外：actor が唯一の書込 owner |
| V1 import / migration と他 provider 専用テスト | 対象外 | 現行形式だけ、provider は Codex と Claude の指定 |

Provider replay は `agent-providers/src/replay.rs` から翻訳層・状態機械・fold・子 actor への command に流す。固定版の 71 NDJSON は改変せず保存し、manifest の SHA-256 と全 native frame の decoding を検証する。これは全 projection の移植完了を意味しない。下表は projection の期待値まで移植した範囲であり、残りの fixture の移植も段階 1・2 の作業として継続する。

| T3 fixture / adapter test | projection・制御の検証 |
|---|---|
| `simple`, `multi_turn`（両 driver） | run 数・status・ordinal・role・応答文・native thread の共有 |
| `queued_turn`, `message_steering`（両 driver） | queued の受付→昇格、timeline の順序、1 run / 1 attempt の steer、元の input intent |
| `proposed_plan`, `todo_list`（Codex） | proposal の active 状態、replay / fixture を含む内容、todo の completed ×3 |
| `tool_call_read_only_on_request`, `tool_call_restricted_granular`（両 driver）, `tool_call_denied_write`（Claude） | 1 件の承認の解決、decline の保持、元の応答文 |
| `claude_compact_after_peer_turn`, `_no_echo` | 元の 3 run / 2 run の分岐、PEER_ACK の帰属、27445→1192 の compaction |
| `claude_background_task_wake`, `claude_background_monitor_wake` | 元の run 数・応答・通知・roster 種別・continuation detail |
| `claude_result_is_error` | 認証の文言、api_error_401 / provider_error、assistant の二重表示なし、次の run の成功 |
| `subagent`（両 driver）, `subagent_v2`, `subagent_v2_nested`（Codex） | 子の run を増やさず、1 / 2 / 3 段の子スレッド、元の結果・prompt・title・親への tool 漏れなし |
| `ClaudeAdapterV2.test.ts` の resume dialog | 元の 1h 30m / 120,000 tokens の質問、選択肢、compact の control response |

### 追加した段階 1・2 の挙動検証

| T3 の原本 | 新設計での検証 |
| --- | --- |
| `turn_interrupt` / `turn_interrupt_mid_tool`（両 provider） | `agent-providers::replay::stop_replays_close_attempts_tools_and_interrupt_rows`。run/attempt/停止カードの status と command terminalization。native の起動前停止・子停止・terminal terminate は command 翻訳のテストでも確認。 |
| `claude_background_task_interrupt` | roster 消去、launch は completed、foreground command は interrupted、continuation なし。 |
| `subagent_v2_approval` / `subagent_v2_nested_approval` | 親の承認解決、親 attempt への帰属、子に要求と承認カードがないこと。 |
| `claude_local_bash_task` / `web_search`（両 provider） / `claude_mcp_tool_presentation` | 原本の assistant 文言、検索 query/result URL、command output、記録された MCP title/source と metadata がない場合の空値。 |
| `AttachmentPrompt.test.ts` | `agent-providers::attachments::tests`。パスの全文、escaped JSON、accessibility 圧縮と bounds、入力上限、image/file 判定の期待値を維持。 |
| `CheckpointCaptureService.ts` の stopped/at-least-once 規則 | `agent-domain::tests::failed_capture_is_retryable_and_stopped_capture_keeps_its_terminal_status`。失敗でキューを解放せず、保存結果で terminal status を変えない。 |

| 追加した原本の検証 | Rust の検証 |
| --- | --- |
| `claude_background_subagent_after_root` / `claude_nested_background_subagent_wake` | 子の作業は子に保存、root の wake は直下の子の終了だけ、通知は該当する子を開く。 |
| `claude_background_subagent_lifecycle` | 7 run の原本の返信、再開後も 2 子のまま、Agent A の prompt/reply の順序と新しい run への帰属。 |
| `claude_nested_subagent_model` | 孫まで実際の observed model を継承し、子の selection に保存。 |
| `claude_background_wake_before_queued_prompt` / `_no_echo` | UUID echo の有無による原本の返信の帰属、混在する停止通知の summary/source。 |
| `claude_idle_resume` / `multi_turn_restart` | 原本の 2 つの完了返信と native session の継続。実プロセスの休眠／再起動は段階 3 の session 管理で検証する。 |
| `Notification.test.ts` の background report | 空、単独、混在、件数表示、command の exit code、monitor の更新文言を pure function で確認。delegated completion の表示は core の接続し直しでも使用する。 |

`provider/userInputAttachments.test.ts` の choices 保持、引用付き path、入力非変更、特殊キーの期待値は `agent-domain/src/answers.rs` に移植した。結合後の添付上限も command 前に検証する。実ファイルの存在・読み取り確認は段階 3 の attachment effect に属する。

| 追加の T3 外部挙動テスト | 新設計の検証 |
| --- | --- |
| `ContextHandoffBudget.test.ts` | `agent-domain/src/context.rs`。Unicode、JSON escaping、selected/omitted の桁境界、roles/order、画像 1〜100 件、model switch 時の occupancy、marker 圧縮、重複排除、omitted IDs、予算不足の期待値を保持。 |
| `ContextHandoffDelivery.ts` の配送境界 | domain の pending/injected/inline と provider の RPC テスト。受け付け前の消費をやめ、曖昧な配送失敗では同じ native session に再送しない。DB/outbox の実行順序は段階 3。 |
| `ClaudeAdapterV2.test.ts` の context usage | input 42,000 + cache creation 2,000 + cache read 5,000 + output 1,000 = 50,000、window 200,000 を翻訳イベントで検証。 |

| 追加の T3 外部挙動テスト | 新設計の検証 |
| --- | --- |
| `provider/TurnTokenUsage.test.ts` | `agent-domain/src/usage.rs` と遅延 native usage の状態機械テスト。Codex 30/15/8、compaction 120/45/23、次ターン 4/1、Claude cached input 150 と thinking 20、ゼロの crash を unavailable とする期待値を維持。 |
| `provider/CodexMcpElicitation.test.ts` | `agent-providers/src/elicitation.rs`。Safari の選択肢と wire response、nullable/boolean フォーム、未収集の required field と URL elicitation、実現できない persistence choices の全期待値を移植。 |

`Notification.test.ts` の委任通知（`2 of 3 delegated tasks finished: Review src/math.ts, Write tests`、単独 task の child link）を domain の状態機械と activity projection に移植した。`DelegatedCompletionDelivery.test.ts` の配送 cancel/親 stop による dispose を cohort ごとの task IDs で確認し、別 cohort を巻き込む処理を取り除いた。

`CheckpointCaptureService.ts` の scope・baseline 境界は domain の明示的 input/effect/result に移した。初回 scoped capture が waiting となること、不足 baseline で保存を確定しないこと、同じ保存の再送を無視すること、scope 変更後の結果と restore effect が元の cwd/ref を保つことを検証した。Git の materialize/capture、共有 scope の他 actor の稼働確認は段階 3 に属する。

`provider/CodexThreadRevert.test.ts` のページ越し境界と循環 cursor の失敗を、保存済みの絶対 head を入力する protocol テストに移植した。削除境界 `boundary` とエラー文言の期待値を維持し、同じ effect の再送で追加の削除が起きないことも確認する。件数を数える内部 helper は新設計には不要。

`ThreadFork.integration.test.ts` と `ThreadMergeBack.integration.test.ts` の native / prior-turn / continue / sibling / fork-local rollback、および Codex の rollback / after-restart / stopped-turn の transcript を `agent-providers/src/replay/graph.rs` に追加した。複数 actor への saga command、固定した fork 履歴、native fork の配送結果、rollback の可視項目、兄弟ごとの delta と source の recall を原本の文言・境界で検証する。fork の内部イベント表の順序は、固定 command の確定→native 成功→子 command の順序へ読み替える。

`provider_thread_resume`、`plan_questions`、`subagent_continue`、`tool_call_read_only`、`tool_call_workspace_never`（両 provider）、`turn_interrupt_restart`、`claude_background_task_after_root`、`claude_compact_after_resume_wake`、`claude_subagent_resume_after_restart` の projection 期待値を replay に追加した。質問 ID `schema_preference`、元の返信、再開した子の会話と 3 command、2 run の compaction 帰属を保持する。休眠・再起動の実プロセス／SQLite 境界は段階 3 で接続する。

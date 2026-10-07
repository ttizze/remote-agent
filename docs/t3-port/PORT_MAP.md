# T3 の翻訳対応表

参照を `4ee6bfd50ef4a089440d5c3662db2298da9cc50e` に固定する。旧実装に対する M1/M2 の完了記録は、関数単位の翻訳完了・T3 のテスト通過を意味しない。この表で照合を終えた範囲だけを翻訳済みとする。

新設計では段階 1 の `agent-domain` と段階 2 の `agent-providers` を先に実装・検証し、push と PR 更新後にレビューを待つ。Host の永続化・履歴取り込みは段階 3、core と3クライアントは段階 4、旧 crate の削除は段階 5 とする。main のマージ、実 Host の起動・再起動、他 worktree の変更は行わない。

生成されたファイル対応表は旧方針の逐語翻訳台帳を保持する。新設計の実装・テストの対応は末尾の「新設計の挙動テスト対応」を正本とし、旧 crate が production に接続されていることを新設計の完了根拠にしない。

## 記録と検証

- `PORT_MAP.json` は全ファイルの source SHA-256、行数、対応先、関数・定数、T3 テストケース、照合した行範囲を保持する。初期の symbol/case リストは検索用の索引であり、匿名関数・parameterized case の完全性の証明には使わない。翻訳時に該当ファイル全体を読み、関数と行範囲を補う。
- `scripts/t3-port/inventory.py --check` は4対象ディレクトリの欠落・余分なファイル・参照 hash の変更を検出する。通常実行は照合記録を保存したまま下表を更新する。対象外のファイルと fixture も省略しない。
- 未翻訳／未移植の行は、既存の短縮された実装や独自テストがあっても完了扱いにしない。T3 の期待値・fixture は変更しない。旧方針の逐語翻訳の完了記録と、新設計の純粋な domain / 翻訳層の検証完了を区別する。新設計の Host / クライアントへの production 接続完了は段階 3・4 の検証後に記録する。
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

| 境界 | この実装の置換 | 保持する意味／検証 |
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
| `apps/server/src/orchestration-v2/AcpRegistryOrchestratorV2.live.test.ts` (255) | — | — | 対象外：この実装の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/Adapters/AcpAdapterV2.test.ts` (14296) | — | — | 対象外：この実装の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/Adapters/AcpAdapterV2.testkit.ts` (246) | — | — | 対象外：この実装の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/Adapters/AcpAdapterV2.ts` (7969) | — | — | 対象外：この実装の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/Adapters/AcpRegistryAdapterV2.test.ts` (486) | — | — | 対象外：この実装の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/Adapters/AcpRegistryAdapterV2.testkit.ts` (109) | — | — | 対象外：この実装の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/Adapters/AcpRegistryAdapterV2.ts` (328) | — | — | 対象外：この実装の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/Adapters/AntigravityAdapterV2.test.ts` (537) | — | — | 対象外：この実装の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/Adapters/AntigravityAdapterV2.ts` (241) | — | — | 対象外：この実装の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/Adapters/ClaudeAdapterV2.test.ts` (7963) | `crates/provider-adapters/src/claude_adapter_v2.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/Adapters/ClaudeAdapterV2.testkit.ts` (2844) | `crates/provider-adapters/src/claude_adapter_v2kit.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/Adapters/ClaudeAdapterV2.ts` (7859) | `crates/provider-adapters/src/claude_adapter_v2.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/Adapters/CodexAdapterV2.test.ts` (7275) | `crates/provider-adapters/src/codex_adapter_v2.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/Adapters/CodexAdapterV2.testkit.ts` (276) | `crates/provider-adapters/src/codex_adapter_v2kit.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/Adapters/CodexAdapterV2.ts` (6317) | `crates/provider-adapters/src/codex_adapter_v2.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/Adapters/CursorAdapterV2.test.ts` (895) | — | — | 対象外：この実装の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/Adapters/CursorAdapterV2.testkit.ts` (1022) | — | — | 対象外：この実装の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/Adapters/CursorAdapterV2.ts` (2677) | — | — | 対象外：この実装の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/Adapters/CursorAgentSdk.test.ts` (304) | — | — | 対象外：この実装の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/Adapters/CursorAgentSdk.ts` (612) | — | — | 対象外：この実装の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/Adapters/DevinAcp.ts` (57) | — | — | 対象外：この実装の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/Adapters/GrokAdapterV2.test.ts` (468) | — | — | 対象外：この実装の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/Adapters/GrokAdapterV2.testkit.ts` (122) | — | — | 対象外：この実装の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/Adapters/GrokAdapterV2.ts` (455) | — | — | 対象外：この実装の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/Adapters/OpenCode2AdapterV2.test.ts` (3921) | — | — | 対象外：この実装の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/Adapters/OpenCode2AdapterV2.testkit.ts` (344) | — | — | 対象外：この実装の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/Adapters/OpenCode2AdapterV2.ts` (4266) | — | — | 対象外：この実装の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/Adapters/OpenCodeAdapterV2.test.ts` (2445) | — | — | 対象外：この実装の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/Adapters/OpenCodeAdapterV2.testkit.ts` (511) | — | — | 対象外：この実装の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/Adapters/OpenCodeAdapterV2.ts` (3758) | — | — | 対象外：この実装の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/Adapters/OpenCodeToolItems.ts` (170) | — | — | 対象外：この実装の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/Adapters/PiAdapterV2.test.ts` (2489) | — | — | 対象外：この実装の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/Adapters/PiAdapterV2.testkit.ts` (524) | — | — | 対象外：この実装の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/Adapters/PiAdapterV2.ts` (3015) | — | — | 対象外：この実装の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/Adapters/PiRpc.ts` (484) | — | — | 対象外：この実装の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/Adapters/ProviderTextDeltaCoalescer.ts` (156) | `crates/provider-adapters/src/provider_text_delta_coalescer.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/Adapters/piT3McpExtensionSource.test.ts` (59) | — | — | 対象外：この実装の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/Adapters/piT3McpExtensionSource.ts` (328) | — | — | 対象外：この実装の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/Adapters/piT3McpInjection.test.ts` (167) | — | — | 対象外：この実装の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/Adapters/piT3McpInjection.ts` (316) | — | — | 対象外：この実装の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
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
| `apps/server/src/orchestration-v2/CursorOrchestratorV2.live.test.ts` (378) | — | — | 対象外：この実装の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/DelegatedCompletionDelivery.test.ts` (1278) | `crates/orchestration/src/delegated_completion_delivery.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/EffectOutbox.ts` (636) | `crates/orchestration/src/effect_outbox.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/EffectWorker.test.ts` (802) | `crates/orchestration/src/effect_worker.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/EffectWorker.ts` (834) | `crates/orchestration/src/effect_worker.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/EventSink.ts` (891) | `crates/orchestration/src/event_sink.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/EventStore.ts` (156) | `crates/orchestration/src/event_store.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/FoundationPersistence.test.ts` (3383) | `crates/orchestration/src/foundation_persistence.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/GrokOrchestratorV2.live.test.ts` (236) | — | — | 対象外：この実装の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/IdAllocator.ts` (435) | `crates/orchestration/src/id_allocator.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/KeyedSerialExecutor.test.ts` (67) | `crates/orchestration/src/keyed_serial_executor.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/KeyedSerialExecutor.ts` (55) | `crates/orchestration/src/keyed_serial_executor.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/LiveStreamBudget.test.ts` (278) | `crates/orchestration/src/live_stream_budget.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/LiveStreamBudget.ts` (342) | `crates/orchestration/src/live_stream_budget.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/Notification.test.ts` (150) | `crates/orchestration/src/notification.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/Notification.ts` (235) | `crates/orchestration/src/notification.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/NotificationMailbox.ts` (23) | `crates/orchestration/src/notification_mailbox.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/OpenCode2OrchestratorV2.integration.test.ts` (1181) | — | — | 対象外：この実装の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/OpenCode2OrchestratorV2.live.test.ts` (1181) | — | — | 対象外：この実装の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
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
| `apps/server/src/orchestration-v2/testkit/fixtures/acp_elicitation/registry_transcript.ndjson` (11) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
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
| `apps/server/src/orchestration-v2/testkit/fixtures/grok_auto_blocked_command/grok_transcript.ndjson` (136) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/grok_auto_blocked_command/input.ts` (30) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/grok_auto_blocked_command/output.ts` (97) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/grok_background_bash/grok_transcript.ndjson` (241) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/grok_background_bash/input.ts` (24) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/grok_background_bash/output.ts` (100) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/grok_background_bash_fast_wake/grok_transcript.ndjson` (258) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/grok_background_bash_fast_wake/input.ts` (18) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/grok_background_bash_fast_wake/output.ts` (11) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/grok_background_subagent/grok_transcript.ndjson` (353) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/grok_background_subagent/input.ts` (23) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/grok_background_subagent/output.ts` (116) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/grok_monitor/grok_transcript.ndjson` (361) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/grok_monitor/input.ts` (25) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/grok_monitor/output.ts` (112) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/grok_prompt_error/grok_transcript.ndjson` (74) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/grok_prompt_error/input.ts` (20) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/grok_prompt_error/output.ts` (45) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/grok_subagent_lineage/grok_transcript.ndjson` (17) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/grok_subagent_lineage/input.ts` (9) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/grok_subagent_lineage/output.ts` (98) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/index.ts` (1633) | `crates/orchestration/src/testkit/fixtures/index.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/message_steering/claude_output.ts` (43) | `crates/orchestration/src/testkit/fixtures/message_steering/claude_output.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/message_steering/claude_transcript.ndjson` (13) | `crates/provider-adapters/src/testkit/fixtures/message_steering/claude_transcript.ndjson` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/message_steering/codex_output.ts` (43) | `crates/orchestration/src/testkit/fixtures/message_steering/codex_output.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/message_steering/codex_transcript.ndjson` (43) | `crates/provider-adapters/src/testkit/fixtures/message_steering/codex_transcript.ndjson` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/message_steering/cursor_output.ts` (56) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/message_steering/cursor_transcript.ndjson` (25) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/message_steering/grok_output.ts` (55) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/message_steering/grok_transcript.ndjson` (67) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/message_steering/input.ts` (31) | `crates/orchestration/src/testkit/fixtures/message_steering/input.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/message_steering/pi_output.ts` (62) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/message_steering/pi_transcript.ndjson` (65) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/message_steering/registry_transcript.ndjson` (14) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/multi_turn/claude_output.ts` (53) | `crates/orchestration/src/testkit/fixtures/multi_turn/claude_output.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/multi_turn/claude_transcript.ndjson` (14) | `crates/provider-adapters/src/testkit/fixtures/multi_turn/claude_transcript.ndjson` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/multi_turn/codex_output.ts` (40) | `crates/orchestration/src/testkit/fixtures/multi_turn/codex_output.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/multi_turn/codex_transcript.ndjson` (46) | `crates/provider-adapters/src/testkit/fixtures/multi_turn/codex_transcript.ndjson` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/multi_turn/cursor_transcript.ndjson` (44) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/multi_turn/grok_transcript.ndjson` (91) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/multi_turn/input.ts` (14) | `crates/orchestration/src/testkit/fixtures/multi_turn/input.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/multi_turn/pi_output.ts` (17) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/multi_turn/pi_transcript.ndjson` (72) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/multi_turn/registry_transcript.ndjson` (12) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/multi_turn_restart/claude_transcript.ndjson` (19) | `crates/provider-adapters/src/testkit/fixtures/multi_turn_restart/claude_transcript.ndjson` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_background/input.ts` (15) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_background/opencode_transcript.ndjson` (69) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_background/output.ts` (114) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_command/input.ts` (5) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_command/opencode_transcript.ndjson` (34) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_command/output.ts` (34) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_compaction/input.ts` (15) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_compaction/opencode_transcript.ndjson` (66) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_compaction/output.ts` (61) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_fork/opencode_transcript.ndjson` (69) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_inbox/input.ts` (24) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_inbox/opencode_transcript.ndjson` (53) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_inbox/output.ts` (72) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_interrupt/input.ts` (11) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_interrupt/opencode_transcript.ndjson` (36) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_interrupt/output.ts` (47) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_nested_background/input.ts` (15) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_nested_background/opencode_transcript.ndjson` (131) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_nested_background/output.ts` (77) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_permission/input.ts` (15) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_permission/opencode_transcript.ndjson` (117) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_permission/output.ts` (81) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_question/input.ts` (11) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_question/opencode_transcript.ndjson` (42) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_question/output.ts` (59) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_resume_after_restart/input.ts` (14) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_resume_after_restart/opencode_transcript.ndjson` (54) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_resume_after_restart/output.ts` (53) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_revert/input.ts` (17) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_revert/opencode_transcript.ndjson` (68) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_revert/output.ts` (39) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_simple/input.ts` (5) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_simple/opencode_transcript.ndjson` (53) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_simple/output.ts` (51) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_skill/input.ts` (5) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_skill/opencode_transcript.ndjson` (32) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_skill/output.ts` (34) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_subagent/input.ts` (5) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_subagent/opencode_transcript.ndjson` (83) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_subagent/output.ts` (74) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_switch/opencode_transcript.ndjson` (88) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_tool_call/input.ts` (5) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_tool_call/opencode_transcript.ndjson` (44) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_tool_call/output.ts` (56) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode_child_approval/input.ts` (10) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode_child_approval/opencode_transcript.ndjson` (40) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode_child_approval/output.ts` (43) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode_running_child_approval/input.ts` (16) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode_running_child_approval/opencode_transcript.ndjson` (41) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode_running_child_approval/output.ts` (88) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode_subagent/input.ts` (7) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode_subagent/opencode_transcript.ndjson` (34) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode_subagent/output.ts` (71) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/pi_compaction/input.ts` (30) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/pi_compaction/output.ts` (93) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/pi_compaction/pi_transcript.ndjson` (160) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/plan_questions/codex_output.ts` (67) | `crates/orchestration/src/testkit/fixtures/plan_questions/codex_output.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/plan_questions/codex_transcript.ndjson` (39) | `crates/provider-adapters/src/testkit/fixtures/plan_questions/codex_transcript.ndjson` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/plan_questions/grok_transcript.ndjson` (11) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/plan_questions/input.ts` (18) | `crates/orchestration/src/testkit/fixtures/plan_questions/input.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/plan_questions/opencode_output.ts` (26) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/plan_questions/opencode_transcript.ndjson` (27) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/proposed_plan/codex_output.ts` (43) | `crates/orchestration/src/testkit/fixtures/proposed_plan/codex_output.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/proposed_plan/codex_transcript.ndjson` (278) | `crates/provider-adapters/src/testkit/fixtures/proposed_plan/codex_transcript.ndjson` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/proposed_plan/cursor_output.ts` (39) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/proposed_plan/cursor_transcript.ndjson` (132) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/proposed_plan/input.ts` (8) | `crates/orchestration/src/testkit/fixtures/proposed_plan/input.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/provider_thread_resume/codex_transcript.ndjson` (79) | `crates/provider-adapters/src/testkit/fixtures/provider_thread_resume/codex_transcript.ndjson` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/provider_thread_resume/cursor_transcript.ndjson` (67) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/provider_thread_resume/input.ts` (21) | `crates/orchestration/src/testkit/fixtures/provider_thread_resume/input.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/provider_thread_resume/pi_output.ts` (68) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/provider_thread_resume/pi_transcript.ndjson` (124) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/queued_cancelled_while_active/codex_output.ts` (46) | `crates/orchestration/src/testkit/fixtures/queued_cancelled_while_active/codex_output.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/queued_cancelled_while_active/input.ts` (25) | `crates/orchestration/src/testkit/fixtures/queued_cancelled_while_active/input.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/queued_turn/claude_transcript.ndjson` (14) | `crates/provider-adapters/src/testkit/fixtures/queued_turn/claude_transcript.ndjson` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/queued_turn/codex_output.ts` (49) | `crates/orchestration/src/testkit/fixtures/queued_turn/codex_output.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/queued_turn/codex_transcript.ndjson` (46) | `crates/provider-adapters/src/testkit/fixtures/queued_turn/codex_transcript.ndjson` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/queued_turn/cursor_transcript.ndjson` (36) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/queued_turn/grok_transcript.ndjson` (88) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/queued_turn/input.ts` (14) | `crates/orchestration/src/testkit/fixtures/queued_turn/input.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/queued_turn/registry_transcript.ndjson` (12) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/shared.ts` (1485) | `crates/orchestration/src/testkit/fixtures/shared.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/simple/claude_output.ts` (33) | `crates/orchestration/src/testkit/fixtures/simple/claude_output.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/simple/claude_transcript.ndjson` (10) | `crates/provider-adapters/src/testkit/fixtures/simple/claude_transcript.ndjson` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/simple/codex_output.ts` (33) | `crates/orchestration/src/testkit/fixtures/simple/codex_output.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/simple/codex_transcript.ndjson` (30) | `crates/provider-adapters/src/testkit/fixtures/simple/codex_transcript.ndjson` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/simple/cursor_transcript.ndjson` (20) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/simple/grok_transcript.ndjson` (76) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/simple/input.ts` (7) | `crates/orchestration/src/testkit/fixtures/simple/input.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/simple/opencode_transcript.ndjson` (23) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/simple/pi_output.ts` (117) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/simple/pi_transcript.ndjson` (46) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/simple/registry_transcript.ndjson` (12) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/skill_invocation/cursor_output.ts` (39) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/skill_invocation/cursor_transcript.ndjson` (64) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/skill_invocation/input.ts` (19) | `crates/orchestration/src/testkit/fixtures/skill_invocation/input.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/stop_background_work_after_failed_turn/input.ts` (33) | `crates/orchestration/src/testkit/fixtures/stop_background_work_after_failed_turn/input.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/stop_background_work_after_failed_turn/output.ts` (44) | `crates/orchestration/src/testkit/fixtures/stop_background_work_after_failed_turn/output.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/stop_background_work_after_failed_turn/registry_transcript.ndjson` (10) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/subagent/claude_output.ts` (123) | `crates/orchestration/src/testkit/fixtures/subagent/claude_output.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/subagent/claude_transcript.ndjson` (29) | `crates/provider-adapters/src/testkit/fixtures/subagent/claude_transcript.ndjson` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/subagent/codex_output.ts` (124) | `crates/orchestration/src/testkit/fixtures/subagent/codex_output.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/subagent/codex_transcript.ndjson` (628) | `crates/provider-adapters/src/testkit/fixtures/subagent/codex_transcript.ndjson` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/subagent/cursor_output.ts` (96) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/subagent/cursor_transcript.ndjson` (353) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
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
| `apps/server/src/orchestration-v2/testkit/fixtures/thread_rollback/pi_output.ts` (83) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/thread_rollback/pi_transcript.ndjson` (163) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/thread_rollback_after_restart/codex_output.ts` (31) | `crates/orchestration/src/testkit/fixtures/thread_rollback_after_restart/codex_output.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/thread_rollback_after_restart/codex_transcript.ndjson` (107) | `crates/provider-adapters/src/testkit/fixtures/thread_rollback_after_restart/codex_transcript.ndjson` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/thread_rollback_after_restart/input.ts` (27) | `crates/orchestration/src/testkit/fixtures/thread_rollback_after_restart/input.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/thread_rollback_after_stop/input.ts` (29) | `crates/orchestration/src/testkit/fixtures/thread_rollback_after_stop/input.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/thread_rollback_after_stop/pi_output.ts` (83) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/thread_rollback_after_stop/pi_transcript.ndjson` (274) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/thread_rollback_to_stopped_turn/codex_output.ts` (66) | `crates/orchestration/src/testkit/fixtures/thread_rollback_to_stopped_turn/codex_output.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/thread_rollback_to_stopped_turn/codex_transcript.ndjson` (279) | `crates/provider-adapters/src/testkit/fixtures/thread_rollback_to_stopped_turn/codex_transcript.ndjson` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/thread_rollback_to_stopped_turn/input.ts` (28) | `crates/orchestration/src/testkit/fixtures/thread_rollback_to_stopped_turn/input.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/todo_list/codex_output.ts` (39) | `crates/orchestration/src/testkit/fixtures/todo_list/codex_output.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/todo_list/codex_transcript.ndjson` (37) | `crates/provider-adapters/src/testkit/fixtures/todo_list/codex_transcript.ndjson` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/todo_list/cursor_output.ts` (72) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/todo_list/cursor_transcript.ndjson` (210) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/todo_list/grok_output.ts` (52) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/todo_list/grok_transcript.ndjson` (329) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/todo_list/input.ts` (7) | `crates/orchestration/src/testkit/fixtures/todo_list/input.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/todo_list/registry_transcript.ndjson` (16) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/tool_call_denied_write/claude_output.ts` (44) | `crates/orchestration/src/testkit/fixtures/tool_call_denied_write/claude_output.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/tool_call_denied_write/claude_transcript.ndjson` (89) | `crates/provider-adapters/src/testkit/fixtures/tool_call_denied_write/claude_transcript.ndjson` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/tool_call_denied_write/input.ts` (24) | `crates/orchestration/src/testkit/fixtures/tool_call_denied_write/input.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/tool_call_read_only/claude_output.ts` (62) | `crates/orchestration/src/testkit/fixtures/tool_call_read_only/claude_output.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/tool_call_read_only/claude_transcript.ndjson` (16) | `crates/provider-adapters/src/testkit/fixtures/tool_call_read_only/claude_transcript.ndjson` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/tool_call_read_only/cursor_output.ts` (58) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/tool_call_read_only/cursor_transcript.ndjson` (49) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/tool_call_read_only/grok_transcript.ndjson` (15) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/tool_call_read_only/input.ts` (7) | `crates/orchestration/src/testkit/fixtures/tool_call_read_only/input.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/tool_call_read_only/registry_transcript.ndjson` (15) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/tool_call_read_only_on_request/claude_transcript.ndjson` (15) | `crates/provider-adapters/src/testkit/fixtures/tool_call_read_only_on_request/claude_transcript.ndjson` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/tool_call_read_only_on_request/codex_transcript.ndjson` (81) | `crates/provider-adapters/src/testkit/fixtures/tool_call_read_only_on_request/codex_transcript.ndjson` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/tool_call_read_only_on_request/grok_transcript.ndjson` (181) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/tool_call_read_only_on_request/input.ts` (10) | `crates/orchestration/src/testkit/fixtures/tool_call_read_only_on_request/input.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/tool_call_read_only_on_request/output.ts` (108) | `crates/orchestration/src/testkit/fixtures/tool_call_read_only_on_request/output.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/tool_call_read_only_on_request/registry_transcript.ndjson` (13) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
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
| `apps/server/src/orchestration-v2/testkit/fixtures/turn_interrupt/grok_transcript.ndjson` (28) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/turn_interrupt/input.ts` (10) | `crates/orchestration/src/testkit/fixtures/turn_interrupt/input.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/turn_interrupt/opencode_transcript.ndjson` (20) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/turn_interrupt/registry_transcript.ndjson` (9) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/turn_interrupt_mid_tool/claude_output.ts` (95) | `crates/orchestration/src/testkit/fixtures/turn_interrupt_mid_tool/claude_output.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/turn_interrupt_mid_tool/claude_transcript.ndjson` (11) | `crates/provider-adapters/src/testkit/fixtures/turn_interrupt_mid_tool/claude_transcript.ndjson` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/turn_interrupt_mid_tool/codex_output.ts` (150) | `crates/orchestration/src/testkit/fixtures/turn_interrupt_mid_tool/codex_output.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/turn_interrupt_mid_tool/codex_transcript.ndjson` (35) | `crates/provider-adapters/src/testkit/fixtures/turn_interrupt_mid_tool/codex_transcript.ndjson` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/turn_interrupt_mid_tool/cursor_output.ts` (76) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/turn_interrupt_mid_tool/cursor_transcript.ndjson` (33) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/turn_interrupt_mid_tool/input.ts` (10) | `crates/orchestration/src/testkit/fixtures/turn_interrupt_mid_tool/input.rs` | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/turn_interrupt_mid_tool/pi_output.ts` (67) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/turn_interrupt_mid_tool/pi_transcript.ndjson` (59) | — | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
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
| `packages/contracts/src/acpRegistry.test.ts` (180) | — | — | 対象外：T3 Connect/ブラウザ・Web 専用/外部サービスの契約。この実装の iroh/ネイティブ境界に該当。 |
| `packages/contracts/src/acpRegistry.ts` (347) | — | — | 対象外：T3 Connect/ブラウザ・Web 専用/外部サービスの契約。この実装の iroh/ネイティブ境界に該当。 |
| `packages/contracts/src/agentSessions.test.ts` (36) | `crates/orchestration/src/contracts/agent_sessions.rs` | — | 未翻訳 |
| `packages/contracts/src/agentSessions.ts` (111) | `crates/orchestration/src/contracts/agent_sessions.rs` | — | 未翻訳 |
| `packages/contracts/src/applicationEvent.test.ts` (63) | `crates/orchestration/src/contracts/application_event.rs` | — | 未翻訳 |
| `packages/contracts/src/applicationEvent.ts` (124) | `crates/orchestration/src/contracts/application_event.rs` | — | 未翻訳 |
| `packages/contracts/src/assets.test.ts` (60) | `crates/orchestration/src/contracts/assets.rs` | — | 未翻訳 |
| `packages/contracts/src/assets.ts` (320) | `crates/orchestration/src/contracts/assets.rs` | — | 未翻訳 |
| `packages/contracts/src/assistantCitations.ts` (31) | `crates/orchestration/src/contracts/assistant_citations.rs` | — | 未翻訳 |
| `packages/contracts/src/auth.ts` (355) | — | — | 対象外：T3 Connect/ブラウザ・Web 専用/外部サービスの契約。この実装の iroh/ネイティブ境界に該当。 |
| `packages/contracts/src/background.test.ts` (18) | `crates/orchestration/src/contracts/background.rs` | — | 未翻訳 |
| `packages/contracts/src/background.ts` (110) | `crates/orchestration/src/contracts/background.rs` | — | 未翻訳 |
| `packages/contracts/src/baseSchemas.ts` (223) | `crates/orchestration/src/contracts/base_schemas.rs` | — | 未翻訳 |
| `packages/contracts/src/browserImport.ts` (165) | — | — | 対象外：T3 Connect/ブラウザ・Web 専用/外部サービスの契約。この実装の iroh/ネイティブ境界に該当。 |
| `packages/contracts/src/browserProfile.test.ts` (111) | — | — | 対象外：T3 Connect/ブラウザ・Web 専用/外部サービスの契約。この実装の iroh/ネイティブ境界に該当。 |
| `packages/contracts/src/browserProfile.ts` (99) | — | — | 対象外：T3 Connect/ブラウザ・Web 専用/外部サービスの契約。この実装の iroh/ネイティブ境界に該当。 |
| `packages/contracts/src/chatAttachment.test.ts` (82) | `crates/orchestration/src/contracts/chat_attachment.rs` | — | 未翻訳 |
| `packages/contracts/src/chatAttachment.ts` (260) | `crates/orchestration/src/contracts/chat_attachment.rs` | — | 未翻訳 |
| `packages/contracts/src/checkpointDiff.test.ts` (75) | `crates/orchestration/src/contracts/checkpoint_diff.rs` | — | 未翻訳 |
| `packages/contracts/src/checkpointDiff.ts` (65) | `crates/orchestration/src/contracts/checkpoint_diff.rs` | — | 未翻訳 |
| `packages/contracts/src/composerContext.test.ts` (271) | `crates/orchestration/src/contracts/composer_context.rs` | — | 未翻訳 |
| `packages/contracts/src/composerContext.ts` (298) | `crates/orchestration/src/contracts/composer_context.rs` | — | 未翻訳 |
| `packages/contracts/src/composerContextClipboard.ts` (27) | `crates/orchestration/src/contracts/composer_context_clipboard.rs` | — | 未翻訳 |
| `packages/contracts/src/desktopAppActivation.ts` (53) | — | — | 対象外：T3 Connect/ブラウザ・Web 専用/外部サービスの契約。この実装の iroh/ネイティブ境界に該当。 |
| `packages/contracts/src/desktopBootstrap.ts` (31) | — | — | 対象外：T3 Connect/ブラウザ・Web 専用/外部サービスの契約。この実装の iroh/ネイティブ境界に該当。 |
| `packages/contracts/src/device.test.ts` (21) | — | — | 対象外：T3 Connect/ブラウザ・Web 専用/外部サービスの契約。この実装の iroh/ネイティブ境界に該当。 |
| `packages/contracts/src/device.ts` (583) | — | — | 対象外：T3 Connect/ブラウザ・Web 専用/外部サービスの契約。この実装の iroh/ネイティブ境界に該当。 |
| `packages/contracts/src/editor.ts` (232) | — | — | 対象外：T3 Connect/ブラウザ・Web 専用/外部サービスの契約。この実装の iroh/ネイティブ境界に該当。 |
| `packages/contracts/src/environment.test.ts` (99) | `crates/orchestration/src/contracts/environment.rs` | — | 未翻訳 |
| `packages/contracts/src/environment.ts` (253) | `crates/orchestration/src/contracts/environment.rs` | — | 未翻訳 |
| `packages/contracts/src/environmentHttp.test.ts` (62) | — | — | 対象外：T3 Connect/ブラウザ・Web 専用/外部サービスの契約。この実装の iroh/ネイティブ境界に該当。 |
| `packages/contracts/src/environmentHttp.ts` (660) | — | — | 対象外：T3 Connect/ブラウザ・Web 専用/外部サービスの契約。この実装の iroh/ネイティブ境界に該当。 |
| `packages/contracts/src/filesystem.test.ts` (33) | `crates/orchestration/src/contracts/filesystem.rs` | — | 未翻訳 |
| `packages/contracts/src/filesystem.ts` (67) | `crates/orchestration/src/contracts/filesystem.rs` | — | 未翻訳 |
| `packages/contracts/src/git.test.ts` (169) | — | — | 対象外：M3 の独立機能・契約。今回保留（会話から使う関連型は該当ファイルで個別に記録）。 |
| `packages/contracts/src/git.ts` (482) | — | — | 対象外：M3 の独立機能・契約。今回保留（会話から使う関連型は該当ファイルで個別に記録）。 |
| `packages/contracts/src/index.ts` (62) | `crates/orchestration/src/contracts/index.rs` | — | 未翻訳 |
| `packages/contracts/src/ipc.test.ts` (38) | `crates/orchestration/src/contracts/ipc.rs` | — | 未翻訳 |
| `packages/contracts/src/ipc.ts` (1391) | `crates/orchestration/src/contracts/ipc.rs` | — | 未翻訳 |
| `packages/contracts/src/keybindings.test.ts` (322) | — | — | 対象外：T3 Connect/ブラウザ・Web 専用/外部サービスの契約。この実装の iroh/ネイティブ境界に該当。 |
| `packages/contracts/src/keybindings.ts` (218) | — | — | 対象外：T3 Connect/ブラウザ・Web 専用/外部サービスの契約。この実装の iroh/ネイティブ境界に該当。 |
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
| `packages/contracts/src/preview.test.ts` (364) | — | — | 対象外：T3 Connect/ブラウザ・Web 専用/外部サービスの契約。この実装の iroh/ネイティブ境界に該当。 |
| `packages/contracts/src/preview.ts` (354) | — | — | 対象外：T3 Connect/ブラウザ・Web 専用/外部サービスの契約。この実装の iroh/ネイティブ境界に該当。 |
| `packages/contracts/src/previewAutomation.ts` (953) | — | — | 対象外：T3 Connect/ブラウザ・Web 専用/外部サービスの契約。この実装の iroh/ネイティブ境界に該当。 |
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
| `packages/contracts/src/relay.test.ts` (61) | — | — | 対象外：T3 Connect/ブラウザ・Web 専用/外部サービスの契約。この実装の iroh/ネイティブ境界に該当。 |
| `packages/contracts/src/relay.ts` (1194) | — | — | 対象外：T3 Connect/ブラウザ・Web 専用/外部サービスの契約。この実装の iroh/ネイティブ境界に該当。 |
| `packages/contracts/src/relayClient.ts` (63) | — | — | 対象外：T3 Connect/ブラウザ・Web 専用/外部サービスの契約。この実装の iroh/ネイティブ境界に該当。 |
| `packages/contracts/src/remoteAccess.ts` (68) | — | — | 対象外：T3 Connect/ブラウザ・Web 専用/外部サービスの契約。この実装の iroh/ネイティブ境界に該当。 |
| `packages/contracts/src/resourceTelemetry.ts` (522) | — | — | 対象外：T3 Connect/ブラウザ・Web 専用/外部サービスの契約。この実装の iroh/ネイティブ境界に該当。 |
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

Provider replay は `agent-providers/src/replay.rs` から翻訳層・状態機械・fold・子 actor への command に流す。単一 session の transcript は、翻訳層が生成した outbound frame を T3 `replay.ts` と同じ正規化で各 `expect_outbound` と順に比較し、余分な frame も失敗にする。固定版の 71 NDJSON は改変せず保存し、manifest の SHA-256 と全 native frame の decoding を検証する。71 本すべてを projection の挙動テストでも使い、下表と追加記録に run / attempt / item / 子スレッド / fork・rollback 境界の原本期待値との対応を示す。hash / decoding の成功だけを挙動一致の根拠にしない。Host の SQLite・実プロセス境界と、core / クライアントの接続 reducer 全体の移植完了を意味するものではない。

| T3 fixture / adapter test | projection・制御の検証 |
|---|---|
| `simple`, `multi_turn`（両 driver） | run 数・status・ordinal・role・応答文・native thread の共有 |
| `queued_turn`, `message_steering`（両 driver） | queued の受付→昇格、timeline の順序、1 run / 1 attempt の steer、元の input intent |
| `proposed_plan`, `todo_list`（Codex） | proposal の active 状態、replay / fixture を含む内容、todo の completed ×3 |
| `tool_call_read_only_on_request`, `tool_call_restricted_granular`（両 driver）, `tool_call_denied_write`（Claude） | output.ts と同じ判定：1 件の要求、accept / decline の決定、承認カードと要求の対応、要求の種類、承認された書き込みの完了と書いた内容、拒否された file change の failed、元の応答文 |
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
| `CheckpointCaptureService.ts` の stopped / missing / error 規則 | `agent-domain::tests::failed_capture_settles_the_run_and_stopped_capture_keeps_its_terminal_status`。missing / error の保存でも run を確定してキューを進め、停止した run の status は変えない。ready でない checkpoint への rollback は拒否する。 |

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

`provider/CodexThreadRevert.test.ts` のページ越し境界と循環 cursor の失敗を、件数を入力する translator の `rollback` テスト（`rollback_finds_the_revert_boundary_across_pages_of_newest_first_turns`、`rollback_rejects_repeated_cursors_instead_of_reverting_incomplete_history`）に移植した。ページの limit 3 → 1、削除境界 `boundary`、エラー文言の期待値を維持する。件数は session が checkpoint の head より後のターンから数える（T3 countTerminalTurnsAfterBoundary）。

`ThreadFork.integration.test.ts` と `ThreadMergeBack.integration.test.ts` の native / prior-turn / continue / sibling / fork-local rollback、および Codex の rollback / after-restart / stopped-turn の transcript を runtime の replay harness（`agent-runtime/src/session/tests/replay.rs`）で厳密に再生する。複数 actor への saga command、固定した fork 履歴、native fork の配送結果、rollback の可視項目、兄弟ごとの delta と source の recall を原本の文言・境界で検証する。fork の内部イベント表の順序は、固定 command の確定→native 成功→子 command の順序へ読み替える。

`provider_thread_resume`、`plan_questions`、`subagent_continue`、`tool_call_read_only`、`tool_call_workspace_never`（両 provider）、`turn_interrupt_restart`、`claude_background_task_after_root`、`claude_compact_after_resume_wake`、`claude_subagent_resume_after_restart` の projection 期待値を replay に追加した。質問 ID `schema_preference`、元の返信、再開した子の会話と 3 command、2 run の compaction 帰属を保持する。休眠・再起動の実プロセス／SQLite 境界は段階 3 で接続する。

`mcp/OrchestratorMcpToolkit.integration.test.ts` の `delegated_task_status/codex` replay を移植した。元の `Delegated API boundary inspected.` と result transfer を保持し、running / queued follow-up がある間の pending 判定、interrupt 後の queued run の完了、`Queued delegated follow-up completed.` と latest transfer の null を確認する。MCP の RPC 接続は段階 3 でこの domain の読み取り結果を利用する。

R3 O6/O8/O9/O13 の再発検証を追加した。control RPC の失敗は run/attempt を Running に保つ（proptest を含む）。子の停止確認前は task/card を Running に保って rollback を拒否し、削除された native 子は親に Cancelled を確定させる。pending rollback 中の provider/interaction/runtime の変更も拒否する。provider の injection 待ちで停止した場合の成功／-32601 fallback の双方で未送信の prompt を抑止する。

`ClaudeAdapterV2.test.ts` の runtime query policy と makeClaudeQueryOptions の thinking/resume/permission override の期待値は `claude_control.rs` の CLI 引数・設定テストで検証する。read-only の 3 tools、global read の allowlist、approval callback、plan の skip-permissions 抑止、300,000 の compaction window を維持する。`claude_subagent_resume_after_restart` replay は query.open ごとに翻訳器を新規作成し、domain の native correlation を復元して子が増えないことを検証する。

`Orchestrator.ts` の provider switch と queued dispatch の coveredRuns / lastDeliveredRunForProviderThread は dispatch 時の handoff 決定に移した。provider を戻した場合は既知の native history を再送せず、その後の run だけを配送する。選択後に戻しただけでは transfer を作らず、queued run の昇格時の selection で差分を固定する。

`CodexAdapterV2.test.ts` の runtime mode 4 種、explicit approval/sandbox、per-turn effort/serviceTier、plan/default collaboration と thread config を wire のテストへ移植した。認証済み MCP config の生成・可用性は Host が所有し、翻訳層には値を明示的に渡す。指示文は `agent-providers/src/instructions.rs` に T3 の文面のまま移植した。新設計の rollback resume でも固定版の cwd/model/tools config を保つ。

`RestartBackgroundNote.test.ts` の provider switch、completion 時刻での順序、同じ label の異なる ID、steer の旧 attempt による配送確認、未受領の chained continuation、ラベル・件数の bounds を `agent-domain/src/recovery.rs` と状態機械テストへ移植した。ID がなかった旧形式の kind + label fallback だけは対象外：未公開製品の現行形式だけを保持する規則に従う。

`RestartContinuation.test.ts` と `ProviderRuntimeRecoveryService.test.ts` の未完了ターン・admitted continuation・held queue・停止／完了／maintenance の除外・新しいユーザー発言の優先・委任結果の復旧を domain の command→projection で検証する。再起動の chain と queue の不変条件は proptest に含めた。startup / shutdown の取消文言を保持し、native 子は旧 attempt の出力を拒否する。`CodexAdapterV2.ts` の継続時の空 input と Claude の通常 prompt を翻訳テストで確認する。native session の強い参照・プロセスの status・設定の取得は段階 3 の Host の入力準備で検証する。

### 段階 3 前のレビュー指摘への対応（2026-10-06）

| T3 の原本 | 新設計の検証 |
| --- | --- |
| `SelectionRestart.integration.test.ts`、`RunExecutionService.test.ts:3192`、`CommandPolicy.test.ts:318` | `restart_supersedes_attempt_and_leaves_native_children_to_their_provider`、`restart_and_steering_respect_capabilities_and_maintenance_turns`。superseded / pending の attempt、子の task は running のまま、停止カードなし。Claude の restart と maintenance の steer を拒否。 |
| `ClaudeAdapterV2.test.ts:2304`（停止後の result） | `a_stopped_claude_turn_finishes_on_its_wrapped_result`。interrupted、partial usage、停止結果カード。 |
| `runtimeLayer.test.ts:3321`（provider / usage limit の失敗後の queue） | `provider_failures_hold_the_queue_and_usage_limits_block_it`。 |
| archive / settle / delete（`runtimeLayer.test.ts:2730, 2795, 2920`、`ThreadDeletion.test.ts`） | `archive_cancels_queued_work_and_detaches_without_unarchive_resuming`、`settle_rejects_blocked_work_and_cancels_automatic_deliveries`、`deletion_cancels_pending_requests_and_releases_thread_resources`、`sending_a_message_clears_settled_and_snoozed_state`。 |
| `SteeringCompletion.integration.test.ts:563, 638` | `dispatch_saves_the_requested_selection_and_late_steers_use_it`。遅れた steer は同じ発言・timeline 行のまま新しい turn に移る。 |
| `CheckpointRollbackService.test.ts`、`runtimeLayer.test.ts` の rollback | `rollback_discards_pending_captures_and_invalidates_later_checkpoints`、`rollback_without_provider_rewind_or_file_restore_still_reports_one_result`、`rollback_resets_post_boundary_sessions_and_uses_replacement_native_identity`。 |
| `ThreadForkService.test.ts:129`、`ThreadFork.execution.test.ts:57`、`ProjectionStore.test.ts`（継承） | `tests::fork::unsuccessful_and_cancelled_runs_fork_from_bounded_portable_history`、`forking_a_fork_keeps_the_ancestor_conversation_and_its_messages`、`a_failed_native_fork_fails_the_run_and_the_next_message_retries_it`。 |
| merge back の受付（`Orchestrator.ts:3442`） | `merge_back_requires_a_fork_of_the_target_and_a_finished_source`。 |
| `ContextHandoffBudget.test.ts:621`、`ProviderSwitch.integration.test.ts:891, 2504`、`ProviderTurnStartService.ts:1032, 1070` | `context_delivery_is_pending_until_acceptance_and_ambiguous_delivery_is_not_repeated`（新しい native thread へ fallback）、`lost_native_session_restarts_the_attempt_with_portable_history`、`a_handoff_that_failed_before_a_native_thread_existed_is_delivered_again`、`inputs_that_never_reached_the_native_session_are_handed_back_to_it`、`native_occupancy_estimate_counts_inputs_and_attachments_that_reached_the_session`、`inline_history_and_restart_notes_precede_the_labelled_user_message`。 |
| `DelegatedCompletionDelivery.test.ts:296, 1211`、`Orchestrator.ts:4570, 7333, 8722` | `always_completions_steer_into_the_running_parent_and_are_delivered_with_it`、`settled_only_completion_waits_only_for_its_spawning_run`、`queued_siblings_share_one_wake_and_cancelling_it_disposes_the_cohort`。 |
| `SubagentProjection.test.ts`、`ClaudeAdapterV2.test.ts:6486` | `delegated_results_use_the_failure_or_latest_answer`、`a_resumed_native_task_reopens_its_card_and_rejects_stale_results`。 |
| queue 編集・要求の応答（`runtimeLayer.test.ts:1114, 3611`、`Orchestrator.ts:6866, 7022, 7228, 7402`）、`ProviderEventIngestor.test.ts:735` | `queued_edits_are_validated_and_automatic_deliveries_are_fixed`、`declined_requests_and_dismissed_questions_close_their_cards_as_cancelled`、`message_capable_questions_stay_answerable_after_their_turn_ends`。 |
| proposed plan の消費（`Orchestrator.control-reads.test.ts:283`）、委任の入力（contracts `:2878`、`SubagentProjection.ts:28`） | `a_proposed_plan_is_consumed_once_at_acceptance`、`delegation_requires_a_task_and_titles_the_child_from_it`。 |
| `ThreadTitleRegenerationService.test.ts`、`ThreadLaunchService.test.ts`（title） | `titles_are_generated_once_and_a_rename_supersedes_the_request`。 |
| `AgentSessionImporter.test.ts` | `imported_sessions_keep_message_times_and_resume_their_native_session`。 |
| `ProjectionStore.ts` の `threadShellFromProjection`、`ProjectionStore.test.ts` の shell | `agent-domain/src/shell.rs` のテスト。 |
| `ProviderFailure.ts` の上限 | `large_text_is_split_across_facts_without_truncation`、`failure::tests`。 |
| `CodexAdapterV2.test.ts:3805, 4004, 2242, 6670, 5719, 2837, 3348–3687, 6455, 1321, 806–936` | `commands_running_at_turn_end_report_later_and_stop_without_a_turn_interrupt`、`a_retained_command_keeps_its_row_and_wakes_the_thread_when_it_finishes`、`asynchronous_codex_questions_become_message_requests_without_prose`、`codex_item_and_subagent_states_use_the_reference_mapping`、`collaboration_calls_do_not_reparent_existing_children`、`codex_failures_and_retries_keep_their_reference_classification_and_lifecycle`、`final_answers_drop_repeats_and_late_empty_completions`、`rerouted_child_models_update_the_child`、`skills::tests`、`codex_tools::tests`、`codex_start_and_steer_send_prepared_images_after_the_text`。 |
| `ClaudeAdapterV2.test.ts:3326, 3130, 3204, 4328, 2376, 2441, 2691`、`ClaudeSkillDispatch.test.ts`、`claudeModelOptions.test.ts`、`model.test.ts:227` | `claude_rosters_replace_background_work_and_foreground_tasks_stay_foreground`、`claude_server_tools_and_typed_results_are_tool_activity`、`claude_bash_output_joins_stdout_and_stderr`、`claude_api_retries_update_one_item_until_recovery_or_failure`、`claude_success_results_marked_as_errors_add_no_answer_or_failure`、`claude_refusal_fallbacks_and_mcp_names_use_the_reference_fields`、`claude_rate_limits_announce_rejected_windows_unless_overage_is_allowed`、`claude_prompts_run_known_skills_and_request_ultrathink_effort`、`claude_models::tests`、`background_rosters_replace_work_and_usage_limits_render_their_wait`。 |
| SDK `forkSession` | `claude_fork::tests`。境界までの main chain、progress を飛ばした親子関係、新 session ID への付け替え、fork title、境界が見つからない場合のエラー、project key。 |
| wire の encoding | `wire_encodings_round_trip_state_facts_commands_and_effects`、`wire_encodings_round_trip_imports_titles_rollbacks_and_workspaces`（JSON と Postcard）。 |

graph replay（fork / rollback / merge back / delegated_task_status）も、単一 session の transcript と同じく外部 frame の完全一致で比較する（下の「provider session の T3 化」）。

### 段階 3: effect の実行・復旧・launch（2026-10-06）

`agent-runtime` の executor、`Runtime`、`launch` を偽の `HostOperations`（`executor::tests::FakeOps`）と偽の provider プロセスで検証する。期待値は T3 のまま、harness だけを置き換えた。

| T3 の原本 | 新設計の検証 |
| --- | --- |
| `CheckpointService.test.ts`（materializes baseline） | `executor::tests::checkpoint::materializes_a_baseline_as_missing_when_its_ref_lookup_fails`（lookup 成功／失敗）。 |
| `CheckpointCaptureService.test.ts` | `captures_a_finished_turn_after_its_thread_start_baseline`、`a_stopped_run_records_its_checkpoint_and_keeps_its_status`、`does_not_capture_a_stopped_run_that_a_rollback_already_discarded`、`a_workspace_outside_git_records_missing_checkpoints_and_finishes_the_run`、`a_failed_capture_records_an_error_checkpoint_and_finishes_the_run`。 |
| `CheckpointScopeOwnership.test.ts` | `a_later_run_reuses_the_scope_baselines`。 |
| `RunFinalizationService.test.ts`（保存後の refresh） | `captures_a_finished_turn_after_its_thread_start_baseline` の `run_finalized` 通知。 |
| `CheckpointRollbackService.test.ts` | `executor::tests::rollback::rejects_a_non_ready_checkpoint_before_any_provider_or_file_work`、`rewinds_safely`（7 ケース）、`a_provider_that_cannot_rewind_fails_the_rollback_and_puts_the_files_back`、symlink の archived thread は `preserves_overlapping_workspace_files` の aliased worktree。追加で `a_rollback_without_a_native_session_resets_nothing_and_still_finishes`。 |
| `CheckpointRestoreSafety.test.ts` | `preserves_overlapping_workspace_files`（nested、ancestor、archived-nested、aliased-nested、project、provider、scope、sibling、stopped-provider、errored-provider、shared-provider、conversation）。実ファイルと実パスで確認する。 |
| `ThreadDeletion.test.ts` | domain の `deletion_cancels_pending_requests_and_releases_thread_resources` と、`executor::tests::cleanup::deletion_cleans_up_terminals_and_attachments_of_the_thread`、`only_the_threads_own_terminals_close_when_another_thread_shares_its_directory`、`archiving_detaches_and_cleans_up_terminals_but_keeps_attachments`。 |
| `ThreadTitleRegenerationService.test.ts` | `title::tests`（formatThreadTitleContext の 4 件）と `executor::tests::title::*`（arm と解除、新しい要求による無効化、会話の要約からの再生成、fallback・同じタイトル・失敗、最初の発言がない場合、最初のタイトルの再試行 success / exhausted / stale）。 |
| `textGeneration/ThreadTitleContext.test.ts`、`TextGenerationPrompts.test.ts`（title と sanitize）、`TextGeneration.test.ts`（リンクの文脈）、`ThreadTitleLinks.test.ts`（純粋な部分）、各 provider の sanitize | `title::tests`。 |
| `ThreadLaunchService.test.ts` | `launch::tests::returns_a_visible_preparing_message_while_provisioning_is_still_blocked`（setup 前に provider を始めない確認を含む）、`provisions_independent_launches_concurrently`、`queues_follow_up_messages_behind_preparation_in_the_final_workspace`、`a_preparation_failure_keeps_the_thread_and_message_visible`（worktree / setup）、`retries_a_failed_workspace_preparation_on_the_same_run`（fetch の診断文を含む）、`a_retry_reuses_a_recorded_worktree`、`replays_a_server_allocated_launch`、`rejects_a_launch_replay_with_a_mismatching_thread_id`、`rejects_a_launch_replay_from_another_project`、`rejects_a_launch_replay_after_the_thread_is_deleted`、`does_not_treat_an_unrelated_accepted_command_receipt_as_a_launch`、`concurrent_launches_of_one_command_share_one_thread_and_one_preparation`（server 割当てと再送の重複排除）、`schedules_an_accepted_preparing_message_exactly_once`、`arms_durable_title_generation_after_accepting_the_first_message`、`generates_an_initial_title_for_an_attachment_only_message`。 |
| `ProviderRuntimeRecoveryService.test.ts:29, 67, 126, 183, 581` | `runtime::tests::startup_recovers_unfinished_threads_before_any_client_command`（再起動した Host で、復旧が必要な thread だけを読み、command より先に要求を expired にし、async question と queue の実行状態を保ち、process-bound effect を取り消す）。 |
| `ProviderRuntimeRecoveryService.test.ts:232`、`RestartContinuation.test.ts`（停止と再起動） | `shutdown_cancels_live_work_and_the_cut_turn_continues_after_restart`、`a_turn_that_finished_before_shutdown_is_neither_cancelled_nor_resumed`、`executor::tests::send::a_continuation_follows_the_setting_when_it_runs`。 |
| `ProviderRuntimeRecoveryService.test.ts:1276` | domain の `a_delegated_child_reports_recovery_cancellation_or_its_continuation_result` と `recovered_native_children_reject_old_output_and_remain_provider_owned`。 |
| `EffectWorker.test.ts:162`（turn がない interrupt） | `session::tests::a_stop_without_a_live_session_closes_the_attempt`。 |
| `EffectWorker.test.ts:761` | `executor::tests::send::settles_a_delegated_child_once_its_restart_continuation_fails_for_good`。継続の設定の置換は `delivers_with_the_effect_command_id_and_the_current_continuation_setting`。 |
| `CodexAdapterV2` の resume 拒否・compact の resume、`ProviderSessionManager` の応答照合 | `agent-providers` と `session::tests` の追加テスト（rejected resume / start の operation、新しいプロセスでの compact、別の request の待ちを解決しない拒否）。 |

対象外にしたもの:

- `CheckpointService.test.ts` の interrupt、`ThreadTitleRegenerationService.test.ts` の interrupted: Effect fiber の割込みを確かめる。本設計では実行中の effect は未確定のまま残り、再起動で再実行される。
- `CheckpointCaptureService.test.ts` の delegatedCompletion の上書きと履歴を読まない確認: T3 の projection store の内部形を確かめる。保存結果は actor の状態から決める。
- `ThreadLaunchService.test.ts` の branch 名の生成と rename（M3）、`reuseExistingThread`、自動化・送信元の属性、import した native session（`Command::Import` で取り込む）、attachment の取り込み（RPC の責務）、記録前に失敗した worktree の削除（部分的な checkout の削除は Host の `create_worktree` が行う。runtime は記録に失敗した worktree を削除する）。
- `ProviderRuntimeRecoveryService.test.ts:522`（取り消すしかない waiting run）: 保存の失敗は結果として run を確定するので、保存を待ったまま残る run は生じない。
- `EffectWorker.test.ts:728`（置換 session の restart）: restart は domain が attempt を superseded にし、Start は独立した effect になるため複合 effect がない。
- `RunFinalizationService.test.ts` の pull request 状態の更新 4 件: この Host は pull request の状態を持たない。保存後の通知（`run_finalized`）だけを受け取る。

### T3 の外部挙動への揃え直し（2026-10-06）

| T3 の原本 | 新設計の検証 |
| --- | --- |
| `ProviderFailure.test.ts`（redaction、制御文字、上限、surrogate） | `agent-domain` の `failure::tests`。 |
| `QueuedRunOrder.test.ts`、`runtimeLayer.test.ts:3203`（wake の位置と並べ替え）、`Orchestrator.ts:7217` | `tests::queue::background_wakes_keep_their_queue_position_and_can_be_passed`、`reordering_follows_the_reference_rules`。 |
| `Orchestrator.ts:7308, 2014`（配送の cancel と cohort）、`:8000`（Stop と cohort） | `cancelling_a_completion_delivery_disposes_running_siblings`、`stopping_the_parent_leaves_delegated_children_running`。 |
| `Orchestrator.ts:7077, 3585`（promote-to-steer） | `promoting_to_steer_keeps_maintenance_separate`。 |
| `runtimeLayer.test.ts:3767`（開始前の interrupt）、`RunExecutionService.ts` の結果文言 | `tests::preparation::interrupts_a_pending_provider_start`、`a_confirmed_stop_records_the_default_request_and_result_messages`。provider の interrupt effect は進行中の Start を取り消す本設計の仕組みなので、「effect がない」の確認だけは持ち込まない。 |
| `Orchestrator.ts` の defer_start と prepared-run.*、`ThreadLaunchService.test.ts:375, 1302, 1468` | `tests::preparation::*` と `launch::tests` の準備行の確認（running、Preparing worktree、Starting setup script、failed、retry 後の completed と cancelled）。 |
| `Orchestrator.ts:2311`（thread mutation の guard）、`ThreadDeletion.ts`、`runtimeLayer.test.ts:3981`（mark unread） | `tests::thread::*`、`unread_uses_the_last_run_and_needs_its_completion`。 |
| `ProviderSessionTransitionPolicy.ts`、`ProviderSwitchService.ts`、`Orchestrator.ts:3301` | `tests::selection::*`、session の `switching_provider_releases_only_that_threads_previous_session`、`claude_respawns_when_launch_flags_change_and_ignores_the_old_process`（runtime mode の変更で切り離す）。 |
| `CheckpointRollbackService.ts:145`、`Orchestrator.ts:8393` | `tests::rollback::*`、`executor::tests::rollback::a_failed_rewind_resets_only_the_active_native_session`。 |
| `Orchestrator.ts:3354, 5361`、`ProviderTurnStartService.ts:605`、`ProjectionStore.ts:1213`、`ThreadForkService.ts` | `tests::fork::*`、`session::tests` の fork 3 件（子の最初の発言で fork する）、graph replay。 |
| `Orchestrator.ts:3442, 4694`（merge back） | `merge_back_waits_for_a_direct_turn_on_any_provider`。 |
| `ProviderTurnStartService.ts:240`、`Orchestrator.ts:8990`（委任結果の handoff） | `delegated_results_reach_a_later_turn_only_after_the_spawning_run_failed`。 |
| `Orchestrator.ts:6318`、`SubagentProjection.ts`、`OrchestratorMcpService.ts:1368` | `tests::delegation::*`。 |
| `runtimeLayer.test.ts:4100, 4282`（手動と自動の継続） | `tests::recovery::*`。`invalid-snooze`（不正な日付）は型で表せず、`replacement` は失敗項目の class を直接書き換える操作が domain の入力にないため対象外。 |
| `ThreadSettlementService.test.ts`（候補、queued turn start、非活動） | `settlement::tests`。pull request の状態による判定は Host が pull request を持たないため対象外。 |
| `runtimeLayer.test.ts:1773`（auto-settle）、`Orchestrator.ts:2361`（metadata.update）、`:4606`（sourcePlanRef） | `tests::metadata::*`。 |
| `codexUsageLimits.test.ts`（merge と reset）、`CodexAdapterV2.ts` の account/rateLimits/updated | `agent-providers` の `codex::rate_limit_tests`、`session::tests::shared_rate_limits_reach_every_thread_and_fill_a_stopped_turns_reset`。 |
| contracts `chatAttachment.ts` | `attachments_follow_the_reference_schemas_and_image_budget`。添付 ID の文字種は Host の `chat:` 形式と合わないため確かめない。 |

### 段階 3: Host への接続（2026-10-06）

`host-daemon` の `conversation` が `HostOperations` と `SessionHost` を実装し、会話の RPC をすべて新しい runtime で答える。旧 `orchestration` の store・effect worker・`provider-adapters`・`host_rpc/import.rs`・`agent_tools.rs` は Host から削除した。Host の結合テストは一時ディレクトリの実 SQLite と、固定版の Codex transcript（`simple`）を再生する偽のプロセスで RPC を通す。

| T3 の原本 | 新設計の検証 |
| --- | --- |
| `checkpointing/CheckpointStore.test.ts`（全 9 件） | `checkpoints::tests` の `detects_git_repositories_including_nested_workspaces`、`returns_full_oversized_checkpoint_diffs_without_truncation`、`keeps_patch_prefixes_when_the_repository_disables_them`、`can_hide_indentation_churn_when_changes_wrap_existing_lines`、`counts_changes_whose_full_patch_exceeds_the_output_limit`、`preserves_file_paths_and_turn_ranges_without_changing_the_user_index`、`uses_head_for_a_missing_baseline_only_when_requested`。ref は T3 v2 の `checkpointRefForScopeOrdinal` と同じ形にする。 |
| `checkpointing/Diffs.test.ts`（全 5 件） | `numstat_summaries_follow_the_reference_parser`。Host の既存の `parse_numstat` を T3 と同じ並び順で比較する。 |
| `checkpointing/CheckpointDiffQuery.test.ts`（全 5 件） | `conversation::diff::tests`（scope の baseline からの diff、範囲外、rollback 済み run の除外、baseline ref の欠落）と、`conversation::tests::conversation_calls_answer_with_typed_errors` の存在しない thread（`ThreadNotFound`）。 |
| `ThreadStream.test.ts`・`ShellStream.test.ts` の RPC 境界、ws.ts の購読 | `a_launched_thread_streams_its_turn_and_reads_back_through_every_query`、`a_stream_resumed_at_its_head_answers_without_replaying`、`the_shell_stream_lists_projects_and_threads_then_follows_changes`。 |
| `ThreadLaunchService.test.ts` の再送（RPC 経由） | 同じ launch の再送が `resumed` になり、別 thread への再送は `CommandIdConflict` になる。 |
| `ProviderRuntimeRecoveryService.test.ts:232`（Host の停止と起動） | `a_turn_cut_by_shutdown_is_settled_when_the_host_starts_again`。 |
| `mcp/OrchestratorMcpToolkit` の読み取りと委任の前提 | `agent_tools_read_a_thread_of_their_own_project`、`conversation::tools::tests`。 |

従来の Host のテストのうち旧 store を前提にしたもの（旧 service の subscription・launch・restore・terminal cleanup）は、同じ挙動を runtime の executor テストと上の結合テストで確かめるため削除した。

### provider session の T3 化（2026-10-06）

Codex の app-server を instance ごとに共有し、起動設定・アカウント・Claude のプロセス再利用を T3 に合わせた。期待値は T3 のまま。

| T3 の原本 | 新設計の検証 |
| --- | --- |
| `Orchestrator.ts` providerSessionIdFor、`IdAllocator.ts` の共有 session、`CodexAdapterV2.ts`（複数 thread） | translator の `one_app_server_routes_each_native_thread_to_its_own_route`、`a_shared_translator_loads_threads_per_route`、session の `codex_threads_share_one_app_server_and_keep_their_own_output`。 |
| `ProviderSessionManager.test.ts` の busy / idle / pin と detach | `a_shared_app_server_stays_while_any_thread_runs`、`detaching_from_the_shared_app_server_interrupts_and_unloads_only_that_thread`、`a_terminal_detach_revokes_credentials_without_a_live_process`。既存の「独立した session」「instance の session を閉じる」「idle の解放」は instance を分けるか共有 session のまま期待値を確認する。 |
| `CodexAdapterV2.ts` resolveRuntime（管理アカウント）と token 更新 | `a_new_app_server_signs_in_with_the_managed_account_and_refreshes_its_token`。 |
| `ThreadFork.integration.test.ts`、`ThreadMergeBack.integration.test.ts`、`CheckpointRollbackService` の replay、`mcp/OrchestratorMcpToolkit.integration.test.ts`（delegated_task_status） | graph transcript 18 本を runtime の harness で外部 frame の完全一致まで比較する（`native_fork_replays_…`、`rollback_replays_…`、`merge_back_replays_…`、`delegated_task_status_replay_…`）。注入した履歴の検証は T3 の `CodexHistoryReplayHarness` と同じ。 |
| `provider/RuntimeInstructions.test.ts`、`provider/CodexDeveloperInstructions.test.ts` | `instructions::tests`。MCP サーバー名（`t3-code` を `orchestration` / `browser`）だけを読み替える。 |
| `ClaudeAdapterV2.ts` makeClaudeQueryOptions・claudeMcpQueryOverrides | `a_claude_launch_pre_approves_the_app_tools_and_appends_the_instructions`。 |
| `provider/Drivers/ClaudeSkills.test.ts`（discovery、優先順位、malformed、colon を含む説明、YAML 1.1 の boolean、skillOverrides、repository root の設定） | `skills::tests` と `claude::skills::tests`。frontmatter は T3 の YAML parser ではなく、Claude Code が使う key だけを読む。 |
| `ClaudeAdapterV2.ts` openQuery（再利用と置き換え、background work による拒否）、forkThread | `claude_replaces_its_process_for_another_model_or_mode`、`a_claude_model_change_during_a_turn_applies_from_the_next_turn`、`claude_keeps_a_process_with_background_work_when_the_selection_changes`、`a_claude_native_fork_copies_the_transcript_through_the_head`（source のプロセスを閉じる）。 |
| `ContextHandoffBudget.ts` handoffTokenCapConfig、`ClaudeAdapterV2.ts` getModelContextWindow | `the_provider_catalog_has_the_reference_handoff_limits`。 |

### Host の T3 化（2026-10-06）

配信形、同期・RPC の細部、chats のフォルダ、worktree、setup、checkpoint の一覧、rollback の受付、Host の sweep と設定を T3 に合わせた。期待値は T3 のまま。

| T3 の原本 | 新設計の検証 |
| --- | --- |
| `WireProjection.test.ts`（serializes once と raw output を serialize しない 2 件を除く全件） | `sync::wire::tests` の `redacts_tool_output_while_keeping_its_input`、`preserves_provider_notices_in_bounded_items_and_live_events`、`omits_oversized_dynamic_tool_results_without_mutating_persistence_data`、`omits_even_small_dynamic_tool_results_while_retaining_input`、`keeps_undefined_dynamic_input_intact`、`preserves_bounded_input_summary_normalization`（6 ケース）、`uses_encoded_json_bytes_for_strings_near_the_dynamic_value_limit`、`summarizes_an_oversized_structured_input`、`truncates_detail_at_a_utf8_boundary_without_changing_the_source`、`omits_command_output_of_every_size`、`bounds_fetched_command_input_without_changing_persistence`、`keeps_failure_evidence_without_retaining_command_output`、`omits_inline_file_bodies_but_preserves_file_identity`、`retains_only_result_identities_and_failure_metadata_in_live_tool_events`。handoff の件は `sync::thread::tests::keeps_copied_handoff_transcripts_out_of_activity_items_and_live_events`。T3 の件のうち JavaScript の `toJSON` 呼出し回数を数える 2 件は Rust に対応する経路がない。 |
| `shared/toolOutput.ts`（compactDynamicToolOutput、toolOutputIndicatesFailure） | `compacts_mcp_envelopes_like_t3`、`recognizes_failure_text_like_t3`。 |
| `ThreadStream.test.ts`（10 MiB の raw replay） | `rejects_a_10_mib_raw_replay_even_when_its_projected_form_fits_the_wire_budget`。T3 と同じく dynamic tool の 10 MiB の出力で確かめる（以前は配信形がなかったため transcript に置き換えていた）。 |
| `ThreadManagementService.ts` getTurnItem（detail read） | `query::tests::reads_withheld_command_output_on_demand_within_its_bound`。 |
| 配信形の事実の fold | `command_output_facts_fold_into_the_projected_item`、proptest `bounded_snapshot_then_later_facts_matches_the_full_fold_within_the_window`（配信形の snapshot と事実で、dynamic tool を含む）。 |
| frame の上限（T3 にない） | `stops_a_turn_window_at_the_frame_budget_so_every_thread_opens`、`shortens_a_single_finished_row_that_alone_exceeds_the_frame_budget`。 |
| `LiveStreamBudget.ts`（LiveStreamBufferError） | `sync::live` のテスト、`closes_a_subscriber_that_falls_behind`、`closes_a_subscriber_that_falls_behind_the_hub`、`closes_a_slow_subscriber_instead_of_waiting_for_it`（`overflowed` の確認）。 |
| `shellReducer.ts:103`（snapshot 以下の sequence を捨てる） | `reports_project_changes_from_the_directory`、`orders_project_changes_between_thread_commits`、`resumes_with_project_changes_made_while_disconnected`。 |
| `ProjectionStore.ts:4978`（shell の行の順） | `lists_snapshot_rows_by_update_time_then_thread_id`。 |
| `ThreadSearch.ts`（UTF-16 の長さと snippet）、contracts `threadSearch.ts` | `validates_the_query_and_limit`（UTF-16 の長さ）、`bounds_snippets_in_utf16_units`。 |
| `CheckpointDiffQuery.ts`（ignoreWhitespace の既定）、`GitVcsDriver.ts:1205` | `ErrorCode` の往復（`diff_failed`）。省略時の `ignore_whitespace` を true にする部分に専用のテストはない。 |
| `composerContextReferences.test.ts`（href、label、projection の全件）、contracts `composerContext.ts`（record の schema） | `composer::tests`、`tests::a_message_reaches_the_provider_with_its_context_projected`。 |
| `RepositoryIdentityResolver.test.ts`（解決の 5 件。cache の件は対象外） | `repository::tests`、`shell_projects_carry_their_repository_identity`。cache・TTL・refine は Host が project を読み直すときに解決するため持たない。 |
| `ThreadSettlementService.test.ts`（worker の非活動 2 件） | `sweep::tests::settles_only_the_project_opted_in_while_environment_settlement_is_disabled`、`dispatches_the_last_activity_time_with_the_snapshot_guard`、`settles_after_the_default_three_days_of_inactivity`、`sweeps_read_live_unarchived_rows`。pull request と terminal の件は Host が pull request と thread の terminal を持たないため対象外。 |
| `UsageLimitRecoveryWorker.ts` | `recovers_usage_limits_only_when_the_settings_opt_in`。判定は既存の `settlement::tests`。 |
| contracts `settings.ts`（既定値と project の上書き）、`projectSettings` の解決 | `conversation_settings_persist_with_t3_defaults_and_bounds`、`project_overrides_take_precedence_and_absent_values_inherit`。 |
| `ThreadTitleLinks.test.ts`（全 3 件）、GitHub / GitLab の `resolveLink` | `title_links::tests`。 |
| `WorktreeSetupTracker.test.ts`（全 8 件） | `setup::tests`。stream は watch channel の最新値で確かめる。 |
| `ThreadLaunchService.ts` の setup card と取消 | `tracks_a_worktree_setup_on_its_card_until_the_turn_starts`、`an_awaited_setup_that_fails_fails_the_run_and_an_asynchronous_one_does_not`、`cancelling_a_setup_removes_its_worktree_and_fails_the_run`、Host の `a_worktree_launch_runs_the_projects_setup_script`（card の配信と取消）。 |
| `ProjectSetupScriptRunner.test.ts`、`ProjectSetupScriptRunner.ts` の出力行と completion sentinel | `conversation::setup::tests` の `setup_runs_in_the_threads_terminal_and_reports_its_exit_code`、`observed_output_splits_redraws_and_ends_with_the_terminal`、`the_sentinel_settles_and_the_wrapper_echo_is_hidden`、`wrapped_commands_report_their_exit_code_after_the_block`、`setup_terminals_get_the_project_paths_without_color`、`the_first_script_run_on_worktree_creation_is_the_setup`、`output_lines_drop_terminal_control_and_stay_bounded`。 |
| `ManagedProjectFolders.test.ts`（folderForThread、claimFreeFolder、Scratch の提供条件） | `gives_each_chat_thread_its_own_folder_named_from_its_message`、`claims_a_folder_after_the_chats_folder_is_deleted`、`offers_chats_under_the_data_directory_outside_a_checkout`、`offers_no_chats_when_the_data_directory_sits_inside_a_checkout`、`launch::tests::runs_a_chat_thread_launched_at_the_root_in_its_own_folder`、Host の `a_chat_launched_at_the_root_runs_in_a_folder_of_its_own`。named project・Scratch project の作成と icon は対象外。 |
| `ThreadLaunchService.ts:331-364`、`GitVcsDriverCore.test.ts`（fetch の診断） | `worktrees::tests::starting_from_origin_falls_back_to_the_local_base`、`a_thread_keeps_its_checkout_when_its_creation_is_retried`、`fetch_failures_report_a_fixed_diagnosis`、`a_failed_origin_fetch_reports_its_diagnosis_and_checks_out_nothing`。 |
| `CheckpointService.ts:376-483`（capture の files）、`checkpointing/Diffs.test.ts` | `executor::tests::checkpoint::a_captured_turn_records_the_files_changed_since_the_previous_checkpoint`、`a_captured_checkpoint_records_its_file_summary_and_baselines_record_none`、`checkpoints::tests::checkpoint_files_summarize_the_changes_between_two_refs`、`numstat_summaries_follow_the_reference_parser`（並び順まで比較）、`numstat_summaries_sort_like_locale_compare`。 |
| `Orchestrator.ts:8477-8498`、`EffectWorker.ts:362-392`、`CheckpointRestoreSafety.test.ts` | `tests::rollback::a_restore_the_host_refused_is_rejected_at_admission`、`the_host_refusal_is_not_part_of_the_rollback_identity`、`executor::tests::rollback::a_workspace_shared_after_admission_fails_the_rollback_after_its_retries`、`rewinds_safely`・`preserves_overlapping_workspace_files`（受付での拒否と effect 時の ROLLBACK_FAILED_MESSAGE）。 |
| command の再送（`Orchestrator.ts:203-226`） | `tests::metadata::a_resent_update_without_the_host_filled_root_replays_its_result`。 |

対象外のまま残したもの: branch 名の生成と rename（M3）、`reuseExistingThread`、pull request の同期と merge による settle、環境の既定 scripts、repository identity の定期的な再解決と web URL。

### MCP toolkit の T3 化（2026-10-06）

`mcp/toolkits/orchestrator`・`thread`・`project` と `McpHttpServer.ts` の登録を `host-daemon/src/conversation/tools` に移植した。期待値は T3 のまま。T3 のテストが mock した ThreadManagementService・ProviderRegistry は `Orchestration` trait の fake に置き換え、mock の projection は同じ意味の domain の state で作る。

| T3 の原本 | 新設計の検証 |
| --- | --- |
| `OrchestratorMcpService.test.ts`（ack の再試行、restart で切れた子、task_cancel 3 件） | `retries_terminal_acknowledgement_with_a_fresh_command_id`、`reports_a_restart_cut_child_as_working_until_its_continuation_settles`、`does_not_dispose_delivery_when_a_nonterminal_task_has_no_active_child_run`、`does_not_dispose_delivery_when_the_child_interrupt_fails`、`returns_cancel_requested_when_post_interrupt_disposal_fails`。restart で切れた子は、T3 の delegatedTaskResultPending の代わりに親の task に結果がまだ公開されていないことで判断する（domain は継続が決まるまで結果を送らない）。 |
| `OrchestratorMcpService.test.ts`「provider resolution」5 件 | `advertises_orchestration_capability_from_registered_adapters`、`delegates_to_an_instance_whose_adapter_resolves_and_by_driver_kind`、`rejects_delegation_to_a_provider_without_a_registered_adapter`、`inherits_an_available_parent_instance_for_driver_targets_and_otherwise_selects_a_healthy_peer`。委任先の Antigravity は、この実装の driver（Codex / Claude）の別 instance に置き換えた。 |
| `OrchestratorMcpService.activity.test.ts` 4 件 | `read_thread_prefers_the_activity_run_status_over_a_newer_cancelled_queued_run`（running と waiting）、`task_status_returns_the_tasks_provider_instance_rather_than_the_driver_kind`、`read_thread_reaches_a_thread_the_user_attached_as_context_but_not_one_an_agent_attached`。 |
| `ThreadMetadataMcpService.test.ts`、`threadMetadataMcp.test.ts` | `thread_update_reports_an_absent_or_unreadable_calling_thread`、`thread_update_validates_each_action_and_links_pull_requests`。 |
| `toolkits/orchestrator/tools.test.ts`（schedule 以外）、`toolkits/core.test.ts`（名前・object root・`$ref` なし、storage の失敗を出さない） | `orchestrator_tool_guidance_matches_t3`、`publishes_unique_tool_names_with_reference_free_object_root_inputs`、`returns_a_bounded_public_failure_without_serializing_storage_causes`。credential の capability は Host に 1 種類しかないため capability_denied の検査は持たない。 |
| `toolkits/project/handlers.test.ts`、`project/handlers.ts` の create（scripts） | `launches_threads_from_a_full_access_caller_and_scratch_threads_into_chats`、`creates_projects_from_a_path_and_rejects_fields_it_cannot_apply`（scripts の保存と結果、既定の model の拒否）。scratch は Chats project の root に launch し、thread ごとのフォルダで動く。 |
| `ClaudeAdapterV2.test.ts` の read-only 一覧と toolkit の照合、claudeMcpQueryOverrides | `a_read_only_claude_sandbox_pre_approves_every_read_only_tool`、`a_read_only_claude_sandbox_pre_approves_only_read_only_app_tools`、`a_claude_launch_pre_approves_the_app_tools_and_appends_the_instructions`。 |
| `provider/T3OrchestrationInstructions.test.ts`（provider 共通の 2 件） | `orchestration_instructions_distinguish_subagents_and_structured_schedules`。 |
| `OrchestratorMcpToolkit.integration.test.ts` の organize・委任・cancel・create_threads・send・queue・wait・interrupt の流れ | `agent_tools_delegate_create_queue_and_interrupt_through_the_runtime`（runtime と、`[hold]` のターンを止める Codex の fake）。schedule の手順は scheduler がないため未移植。 |
| ThreadManagementService の waitForThread / interruptThread、readThread の既定値 | `wait_selects_the_latest_run_clamps_its_budget_and_reports_timeouts`、`interrupt_picks_the_newest_interruptible_run_and_reports_t3_statuses`、`read_defaults_to_fifty_messages_of_twenty_thousand_units_and_pages_long_text`、`archived_callers_read_but_cannot_change_threads`、`list_orders_newest_first_and_filters_status_settlement_title_and_subagents`、`queue_tools_page_read_and_change_queued_messages`、`pending_request_tools_answer_only_open_user_questions`、`organize_maps_each_action_and_requires_snooze_time`、`search_returns_only_matches_of_the_calling_project`、`send_maps_t3_modes_and_rejects_escalation`、`transfers_and_configuration_read_the_addressed_thread`。 |
| `SelectionRestart.integration.test.ts`「detaches the old provider session after an active provider handoff」と dispatchSteerIntoRun の instance 変更 | `detaches_the_old_provider_session_after_an_active_provider_handoff`、`a_steer_onto_another_instance_restarts_the_run_with_a_full_handoff`、`a_claude_run_cannot_be_steered_onto_another_instance`、`promoting_to_steer_after_a_provider_switch_restarts_on_the_new_instance`。 |

公開しない T3 の tool（裏付けの機能がない）：`schedule_task`・`list_scheduled_tasks`・`update_scheduled_task`・`delete_scheduled_task`・`run_scheduled_task_now`（scheduler）、`t3_project_update`（Host が変えられる project の設定は scripts だけで、title・workspaceRoot・既定の model などを持たない）、`t3_project_delete`・`t3_project_clone`（project の削除・clone）、`t3_worktree_*`、`t3_attachment_*`、`t3_thread_send_attachments`、`t3_environment_*`、`t3_preview_*`・`preview_*`、`device_*`、pull request の tool。`orchestrator_capabilities` の `scheduledTasks` は false にする。

### 段階 1・2 の検証記録（2026-10-06）

実装・テストの最終 revision は `4a52416ab649fa888b80322f7a2884715d270beb`。以後のコミットは検証記録のみ。新しい domain / provider 層の実装と上記の挙動検証を終え、push と PR 更新後にレビューを待つ。

- `cargo test -p agent-domain -p agent-providers`: **128 件通過**（各 64 件）。状態機械の proptest と、固定版 71 transcript の projection replay を含む。
- `NEXTEST_TEST_THREADS=4 scripts/dev-env.sh just unit-tests`: **533 件通過、既存 5 件 skip**。standalone agent-peer の 5 テスト群も通過。
- `scripts/dev-env.sh cargo clippy --workspace --all-targets --features agent-core/bindings -- -D warnings` と standalone agent-peer の all-targets clippy: 通過。
- workspace と standalone agent-peer の `cargo fmt -- --check`、`git diff --check`: 通過。
- `python3 scripts/t3-port/inventory.py --check`: **960 ファイル**の固定 source hash 一致。新しい 2 crate と書き直した文書・Host 音声テスト・transport のコメントに製品名がないことを確認。

途中の全体実行では既存 adapter・transport・Host の起動／通信のタイムアウトが発生した。単独検証を行い、音声 fixture が無関係な loopback 接続を provider の要求として数える問題を修正した。fixture に discovery の GET を入れて再現し、対象 route と TLS の接続だけで既存の期待値を検証する。タイムアウト値は変更せず、最終の全体実行は同時実行数 4 で通過した。

段階 3 の actor / SQLite / outbox / provider session / 同期 / 履歴取り込み、段階 4 の core と 3 クライアントの接続、段階 5 の旧 crate 削除は未実施。稼働 Host・他 worktree・main に操作を加えていない。CI 待ち、cargo-mutants、E2E、Simulator の UI テスト、実 provider の受入検証は実施していない。

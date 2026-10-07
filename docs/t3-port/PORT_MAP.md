# T3 の翻訳対応表

参照を `4ee6bfd50ef4a089440d5c3662db2298da9cc50e` に固定する。旧実装に対する M1/M2 の完了記録は、関数単位の翻訳完了・T3 のテスト通過を意味しない。この表で照合を終えた範囲だけを翻訳済みとする。

新設計では段階 1 の `agent-domain` と段階 2 の `agent-providers` を先に実装・検証し、push と PR 更新後にレビューを待つ。Host の永続化・履歴取り込みは段階 3、core と3クライアントは段階 4、旧 crate の削除は段階 5 とする。main のマージ、実 Host の起動・再起動、他 worktree の変更は行わない。

生成されたファイル対応表は、固定版の全ファイルと対象外の理由を保持する。旧方針の翻訳先（`crates/orchestration`、`crates/provider-adapters`）は段階 5 で crate とともに削除した。実装・テストの対応は末尾の「新設計の挙動テスト対応」以降を正本とする。

## 記録と検証

- `PORT_MAP.json` は全ファイルの source SHA-256、行数、対応先、関数・定数、T3 テストケース、照合した行範囲を保持する。初期の symbol/case リストは検索用の索引であり、匿名関数・parameterized case の完全性の証明には使わない。翻訳時に該当ファイル全体を読み、関数と行範囲を補う。
- `scripts/t3-port/inventory.py --check` は4対象ディレクトリの欠落・余分なファイル・参照 hash の変更を検出する。通常実行は照合記録を保存したまま下表を更新する。対象外のファイルと fixture も省略しない。
- 未翻訳／未移植の行は、既存の短縮された実装や独自テストがあっても完了扱いにしない。T3 の期待値・fixture は変更しない。旧方針の逐語翻訳の完了記録と、新設計の純粋な domain / 翻訳層の検証完了を区別する。新設計の Host / クライアントへの production 接続完了は段階 3・4 の検証後に記録する。
- Effect の Service/Layer/Ref/Deferred/Scope は Rust の構造体・owner・future・task に置き換える。決定順序、トランザクション境界、cancel の勝ち方、ID、エラー分類、再送・再取得の意味は維持する。
- 該当 source の外にある persistence・shared helper・Claude SDK も、使用する関数を後述の追加依存対応に記録して翻訳する。

## 差分

許される境界を明示する。これ以外の挙動差は、該当行へ理由を記載できる場合だけ残す。

| 境界 | この実装の置換 | 保持する意味／検証 |
|---|---|---|
| HTTP / WebSocket | iroh、既存の Postcard framing、QR と端末鍵 | RPC の引数・結果、snapshot/replay/synchronized、afterSequence、順序・fallback 条件。QUIC の handshake/stream decode 保護は通信 owner に置く。 |
| Claude Agent SDK | `@anthropic-ai/claude-agent-sdk@0.3.276` を Node から直接使用 | 公開 query API、canUseTool、interrupt、permission 変更、resume/fork。SDK 内部の制御と履歴変換は再実装しない。Rust は共通イベントへ翻訳する。 |
| browser/ReactNative の描画 | GPUI / SwiftUI / Compose | client-runtime の結果を表示する。UI の条件・ラベル・操作は元コンポーネントに合わせ、Web 専用機能を追加しない。 |
| T3 Connect / 外部サービス | 対象外 | T3 Connect の HTTP 認証/relay は作らない。iroh の既存接続機能を保持する。 |
| V1 migration | 対象外 | ユーザー指示により互換性・旧形式移行は不要。 |

本体に置いた独自 gate、wake の frame/byte 上限、手作りの ID、短縮した policy、独自 retry 条件は、T3 に対応する実装がなければ削除する。既存の PLAN 判断の記録にあるこれらの記述も翻訳の根拠としては使用しない。

## 追加依存対応

SDK の取得記録（tarball の integrity/hash と制御関数）、persistence の各 repository/SQL、shared helper は該当モジュールの翻訳時に追記する。

<!-- generated inventory -->

## ファイル対応表

各ファイルの状態と対象外の理由を示す。新設計での対応先は「新設計の挙動テスト対応」以降に記録する。

### `apps/server/src/orchestration-v2`

本体 94 ファイル / 81,238 行。テスト 106 ファイル / 104,071 行。testkit/fixture は別行で全件追跡する。

| T3 のファイル（行数） | 移植した T3 テスト | 状態・理由 |
|---|---|---|
| `apps/server/src/orchestration-v2/AcpRegistryOrchestratorV2.live.test.ts` (255) | — | 対象外：この実装の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/Adapters/AcpAdapterV2.test.ts` (14296) | — | 対象外：この実装の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/Adapters/AcpAdapterV2.testkit.ts` (246) | — | 対象外：この実装の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/Adapters/AcpAdapterV2.ts` (7969) | — | 対象外：この実装の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/Adapters/AcpRegistryAdapterV2.test.ts` (486) | — | 対象外：この実装の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/Adapters/AcpRegistryAdapterV2.testkit.ts` (109) | — | 対象外：この実装の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/Adapters/AcpRegistryAdapterV2.ts` (328) | — | 対象外：この実装の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/Adapters/AntigravityAdapterV2.test.ts` (537) | — | 対象外：この実装の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/Adapters/AntigravityAdapterV2.ts` (241) | — | 対象外：この実装の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/Adapters/ClaudeAdapterV2.test.ts` (7963) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/Adapters/ClaudeAdapterV2.testkit.ts` (2844) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/Adapters/ClaudeAdapterV2.ts` (7859) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/Adapters/CodexAdapterV2.test.ts` (7275) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/Adapters/CodexAdapterV2.testkit.ts` (276) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/Adapters/CodexAdapterV2.ts` (6317) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/Adapters/CursorAdapterV2.test.ts` (895) | — | 対象外：この実装の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/Adapters/CursorAdapterV2.testkit.ts` (1022) | — | 対象外：この実装の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/Adapters/CursorAdapterV2.ts` (2677) | — | 対象外：この実装の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/Adapters/CursorAgentSdk.test.ts` (304) | — | 対象外：この実装の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/Adapters/CursorAgentSdk.ts` (612) | — | 対象外：この実装の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/Adapters/DevinAcp.ts` (57) | — | 対象外：この実装の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/Adapters/GrokAdapterV2.test.ts` (468) | — | 対象外：この実装の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/Adapters/GrokAdapterV2.testkit.ts` (122) | — | 対象外：この実装の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/Adapters/GrokAdapterV2.ts` (455) | — | 対象外：この実装の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/Adapters/OpenCode2AdapterV2.test.ts` (3921) | — | 対象外：この実装の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/Adapters/OpenCode2AdapterV2.testkit.ts` (344) | — | 対象外：この実装の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/Adapters/OpenCode2AdapterV2.ts` (4266) | — | 対象外：この実装の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/Adapters/OpenCodeAdapterV2.test.ts` (2445) | — | 対象外：この実装の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/Adapters/OpenCodeAdapterV2.testkit.ts` (511) | — | 対象外：この実装の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/Adapters/OpenCodeAdapterV2.ts` (3758) | — | 対象外：この実装の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/Adapters/OpenCodeToolItems.ts` (170) | — | 対象外：この実装の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/Adapters/PiAdapterV2.test.ts` (2489) | — | 対象外：この実装の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/Adapters/PiAdapterV2.testkit.ts` (524) | — | 対象外：この実装の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/Adapters/PiAdapterV2.ts` (3015) | — | 対象外：この実装の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/Adapters/PiRpc.ts` (484) | — | 対象外：この実装の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/Adapters/ProviderTextDeltaCoalescer.ts` (156) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/Adapters/piT3McpExtensionSource.test.ts` (59) | — | 対象外：この実装の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/Adapters/piT3McpExtensionSource.ts` (328) | — | 対象外：この実装の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/Adapters/piT3McpInjection.test.ts` (167) | — | 対象外：この実装の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/Adapters/piT3McpInjection.ts` (316) | — | 対象外：この実装の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/AttachmentClaims.test.ts` (316) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/AttachmentClaims.ts` (149) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/AttachmentPrompt.test.ts` (150) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/AttachmentPrompt.ts` (222) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/BackgroundWorkStop.integration.test.ts` (446) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/CheckpointCaptureService.test.ts` (737) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/CheckpointCaptureService.ts` (285) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/CheckpointRestoreSafety.test.ts` (198) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/CheckpointRestoreSafety.ts` (88) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/CheckpointRollbackService.test.ts` (509) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/CheckpointRollbackService.ts` (362) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/CheckpointScopeOwnership.test.ts` (237) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/CheckpointService.test.ts` (103) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/CheckpointService.ts` (585) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/CommandPolicy.test.ts` (631) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/CommandPolicy.ts` (491) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/CommandReceiptStore.ts` (214) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ContextHandoffBudget.test.ts` (691) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ContextHandoffBudget.ts` (282) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ContextHandoffDelivery.ts` (159) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ContextHandoffService.test.ts` (164) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ContextHandoffService.ts` (498) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/CursorOrchestratorV2.live.test.ts` (378) | — | 対象外：この実装の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/DelegatedCompletionDelivery.test.ts` (1278) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/EffectOutbox.ts` (636) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/EffectWorker.test.ts` (802) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/EffectWorker.ts` (834) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/EventSink.ts` (891) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/EventStore.ts` (156) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/FoundationPersistence.test.ts` (3383) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/GrokOrchestratorV2.live.test.ts` (236) | — | 対象外：この実装の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/IdAllocator.ts` (435) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/KeyedSerialExecutor.test.ts` (67) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/KeyedSerialExecutor.ts` (55) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/LiveStreamBudget.test.ts` (278) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/LiveStreamBudget.ts` (342) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/Notification.test.ts` (150) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/Notification.ts` (235) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/NotificationMailbox.ts` (23) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/OpenCode2OrchestratorV2.integration.test.ts` (1181) | — | 対象外：この実装の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/OpenCode2OrchestratorV2.live.test.ts` (1181) | — | 対象外：この実装の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。 |
| `apps/server/src/orchestration-v2/Orchestrator.control-reads.test.ts` (536) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/Orchestrator.migration.test.ts` (100) | — | 対象外：製品は未公開。ユーザー指示により旧 V1 の互換性・移行を作らない。 |
| `apps/server/src/orchestration-v2/Orchestrator.ts` (10272) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ProjectCommands.test.ts` (255) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ProjectCommands.ts` (241) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ProjectSettingsUpgrade.integration.test.ts` (195) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ProjectStore.test.ts` (56) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ProjectStore.ts` (278) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ProjectionControlReads.test.ts` (354) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ProjectionMaintenance.ts` (375) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ProjectionRecovery.test.ts` (517) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ProjectionSettlement.test.ts` (525) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ProjectionStore.test.ts` (4470) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ProjectionStore.ts` (6176) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ProviderAdapter.ts` (598) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ProviderAdapterDriver.ts` (44) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ProviderAdapterRegistry.test.ts` (330) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ProviderAdapterRegistry.ts` (369) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ProviderContinuationRequests.ts` (76) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ProviderContinuationService.test.ts` (820) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ProviderContinuationService.ts` (233) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ProviderEventIngestor.test.ts` (1341) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ProviderEventIngestor.ts` (641) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ProviderFailure.test.ts` (209) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ProviderFailure.ts` (251) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ProviderRuntimeRecoveryPerformance.test.ts` (510) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ProviderRuntimeRecoveryService.regression.test.ts` (82) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ProviderRuntimeRecoveryService.test.ts` (1422) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ProviderRuntimeRecoveryService.ts` (815) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ProviderSelectionTransition.test.ts` (55) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ProviderSelectionTransition.ts` (37) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ProviderSessionManager.test.ts` (3480) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ProviderSessionManager.ts` (2078) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ProviderSessionTransitionPolicy.test.ts` (176) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ProviderSessionTransitionPolicy.ts` (96) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ProviderSwitchService.test.ts` (343) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ProviderSwitchService.ts` (219) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ProviderTurnControlService.test.ts` (325) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ProviderTurnControlService.ts` (350) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ProviderTurnStartService.memory.test.ts` (25) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ProviderTurnStartService.test.ts` (857) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ProviderTurnStartService.testkit.ts` (16) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ProviderTurnStartService.ts` (1266) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ProviderTurnTokenUsage.test.ts` (43) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/PullRequestSyncReactor.test.ts` (966) | — | 対象外：今回の指示では M3（PR・workflow 周辺機能）を実装しない。 |
| `apps/server/src/orchestration-v2/PullRequestSyncReactor.ts` (411) | — | 対象外：今回の指示では M3（PR・workflow 周辺機能）を実装しない。 |
| `apps/server/src/orchestration-v2/PullRequestWatchReactor.ts` (280) | — | 対象外：今回の指示では M3（PR・workflow 周辺機能）を実装しない。 |
| `apps/server/src/orchestration-v2/QueuedRunOrder.test.ts` (51) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/QueuedRunOrder.ts` (32) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/RandomUuid.ts` (13) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ResourceCleanupService.ts` (71) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/RestartBackgroundNote.test.ts` (237) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/RestartBackgroundNote.ts` (276) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/RestartContinuation.test.ts` (809) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/RestartContinuation.ts` (186) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/RunCompletionReads.test.ts` (162) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/RunExecutionService.test.ts` (4018) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/RunExecutionService.ts` (1467) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/RunFinalizationService.test.ts` (131) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/RunFinalizationService.ts` (121) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/RuntimePolicy.test.ts` (156) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/RuntimePolicy.ts` (170) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/RuntimeRequestService.test.ts` (293) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/RuntimeRequestService.ts` (138) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/SelectionRestart.integration.test.ts` (996) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ShellStream.test.ts` (495) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ShellStream.ts` (260) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/SteeringCompletion.integration.test.ts` (704) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/SubagentProjection.test.ts` (308) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/SubagentProjection.ts` (253) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ThreadCommandExecutor.ts` (13) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ThreadDeletion.test.ts` (298) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ThreadDeletion.ts` (228) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ThreadFork.execution.test.ts` (251) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ThreadForkService.test.ts` (193) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ThreadForkService.ts` (139) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ThreadLaunchService.test.ts` (2218) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ThreadLaunchService.ts` (966) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ThreadLifecycleService.test.ts` (60) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ThreadLifecycleService.ts` (142) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ThreadLiveEventCoalescer.test.ts` (266) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ThreadLiveEventCoalescer.ts` (255) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ThreadManagementService.test.ts` (421) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ThreadManagementService.ts` (782) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ThreadMessageIntake.test.ts` (766) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ThreadMessageIntake.ts` (239) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ThreadPullRequestService.test.ts` (322) | — | 対象外：今回の指示では M3（PR・workflow 周辺機能）を実装しない。 |
| `apps/server/src/orchestration-v2/ThreadPullRequestService.ts` (408) | — | 対象外：今回の指示では M3（PR・workflow 周辺機能）を実装しない。 |
| `apps/server/src/orchestration-v2/ThreadSearch.test.ts` (201) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ThreadSearch.ts` (168) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ThreadSettlementService.test.ts` (1124) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ThreadSettlementService.ts` (597) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ThreadStream.test.ts` (264) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ThreadStream.ts` (96) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ThreadTitleRegenerationService.test.ts` (506) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ThreadTitleRegenerationService.ts` (150) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/ThreadTransportPerformance.test.ts` (254) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/TurnItemPositionStore.ts` (107) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/TurnStartReads.test.ts` (369) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/UsageLimitRecoveryWorker.ts` (115) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/UserFacingErrors.test.ts` (68) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/UserFacingErrors.ts` (110) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/V1ImportBoundary.test.ts` (91) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/WireProjection.test.ts` (351) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/WireProjection.ts` (217) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/assistantStreaming.test.ts` (150) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/assistantStreaming.ts` (129) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/builtInProviderAdapterDrivers.ts` (47) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/http.ts` (261) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/legacy/LegacyV1Cutover.integration.test.ts` (1045) | — | 対象外：製品は未公開。ユーザー指示により旧 V1 の互換性・移行を作らない。 |
| `apps/server/src/orchestration-v2/legacy/LegacyV1ThreadImporter.test.ts` (532) | — | 対象外：製品は未公開。ユーザー指示により旧 V1 の互換性・移行を作らない。 |
| `apps/server/src/orchestration-v2/legacy/LegacyV1ThreadImporter.ts` (833) | — | 対象外：製品は未公開。ユーザー指示により旧 V1 の互換性・移行を作らない。 |
| `apps/server/src/orchestration-v2/pullRequestWatch.test.ts` (217) | — | 対象外：今回の指示では M3（PR・workflow 周辺機能）を実装しない。 |
| `apps/server/src/orchestration-v2/pullRequestWatch.ts` (209) | — | 対象外：今回の指示では M3（PR・workflow 周辺機能）を実装しない。 |
| `apps/server/src/orchestration-v2/runtimeLayer.test.ts` (4758) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/runtimeLayer.ts` (325) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/ClaudeReplayFixtures.integration.test.ts` (382) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/CodexReplayFixtures.integration.test.ts` (773) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/DeterministicRuntime.ts` (13) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/OrchestratorReplayFixtures.contract.test.ts` (158) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/OrchestratorReplayFixtures.integration.test.ts` (296) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/OrchestratorReplayRecovery.integration.test.ts` (424) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/OrchestratorReplayRestartBackgroundNote.integration.test.ts` (262) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/OrchestratorScenario.ts` (739) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/ProviderReplayGate.testkit.ts` (119) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/ProviderReplayHarness.ts` (508) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/ProviderSwitch.integration.test.ts` (3315) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/ReplayFixtureWorkspace.ts` (86) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/ReplayTranscriptNdjson.ts` (248) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/ThreadFork.integration.test.ts` (1200) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/ThreadMergeBack.integration.test.ts` (732) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/acp_elicitation/registry_transcript.ndjson` (11) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_background_monitor_wake/claude_transcript.ndjson` (87) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_background_monitor_wake/input.ts` (29) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_background_monitor_wake/output.ts` (67) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_background_subagent_after_root/claude_transcript.ndjson` (137) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_background_subagent_after_root/input.ts` (31) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_background_subagent_after_root/output.ts` (100) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_background_subagent_lifecycle/claude_transcript.ndjson` (334) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_background_subagent_lifecycle/input.ts` (38) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_background_subagent_lifecycle/output.ts` (251) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_background_task_after_root/claude_transcript.ndjson` (13) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_background_task_after_root/input.ts` (26) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_background_task_after_root/output.ts` (96) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_background_task_interrupt/claude_transcript.ndjson` (103) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_background_task_interrupt/input.ts` (21) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_background_task_interrupt/output.ts` (50) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_background_task_wake/claude_transcript.ndjson` (111) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_background_task_wake/input.ts` (33) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_background_task_wake/output.ts` (102) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_background_wake_before_queued_prompt/claude_transcript.ndjson` (338) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_background_wake_before_queued_prompt/input.ts` (34) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_background_wake_before_queued_prompt/output.ts` (96) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_background_wake_before_queued_prompt_no_echo/claude_transcript.ndjson` (262) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_background_wake_before_queued_prompt_no_echo/input.ts` (23) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_background_wake_before_queued_prompt_no_echo/output.ts` (36) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_compact_after_peer_turn/claude_transcript.ndjson` (38) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_compact_after_peer_turn/input.ts` (33) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_compact_after_peer_turn/output.ts` (58) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_compact_after_peer_turn_no_echo/claude_transcript.ndjson` (25) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_compact_after_peer_turn_no_echo/output.ts` (35) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_compact_after_resume_wake/claude_transcript.ndjson` (43) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_compact_after_resume_wake/input.ts` (16) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_compact_after_resume_wake/output.ts` (36) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_idle_resume/claude_transcript.ndjson` (19) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_idle_resume/input.ts` (25) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_idle_resume/output.ts` (39) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_local_bash_task/claude_transcript.ndjson` (12) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_local_bash_task/input.ts` (7) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_local_bash_task/output.ts` (58) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_mcp_tool_presentation/claude_transcript.ndjson` (94) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_mcp_tool_presentation/input.ts` (14) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_mcp_tool_presentation/output.ts` (51) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_nested_background_subagent_wake/claude_transcript.ndjson` (163) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_nested_background_subagent_wake/input.ts` (31) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_nested_background_subagent_wake/output.ts` (58) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_nested_subagent_model/claude_transcript.ndjson` (105) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_nested_subagent_model/input.ts` (17) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_nested_subagent_model/output.ts` (60) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_result_is_error/claude_transcript.ndjson` (10) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_result_is_error/input.ts` (22) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_result_is_error/output.ts` (73) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/claude_subagent_resume_after_restart/claude_transcript.ndjson` (61) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/delegated_task_status/codex_transcript.ndjson` (41) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/grok_auto_blocked_command/grok_transcript.ndjson` (136) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/grok_auto_blocked_command/input.ts` (30) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/grok_auto_blocked_command/output.ts` (97) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/grok_background_bash/grok_transcript.ndjson` (241) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/grok_background_bash/input.ts` (24) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/grok_background_bash/output.ts` (100) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/grok_background_bash_fast_wake/grok_transcript.ndjson` (258) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/grok_background_bash_fast_wake/input.ts` (18) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/grok_background_bash_fast_wake/output.ts` (11) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/grok_background_subagent/grok_transcript.ndjson` (353) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/grok_background_subagent/input.ts` (23) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/grok_background_subagent/output.ts` (116) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/grok_monitor/grok_transcript.ndjson` (361) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/grok_monitor/input.ts` (25) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/grok_monitor/output.ts` (112) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/grok_prompt_error/grok_transcript.ndjson` (74) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/grok_prompt_error/input.ts` (20) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/grok_prompt_error/output.ts` (45) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/grok_subagent_lineage/grok_transcript.ndjson` (17) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/grok_subagent_lineage/input.ts` (9) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/grok_subagent_lineage/output.ts` (98) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/index.ts` (1633) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/message_steering/claude_output.ts` (43) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/message_steering/claude_transcript.ndjson` (13) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/message_steering/codex_output.ts` (43) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/message_steering/codex_transcript.ndjson` (43) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/message_steering/cursor_output.ts` (56) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/message_steering/cursor_transcript.ndjson` (25) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/message_steering/grok_output.ts` (55) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/message_steering/grok_transcript.ndjson` (67) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/message_steering/input.ts` (31) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/message_steering/pi_output.ts` (62) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/message_steering/pi_transcript.ndjson` (65) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/message_steering/registry_transcript.ndjson` (14) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/multi_turn/claude_output.ts` (53) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/multi_turn/claude_transcript.ndjson` (14) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/multi_turn/codex_output.ts` (40) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/multi_turn/codex_transcript.ndjson` (46) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/multi_turn/cursor_transcript.ndjson` (44) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/multi_turn/grok_transcript.ndjson` (91) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/multi_turn/input.ts` (14) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/multi_turn/pi_output.ts` (17) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/multi_turn/pi_transcript.ndjson` (72) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/multi_turn/registry_transcript.ndjson` (12) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/multi_turn_restart/claude_transcript.ndjson` (19) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_background/input.ts` (15) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_background/opencode_transcript.ndjson` (69) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_background/output.ts` (114) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_command/input.ts` (5) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_command/opencode_transcript.ndjson` (34) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_command/output.ts` (34) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_compaction/input.ts` (15) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_compaction/opencode_transcript.ndjson` (66) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_compaction/output.ts` (61) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_fork/opencode_transcript.ndjson` (69) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_inbox/input.ts` (24) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_inbox/opencode_transcript.ndjson` (53) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_inbox/output.ts` (72) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_interrupt/input.ts` (11) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_interrupt/opencode_transcript.ndjson` (36) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_interrupt/output.ts` (47) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_nested_background/input.ts` (15) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_nested_background/opencode_transcript.ndjson` (131) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_nested_background/output.ts` (77) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_permission/input.ts` (15) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_permission/opencode_transcript.ndjson` (117) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_permission/output.ts` (81) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_question/input.ts` (11) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_question/opencode_transcript.ndjson` (42) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_question/output.ts` (59) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_resume_after_restart/input.ts` (14) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_resume_after_restart/opencode_transcript.ndjson` (54) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_resume_after_restart/output.ts` (53) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_revert/input.ts` (17) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_revert/opencode_transcript.ndjson` (68) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_revert/output.ts` (39) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_simple/input.ts` (5) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_simple/opencode_transcript.ndjson` (53) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_simple/output.ts` (51) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_skill/input.ts` (5) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_skill/opencode_transcript.ndjson` (32) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_skill/output.ts` (34) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_subagent/input.ts` (5) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_subagent/opencode_transcript.ndjson` (83) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_subagent/output.ts` (74) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_switch/opencode_transcript.ndjson` (88) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_tool_call/input.ts` (5) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_tool_call/opencode_transcript.ndjson` (44) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode2_tool_call/output.ts` (56) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode_child_approval/input.ts` (10) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode_child_approval/opencode_transcript.ndjson` (40) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode_child_approval/output.ts` (43) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode_running_child_approval/input.ts` (16) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode_running_child_approval/opencode_transcript.ndjson` (41) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode_running_child_approval/output.ts` (88) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode_subagent/input.ts` (7) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode_subagent/opencode_transcript.ndjson` (34) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/opencode_subagent/output.ts` (71) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/pi_compaction/input.ts` (30) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/pi_compaction/output.ts` (93) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/pi_compaction/pi_transcript.ndjson` (160) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/plan_questions/codex_output.ts` (67) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/plan_questions/codex_transcript.ndjson` (39) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/plan_questions/grok_transcript.ndjson` (11) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/plan_questions/input.ts` (18) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/plan_questions/opencode_output.ts` (26) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/plan_questions/opencode_transcript.ndjson` (27) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/proposed_plan/codex_output.ts` (43) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/proposed_plan/codex_transcript.ndjson` (278) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/proposed_plan/cursor_output.ts` (39) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/proposed_plan/cursor_transcript.ndjson` (132) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/proposed_plan/input.ts` (8) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/provider_thread_resume/codex_transcript.ndjson` (79) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/provider_thread_resume/cursor_transcript.ndjson` (67) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/provider_thread_resume/input.ts` (21) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/provider_thread_resume/pi_output.ts` (68) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/provider_thread_resume/pi_transcript.ndjson` (124) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/queued_cancelled_while_active/codex_output.ts` (46) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/queued_cancelled_while_active/input.ts` (25) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/queued_turn/claude_transcript.ndjson` (14) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/queued_turn/codex_output.ts` (49) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/queued_turn/codex_transcript.ndjson` (46) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/queued_turn/cursor_transcript.ndjson` (36) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/queued_turn/grok_transcript.ndjson` (88) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/queued_turn/input.ts` (14) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/queued_turn/registry_transcript.ndjson` (12) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/shared.ts` (1485) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/simple/claude_output.ts` (33) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/simple/claude_transcript.ndjson` (10) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/simple/codex_output.ts` (33) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/simple/codex_transcript.ndjson` (30) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/simple/cursor_transcript.ndjson` (20) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/simple/grok_transcript.ndjson` (76) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/simple/input.ts` (7) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/simple/opencode_transcript.ndjson` (23) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/simple/pi_output.ts` (117) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/simple/pi_transcript.ndjson` (46) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/simple/registry_transcript.ndjson` (12) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/skill_invocation/cursor_output.ts` (39) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/skill_invocation/cursor_transcript.ndjson` (64) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/skill_invocation/input.ts` (19) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/stop_background_work_after_failed_turn/input.ts` (33) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/stop_background_work_after_failed_turn/output.ts` (44) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/stop_background_work_after_failed_turn/registry_transcript.ndjson` (10) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/subagent/claude_output.ts` (123) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/subagent/claude_transcript.ndjson` (29) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/subagent/codex_output.ts` (124) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/subagent/codex_transcript.ndjson` (628) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/subagent/cursor_output.ts` (96) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/subagent/cursor_transcript.ndjson` (353) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/subagent/input.ts` (7) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/subagent_continue/codex_output.ts` (60) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/subagent_continue/codex_transcript.ndjson` (138) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/subagent_continue/input.ts` (14) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/subagent_v2/codex_output.ts` (86) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/subagent_v2/codex_transcript.ndjson` (49) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/subagent_v2/input.ts` (17) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/subagent_v2_approval/codex_output.ts` (87) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/subagent_v2_approval/codex_transcript.ndjson` (68) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/subagent_v2_approval/input.ts` (21) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/subagent_v2_nested/codex_output.ts` (135) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/subagent_v2_nested/codex_transcript.ndjson` (98) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/subagent_v2_nested_approval/codex_output.ts` (105) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/subagent_v2_nested_approval/codex_transcript.ndjson` (84) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/subagent_v2_nested_approval/input.ts` (15) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/thread_fork_native/claude_transcript.ndjson` (21) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/thread_fork_native/codex_transcript.ndjson` (21) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/thread_fork_native_continue/claude_transcript.ndjson` (25) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/thread_fork_native_continue/codex_transcript.ndjson` (82) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/thread_fork_native_fork_local_rollback/claude_transcript.ndjson` (35) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/thread_fork_native_prior_turn/claude_transcript.ndjson` (26) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/thread_fork_native_prior_turn/codex_transcript.ndjson` (928) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/thread_fork_native_siblings/claude_transcript.ndjson` (32) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/thread_fork_native_siblings/codex_transcript.ndjson` (107) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/thread_merge_back_continue/claude_transcript.ndjson` (34) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/thread_merge_back_continue/codex_transcript.ndjson` (95) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/thread_merge_back_siblings/claude_transcript.ndjson` (49) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/thread_merge_back_siblings/codex_transcript.ndjson` (155) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/thread_rollback/claude_output.ts` (71) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/thread_rollback/claude_transcript.ndjson` (24) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/thread_rollback/codex_output.ts` (72) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/thread_rollback/codex_transcript.ndjson` (99) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/thread_rollback/input.ts` (21) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/thread_rollback/pi_output.ts` (83) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/thread_rollback/pi_transcript.ndjson` (163) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/thread_rollback_after_restart/codex_output.ts` (31) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/thread_rollback_after_restart/codex_transcript.ndjson` (107) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/thread_rollback_after_restart/input.ts` (27) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/thread_rollback_after_stop/input.ts` (29) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/thread_rollback_after_stop/pi_output.ts` (83) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/thread_rollback_after_stop/pi_transcript.ndjson` (274) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/thread_rollback_to_stopped_turn/codex_output.ts` (66) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/thread_rollback_to_stopped_turn/codex_transcript.ndjson` (279) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/thread_rollback_to_stopped_turn/input.ts` (28) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/todo_list/codex_output.ts` (39) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/todo_list/codex_transcript.ndjson` (37) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/todo_list/cursor_output.ts` (72) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/todo_list/cursor_transcript.ndjson` (210) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/todo_list/grok_output.ts` (52) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/todo_list/grok_transcript.ndjson` (329) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/todo_list/input.ts` (7) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/todo_list/registry_transcript.ndjson` (16) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/tool_call_denied_write/claude_output.ts` (44) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/tool_call_denied_write/claude_transcript.ndjson` (89) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/tool_call_denied_write/input.ts` (24) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/tool_call_read_only/claude_output.ts` (62) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/tool_call_read_only/claude_transcript.ndjson` (16) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/tool_call_read_only/cursor_output.ts` (58) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/tool_call_read_only/cursor_transcript.ndjson` (49) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/tool_call_read_only/grok_transcript.ndjson` (15) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/tool_call_read_only/input.ts` (7) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/tool_call_read_only/registry_transcript.ndjson` (15) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/tool_call_read_only_on_request/claude_transcript.ndjson` (15) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/tool_call_read_only_on_request/codex_transcript.ndjson` (81) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/tool_call_read_only_on_request/grok_transcript.ndjson` (181) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/tool_call_read_only_on_request/input.ts` (10) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/tool_call_read_only_on_request/output.ts` (108) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/tool_call_read_only_on_request/registry_transcript.ndjson` (13) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/tool_call_restricted_granular/claude_output.ts` (41) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/tool_call_restricted_granular/claude_transcript.ndjson` (15) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/tool_call_restricted_granular/codex_output.ts` (36) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/tool_call_restricted_granular/codex_transcript.ndjson` (80) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/tool_call_restricted_granular/input.ts` (10) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/tool_call_workspace_never/claude_output.ts` (34) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/tool_call_workspace_never/claude_transcript.ndjson` (13) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/tool_call_workspace_never/codex_output.ts` (29) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/tool_call_workspace_never/codex_transcript.ndjson` (68) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/tool_call_workspace_never/input.ts` (7) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/turn_interrupt/claude_output.ts` (51) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/turn_interrupt/claude_transcript.ndjson` (7) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/turn_interrupt/codex_output.ts` (49) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/turn_interrupt/codex_transcript.ndjson` (25) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/turn_interrupt/grok_transcript.ndjson` (28) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/turn_interrupt/input.ts` (10) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/turn_interrupt/opencode_transcript.ndjson` (20) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/turn_interrupt/registry_transcript.ndjson` (9) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/turn_interrupt_mid_tool/claude_output.ts` (95) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/turn_interrupt_mid_tool/claude_transcript.ndjson` (11) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/turn_interrupt_mid_tool/codex_output.ts` (150) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/turn_interrupt_mid_tool/codex_transcript.ndjson` (35) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/turn_interrupt_mid_tool/cursor_output.ts` (76) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/turn_interrupt_mid_tool/cursor_transcript.ndjson` (33) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/turn_interrupt_mid_tool/input.ts` (10) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/turn_interrupt_mid_tool/pi_output.ts` (67) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/turn_interrupt_mid_tool/pi_transcript.ndjson` (59) | — | 対象外：Codex/Claude 以外の provider 専用 replay fixture。この実装 では対象外。 |
| `apps/server/src/orchestration-v2/testkit/fixtures/turn_interrupt_restart/claude_output.ts` (129) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/turn_interrupt_restart/claude_transcript.ndjson` (20) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/turn_interrupt_restart/input.ts` (15) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/web_search/claude_output.ts` (58) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/web_search/claude_transcript.ndjson` (16) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/web_search/codex_output.ts` (42) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/web_search/codex_transcript.ndjson` (54) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/fixtures/web_search/input.ts` (7) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/testkit/pullRequestFixtures.ts` (70) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/threadHistoryPaging.test.ts` (702) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/threadHistoryPaging.ts` (532) | — | 未翻訳 |
| `apps/server/src/orchestration-v2/workflowScriptQuery.test.ts` (74) | — | 対象外：今回の指示では M3（PR・workflow 周辺機能）を実装しない。 |
| `apps/server/src/orchestration-v2/workflowScriptQuery.ts` (127) | — | 対象外：今回の指示では M3（PR・workflow 周辺機能）を実装しない。 |

### `apps/server/src/project`

本体 15 ファイル / 5,595 行。テスト 15 ファイル / 7,354 行。testkit/fixture は別行で全件追跡する。

| T3 のファイル（行数） | 移植した T3 テスト | 状態・理由 |
|---|---|---|
| `apps/server/src/project/AgentSessionImporter.test.ts` (147) | — | 未翻訳 |
| `apps/server/src/project/AgentSessionImporter.ts` (409) | — | 未翻訳 |
| `apps/server/src/project/AgentSessionJson.test.ts` (60) | — | 未翻訳 |
| `apps/server/src/project/AgentSessionJson.ts` (201) | — | 未翻訳 |
| `apps/server/src/project/AgentSessionScanner.test.ts` (3196) | — | 未翻訳 |
| `apps/server/src/project/AgentSessionScanner.ts` (1495) | — | 未翻訳 |
| `apps/server/src/project/ManagedProjectFolders.test.ts` (572) | — | 一部翻訳：named project の作成（createNamedProject、newProjectFolderName）を移植。Scratch project の作成と icon の設定は対象外。 |
| `apps/server/src/project/ManagedProjectFolders.ts` (510) | — | 一部翻訳：named project の作成（createNamedProject、newProjectFolderName）を移植。Scratch project の作成と icon の設定は対象外。 |
| `apps/server/src/project/ProjectCloneTracker.test.ts` (303) | — | 対象外：履歴取り込み以外の project 機能は今回の翻訳範囲外。残す Host のプロジェクト機能との接続点は importer に記録。 |
| `apps/server/src/project/ProjectCloneTracker.ts` (479) | — | 対象外：履歴取り込み以外の project 機能は今回の翻訳範囲外。残す Host のプロジェクト機能との接続点は importer に記録。 |
| `apps/server/src/project/ProjectEnrichmentService.test.ts` (343) | — | 一部翻訳：repository identity の enrichment を移植（favicon の 2 件は Host に favicon がないため対象外）。 |
| `apps/server/src/project/ProjectEnrichmentService.ts` (297) | — | 一部翻訳：repository identity の enrichment を移植。favicon は対象外。 |
| `apps/server/src/project/ProjectFaviconResolver.test.ts` (468) | — | 対象外：履歴取り込み以外の project 機能は今回の翻訳範囲外。残す Host のプロジェクト機能との接続点は importer に記録。 |
| `apps/server/src/project/ProjectFaviconResolver.ts` (347) | — | 対象外：履歴取り込み以外の project 機能は今回の翻訳範囲外。残す Host のプロジェクト機能との接続点は importer に記録。 |
| `apps/server/src/project/ProjectMutation.test.ts` (93) | — | 対象外：履歴取り込み以外の project 機能は今回の翻訳範囲外。残す Host のプロジェクト機能との接続点は importer に記録。 |
| `apps/server/src/project/ProjectMutation.ts` (53) | — | 対象外：履歴取り込み以外の project 機能は今回の翻訳範囲外。残す Host のプロジェクト機能との接続点は importer に記録。 |
| `apps/server/src/project/ProjectService.deletion.test.ts` (486) | — | 対象外：履歴取り込み以外の project 機能は今回の翻訳範囲外。残す Host のプロジェクト機能との接続点は importer に記録。 |
| `apps/server/src/project/ProjectService.test.ts` (805) | — | 対象外：履歴取り込み以外の project 機能は今回の翻訳範囲外。残す Host のプロジェクト機能との接続点は importer に記録。 |
| `apps/server/src/project/ProjectService.ts` (572) | — | 対象外：履歴取り込み以外の project 機能は今回の翻訳範囲外。残す Host のプロジェクト機能との接続点は importer に記録。 |
| `apps/server/src/project/ProjectSetupScriptRunner.test.ts` (119) | — | 対象外：履歴取り込み以外の project 機能は今回の翻訳範囲外。残す Host のプロジェクト機能との接続点は importer に記録。 |
| `apps/server/src/project/ProjectSetupScriptRunner.ts` (456) | — | 対象外：履歴取り込み以外の project 機能は今回の翻訳範囲外。残す Host のプロジェクト機能との接続点は importer に記録。 |
| `apps/server/src/project/RepositoryIdentityResolver.test.ts` (393) | — | 翻訳済み：全 10 件を移植。 |
| `apps/server/src/project/RepositoryIdentityResolver.ts` (197) | — | 翻訳済み。 |
| `apps/server/src/project/T3ProjectFileLoader.test.ts` (89) | — | 対象外：履歴取り込み以外の project 機能は今回の翻訳範囲外。残す Host のプロジェクト機能との接続点は importer に記録。 |
| `apps/server/src/project/T3ProjectFileLoader.ts` (109) | — | 対象外：履歴取り込み以外の project 機能は今回の翻訳範囲外。残す Host のプロジェクト機能との接続点は importer に記録。 |
| `apps/server/src/project/WorktreeSetupTracker.test.ts` (226) | — | 対象外：履歴取り込み以外の project 機能は今回の翻訳範囲外。残す Host のプロジェクト機能との接続点は importer に記録。 |
| `apps/server/src/project/WorktreeSetupTracker.ts` (366) | — | 対象外：履歴取り込み以外の project 機能は今回の翻訳範囲外。残す Host のプロジェクト機能との接続点は importer に記録。 |
| `apps/server/src/project/gitCloneProgress.ts` (44) | — | 対象外：履歴取り込み以外の project 機能は今回の翻訳範囲外。残す Host のプロジェクト機能との接続点は importer に記録。 |
| `apps/server/src/project/http.test.ts` (54) | — | 対象外：履歴取り込み以外の project 機能は今回の翻訳範囲外。残す Host のプロジェクト機能との接続点は importer に記録。 |
| `apps/server/src/project/http.ts` (60) | — | 対象外：履歴取り込み以外の project 機能は今回の翻訳範囲外。残す Host のプロジェクト機能との接続点は importer に記録。 |

### `packages/client-runtime`

本体 183 ファイル / 35,146 行。テスト 120 ファイル / 40,301 行。testkit/fixture は別行で全件追跡する。

| T3 のファイル（行数） | 移植した T3 テスト | 状態・理由 |
|---|---|---|
| `packages/client-runtime/package.json` (400) | — | 未翻訳 |
| `packages/client-runtime/src/authorization/index.ts` (6) | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/authorization/layer.test.ts` (937) | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/authorization/remote.test.ts` (488) | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/authorization/remote.ts` (250) | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/authorization/service.ts` (508) | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/authorization/tokenStore.ts` (38) | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/codexArtifactTemplates.test.ts` (119) | — | 対象外：今回の指示では M3 の Git/PR/usage/全設定・更新配布 UI は実装しない。会話に必要な契約・回復経路は除外しない。 |
| `packages/client-runtime/src/codexArtifactTemplates.ts` (136) | — | 対象外：今回の指示では M3 の Git/PR/usage/全設定・更新配布 UI は実装しない。会話に必要な契約・回復経路は除外しない。 |
| `packages/client-runtime/src/codexFileCitations.test.ts` (77) | — | 未翻訳 |
| `packages/client-runtime/src/codexFileCitations.ts` (56) | — | 未翻訳 |
| `packages/client-runtime/src/codexMarkdownDirectives.test.ts` (175) | — | 未翻訳 |
| `packages/client-runtime/src/codexMarkdownDirectives.ts` (388) | — | 未翻訳 |
| `packages/client-runtime/src/composerThreadItems.test.ts` (50) | — | 未翻訳 |
| `packages/client-runtime/src/composerThreadItems.ts` (51) | — | 未翻訳 |
| `packages/client-runtime/src/connection/catalog.ts` (132) | — | 未翻訳 |
| `packages/client-runtime/src/connection/compatibility.test.ts` (78) | — | 未翻訳 |
| `packages/client-runtime/src/connection/compatibility.ts` (42) | — | 未翻訳 |
| `packages/client-runtime/src/connection/connectivity.ts` (19) | — | 未翻訳 |
| `packages/client-runtime/src/connection/credentialStore.ts` (23) | — | 未翻訳 |
| `packages/client-runtime/src/connection/driver.ts` (65) | — | 未翻訳 |
| `packages/client-runtime/src/connection/errors.test.ts` (131) | — | 未翻訳 |
| `packages/client-runtime/src/connection/errors.ts` (192) | — | 未翻訳 |
| `packages/client-runtime/src/connection/githubRoutingPermissions.ts` (147) | — | 未翻訳 |
| `packages/client-runtime/src/connection/index.ts` (20) | — | 未翻訳 |
| `packages/client-runtime/src/connection/layer.ts` (99) | — | 未翻訳 |
| `packages/client-runtime/src/connection/model.ts` (178) | — | 未翻訳 |
| `packages/client-runtime/src/connection/onboarding.test.ts` (335) | — | 未翻訳 |
| `packages/client-runtime/src/connection/onboarding.ts` (279) | — | 未翻訳 |
| `packages/client-runtime/src/connection/outdatedHostUpdate.test.ts` (278) | — | 対象外：今回の指示では M3 の Git/PR/usage/全設定・更新配布 UI は実装しない。会話に必要な契約・回復経路は除外しない。 |
| `packages/client-runtime/src/connection/outdatedHostUpdate.ts` (190) | — | 対象外：今回の指示では M3 の Git/PR/usage/全設定・更新配布 UI は実装しない。会話に必要な契約・回復経路は除外しない。 |
| `packages/client-runtime/src/connection/presentation.test.ts` (199) | — | 未翻訳 |
| `packages/client-runtime/src/connection/presentation.ts` (110) | — | 未翻訳 |
| `packages/client-runtime/src/connection/profileStore.ts` (20) | — | 未翻訳 |
| `packages/client-runtime/src/connection/registry.test.ts` (1584) | — | 未翻訳 |
| `packages/client-runtime/src/connection/registry.ts` (919) | — | 未翻訳 |
| `packages/client-runtime/src/connection/resolver.test.ts` (563) | — | 未翻訳 |
| `packages/client-runtime/src/connection/resolver.ts` (314) | — | 未翻訳 |
| `packages/client-runtime/src/connection/supervisor.test.ts` (1635) | — | 未翻訳 |
| `packages/client-runtime/src/connection/supervisor.ts` (847) | — | 未翻訳 |
| `packages/client-runtime/src/connection/wakeups.ts` (34) | — | 未翻訳 |
| `packages/client-runtime/src/delayedStatus.test.ts` (65) | — | 未翻訳 |
| `packages/client-runtime/src/delayedStatus.ts` (86) | — | 未翻訳 |
| `packages/client-runtime/src/device/androidFoldScene.test.ts` (78) | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/device/androidFoldScene.ts` (420) | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/device/deviceFraming.test.ts` (53) | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/device/deviceFraming.ts` (82) | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/device/deviceMotion.test.ts` (175) | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/device/deviceMotion.ts` (279) | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/device/deviceViewSnap.ts` (26) | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/device/duoControl.test.ts` (109) | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/device/duoControl.ts` (113) | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/device/duoScene.test.ts` (108) | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/device/duoScene.ts` (244) | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/device/duoSnap.test.ts` (54) | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/device/duoSnap.ts` (86) | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/device/duoStream.test.ts` (285) | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/device/duoViewer.test.ts` (703) | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/device/duoViewer.ts` (566) | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/device/frame.test.ts` (32) | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/device/frame.ts` (24) | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/device/hubAccess.ts` (18) | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/device/model.test.ts` (126) | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/device/model.ts` (88) | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/device/modelScene.test.ts` (121) | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/device/modelScene.ts` (118) | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/device/phoneInteraction.test.ts` (105) | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/device/phoneInteraction.ts` (83) | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/device/phoneScene.test.ts` (150) | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/device/phoneScene.ts` (302) | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/device/phoneViewer.test.ts` (530) | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/device/phoneViewer.ts` (444) | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/device/renderScheduler.test.ts` (27) | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/device/renderScheduler.ts` (23) | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/device/screenshot.test.ts` (50) | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/device/screenshot.ts` (30) | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/device/shapeProfile.test.ts` (27) | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/device/shapeProfile.ts` (124) | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/device/stream.test.ts` (738) | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/device/stream.ts` (1091) | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/device/streamFrames.test.ts` (97) | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/environment/descriptor.ts` (17) | — | 未翻訳 |
| `packages/client-runtime/src/environment/endpoint.test.ts` (61) | — | 未翻訳 |
| `packages/client-runtime/src/environment/endpoint.ts` (9) | — | 未翻訳 |
| `packages/client-runtime/src/environment/index.ts` (4) | — | 未翻訳 |
| `packages/client-runtime/src/environment/knownEnvironment.test.ts` (63) | — | 未翻訳 |
| `packages/client-runtime/src/environment/knownEnvironment.ts` (41) | — | 未翻訳 |
| `packages/client-runtime/src/environment/scoped.ts` (69) | — | 未翻訳 |
| `packages/client-runtime/src/errors/errorTrace.test.ts` (44) | — | 未翻訳 |
| `packages/client-runtime/src/errors/errorTrace.ts` (50) | — | 未翻訳 |
| `packages/client-runtime/src/errors/index.ts` (4) | — | 未翻訳 |
| `packages/client-runtime/src/errors/network.ts` (4) | — | 未翻訳 |
| `packages/client-runtime/src/errors/orchestration.test.ts` (50) | — | 未翻訳 |
| `packages/client-runtime/src/errors/orchestration.ts` (16) | — | 未翻訳 |
| `packages/client-runtime/src/errors/safeLog.test.ts` (55) | — | 未翻訳 |
| `packages/client-runtime/src/errors/safeLog.ts` (107) | — | 未翻訳 |
| `packages/client-runtime/src/errors/transport.test.ts` (87) | — | 未翻訳 |
| `packages/client-runtime/src/errors/transport.ts` (46) | — | 未翻訳 |
| `packages/client-runtime/src/filePreview.test.ts` (67) | — | 未翻訳 |
| `packages/client-runtime/src/filePreview.ts` (44) | — | 未翻訳 |
| `packages/client-runtime/src/handoff.test.ts` (65) | — | 未翻訳 |
| `packages/client-runtime/src/handoff.ts` (64) | — | 未翻訳 |
| `packages/client-runtime/src/load-balancing.ts` (40) | — | 未翻訳 |
| `packages/client-runtime/src/markdownImages.test.ts` (71) | — | 未翻訳 |
| `packages/client-runtime/src/markdownImages.ts` (72) | — | 未翻訳 |
| `packages/client-runtime/src/markdownLinks.test.ts` (320) | — | 未翻訳 |
| `packages/client-runtime/src/markdownLinks.ts` (369) | — | 未翻訳 |
| `packages/client-runtime/src/mediaActions.ts` (8) | — | 未翻訳 |
| `packages/client-runtime/src/mediaReference.test.ts` (39) | — | 未翻訳 |
| `packages/client-runtime/src/mediaReference.ts` (92) | — | 未翻訳 |
| `packages/client-runtime/src/mediaSource.test.ts` (141) | — | 未翻訳 |
| `packages/client-runtime/src/mediaSource.ts` (104) | — | 未翻訳 |
| `packages/client-runtime/src/operations/commands.performance.test.ts` (285) | — | 未翻訳 |
| `packages/client-runtime/src/operations/commands.test.ts` (972) | — | 未翻訳 |
| `packages/client-runtime/src/operations/commands.ts` (1067) | — | 未翻訳 |
| `packages/client-runtime/src/operations/index.ts` (3) | — | 未翻訳 |
| `packages/client-runtime/src/operations/projects.test.ts` (277) | — | 未翻訳 |
| `packages/client-runtime/src/operations/projects.ts` (367) | — | 未翻訳 |
| `packages/client-runtime/src/operations/threadTitle.test.ts` (47) | — | 未翻訳 |
| `packages/client-runtime/src/operations/threadTitle.ts` (33) | — | 未翻訳 |
| `packages/client-runtime/src/platform/capabilities.ts` (74) | — | 未翻訳 |
| `packages/client-runtime/src/platform/index.ts` (7) | — | 未翻訳 |
| `packages/client-runtime/src/platform/orchestrationCache.test.ts` (186) | — | 未翻訳 |
| `packages/client-runtime/src/platform/orchestrationCache.ts` (36) | — | 未翻訳 |
| `packages/client-runtime/src/platform/persistence.ts` (139) | — | 未翻訳 |
| `packages/client-runtime/src/platform/source.ts` (15) | — | 未翻訳 |
| `packages/client-runtime/src/platform/storageDocument.test.ts` (414) | — | 未翻訳 |
| `packages/client-runtime/src/platform/storageDocument.ts` (206) | — | 未翻訳 |
| `packages/client-runtime/src/projectFaviconCache.test.ts` (323) | — | 対象外：今回の指示では M3 の Git/PR/usage/全設定・更新配布 UI は実装しない。会話に必要な契約・回復経路は除外しない。 |
| `packages/client-runtime/src/projectFaviconCache.ts` (263) | — | 対象外：今回の指示では M3 の Git/PR/usage/全設定・更新配布 UI は実装しない。会話に必要な契約・回復経路は除外しない。 |
| `packages/client-runtime/src/providerSkills.test.ts` (277) | — | 未翻訳 |
| `packages/client-runtime/src/providerSkills.ts` (134) | — | 未翻訳 |
| `packages/client-runtime/src/relay/discovery.test.ts` (432) | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/relay/discovery.ts` (350) | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/relay/errorPresentation.test.ts` (57) | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/relay/errorPresentation.ts` (66) | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/relay/index.ts` (4) | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/relay/managedRelay.test.ts` (708) | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/relay/managedRelay.ts` (940) | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/relay/managedRelayState.test.ts` (449) | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/relay/managedRelayState.ts` (445) | — | 対象外：T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。 |
| `packages/client-runtime/src/remotePerformance.bench.ts` (135) | — | 対象外：Web/relay の benchmark。Rust の意味論テストには該当しない。 |
| `packages/client-runtime/src/repairMarkdownFileLinks.ts` (84) | — | 未翻訳 |
| `packages/client-runtime/src/rpc/client.test.ts` (838) | — | 未翻訳 |
| `packages/client-runtime/src/rpc/client.ts` (404) | — | 未翻訳 |
| `packages/client-runtime/src/rpc/http.ts` (176) | — | 未翻訳 |
| `packages/client-runtime/src/rpc/index.ts` (4) | — | 未翻訳 |
| `packages/client-runtime/src/rpc/protocol.ts` (8) | — | 未翻訳 |
| `packages/client-runtime/src/rpc/session.test.ts` (1268) | — | 未翻訳 |
| `packages/client-runtime/src/rpc/session.ts` (398) | — | 未翻訳 |
| `packages/client-runtime/src/state/archivedThreads.test.ts` (40) | — | 未翻訳 |
| `packages/client-runtime/src/state/archivedThreads.ts` (69) | — | 未翻訳 |
| `packages/client-runtime/src/state/assets.test.ts` (513) | — | 未翻訳 |
| `packages/client-runtime/src/state/assets.ts` (239) | — | 未翻訳 |
| `packages/client-runtime/src/state/attachments.test.ts` (202) | — | 未翻訳 |
| `packages/client-runtime/src/state/attachments.ts` (236) | — | 未翻訳 |
| `packages/client-runtime/src/state/auth.test.ts` (77) | — | 未翻訳 |
| `packages/client-runtime/src/state/auth.ts` (90) | — | 未翻訳 |
| `packages/client-runtime/src/state/boundedThreadSnapshotHttp.test.ts` (214) | — | 未翻訳 |
| `packages/client-runtime/src/state/boundedThreadSnapshotHttp.ts` (162) | — | 未翻訳 |
| `packages/client-runtime/src/state/cachePersistence.ts` (19) | — | 未翻訳 |
| `packages/client-runtime/src/state/checkpointDiff.ts` (25) | — | 未翻訳 |
| `packages/client-runtime/src/state/composerDispatch.test.ts` (72) | — | 未翻訳 |
| `packages/client-runtime/src/state/composerDispatch.ts` (32) | — | 未翻訳 |
| `packages/client-runtime/src/state/composerPathSearch.ts` (19) | — | 未翻訳 |
| `packages/client-runtime/src/state/connections.ts` (188) | — | 未翻訳 |
| `packages/client-runtime/src/state/customSnooze.test.ts` (66) | — | 未翻訳 |
| `packages/client-runtime/src/state/device.ts` (99) | — | 未翻訳 |
| `packages/client-runtime/src/state/deviceHubAccess.ts` (60) | — | 未翻訳 |
| `packages/client-runtime/src/state/entities.test.ts` (734) | — | 未翻訳 |
| `packages/client-runtime/src/state/entities.ts` (125) | — | 未翻訳 |
| `packages/client-runtime/src/state/environmentHttpAuth.test.ts` (670) | — | 未翻訳 |
| `packages/client-runtime/src/state/environmentHttpAuth.ts` (208) | — | 未翻訳 |
| `packages/client-runtime/src/state/filesystem.test.ts` (70) | — | 未翻訳 |
| `packages/client-runtime/src/state/filesystem.ts` (83) | — | 未翻訳 |
| `packages/client-runtime/src/state/git.ts` (23) | — | 対象外：今回の指示では M3 の Git/PR/usage/全設定・更新配布 UI は実装しない。会話に必要な契約・回復経路は除外しない。 |
| `packages/client-runtime/src/state/gitActions.ts` (358) | — | 対象外：今回の指示では M3 の Git/PR/usage/全設定・更新配布 UI は実装しない。会話に必要な契約・回復経路は除外しない。 |
| `packages/client-runtime/src/state/itemSupport.test.ts` (205) | — | 未翻訳 |
| `packages/client-runtime/src/state/itemSupport.ts` (138) | — | 未翻訳 |
| `packages/client-runtime/src/state/models.ts` (329) | — | 未翻訳 |
| `packages/client-runtime/src/state/orchestration.ts` (69) | — | 未翻訳 |
| `packages/client-runtime/src/state/orchestrationV2Projection.test.ts` (351) | — | 未翻訳 |
| `packages/client-runtime/src/state/orchestrationV2Projection.ts` (286) | — | 未翻訳 |
| `packages/client-runtime/src/state/orchestrationV2TestFixtures.ts` (110) | — | 未翻訳 |
| `packages/client-runtime/src/state/outdatedServerUpdate.ts` (78) | — | 対象外：今回の指示では M3 の Git/PR/usage/全設定・更新配布 UI は実装しない。会話に必要な契約・回復経路は除外しない。 |
| `packages/client-runtime/src/state/presentation.test.ts` (200) | — | 未翻訳 |
| `packages/client-runtime/src/state/presentation.ts` (221) | — | 未翻訳 |
| `packages/client-runtime/src/state/preview.test.ts` (18) | — | 未翻訳 |
| `packages/client-runtime/src/state/preview.ts` (116) | — | 未翻訳 |
| `packages/client-runtime/src/state/projectCommands.test.ts` (77) | — | 未翻訳 |
| `packages/client-runtime/src/state/projectCommands.ts` (168) | — | 未翻訳 |
| `packages/client-runtime/src/state/projectEntities.ts` (105) | — | 未翻訳 |
| `packages/client-runtime/src/state/projectGrouping.test.ts` (281) | — | 未翻訳 |
| `packages/client-runtime/src/state/projectGrouping.ts` (322) | — | 未翻訳 |
| `packages/client-runtime/src/state/projects.ts` (208) | — | 未翻訳 |
| `packages/client-runtime/src/state/providerInstanceDisplay.test.ts` (136) | — | 未翻訳 |
| `packages/client-runtime/src/state/providerInstanceDisplay.ts` (103) | — | 未翻訳 |
| `packages/client-runtime/src/state/pullRequestDiffHttp.test.ts` (120) | — | 対象外：今回の指示では M3 の Git/PR/usage/全設定・更新配布 UI は実装しない。会話に必要な契約・回復経路は除外しない。 |
| `packages/client-runtime/src/state/pullRequestDiffHttp.ts` (102) | — | 対象外：今回の指示では M3 の Git/PR/usage/全設定・更新配布 UI は実装しない。会話に必要な契約・回復経路は除外しない。 |
| `packages/client-runtime/src/state/pullRequestRouting.ts` (423) | — | 対象外：今回の指示では M3 の Git/PR/usage/全設定・更新配布 UI は実装しない。会話に必要な契約・回復経路は除外しない。 |
| `packages/client-runtime/src/state/pullRequests.test.ts` (1568) | — | 対象外：今回の指示では M3 の Git/PR/usage/全設定・更新配布 UI は実装しない。会話に必要な契約・回復経路は除外しない。 |
| `packages/client-runtime/src/state/pullRequests.ts` (525) | — | 対象外：今回の指示では M3 の Git/PR/usage/全設定・更新配布 UI は実装しない。会話に必要な契約・回復経路は除外しない。 |
| `packages/client-runtime/src/state/relayDiscovery.ts` (41) | — | 未翻訳 |
| `packages/client-runtime/src/state/review.ts` (75) | — | 未翻訳 |
| `packages/client-runtime/src/state/runtime.test.ts` (954) | — | 未翻訳 |
| `packages/client-runtime/src/state/runtime.ts` (756) | — | 未翻訳 |
| `packages/client-runtime/src/state/server.test.ts` (928) | — | 未翻訳 |
| `packages/client-runtime/src/state/server.ts` (1305) | — | 未翻訳 |
| `packages/client-runtime/src/state/serverConfigProjection.ts` (99) | — | 未翻訳 |
| `packages/client-runtime/src/state/serverUsage.test.ts` (263) | — | 未翻訳 |
| `packages/client-runtime/src/state/session.ts` (165) | — | 未翻訳 |
| `packages/client-runtime/src/state/sharedSettings.test.ts` (404) | — | 対象外：今回の指示では M3 の Git/PR/usage/全設定・更新配布 UI は実装しない。会話に必要な契約・回復経路は除外しない。 |
| `packages/client-runtime/src/state/sharedSettings.ts` (178) | — | 対象外：今回の指示では M3 の Git/PR/usage/全設定・更新配布 UI は実装しない。会話に必要な契約・回復経路は除外しない。 |
| `packages/client-runtime/src/state/shell-sync.test.ts` (956) | — | 未翻訳 |
| `packages/client-runtime/src/state/shell.test.ts` (234) | — | 未翻訳 |
| `packages/client-runtime/src/state/shell.ts` (453) | — | 未翻訳 |
| `packages/client-runtime/src/state/shellCommands.ts` (16) | — | 未翻訳 |
| `packages/client-runtime/src/state/shellReducer.test.ts` (450) | — | 未翻訳 |
| `packages/client-runtime/src/state/shellReducer.ts` (151) | — | 未翻訳 |
| `packages/client-runtime/src/state/shellSnapshotHttp.ts` (94) | — | 未翻訳 |
| `packages/client-runtime/src/state/snapshots.ts` (20) | — | 未翻訳 |
| `packages/client-runtime/src/state/sourceControl.test.ts` (170) | — | 未翻訳 |
| `packages/client-runtime/src/state/sourceControl.ts` (86) | — | 未翻訳 |
| `packages/client-runtime/src/state/subagentDisplay.test.ts` (166) | — | 未翻訳 |
| `packages/client-runtime/src/state/subagentDisplay.ts` (132) | — | 未翻訳 |
| `packages/client-runtime/src/state/subagentRuntime.ts` (147) | — | 未翻訳 |
| `packages/client-runtime/src/state/terminal.ts` (97) | — | 未翻訳 |
| `packages/client-runtime/src/state/terminalOutput.ts` (320) | — | 未翻訳 |
| `packages/client-runtime/src/state/terminalSession.test.ts` (494) | — | 未翻訳 |
| `packages/client-runtime/src/state/terminalSession.ts` (213) | — | 未翻訳 |
| `packages/client-runtime/src/state/threadCheckpoints.ts` (52) | — | 未翻訳 |
| `packages/client-runtime/src/state/threadCommands.test.ts` (377) | — | 未翻訳 |
| `packages/client-runtime/src/state/threadCommands.ts` (487) | — | 未翻訳 |
| `packages/client-runtime/src/state/threadDetail.test.ts` (328) | — | 未翻訳 |
| `packages/client-runtime/src/state/threadDetail.ts` (199) | — | 未翻訳 |
| `packages/client-runtime/src/state/threadExecution.test.ts` (679) | — | 未翻訳 |
| `packages/client-runtime/src/state/threadExecution.ts` (388) | — | 未翻訳 |
| `packages/client-runtime/src/state/threadFeedback.test.ts` (176) | — | 未翻訳 |
| `packages/client-runtime/src/state/threadFeedback.ts` (119) | — | 未翻訳 |
| `packages/client-runtime/src/state/threadHistoryController.test.ts` (63) | — | 未翻訳 |
| `packages/client-runtime/src/state/threadHistoryController.ts` (93) | — | 未翻訳 |
| `packages/client-runtime/src/state/threadHistoryHttp.ts` (40) | — | 未翻訳 |
| `packages/client-runtime/src/state/threadHistoryMerge.test.ts` (416) | — | 未翻訳 |
| `packages/client-runtime/src/state/threadHistoryMerge.ts` (156) | — | 未翻訳 |
| `packages/client-runtime/src/state/threadInbox.test.ts` (86) | — | 未翻訳 |
| `packages/client-runtime/src/state/threadInbox.ts` (145) | — | 未翻訳 |
| `packages/client-runtime/src/state/threadLifecycle.ts` (102) | — | 未翻訳 |
| `packages/client-runtime/src/state/threadRelationships.test.ts` (505) | — | 未翻訳 |
| `packages/client-runtime/src/state/threadRelationships.ts` (251) | — | 未翻訳 |
| `packages/client-runtime/src/state/threadRequests.test.ts` (153) | — | 未翻訳 |
| `packages/client-runtime/src/state/threadRequests.ts` (152) | — | 未翻訳 |
| `packages/client-runtime/src/state/threadRetention.ts` (3) | — | 未翻訳 |
| `packages/client-runtime/src/state/threadSearch.test.ts` (130) | — | 未翻訳 |
| `packages/client-runtime/src/state/threadSearch.ts` (85) | — | 未翻訳 |
| `packages/client-runtime/src/state/threadSettled.ts` (354) | — | 未翻訳 |
| `packages/client-runtime/src/state/threadShell.test.ts` (201) | — | 未翻訳 |
| `packages/client-runtime/src/state/threadShell.ts` (229) | — | 未翻訳 |
| `packages/client-runtime/src/state/threadSnapshotHttp.ts` (97) | — | 未翻訳 |
| `packages/client-runtime/src/state/threadSnoozed.test.ts` (369) | — | 未翻訳 |
| `packages/client-runtime/src/state/threadSort.test.ts` (513) | — | 未翻訳 |
| `packages/client-runtime/src/state/threadSort.ts` (400) | — | 未翻訳 |
| `packages/client-runtime/src/state/threadState.ts` (25) | — | 未翻訳 |
| `packages/client-runtime/src/state/threadSubagents.test.ts` (151) | — | 未翻訳 |
| `packages/client-runtime/src/state/threadSubagents.ts` (90) | — | 未翻訳 |
| `packages/client-runtime/src/state/threadWorkflows.test.ts` (424) | — | 未翻訳 |
| `packages/client-runtime/src/state/threadWorkflows.ts` (190) | — | 未翻訳 |
| `packages/client-runtime/src/state/threads-atoms.test.ts` (598) | — | 未翻訳 |
| `packages/client-runtime/src/state/threads-sync.test.ts` (1915) | — | 未翻訳 |
| `packages/client-runtime/src/state/threads.ts` (1009) | — | 未翻訳 |
| `packages/client-runtime/src/state/turnItemPresentation.test.ts` (104) | — | 未翻訳 |
| `packages/client-runtime/src/state/turnItemPresentation.ts` (49) | — | 未翻訳 |
| `packages/client-runtime/src/state/usage.test.ts` (323) | — | 対象外：今回の指示では M3 の Git/PR/usage/全設定・更新配布 UI は実装しない。会話に必要な契約・回復経路は除外しない。 |
| `packages/client-runtime/src/state/usage.ts` (135) | — | 対象外：今回の指示では M3 の Git/PR/usage/全設定・更新配布 UI は実装しない。会話に必要な契約・回復経路は除外しない。 |
| `packages/client-runtime/src/state/vcs.test.ts` (645) | — | 対象外：今回の指示では M3 の Git/PR/usage/全設定・更新配布 UI は実装しない。会話に必要な契約・回復経路は除外しない。 |
| `packages/client-runtime/src/state/vcs.ts` (363) | — | 対象外：今回の指示では M3 の Git/PR/usage/全設定・更新配布 UI は実装しない。会話に必要な契約・回復経路は除外しない。 |
| `packages/client-runtime/src/state/vcsAction.test.ts` (720) | — | 対象外：今回の指示では M3 の Git/PR/usage/全設定・更新配布 UI は実装しない。会話に必要な契約・回復経路は除外しない。 |
| `packages/client-runtime/src/state/vcsAction.ts` (595) | — | 対象外：今回の指示では M3 の Git/PR/usage/全設定・更新配布 UI は実装しない。会話に必要な契約・回復経路は除外しない。 |
| `packages/client-runtime/src/state/vcsCommandScheduler.ts` (13) | — | 対象外：今回の指示では M3 の Git/PR/usage/全設定・更新配布 UI は実装しない。会話に必要な契約・回復経路は除外しない。 |
| `packages/client-runtime/src/state/vcsRef.ts` (9) | — | 対象外：今回の指示では M3 の Git/PR/usage/全設定・更新配布 UI は実装しない。会話に必要な契約・回復経路は除外しない。 |
| `packages/client-runtime/src/state/vcsRefInvalidation.ts` (83) | — | 対象外：今回の指示では M3 の Git/PR/usage/全設定・更新配布 UI は実装しない。会話に必要な契約・回復経路は除外しない。 |
| `packages/client-runtime/src/t3ToolSummary.test.ts` (251) | — | 未翻訳 |
| `packages/client-runtime/src/t3ToolSummary.ts` (403) | — | 未翻訳 |
| `packages/client-runtime/src/textPaste.test.ts` (156) | — | 未翻訳 |
| `packages/client-runtime/src/textPaste.ts` (76) | — | 未翻訳 |
| `packages/client-runtime/src/threadPullRequestCompatibility.test.ts` (133) | — | 対象外：今回の指示では M3 の Git/PR/usage/全設定・更新配布 UI は実装しない。会話に必要な契約・回復経路は除外しない。 |
| `packages/client-runtime/src/threadPullRequestCompatibility.ts` (75) | — | 対象外：今回の指示では M3 の Git/PR/usage/全設定・更新配布 UI は実装しない。会話に必要な契約・回復経路は除外しない。 |
| `packages/client-runtime/src/userMessage.test.ts` (70) | — | 未翻訳 |
| `packages/client-runtime/src/userMessage.ts` (31) | — | 未翻訳 |
| `packages/client-runtime/src/voice-input/controller.test.ts` (640) | — | 未翻訳 |
| `packages/client-runtime/src/voice-input/controller.ts` (515) | — | 未翻訳 |
| `packages/client-runtime/src/voice-input/index.ts` (20) | — | 未翻訳 |
| `packages/client-runtime/src/voice-input/transcription.ts` (37) | — | 未翻訳 |
| `packages/client-runtime/src/work-log/commandLabel.test.ts` (495) | — | 未翻訳 |
| `packages/client-runtime/src/work-log/commandLabel.ts` (1386) | — | 未翻訳 |
| `packages/client-runtime/src/work-log/itemDetail.ts` (248) | — | 未翻訳 |
| `packages/client-runtime/src/work-log/presentation.test.ts` (962) | — | 未翻訳 |
| `packages/client-runtime/src/work-log/presentation.ts` (756) | — | 未翻訳 |
| `packages/client-runtime/src/work-log/scrollAnchor.test.ts` (63) | — | 未翻訳 |
| `packages/client-runtime/src/work-log/scrollAnchor.ts` (33) | — | 未翻訳 |
| `packages/client-runtime/src/work-log/toolPresentation.test.ts` (107) | — | 未翻訳 |
| `packages/client-runtime/src/work-log/toolPresentation.ts` (131) | — | 未翻訳 |
| `packages/client-runtime/src/work-log/userInput.test.ts` (37) | — | 未翻訳 |
| `packages/client-runtime/src/work-log/userInput.ts` (44) | — | 未翻訳 |
| `packages/client-runtime/src/worktreeSetup.ts` (64) | — | 対象外：今回の指示では M3 の Git/PR/usage/全設定・更新配布 UI は実装しない。会話に必要な契約・回復経路は除外しない。 |
| `packages/client-runtime/tsconfig.json` (4) | — | 未翻訳 |
| `packages/client-runtime/vite.config.ts` (10) | — | 未翻訳 |

### `packages/contracts`

本体 63 ファイル / 24,888 行。テスト 36 ファイル / 7,142 行。testkit/fixture は別行で全件追跡する。

| T3 のファイル（行数） | 移植した T3 テスト | 状態・理由 |
|---|---|---|
| `packages/contracts/package.json` (34) | — | 未翻訳 |
| `packages/contracts/src/acpRegistry.test.ts` (180) | — | 対象外：T3 Connect/ブラウザ・Web 専用/外部サービスの契約。この実装の iroh/ネイティブ境界に該当。 |
| `packages/contracts/src/acpRegistry.ts` (347) | — | 対象外：T3 Connect/ブラウザ・Web 専用/外部サービスの契約。この実装の iroh/ネイティブ境界に該当。 |
| `packages/contracts/src/agentSessions.test.ts` (36) | — | 未翻訳 |
| `packages/contracts/src/agentSessions.ts` (111) | — | 未翻訳 |
| `packages/contracts/src/applicationEvent.test.ts` (63) | — | 未翻訳 |
| `packages/contracts/src/applicationEvent.ts` (124) | — | 未翻訳 |
| `packages/contracts/src/assets.test.ts` (60) | — | 未翻訳 |
| `packages/contracts/src/assets.ts` (320) | — | 未翻訳 |
| `packages/contracts/src/assistantCitations.ts` (31) | — | 未翻訳 |
| `packages/contracts/src/auth.ts` (355) | — | 対象外：T3 Connect/ブラウザ・Web 専用/外部サービスの契約。この実装の iroh/ネイティブ境界に該当。 |
| `packages/contracts/src/background.test.ts` (18) | — | 未翻訳 |
| `packages/contracts/src/background.ts` (110) | — | 未翻訳 |
| `packages/contracts/src/baseSchemas.ts` (223) | — | 未翻訳 |
| `packages/contracts/src/browserImport.ts` (165) | — | 対象外：T3 Connect/ブラウザ・Web 専用/外部サービスの契約。この実装の iroh/ネイティブ境界に該当。 |
| `packages/contracts/src/browserProfile.test.ts` (111) | — | 対象外：T3 Connect/ブラウザ・Web 専用/外部サービスの契約。この実装の iroh/ネイティブ境界に該当。 |
| `packages/contracts/src/browserProfile.ts` (99) | — | 対象外：T3 Connect/ブラウザ・Web 専用/外部サービスの契約。この実装の iroh/ネイティブ境界に該当。 |
| `packages/contracts/src/chatAttachment.test.ts` (82) | — | 未翻訳 |
| `packages/contracts/src/chatAttachment.ts` (260) | — | 未翻訳 |
| `packages/contracts/src/checkpointDiff.test.ts` (75) | — | 未翻訳 |
| `packages/contracts/src/checkpointDiff.ts` (65) | — | 未翻訳 |
| `packages/contracts/src/composerContext.test.ts` (271) | — | 未翻訳 |
| `packages/contracts/src/composerContext.ts` (298) | — | 未翻訳 |
| `packages/contracts/src/composerContextClipboard.ts` (27) | — | 未翻訳 |
| `packages/contracts/src/desktopAppActivation.ts` (53) | — | 対象外：T3 Connect/ブラウザ・Web 専用/外部サービスの契約。この実装の iroh/ネイティブ境界に該当。 |
| `packages/contracts/src/desktopBootstrap.ts` (31) | — | 対象外：T3 Connect/ブラウザ・Web 専用/外部サービスの契約。この実装の iroh/ネイティブ境界に該当。 |
| `packages/contracts/src/device.test.ts` (21) | — | 対象外：T3 Connect/ブラウザ・Web 専用/外部サービスの契約。この実装の iroh/ネイティブ境界に該当。 |
| `packages/contracts/src/device.ts` (583) | — | 対象外：T3 Connect/ブラウザ・Web 専用/外部サービスの契約。この実装の iroh/ネイティブ境界に該当。 |
| `packages/contracts/src/editor.ts` (232) | — | 対象外：T3 Connect/ブラウザ・Web 専用/外部サービスの契約。この実装の iroh/ネイティブ境界に該当。 |
| `packages/contracts/src/environment.test.ts` (99) | — | 未翻訳 |
| `packages/contracts/src/environment.ts` (253) | — | 未翻訳 |
| `packages/contracts/src/environmentHttp.test.ts` (62) | — | 対象外：T3 Connect/ブラウザ・Web 専用/外部サービスの契約。この実装の iroh/ネイティブ境界に該当。 |
| `packages/contracts/src/environmentHttp.ts` (660) | — | 対象外：T3 Connect/ブラウザ・Web 専用/外部サービスの契約。この実装の iroh/ネイティブ境界に該当。 |
| `packages/contracts/src/filesystem.test.ts` (33) | — | 未翻訳 |
| `packages/contracts/src/filesystem.ts` (67) | — | 未翻訳 |
| `packages/contracts/src/git.test.ts` (169) | — | 対象外：M3 の独立機能・契約。今回保留（会話から使う関連型は該当ファイルで個別に記録）。 |
| `packages/contracts/src/git.ts` (482) | — | 対象外：M3 の独立機能・契約。今回保留（会話から使う関連型は該当ファイルで個別に記録）。 |
| `packages/contracts/src/index.ts` (62) | — | 未翻訳 |
| `packages/contracts/src/ipc.test.ts` (38) | — | 未翻訳 |
| `packages/contracts/src/ipc.ts` (1391) | — | 未翻訳 |
| `packages/contracts/src/keybindings.test.ts` (322) | — | 対象外：T3 Connect/ブラウザ・Web 専用/外部サービスの契約。この実装の iroh/ネイティブ境界に該当。 |
| `packages/contracts/src/keybindings.ts` (218) | — | 対象外：T3 Connect/ブラウザ・Web 専用/外部サービスの契約。この実装の iroh/ネイティブ境界に該当。 |
| `packages/contracts/src/model.ts` (232) | — | 未翻訳 |
| `packages/contracts/src/modelSelection.test.ts` (124) | — | 未翻訳 |
| `packages/contracts/src/modelSelection.ts` (63) | — | 未翻訳 |
| `packages/contracts/src/orchestrationDispatch.test.ts` (31) | — | 未翻訳 |
| `packages/contracts/src/orchestrationDispatch.ts` (16) | — | 未翻訳 |
| `packages/contracts/src/orchestrationProject.ts` (28) | — | 未翻訳 |
| `packages/contracts/src/orchestrationV2.test.ts` (1250) | — | 未翻訳 |
| `packages/contracts/src/orchestrationV2.ts` (3405) | — | 未翻訳 |
| `packages/contracts/src/orchestratorMcp.test.ts` (166) | — | 未翻訳 |
| `packages/contracts/src/orchestratorMcp.ts` (589) | — | 未翻訳 |
| `packages/contracts/src/preview.test.ts` (364) | — | 対象外：T3 Connect/ブラウザ・Web 専用/外部サービスの契約。この実装の iroh/ネイティブ境界に該当。 |
| `packages/contracts/src/preview.ts` (354) | — | 対象外：T3 Connect/ブラウザ・Web 専用/外部サービスの契約。この実装の iroh/ネイティブ境界に該当。 |
| `packages/contracts/src/previewAutomation.ts` (953) | — | 対象外：T3 Connect/ブラウザ・Web 専用/外部サービスの契約。この実装の iroh/ネイティブ境界に該当。 |
| `packages/contracts/src/project.test.ts` (288) | — | 未翻訳 |
| `packages/contracts/src/project.ts` (536) | — | 未翻訳 |
| `packages/contracts/src/projectClone.ts` (122) | — | 対象外：M3 の独立機能・契約。今回保留（会話から使う関連型は該当ファイルで個別に記録）。 |
| `packages/contracts/src/provider.test.ts` (318) | — | 未翻訳 |
| `packages/contracts/src/provider.ts` (164) | — | 未翻訳 |
| `packages/contracts/src/providerInstance.test.ts` (206) | — | 未翻訳 |
| `packages/contracts/src/providerInstance.ts` (168) | — | 未翻訳 |
| `packages/contracts/src/providerPolicy.ts` (84) | — | 未翻訳 |
| `packages/contracts/src/providerRuntime.test.ts` (230) | — | 未翻訳 |
| `packages/contracts/src/providerRuntime.ts` (1204) | — | 未翻訳 |
| `packages/contracts/src/providerSetup.test.ts` (53) | — | 未翻訳 |
| `packages/contracts/src/providerSetup.ts` (255) | — | 未翻訳 |
| `packages/contracts/src/providerUsageLimits.ts` (176) | — | 未翻訳 |
| `packages/contracts/src/pullRequest.test.ts` (340) | — | 対象外：M3 の独立機能・契約。今回保留（会話から使う関連型は該当ファイルで個別に記録）。 |
| `packages/contracts/src/pullRequest.ts` (1424) | — | 対象外：M3 の独立機能・契約。今回保留（会話から使う関連型は該当ファイルで個別に記録）。 |
| `packages/contracts/src/relay.test.ts` (61) | — | 対象外：T3 Connect/ブラウザ・Web 専用/外部サービスの契約。この実装の iroh/ネイティブ境界に該当。 |
| `packages/contracts/src/relay.ts` (1194) | — | 対象外：T3 Connect/ブラウザ・Web 専用/外部サービスの契約。この実装の iroh/ネイティブ境界に該当。 |
| `packages/contracts/src/relayClient.ts` (63) | — | 対象外：T3 Connect/ブラウザ・Web 専用/外部サービスの契約。この実装の iroh/ネイティブ境界に該当。 |
| `packages/contracts/src/remoteAccess.ts` (68) | — | 対象外：T3 Connect/ブラウザ・Web 専用/外部サービスの契約。この実装の iroh/ネイティブ境界に該当。 |
| `packages/contracts/src/resourceTelemetry.ts` (522) | — | 対象外：T3 Connect/ブラウザ・Web 専用/外部サービスの契約。この実装の iroh/ネイティブ境界に該当。 |
| `packages/contracts/src/review.ts` (70) | — | 未翻訳 |
| `packages/contracts/src/rpc.test.ts` (70) | — | 未翻訳 |
| `packages/contracts/src/rpc.ts` (1877) | — | 未翻訳 |
| `packages/contracts/src/scheduledTask.test.ts` (52) | — | 対象外：M3 の独立機能・契約。今回保留（会話から使う関連型は該当ファイルで個別に記録）。 |
| `packages/contracts/src/scheduledTask.ts` (185) | — | 対象外：M3 の独立機能・契約。今回保留（会話から使う関連型は該当ファイルで個別に記録）。 |
| `packages/contracts/src/server.test.ts` (268) | — | 未翻訳 |
| `packages/contracts/src/server.ts` (955) | — | 未翻訳 |
| `packages/contracts/src/settings.test.ts` (1107) | — | 対象外：M3 の独立機能・契約。今回保留（会話から使う関連型は該当ファイルで個別に記録）。 |
| `packages/contracts/src/settings.ts` (1807) | — | 対象外：M3 の独立機能・契約。今回保留（会話から使う関連型は該当ファイルで個別に記録）。 |
| `packages/contracts/src/sourceControl.ts` (188) | — | 対象外：M3 の独立機能・契約。今回保留（会話から使う関連型は該当ファイルで個別に記録）。 |
| `packages/contracts/src/t3ProjectFile.test.ts` (71) | — | 未翻訳 |
| `packages/contracts/src/t3ProjectFile.ts` (143) | — | 未翻訳 |
| `packages/contracts/src/terminal.test.ts` (347) | — | 未翻訳 |
| `packages/contracts/src/terminal.ts` (381) | — | 未翻訳 |
| `packages/contracts/src/threadMetadataMcp.test.ts` (100) | — | 未翻訳 |
| `packages/contracts/src/threadMetadataMcp.ts` (122) | — | 未翻訳 |
| `packages/contracts/src/threadPullRequest.test.ts` (56) | — | 対象外：M3 の独立機能・契約。今回保留（会話から使う関連型は該当ファイルで個別に記録）。 |
| `packages/contracts/src/threadPullRequest.ts` (128) | — | 対象外：M3 の独立機能・契約。今回保留（会話から使う関連型は該当ファイルで個別に記録）。 |
| `packages/contracts/src/threadSearch.ts` (42) | — | 未翻訳 |
| `packages/contracts/src/threadTitle.ts` (10) | — | 未翻訳 |
| `packages/contracts/src/usage.ts` (246) | — | 対象外：M3 の独立機能・契約。今回保留（会話から使う関連型は該当ファイルで個別に記録）。 |
| `packages/contracts/src/usageLimitSourceId.ts` (9) | — | 対象外：M3 の独立機能・契約。今回保留（会話から使う関連型は該当ファイルで個別に記録）。 |
| `packages/contracts/src/vcs.ts` (287) | — | 対象外：M3 の独立機能・契約。今回保留（会話から使う関連型は該当ファイルで個別に記録）。 |
| `packages/contracts/src/worktreeMcp.ts` (127) | — | 対象外：M3 の独立機能・契約。今回保留（会話から使う関連型は該当ファイルで個別に記録）。 |
| `packages/contracts/src/worktreeSetup.ts` (124) | — | 対象外：M3 の独立機能・契約。今回保留（会話から使う関連型は該当ファイルで個別に記録）。 |
| `packages/contracts/tsconfig.json` (5) | — | 未翻訳 |

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
| SDK `forkSession` | `claude/sdk/bridge.test.mjs` が実際の固定版 SDK で境界・UUID/親子関係・予約した会話 ID・不存在の境界を検証。`agent-runtime` の replay と予約前保存防止試験を維持。project key は `claude_fork::tests`。 |
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
- `ThreadLaunchService.test.ts` の `reuseExistingThread`、自動化・送信元の属性、import した native session（`Command::Import` で取り込む）、attachment の取り込み（RPC の責務）、記録前に失敗した worktree の削除（部分的な checkout の削除は Host の `create_worktree` が行う。runtime は記録に失敗した worktree を削除する）。
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
| contracts `chatAttachment.ts` | `attachments_follow_the_reference_schemas_and_image_budget`、`attachment_ids_follow_the_reference_schema`（proptest）。Host の添付 ID は `pending-` / `chat-` の形で schema に合う（ARCHITECTURE 2026-10-07）。 |

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
| `provider/Drivers/ClaudeSkills.test.ts`（discovery、優先順位、malformed、colon を含む説明、YAML 1.1 の boolean、skillOverrides、repository root の設定） | `skills::tests` と `claude::skills::tests`。frontmatter は T3 の YAML parser ではなく、Claude Code が使う key だけを読む。多 byte 文字と行頭の扱いは `skill_frontmatter_with_multibyte_text_is_read`、`colon_scalars_are_quoted_on_every_line_start` と、T3 の正規表現を参照実装にした proptest（`frontmatter_block_matches_the_pattern`、`any_frontmatter_block_matches_the_pattern`、`colon_scalars_are_quoted_as_the_replacement_does` ほか）。 |
| `ClaudeAdapterV2.ts` openQuery（再利用と置き換え、background work による拒否）、forkThread | `claude_replaces_its_process_for_another_model_or_mode`、`a_claude_model_change_during_a_turn_applies_from_the_next_turn`、`claude_keeps_a_process_with_background_work_when_the_selection_changes`、`a_claude_native_fork_copies_the_transcript_through_the_head`（source のプロセスを閉じる）。 |
| `ContextHandoffBudget.ts` handoffTokenCapConfig、`ClaudeAdapterV2.ts` getModelContextWindow | `the_provider_catalog_has_the_reference_handoff_limits`。 |

### Host の T3 化（2026-10-06）

配信形、同期・RPC の細部、chats のフォルダ、worktree、setup、checkpoint の一覧、rollback の受付、Host の sweep と設定を T3 に合わせた。期待値は T3 のまま。

| T3 の原本 | 新設計の検証 |
| --- | --- |
| `WireProjection.test.ts`（serializes once と raw output を serialize しない 2 件を除く全件） | `sync::wire::tests` の `redacts_tool_output_while_keeping_its_input`、`preserves_provider_notices_in_bounded_items_and_live_events`、`omits_oversized_dynamic_tool_results_without_mutating_persistence_data`、`omits_even_small_dynamic_tool_results_while_retaining_input`、`keeps_undefined_dynamic_input_intact`、`preserves_bounded_input_summary_normalization`（6 ケース）、`uses_encoded_json_bytes_for_strings_near_the_dynamic_value_limit`、`summarizes_an_oversized_structured_input`、`truncates_detail_at_a_utf8_boundary_without_changing_the_source`、`omits_command_output_of_every_size`、`bounds_fetched_command_input_without_changing_persistence`、`keeps_failure_evidence_without_retaining_command_output`、`omits_inline_file_bodies_but_preserves_file_identity`、`retains_only_result_identities_and_failure_metadata_in_live_tool_events`。handoff の件は `sync::thread::tests::keeps_copied_handoff_transcripts_out_of_activity_items_and_live_events`。T3 の件のうち JavaScript の `toJSON` 呼出し回数を数える 2 件は Rust に対応する経路がない。 |
| `shared/toolOutput.ts`（compactDynamicToolOutput、toolOutputIndicatesFailure） | `compacts_mcp_envelopes_like_t3`、`recognizes_failure_text_like_t3`。 |
| `ThreadStream.test.ts`（10 MiB の raw replay） | `rejects_a_10_mib_raw_replay_even_when_its_projected_form_fits_the_wire_budget`。T3 と同じく dynamic tool の 10 MiB の出力で確かめる（以前は配信形がなかったため transcript に置き換えていた）。 |
| `ThreadManagementService.ts` getTurnItem（detail read）、`WireProjection.ts` projectTurnItemForDetail の subagent | `query::tests::reads_withheld_command_output_on_demand_within_its_bound`、`reads_a_subagent_task_on_demand_within_the_detail_bound`。 |
| 配信形の事実の fold | `command_output_facts_fold_into_the_projected_item`、proptest `bounded_snapshot_then_later_facts_matches_the_full_fold_within_the_window`（配信形の snapshot と事実で、dynamic tool を含む）。 |
| frame の上限（T3 にない） | `stops_a_turn_window_at_the_frame_budget_so_every_thread_opens`、`shortens_a_single_finished_row_that_alone_exceeds_the_frame_budget`、`shortens_a_finished_plan_whose_markdown_alone_exceeds_the_frame_budget`、proptest `a_fitted_row_fits_its_budget`。 |
| `LiveStreamBudget.ts`（LiveStreamBufferError） | `sync::live` のテスト、`closes_a_subscriber_that_falls_behind`、`closes_a_subscriber_that_falls_behind_the_hub`、`closes_a_slow_subscriber_instead_of_waiting_for_it`（`overflowed` の確認）。 |
| `shellReducer.ts:103`（snapshot 以下の sequence を捨てる） | `reports_project_changes_from_the_directory`、`orders_project_changes_between_thread_commits`、`resumes_with_project_changes_made_while_disconnected`。 |
| `ProjectionStore.ts:4978`（shell の行の順） | `lists_snapshot_rows_by_update_time_then_thread_id`。 |
| `ThreadSearch.ts`（UTF-16 の長さと snippet）、contracts `threadSearch.ts` | `validates_the_query_and_limit`（UTF-16 の長さ）、`bounds_snippets_in_utf16_units`。 |
| `CheckpointDiffQuery.ts`（ignoreWhitespace の既定）、`GitVcsDriver.ts:1205` | `ErrorCode` の往復（`diff_failed`）。省略時の `ignore_whitespace` を true にする部分に専用のテストはない。 |
| `composerContextReferences.test.ts`（href、label、projection の全件）、contracts `composerContext.ts`（record の schema） | `composer::tests`（`attachment_records_keep_ids_the_schema_accepts` を含む）、`tests::a_message_reaches_the_provider_with_its_context_projected`、`ThreadMessageIntake.ts` の claim の付け替えは Host の `claimed_uploads_keep_their_context_records`。 |
| `RepositoryIdentityResolver.test.ts`（全 10 件） | `repository::tests`（`refreshes_the_git_root_only_when_requested`、`retries_git_root_discovery_after_the_negative_ttl`、`refreshes_the_primary_upstream_after_add_or_replace_before_cache_expiry`、`keeps_null_identities_cached_across_repeated_resolves_until_the_negative_ttl_expires`、`refreshes_cached_identities_after_the_positive_ttl_…` ほか）、`conversation::tests::shell_projects_carry_their_repository_identity`、`subscribed_shells_see_a_changed_remote_once_the_cached_identity_expires`。 |
| `ProjectEnrichmentService.test.ts`（favicon の 2 件を除く 4 件） | `repository::enrichment::tests`。null と失敗を通知しない点は ARCHITECTURE 2026-10-07 の項による。 |
| `SourceControlDiscovery.test.ts` の Forgejo 照合 2 件、packages/shared `sourceControl.test.ts` の provider 判定、`server.ts:285-319` の web URL | `repository::forgejo::tests`、`repository::tests::detects_*`・`matches_self_hosted_providers_by_complete_dns_labels`・`preserves_ports_while_classifying_by_hostname`、`gives_a_remote_a_logged_in_server_its_web_address`。 |
| `ThreadSettlementService.test.ts`（worker の非活動 2 件） | `sweep::tests::settles_only_the_project_opted_in_while_environment_settlement_is_disabled`、`dispatches_the_last_activity_time_with_the_snapshot_guard`、`settles_after_the_default_three_days_of_inactivity`、`sweeps_read_live_unarchived_rows`。pull request と terminal の件は Host が pull request と thread の terminal を持たないため対象外。 |
| `UsageLimitRecoveryWorker.ts` | `recovers_usage_limits_only_when_the_settings_opt_in`。判定は既存の `settlement::tests`。 |
| contracts `settings.ts`（既定値と project の上書き）、`projectSettings` の解決 | `conversation_settings_persist_with_t3_defaults_and_bounds`、`project_overrides_take_precedence_and_absent_values_inherit`。 |
| `ThreadTitleLinks.test.ts`（全 3 件）、GitHub / GitLab の `resolveLink` | `title_links::tests`。 |
| `WorktreeSetupTracker.test.ts`（全 8 件） | `setup::tests`。stream は watch channel の最新値で確かめる。 |
| `ThreadLaunchService.ts` の setup card と取消 | `tracks_a_worktree_setup_on_its_card_until_the_turn_starts`、`an_awaited_setup_that_fails_fails_the_run_and_an_asynchronous_one_does_not`、`cancelling_a_setup_removes_its_worktree_and_fails_the_run`、Host の `a_worktree_launch_runs_the_projects_setup_script`（card の配信と取消）。 |
| `ProjectSetupScriptRunner.test.ts`、`ProjectSetupScriptRunner.ts` の出力行と completion sentinel | `conversation::setup::tests` の `setup_runs_in_the_threads_terminal_and_reports_its_exit_code`、`observed_output_splits_redraws_and_ends_with_the_terminal`、`the_sentinel_settles_and_the_wrapper_echo_is_hidden`、`wrapped_commands_report_their_exit_code_after_the_block`、`setup_terminals_get_the_project_paths_without_color`、`the_first_script_run_on_worktree_creation_is_the_setup`、`output_lines_drop_terminal_control_and_stay_bounded`。 |
| `ManagedProjectFolders.test.ts`（folderForThread、claimFreeFolder、Scratch の提供条件） | `gives_each_chat_thread_its_own_folder_named_from_its_message`、`claims_a_folder_after_the_chats_folder_is_deleted`、`offers_chats_under_the_data_directory_outside_a_checkout`、`offers_no_chats_when_the_data_directory_sits_inside_a_checkout`、`launch::tests::runs_a_chat_thread_launched_at_the_root_in_its_own_folder`、Host の `a_chat_launched_at_the_root_runs_in_a_folder_of_its_own`。named project の作成は `projects::named::tests` の 7 件（`starts_a_named_project_as_a_committed_repository_and_suffixes_a_taken_name`、`gives_concurrent_named_projects_with_the_same_name_distinct_folders`、`keeps_a_named_project_and_reports_why_when_git_cannot_commit`、`keeps_a_folder_that_another_project_owns_when_the_create_conflicts`、`removes_the_folder_when_the_project_create_is_rejected_for_another_reason`、`removes_the_folder_when_the_create_is_cancelled_before_the_project_exists`、`removes_the_folder_when_the_repository_cannot_be_made`）。Scratch project の作成と icon の設定は対象外。 |
| `packages/shared/src/path.test.ts` の newProjectFolderName | `derives_the_folder_name_of_a_project_started_from_a_name`、proptest `folder_names_are_one_short_portable_segment`・`lowercase_words_keep_their_text`。 |
| `ThreadLaunchService.ts:331-364`、`GitVcsDriverCore.test.ts`（fetch の診断） | `worktrees::tests::starting_from_origin_falls_back_to_the_local_base`、`a_thread_keeps_its_checkout_when_its_creation_is_retried`、`fetch_failures_report_a_fixed_diagnosis`、`a_failed_origin_fetch_reports_its_diagnosis_and_checks_out_nothing`。 |
| `GitVcsDriverCore.ts` createWorktree（`worktree add -b` が名前を検証する、submodule の初期化）、removeWorktree（force）、renameBranch と resolveAvailableBranchName | `git::tests::branch_names_follow_git_check_ref_format`、proptest `branch_names_match_git`（`git check-ref-format` を参照にする）、`worktrees::tests::option_shaped_branch_and_base_names_change_no_branch`、`new_checkouts_initialize_submodules_recursively_best_effort`、`a_subproject_checkout_is_recorded_and_abandoned_by_its_root`、`renaming_a_launch_branch_picks_a_free_name_and_keeps_the_checkout`。 |
| `ThreadLaunchService.ts:378`（onWorktreeClaimed）と取消の後始末、Effect の割込みによる Git の停止 | `launch::tests::cancelling_during_the_checkout_waits_for_it_and_removes_what_it_made`、`worktrees::tests::a_cancelled_checkout_stops_git_and_leaves_nothing_behind`、`an_abandoned_checkout_stops_before_the_state_is_released`。 |
| `ThreadLaunchService.test.ts` の branch 名（全 5 件）、`TextGenerationPrompts.test.ts`「buildBranchNamePrompt」（全 5 件）、packages/shared `git.test.ts` の formatGeneratedBranchName（全件）と isTemporaryWorktreeBranch | `launch::tests::renames_a_temporary_branch_off_the_provisioning_critical_path`、`keeps_an_explicit_branch_name_instead_of_generating_one`、`keeps_the_temporary_branch_when_branch_generation_fails`、`renames_a_temporary_branch_on_an_existing_worktree_to_a_generated_name`、`a_retry_reuses_a_recorded_worktree_without_undoing_its_branch_rename`、`names_follow_the_projects_branch_naming`、`branch::tests`（prompt・整形・一時 branch の判定と proptest 3 件）、`settings_tests::project_overrides_take_precedence_and_absent_values_inherit`（命名の設定）。T3 の `t3code` の prefix と一時 branch の形は Host の `agent` と `agent/session-<12 桁>` に読み替えた。T3 のテストは整形済みの名前を返す TextGeneration を mock するので、Host のテストは semantic の命名で同じ期待値（`generated-branch`）を確かめる。 |
| `Orchestrator.ts:8393-8569` dispatchCheckpointRollback と `EffectOutbox.ts:280-315` の thread ごとの順序 | `tests::a_rollback_requested_while_a_run_waits_executes_after_that_run_starts`、`a_rollback_behind_a_stopped_waiting_run_executes_before_a_later_message`、`executor::tests::rollback::a_rollback_requested_while_a_message_waits_executes_after_its_turn_starts`。 |
| `ThreadMessageIntake.ts:53-86`、`provider/userInputAttachments.ts:31-48`（回答の添付の確認） | `runtime::tests::an_answer_with_an_unavailable_attachment_fails_without_a_receipt`（`ThreadMessageIntake.test.ts`「a retried response re-claims the preserved pending uploads」に相当）。 |
| `CheckpointService.ts:376-483`（capture の files）、`checkpointing/Diffs.test.ts` | `executor::tests::checkpoint::a_captured_turn_records_the_files_changed_since_the_previous_checkpoint`、`a_captured_checkpoint_records_its_file_summary_and_baselines_record_none`、`checkpoints::tests::checkpoint_files_summarize_the_changes_between_two_refs`、`numstat_summaries_follow_the_reference_parser`（並び順まで比較）、`numstat_summaries_sort_like_locale_compare`。 |
| `Orchestrator.ts:8477-8498`、`EffectWorker.ts:362-392`、`CheckpointRestoreSafety.test.ts` | `tests::rollback::a_restore_the_host_refused_is_rejected_at_admission`、`the_host_refusal_is_not_part_of_the_rollback_identity`、`executor::tests::rollback::a_workspace_shared_after_admission_fails_the_rollback_after_its_retries`、`rewinds_safely`・`preserves_overlapping_workspace_files`（受付での拒否と effect 時の ROLLBACK_FAILED_MESSAGE）。 |
| command の再送（`Orchestrator.ts:203-226`） | `tests::metadata::a_resent_update_without_the_host_filled_root_replays_its_result`。 |

対象外のまま残したもの: `reuseExistingThread`、pull request の同期と merge による settle、環境の既定 scripts、`worktreeSubmodules` の設定とそれを持つ project file、branch 名を書く model の選択（sourceControlWriterModelSelection）。

### MCP toolkit の T3 化（2026-10-06）

`mcp/toolkits/orchestrator`・`thread`・`project` と `McpHttpServer.ts` の登録を `host-daemon/src/conversation/tools` に移植した。期待値は T3 のまま。T3 のテストが mock した ThreadManagementService・ProviderRegistry は `Orchestration` trait の fake に置き換え、mock の projection は同じ意味の domain の state で作る。

| T3 の原本 | 新設計の検証 |
| --- | --- |
| `OrchestratorMcpService.test.ts`（ack の再試行、restart で切れた子、task_cancel 3 件） | `retries_terminal_acknowledgement_with_a_fresh_command_id`、`reports_a_restart_cut_child_as_working_until_its_continuation_settles`、`does_not_dispose_delivery_when_a_nonterminal_task_has_no_active_child_run`、`does_not_dispose_delivery_when_the_child_interrupt_fails`、`returns_cancel_requested_when_post_interrupt_disposal_fails`。restart で切れた子は、T3 の delegatedTaskResultPending の代わりに親の task に結果がまだ公開されていないことで判断する（domain は継続が決まるまで結果を送らない）。 |
| `OrchestratorMcpService.test.ts`「provider resolution」5 件 | `advertises_orchestration_capability_from_registered_adapters`、`delegates_to_an_instance_whose_adapter_resolves_and_by_driver_kind`、`rejects_delegation_to_a_provider_without_a_registered_adapter`、`inherits_an_available_parent_instance_for_driver_targets_and_otherwise_selects_a_healthy_peer`。委任先の Antigravity は、この実装の driver（Codex / Claude）の別 instance に置き換えた。 |
| `OrchestratorMcpService.activity.test.ts` 4 件 | `read_thread_prefers_the_activity_run_status_over_a_newer_cancelled_queued_run`（running と waiting）、`task_status_returns_the_tasks_provider_instance_rather_than_the_driver_kind`、`read_thread_reaches_a_thread_the_user_attached_as_context_but_not_one_an_agent_attached`。 |
| `ThreadMetadataMcpService.test.ts`、`threadMetadataMcp.test.ts` | `thread_update_reports_an_absent_or_unreadable_calling_thread`、`thread_update_validates_each_action_and_links_pull_requests`。 |
| `toolkits/orchestrator/tools.test.ts`（schedule 以外）、`toolkits/core.test.ts`（名前・object root・`$ref` なし、storage の失敗を出さない） | `orchestrator_tool_guidance_matches_t3`、`publishes_unique_tool_names_with_reference_free_object_root_inputs`、`returns_a_bounded_public_failure_without_serializing_storage_causes`。credential の capability は Host に 1 種類しかないため capability_denied の検査は持たない。 |
| `toolkits/project/handlers.test.ts`、`project/handlers.ts` の create（scripts） | `launches_threads_from_a_full_access_caller_and_scratch_threads_into_chats`、`starts_a_project_from_just_a_title_when_workspace_root_is_omitted`、`creates_projects_from_a_path_and_rejects_fields_it_cannot_apply`（scripts の保存と結果、path と既定の model の受理）、`launches_claim_pending_uploads_into_the_new_thread_and_reject_other_attachments`。scratch は Chats project の root に launch し、thread ごとのフォルダで動く。 |
| `ClaudeAdapterV2.test.ts` の read-only 一覧と toolkit の照合、claudeMcpQueryOverrides | `a_read_only_claude_sandbox_pre_approves_every_read_only_tool`、`a_read_only_claude_sandbox_pre_approves_only_read_only_app_tools`、`a_claude_launch_pre_approves_the_app_tools_and_appends_the_instructions`。 |
| `provider/T3OrchestrationInstructions.test.ts`（provider 共通の 2 件） | `orchestration_instructions_distinguish_subagents_and_structured_schedules`。 |
| `OrchestratorMcpToolkit.integration.test.ts` の organize・委任・cancel・create_threads・send・queue・wait・interrupt の流れ | `agent_tools_delegate_create_queue_and_interrupt_through_the_runtime`（runtime と、`[hold]` のターンを止める Codex の fake）。schedule の手順は scheduler がないため未移植。 |
| ThreadManagementService の waitForThread / interruptThread、readThread の既定値 | `wait_selects_the_latest_run_clamps_its_budget_and_reports_timeouts`、`interrupt_picks_the_newest_interruptible_run_and_reports_t3_statuses`、`read_defaults_to_fifty_messages_of_twenty_thousand_units_and_pages_long_text`、`archived_callers_read_but_cannot_change_threads`、`list_orders_newest_first_and_filters_status_settlement_title_and_subagents`、`queue_tools_page_read_and_change_queued_messages`、`pending_request_tools_answer_only_open_user_questions`、`organize_maps_each_action_and_requires_snooze_time`、`search_returns_only_matches_of_the_calling_project`、`send_maps_t3_modes_and_rejects_escalation`、`transfers_and_configuration_read_the_addressed_thread`。 |
| `OrchestratorMcpToolkit.integration.test.ts` の同じ clientRequestId の send | `conversation::tests::a_retried_thread_send_returns_its_first_run_after_that_run_starts`、`tools::tests::a_retried_send_replays_its_receipt_after_the_target_starts_running`。 |
| Effect McpServer の結果の形と大きさ | `the_largest_valid_thread_read_reaches_the_provider_session`、`a_result_beyond_the_framing_guard_is_answered_with_the_internal_tool_error`。 |
| `SelectionRestart.integration.test.ts`「detaches the old provider session after an active provider handoff」と dispatchSteerIntoRun の instance 変更 | `detaches_the_old_provider_session_after_an_active_provider_handoff`、`a_steer_onto_another_instance_restarts_the_run_with_a_full_handoff`、`a_claude_run_cannot_be_steered_onto_another_instance`、`promoting_to_steer_after_a_provider_switch_restarts_on_the_new_instance`。 |

公開しない T3 の tool（裏付けの機能がない）：`schedule_task`・`list_scheduled_tasks`・`update_scheduled_task`・`delete_scheduled_task`・`run_scheduled_task_now`（scheduler）、`t3_project_update`（Host が変えられる project の設定は scripts だけで、title・workspaceRoot・既定の model などを持たない）、`t3_project_delete`・`t3_project_clone`（project の削除・clone）、`t3_worktree_*`、`t3_attachment_*`、`t3_thread_send_attachments`、`t3_environment_*`、`t3_preview_*`・`preview_*`、`device_*`、pull request の tool。`orchestrator_capabilities` の `scheduledTasks` は false にする。

### 段階 4: Host（2026-10-07）

| T3 の原本 | 新設計の検証 |
| --- | --- |
| `terminal/Manager.test.ts`（subprocess の activity、ps の共有 snapshot、snapshot 失敗時の状態保持、backoff、closeIdle、runtime env、attach と再起動、thread 単位の close） | `terminals::tests` の `running_shells_report_their_command_on_the_metadata_stream`、`one_process_snapshot_names_each_shells_command`、`a_childless_copy_of_the_shell_is_not_a_command`、`failed_snapshots_back_off_up_to_a_minute`、`close_idle_closes_quiet_shells_only`、`shells_drop_host_variables_and_take_the_callers`、`devices_share_a_threads_terminals_across_disconnects`、`exited_terminals_stay_until_closed_and_restart_on_request`、`thread_cleanup_closes_only_that_threads_terminals_and_their_jobs`。 |
| `contracts/terminal.test.ts`、`terminalLabels.ts` | `requests_validate_ids_sizes_and_environment`、`tab_labels_number_term_ids_and_keep_other_ids`。 |
| `Manager.test.ts`「subscribes terminal metadata…」と `ThreadSettlementService.ts` の closeIdleTerminals | `conversation::tests::settling_a_thread_closes_its_idle_terminals_on_the_metadata_stream`。 |
| `ProjectSetupScriptRunner.test.ts`、wrapCommandForCompletion | `conversation::setup::tests`（上の段階 3 の表を参照）。 |
| `CodexProvider.test.ts`（capability の対応、優先 default）、`ModelManifest.test.ts`（legacy の分類）、`ClaudeModelCatalog.test.ts`（版による絞り込みと upgrade の文面） | `model_catalog::tests`。manifest の差し替えを使う件は同梱の manifest で同じ規則を確かめる。 |
| `claudeModelOptions.test.ts` | `claude_models::tests::model_options_compile_suffixes_effort_and_settings`（manifest の descriptor から解決）。 |
| `shared/model.test.ts`「descriptor helpers」 | `agent_domain::options::tests::selections_resolve_to_listed_choices_or_the_default`。 |
| `ClaudeProvider.ts` の parseClaudeInitializationCommands と dedupeSlashCommands、`CodexProvider.ts` の parseCodexSkillsListResponse、`ProviderRegistry.ts` の upsertProviderWorkspaceSnapshot | `host_rpc::commands::tests`。Claude の skill の一覧は `claude::skills::tests`。 |
| `WorkspaceEntries.test.ts`「search」、`WorkspaceSearchIndex.test.ts` の画像検索 | `workspace_search::tests`。typo は fff の代わりに Host の規則で確かめる。 |
| `GitVcsDriverCore.test.ts`「review diff previews」の Changes 5 件と whitespace・単一 file、「repository status」、listRefs 2 件 | `vcs::tests`。 |
| `ProjectFaviconResolver.test.ts`（`t3.json` の 4 件を除く） | `favicon::tests` の `serves_repeated_resolves_from_the_cache_instead_of_walking_again`、`falls_back_at_once_when_a_cached_favicon_is_deleted`、`looks_again_for_an_icon_added_after_a_miss_once_the_miss_expires`、`prefers_well_known_favicon_files_in_order`、`uses_a_saved_icon`、`uses_a_saved_icon_outside_the_workspace`、`falls_back_when_the_saved_icon_is_missing_from_a_checkout`、`resolves_icon_links_from_project_source_files`、`resolves_icon_metadata_objects`（object-literal の 4 件）、`scans_large_icon_sources_without_an_icon_quickly`、`finds_nothing_without_an_icon_file`、`a_missing_root_fails`、`unreadable_candidates_and_sources_fail_the_lookup`、`skips_icon_links_outside_the_workspace`（外の href と後の source の 2 件）。cache の容量と失敗を残さないことは `failures_are_not_remembered_and_the_least_recently_used_answer_is_dropped`、正規表現は `reads_icon_links_like_the_reference_patterns`。`t3.json` の iconPath は Host に project file がないので対象外。 |
| `AssetAccess.test.ts` の project favicon（「issues project favicon capabilities with a signed fallback」「…for a saved override」「issues an exact capability for a saved favicon outside the workspace」「ignores a client favicon path hint」「keeps automatic favicon resolution separate from a saved override」「rejects a resolved project favicon with a non-image extension」） | `favicon::tests` の `names_the_icon_by_its_content_and_falls_back_when_it_is_deleted`、`serves_the_saved_icon_and_otherwise_the_discovered_one`（要求は project ID だけで path の hint を持てない）、`serves_a_saved_icon_outside_the_workspace`、`a_file_that_is_not_an_image_is_not_served`、`an_icon_linked_out_of_the_workspace_is_not_served`。RPC と保存は `conversation::tests::projects_keep_a_saved_icon_path_and_serve_their_icon`、`projects::tests::updates_store_trimmed_scripts_and_keep_omitted_fields`、wire は `operations::terminal_tests::project_favicon_requests_keep_their_wire_shape`。署名と期限の bucket は URL を使わないので対象外。 |
| `contracts/project.ts` ProjectFaviconPath | `agent_protocol::models::tests::saved_favicon_paths_are_trimmed_image_paths`。 |
| `pathExpansion.test.ts` expandHomePath、`WorkspacePaths.ts` normalizeWorkspaceRoot、`WorkspaceEntries.ts` browse の `~` | `projects::tests::a_leading_tilde_names_the_home_directory`、`a_project_added_at_the_tilde_is_the_home_directory`、`paths_fold_dots_without_the_filesystem`、`workspace_files::tests::listings_start_from_the_home_folder_for_a_leading_tilde`。 |
| `Orchestrator.ts` の thread.settle / unsettle と自動の unsettle、lineage | `agent_domain::shell::tests::unsettling_records_when_the_thread_left_settled`、`rows_name_how_a_thread_came_from_its_parent`。 |
| `Manager.test.ts` の BoundedTerminalHistory 3 件（「preserves line and byte limits across arbitrary chunks, Unicode, ANSI sequences, and clear」「bounds long partial lines and joins surrogate pairs across chunk boundaries」「preserves retained lines as older storage is compacted」） | `terminals::history::tests` の `keeps_the_newest_lines_and_bytes_across_any_chunks_and_clears`（proptest）、`bounds_long_partial_lines_and_joins_characters_split_between_chunks`（surrogate の代わりに chunk に分かれた UTF-8 の文字）、`keeps_the_newest_lines_as_older_text_is_dropped`。 |
| `Manager.test.ts`「caps persisted history to configured line limit」「caps incrementally appended history without losing partial or empty lines」「reads only a Unicode-safe tail from oversized %s history」（current のみ。legacy の file 名は持たない） | `caps_persisted_history_to_the_line_limit`、`caps_appended_history_without_losing_partial_or_empty_lines`、`reads_only_a_whole_character_tail_of_an_oversized_history`、読めない履歴は `an_unreadable_history_fails_the_open`。 |
| `Manager.test.ts`「strips replay-unsafe terminal query and reply sequences from persisted history」「strips replayable CSI and DCS traffic while preserving setters」「handles CSI and DCS query sequences split across output chunks」「preserves clear and style control sequences while dropping chunk-split query traffic」「does not leak final bytes from ESC sequences with intermediate bytes」「preserves chunk-split ESC sequences with intermediate bytes without leaking final bytes」 | `strips_terminal_queries_and_replies_from_the_history`、`strips_csi_and_dcs_queries_but_keeps_setters`、`strips_queries_split_between_output_chunks`、`keeps_clear_and_style_sequences_while_dropping_split_queries`、`keeps_escape_sequences_with_intermediate_bytes_whole`、`a_finished_process_drops_its_unfinished_sequence`。 |
| `Manager.test.ts`「bounds persisted and attached history without truncating live output」 | `bounds_persisted_history_by_bytes`（保存の内容）、`terminals::tests::bounds_kept_history_without_truncating_live_output`（実 shell で出力は切らず、保存と再 attach の履歴は上限内）。 |
| 履歴の再オープン、`Manager.test.ts`「deletes history file when close(deleteHistory=true)」「closes all terminals for a thread when close omits terminalId」の履歴の削除 | `a_terminal_replays_its_history_after_the_host_restarts`、`closing_keeps_history_unless_asked_and_thread_cleanup_deletes_it`。 |
| `Manager.test.ts`「clears transcript and emits cleared event」「restarts terminal with empty transcript and respawns pty」「emits exited event and reopens with clean transcript after exit」 | `clearing_a_terminal_empties_its_screens_and_history`、`restarting_a_running_terminal_starts_a_new_shell_with_an_empty_history`、`exited_terminals_stay_until_closed_and_restart_on_request`、protocol の `operations::terminal_tests::clear_restart_and_kill_requests_keep_their_wire_shape`。 |
| `Manager.test.ts`「reports a missing cwd without an artificial cause」「reports a cwd that is not a directory」「preserves non-notFound cwd stat failures」 | `a_terminal_needs_a_directory_it_can_reach`。 |
| `Manager.ts` windowsProcessTableSnapshot | `windows_listings_name_each_shells_command`（解析のみ。実行は Windows の Host で行う）。 |
| 一覧の行と `projects.json` の形式 | `store::tests::rows_of_an_older_shell_format_are_rebuilt_from_facts_on_open`、`projects::tests::a_projects_file_in_another_format_is_refused`。 |
| `Orchestrator.ts:8393-8569` dispatchCheckpointRollback（active run を確かめない）、`CheckpointRollbackService.ts` の runsToRollback | `agent_domain::tests::a_rollback_while_a_run_is_active_is_accepted_and_keeps_that_run`、`executor::tests::rollback::a_rollback_requested_while_a_turn_runs_executes_without_waiting_for_it`。 |
| `AttachmentClaims.ts` claimPendingAttachments・validateAttachmentLimits（名前を照合しない）・releaseClaimedAttachments、`ThreadMessageIntake.ts` の回答の claim と release、launchThread の release | `workspace_files::attachments::tests::claim_failures_say_why_in_the_reference_wording`、`claims_copy_pending_uploads_into_thread_storage_and_delete_only_there`（名前の違う claim も受け付ける）、`a_question_response_is_bounded_and_released_as_a_whole`、`a_rejected_claim_never_removes_a_copy_an_accepted_claim_holds`（決定的な ID の copy を共有する claim の手放し）、`conversation::tests::conversation_calls_answer_with_typed_errors`（回答の上限で receipt が残らない）、`tools::tests::a_launch_that_was_not_accepted_releases_its_claimed_uploads`。 |
| `composerContext.ts` の TrimmedNonEmptyString（decode 時の trim） | `composer::tests::records_are_trimmed_before_they_are_checked`。 |
| `ThreadForkService.ts` の subagent 項目の複製 | `agent_domain::tests::fork::a_fork_keeps_its_subagent_tasks_as_they_were_at_the_fork`、`query::tests::a_forks_inherited_subagent_item_reads_its_task_as_it_was_at_the_fork`（fork の後の元の progress と result は見えず、client には送らない）。 |

### 段階 1・2 の検証記録（2026-10-06）

実装・テストの最終 revision は `4a52416ab649fa888b80322f7a2884715d270beb`。以後のコミットは検証記録のみ。新しい domain / provider 層の実装と上記の挙動検証を終え、push と PR 更新後にレビューを待つ。

- `cargo test -p agent-domain -p agent-providers`: **128 件通過**（各 64 件）。状態機械の proptest と、固定版 71 transcript の projection replay を含む。
- `NEXTEST_TEST_THREADS=4 scripts/dev-env.sh just unit-tests`: **533 件通過、既存 5 件 skip**。standalone agent-peer の 5 テスト群も通過。
- `scripts/dev-env.sh cargo clippy --workspace --all-targets --features agent-core/bindings -- -D warnings` と standalone agent-peer の all-targets clippy: 通過。
- workspace と standalone agent-peer の `cargo fmt -- --check`、`git diff --check`: 通過。
- `python3 scripts/t3-port/inventory.py --check`: **960 ファイル**の固定 source hash 一致。新しい 2 crate と書き直した文書・Host 音声テスト・transport のコメントに製品名がないことを確認。

途中の全体実行では既存 adapter・transport・Host の起動／通信のタイムアウトが発生した。単独検証を行い、音声 fixture が無関係な loopback 接続を provider の要求として数える問題を修正した。fixture に discovery の GET を入れて再現し、対象 route と TLS の接続だけで既存の期待値を検証する。タイムアウト値は変更せず、最終の全体実行は同時実行数 4 で通過した。

段階 3 の actor / SQLite / outbox / provider session / 同期 / 履歴取り込み、段階 4 の core と 3 クライアントの接続、段階 5 の旧 crate 削除は未実施。稼働 Host・他 worktree・main に操作を加えていない。CI 待ち、cargo-mutants、E2E、Simulator の UI テスト、実 provider の受入検証は実施していない。

## 段階 4

### agent-core の同期・outbox・接続（2026-10-07）

`agent-core` の中核を新しい会話 protocol の上で作り直した。`orchestration` への依存を外し、状態は `agent_domain::apply` で fact を畳み込む。期待値は T3 のまま。JavaScript の object identity を確かめる assertion は、id・順序・内容の比較か、`Cow::Borrowed`（変更なし）の確認に置き換えた。表示用データ（一覧、timeline、composer など）と UniFFI の会話 view は段階 4 の後続（A4・A5）で T3 から移植する。それまで bindings は会話以外の getter だけを公開する。

| T3 の原本 | agent-core の実装と検証 |
| --- | --- |
| `client-runtime/state/threads.ts`、`threadState.ts`、`orchestrationV2Projection.ts:89-150`（partial window）、`threads-sync.test.ts` | `sync::thread::ThreadSync`。`sync::thread::tests` の `publishes_cached_data_immediately_from_a_warm_cache`、`resumes_a_warm_cache_via_after_sequence`、`reduces_live_events_and_persists_the_latest_thread`、`keeps_progressive_history_metadata_from_a_bounded_socket_snapshot`、`installs_bounded_history_and_resumes_via_after_sequence`、`persists_progressive_meta_with_a_settled_bounded_snapshot`、`warm_resume_restores_progressive_history_meta`、`a_complete_cache_offers_no_load_earlier`、`a_full_snapshot_clears_progressive_history_meta`、`a_bounded_snapshot_replaces_progressive_meta_during_resume`、`live_events_preserve_progressive_history_meta`、`a_dropped_partial_window_item_is_a_true_noop`、`installs_and_advances_the_latest_local_ordinal_for_partial_windows`、`warm_resume_restores_the_latest_local_ordinal`、`marks_a_cold_definitive_miss_deleted_without_retrying`、`ignores_replayed_events_at_or_below_the_snapshot_sequence`、`a_deletion_clears_the_data_and_its_pending_cache_write`、`preserves_data_after_a_domain_failure_and_resumes_on_a_replacement_session`、`recovers_from_a_transient_failure_on_the_next_subscription`、`a_ready_connection_does_not_downgrade_a_live_thread`、`keeps_replayed_updates_synchronizing_until_the_completion_marker`、`resubscribes_from_the_latest_applied_sequence`（交換した session と foreground の両方）、`a_retained_live_thread_stays_live_on_its_first_resume_only`。接続側は `connection::tests` の `a_reopened_thread_resumes_from_its_retained_state_until_the_retention_ends`、`a_reopened_thread_shows_its_disk_cache_and_resumes_from_its_cursor`、`a_cold_missing_thread_is_deleted_and_deselected`。 |
| `state/threadHistoryMerge.ts`、`threadHistoryMerge.test.ts` | `sync::history`。`prepends_older_rows_and_dedupes_by_source_identity`、`retains_live_rows_that_arrived_after_the_older_page_was_fetched`、`does_not_resurrect_a_local_item_changed_while_an_older_page_was_in_flight`、`marks_history_expanded_after_a_successful_page`、`only_applies_history_responses_for_the_active_request_cursor`、`clears_loading_on_interrupt_only_for_the_active_request_cursor`、`shows_the_load_earlier_control_when_history_remains_or_a_local_error_is_set`、`retains_merged_older_rows_when_a_live_item_update_arrives`、`keeps_a_page_interrupt_row_visible_when_its_request_was_retained_at_open`。行の可視性は domain の `visible_items` が run から決めるので、保持済みの interrupt request は page を待たずに表示される。`sync::thread::tests::history_results_apply_only_to_the_cursor_that_requested_them` は loadEarlier の busy・古い cursor・失敗の扱い。 |
| `state/shellReducer.ts`、`shellReducer.test.ts` | `sync::shell`。`updates_a_thread_in_place_without_moving_its_siblings`、`ignores_stale_project_updates_without_mutating_the_snapshot`、`applies_project_updates_and_removals`、`keeps_prior_repository_identity_when_a_delta_arrives_without_one`、`does_not_keep_prior_repository_identity_after_a_root_move`、`merges_full_snapshot_projects_without_dropping_known_identity`、`an_authoritative_lower_sequence_reset_replaces_client_ahead_state`、`removes_a_thread_that_left_the_location`（archive の delta を通常の shell に入れない、両方の一覧から消す）。 |
| `state/shell.ts`、`shell-sync.test.ts` | `sync::shell::ShellCache`。`publishes_synchronizing_data_then_live_and_stays_live_when_ready`、`batches_live_events_into_one_change_per_received_batch`（buffer 全体と 16 件ずつの分割で T3 と同じ sequence 列）、`a_new_session_reloads_the_authoritative_snapshot_over_a_warm_cache`、`resubscribes_from_the_in_memory_cursor_within_the_same_session`（`[10, 40, 40]` と交換後の再読み込み）、`a_resumed_project_list_does_not_skip_the_replayed_rows`、`disconnects_and_failures_keep_cached_data`。書き込みは `sync::cache::tests::throttles_shell_writes_and_flushes_the_latest_snapshot_on_disconnect`、`rewrites_a_same_sequence_shell_with_different_content`。 |
| `state/cachePersistence.ts`、`threads.ts` の persist 規則 | `sync::cache`。`coalesces_streaming_writes_and_flushes_the_latest_on_teardown`（`[1]`→`[1, 10]`→`[1, 10, 11]`）、`flushes_newer_data_after_an_older_write_completes`、`persists_a_settled_thread_once_and_skips_an_unchanged_warm_return`、`persists_a_complete_bounded_window_with_its_meta`、`retries_a_failed_write_at_teardown`、`does_not_persist_active_expanded_or_deleted_threads`、`disk_entries_round_trip_and_removal_is_idempotent`。 |
| `state/threadRetention.ts` | `sync::THREAD_SNAPSHOT_IDLE_TTL_MS`（5 分）。選択中の thread だけが live の購読を持ち、離れた thread は 5 分間メモリに残って cursor から再開する。 |
| `rpc/client.ts` の subscribeDynamic（expected failure の再試行） | `sync::resubscribe_delay_ms`、`sync::tests::subscription_retries_double_from_250_ms_up_to_30_seconds`。最初の正常な項目で回数を戻す。 |
| `operations/commands.ts`、`commands.test.ts` | `commands::build`。`preserves_caller_command_ids`、`resolves_run_ordinal_zero_to_the_thread_start_checkpoint`、`only_ready_checkpoints_resolve_from_a_run_ordinal`、`preserves_plan_implementation_provenance`、`preserves_an_existing_worktree_and_branch_during_a_first_message_launch`、`provisions_an_origin_based_worktree`、`sends_the_users_delivery_intent_for_the_host_to_resolve`、`builds_relationship_and_queue_commands_without_reshaping`、`selects_models_through_the_host_without_choosing_a_switch`、`rolls_back_an_identified_checkpoint_with_the_chosen_file_restore`、`dispatches_settle_and_unsettle_without_timestamps`、`sends_an_active_order_key`、`dismisses_and_answers_pending_requests`、`dispatches_an_explicit_idle_start_with_its_continuation`、`interrupts_a_known_run_or_the_stop_target_and_holds_the_queue`、`detaches_each_provider_session_with_a_derived_command_id`。Host は command context を解決するので、T3 の server-resolved の経路だけを移植した。 |
| `shared/orchestrationV2PendingBackgroundWork.ts`、commands.test の Stop 6 件 | `commands::workflows::pending_background_work`、`interrupt_target`。`stop_targets_runs_with_background_commands_except_rolled_back_ones`、`pending_work_names_the_roster_and_running_items_once_per_task`。provider thread の roster は domain の `background_work` に当たる。 |
| `state/composerDispatch.ts`、`composerDispatch.test.ts` | `commands::build::resolve_composer_dispatch_mode`。`starts_an_ordinary_turn_while_idle`、`steers_by_default_and_reserves_the_alternate_for_queueing`、`queues_as_the_alternate_when_restarting_is_the_default`、`uses_the_configured_behavior_only_during_a_running_turn`、`names_the_alternate_action_so_the_affordance_can_be_labelled`。設定の既定値は T3 の settings と同じ queue（`persistence::tests::a_new_device_defaults_to_queueing_follow_ups`）。 |
| `operations/threadTitle.ts`、`threadTitle.test.ts` | `commands::build::thread_title_seed`。`title_seeds_*` の 5 件。 |
| web `proposedPlan.ts`（実装の文面とタイトル） | `commands::build::plan_follow_up`、`plan_implementation_thread_title`、`plan_follow_up_implements_on_an_empty_draft_and_refines_otherwise`。 |
| `state/threadWorkflows.ts`、`threadWorkflows.test.ts` | `commands::workflows`。`sorts_queued_messages_and_gates_reorder_and_promotion`、`keeps_held_messages_visible_and_clears_the_hold_when_they_leave_the_queue`、`hides_automatic_completion_delivery_from_the_visible_queue`、`removes_only_the_promoted_head_from_the_visible_queue`、`does_not_promote_queued_work_into_a_preparing_starting_or_waiting_run`、`does_not_promote_queued_work_until_the_provider_accepts_the_turn`、`allows_forks_only_from_completed_assistant_messages_of_a_run`、`merges_the_newest_provider_finished_run_while_its_checkpoint_is_pending`、`does_not_merge_older_history_while_a_newer_run_is_active`。T3 の provider turn の running は、attempt が受け付けられて running であることに当たる。capability は `TurnSupport::for_driver` から得る。 |
| `state/threadLifecycle.ts`、`threadCommands.ts` の楽観的更新、`threadCommands.test.ts` | `commands::lifecycle::LifecycleOverlay`、`commands::outbox::Outbox::overlay_shell`。`shows_each_action_before_a_delayed_reply_and_rolls_back_a_rejection`（9 種の操作）、`keeps_the_preview_after_acknowledgement_until_the_matching_shell_update`、`shows_a_queued_reverse_action_and_keeps_it_when_the_earlier_one_fails`、`does_not_restore_a_remotely_removed_thread`、`keeps_pending_approvals_visible_while_a_lifecycle_request_is_pending`、`restores_a_confirmed_settle_or_snooze_when_a_queued_undo_fails`、`preserves_a_newer_approval_when_the_reply_arrives_after_the_shell`、`shows_an_accepted_settle_or_snooze_over_an_old_input_request`。接続側は `connection::tests::lifecycle_previews_show_until_the_shell_confirms_them`。 |
| mobile `state/thread-outbox-model.ts`、`thread-outbox.test.ts` | `commands::outbox`。`backs_off_retries_and_caps_them_at_sixteen_seconds`、`only_removes_a_missing_thread_message_after_the_shell_is_live`、`sends_existing_thread_messages_whenever_connected`、`sends_creations_once_connected_and_live_and_removes_created_ones`、`retries_transport_failures_but_drops_host_decided_refusals`、`replaces_an_entry_when_a_retry_uses_the_same_id`、`persisted_entries_round_trip_and_resend_requests_in_flight`、`stop_retrying_discards_only_requests_not_in_flight`。 |
| mobile `features/threads/pending-thread-feed.ts`、`pending-thread-feed.test.ts` | `Outbox::undelivered_messages`。`pending_messages_keep_their_context_while_waiting`、`pending_messages_follow_send_order_until_the_thread_folds_them`、`a_launch_carries_its_first_message_as_a_pending_row`。行への組み込みは timeline の移植（A4）で行う。 |
| 旧 `store.rs` の接続・配送・周辺のテスト | `connection::tests`。`a_healthy_resume_reuses_the_connection_and_timed_out_sends_keep_their_id_and_order`（同じ command id の再送と thread ごとの順序）、`snapshot_revisions_are_ordered_within_each_store_only`、`a_burst_of_input_over_the_stream_channel_capacity_keeps_every_edit`、`shutdown_closes_snapshot_waiters_and_preserves_local_edits`、`dictation_preparation_uses_the_lossless_intent_queue_when_stream_events_are_full`、`a_cancelled_dictation_cannot_append_a_late_transcript`、`a_transcription_appends_to_its_draft_without_replacing_new_text`、`a_remote_pairing_receipt_observes_the_registered_host`、`a_late_turn_diff_cannot_replace_another_range_or_thread`、`file_reloads_and_saves_preserve_edits_and_their_base_revision`、`a_failed_terminal_start_is_terminal_and_terminal_output_stays_bounded`、`mobile_cold_start_keeps_drafts_without_reopening_the_saved_selection`、`text_edits_preserve_newer_model_and_mode_choices`、`failed_background_visits_do_not_replace_a_user_notice`、`a_queued_edit_ends_when_its_run_leaves_the_queue_and_restores_the_main_draft`、`a_rollback_returns_the_rolled_back_message_to_the_composer_after_success_only`、`a_launch_opens_its_thread_once_the_shell_shows_it`、`a_late_launch_does_not_navigate_away_from_another_thread`、`a_send_clears_the_composer_shows_the_message_and_restores_it_when_refused`、`a_running_thread_queues_follow_ups_and_the_alternate_steers`、`stopping_retries_returns_the_unsent_message_to_the_composer`。 |

proptest: `sync::thread::tests::the_cursor_never_moves_back_under_duplicate_and_out_of_order_replays`、`folding_facts_in_split_batches_equals_folding_them_whole`、`commands::outbox::tests::each_thread_delivers_in_send_order_one_request_at_a_time`。

対象外にしたもの（理由）:

- `threads-sync.test.ts` の HTTP snapshot を使う 6 件（seeds from HTTP、unavailable への fallback、no-http / no-controller の交渉、HTTP の atomic install、warm cache の HTTP 省略）: 会話 protocol に HTTP の snapshot はなく、購読の最初の応答が snapshot になる。同じ規則は socket の bounded snapshot で確かめた。`accept_bounded_snapshot` は履歴 RPC が常にあるので常に true にする。
- 同 file の Effect の scope と fiber の競合 4 件（canceled event、old scope と successor、delayed cache read、old deletion）: owner が状態と cursor を 1 つの event で更新し、閉じた購読の項目は stream の世代で捨てるので、競合が起きない。
- 同 file の unknown event 1 件と `shellReducer.test.ts` の未知の future event 1 件: Postcard の enum に未知の variant は届かない（ALPN で版をそろえる）。
- `shellReducer.test.ts` の enrichment 8 件、`shell-sync.test.ts` の enrichment の再保存と scope teardown: Host は repository identity の enrichment frame を送らない（2026-10-06 の決定）。teardown は owner が停止してから書くので後着の項目がない。
- `threadHistoryController.test.ts` 2 件: 登録と解除の API がない。履歴の要求は owner が保持する thread の状態に直接渡す。
- `commands.test.ts` の project mutation 2 件: project は `host/project/*` の RPC で、会話 command ではない（A4 の projects で扱う）。command context を解決しない古い server 向けの 3 件と `reuseExistingThread`: Host は常に解決し、空 thread の再利用は持たない（2026-10-06 の決定）。
- `threadWorkflows.test.ts` の `threadSupportsProviderHandoff`、`canDetachThreadProviderSession`、capability がない provider の 1 件: projection に provider session の capability と状態がない。Codex と Claude はどちらも queue を受ける。model picker（A4）で扱う。
- `thread-outbox.test.ts` の保存先・manager の並行性・schema の旧版・`resolveThreadOutboxDispatchStep`・settings-sync の段階: 保存は device state の encode で 1 回に行い、旧形式は持たない。送信前の server config と file 上限の確認、送信前の設定同期は Host への送信の形が違う（選択は発言に含める）。
- `threadCommands.test.ts` の environment の分離: Store は Host ごとに 1 つ。

T3 に合わせた判断:

- `live_buffer_full` を含む `Failed` は T3 の expected failure と同じく、エラーを示して 250 ms から倍々（30 秒まで）で cursor から購読し直す。
- `payload_budget_exceeded` は T3 のクライアントと同じく使わない。
- shell の `ThreadRemoved` は一覧からだけ消す。thread の削除は thread の `ThreadDeleted` か、データのない購読への `ThreadNotFound`（T3 の HTTP 404）で決める。
- 新しい接続の shell は `after_sequence` なしで snapshot を取り直す（T3 の authoritative reload）。同じ接続での再購読は cursor から再開する。再開した stream の最初の `Projects` は cursor を動かさない。後に続く replay の行を落とさないため。
- 送信は composer をすぐ空にして未確定の発言を示し、Host が拒否したら本文を composer に戻す（2026-10-07 の決定）。lifecycle の preview は再試行の間も残す。
- 届かなかった要求の再試行は 1 秒から 16 秒（mobile outbox）。要求中の timeout は同じ command id で 250 ms から 5 秒（旧 store）。

未移植: title seed の citation link の平文化（`assistantCitationsToPlainText`）は markdown の citation の移植（A4）で入れる。会話の view（一覧、timeline、composer、question の検証、mode の選択肢）と、それを返す bindings の getter は削除し、A4・A5 で T3 から移植する。

### A4 の表示用データ: composer・キュー・要求・計画・agent・関係・checkpoint・setup・状態表示・header・拒否の文面（2026-10-07）

`agent-core/src/view` に T3 の表示ロジックを純粋関数として移植した。入力は fold 済みの `agent_domain::State`・`ThreadShell`・`Snapshot`、または T3 の関数と同じ明示的な入力で、時刻は `now_ms` で受ける。公開する型は A5 の UniFFI 公開に合わせて record / enum にした（id は `String`、時刻は epoch ミリ秒）。テストは各実装の隣の `tests` にあり、期待値は T3 のまま。

| T3 の原本 | agent-core の実装と検証 |
| --- | --- |
| web `components/chat/ComposerPrimaryActions.tsx`、`ComposerPrimaryActions.test.tsx`、`session-logic.ts` の `derivePhase` / `deriveCanInterruptRunningThread`、`threadExecution.ts` の runtime と中断可否、ChatView の `resumableRunId` | `view::composer::actions`（`composer_primary_action`、`thread_runtime`、`session_phase`、`can_interrupt_running_thread`、`resumable_run`、`can_resume`）。`disables_and_labels_the_send_button_while_feedback_is_uploading`、`offers_stop_generation_while_a_running_turn_is_waiting_for_user_input`、`does_not_offer_stop_generation_for_a_pending_request_without_a_running_turn`、`offers_stop_while_a_run_is_preparing_or_starting_not_just_once_it_is_running`。threadExecution.test からは `keeps_a_subscription_limit_visible_while_later_messages_stay_queued`、`keeps_live_activity_attached_to_an_executing_run_when_a_newer_run_is_queued`、`presents_a_held_queue_as_the_stopped_run_instead_of_queued_work`、`does_not_expose_a_queued_only_or_checkpoint_waiting_run_as_interruptible`。追加: `labels_answer_submission_by_question_position_and_layout`、`offers_refine_with_text_and_implement_with_its_new_thread_menu_otherwise`、`stops_only_without_sendable_content_and_outside_a_queued_edit`、`flips_the_send_icon_between_steer_and_queue_with_the_alternate_modifier`、`reports_why_sending_waits_and_resumes_an_empty_resumable_thread`、`background_work_that_wakes_the_agent_parks_the_thread_at_idle`。 |
| mobile `composerSendPresentation.ts`、`.test.ts`、`ThreadComposer.tsx` の Stop 表示 | `view::composer::actions::mobile_send_presentation`、`mobile_shows_stop`。`sends_plainly_while_the_thread_is_idle`、`says_queue_while_the_outbox_is_holding_the_message_back`、`follows_the_configured_behavior_once_a_turn_is_running`、`never_promises_steering_the_provider_cannot_do`、`keeps_the_save_affordance_while_a_queued_message_is_being_edited`。追加: `mobile_stop_replaces_send_only_for_an_empty_draft_outside_an_edit`。 |
| web `ChatComposer.tsx` の placeholder と入力の無効化、`composerPlaceholder.ts`、`components/chat/composerSubmission.ts`、`.test.ts`、`ChatView.logic.ts` の `deriveComposerSendState`、mobile の送信保留理由 | `view::composer::prompt`（`composer_editor`、`prompt_length_validation_message`、`submission_validation_message`、`has_sendable_content`、`mobile_send_blocked_reason`）。`keeps_an_oversized_draft_editable_and_sends_a_corrected_follow_up`、`allows_a_draft_at_the_shared_character_limit_through_the_normal_send_path`、`blocks_when_appended_context_pushes_the_provider_input_over_the_shared_limit`、`allows_fully_composed_provider_input_at_the_shared_character_limit`、`blocks_a_generated_plan_follow_up_that_exceeds_the_shared_limit`、`allows_surrounding_whitespace_that_the_provider_turn_contract_trims`、`dispatches_pending_user_input_answers_on_their_separate_response_path`。追加: `counts_utf16_units_and_groups_large_excess`、`the_placeholder_follows_what_the_composer_waits_on`（"Resolve this approval request to continue" ほか）、`context_links_alone_are_not_typed_content`、`mobile_holds_a_send_for_the_first_reason_that_applies`。 |
| web `ContextWindowMeter.logic.ts`、`.test.ts`、`ContextWindowMeter.tsx`、`lib/contextWindow.ts`、shared `claudeCompaction.ts` | `view::composer::context_meter`。`latest_context_window` は、最新 attempt の `context_usage`、native session の値、最後の compaction の順に使う。移植: `rejects_a_fallback_in_a_different_locked_continuation_group`、`accepts_an_enabled_fallback_in_the_locked_continuation_group`、`falls_back_to_the_selected_model_slug_when_model_metadata_is_unavailable`、`describes_compaction_in_terms_of_the_selected_model`、`uses_neutral_copy_when_the_model_is_unavailable`、`shows_the_configured_auto_compaction_threshold`、`matches_claudes_old_session_age_and_context_thresholds`、`does_not_prompt_for_recent_or_smaller_sessions`、`does_not_show_claudes_resume_prompt_for_another_provider`、`recognizes_the_native_resume_dialogs_permanent_dismissal`、`ignores_the_same_answer_on_an_unrelated_question`、`ignores_unrelated_questions_that_end_with_claudes_compaction_prompt`、`ignores_pending_questions_and_answers_that_are_not_resolved`、`shouldReserveContextWindowMeter` の 6 件（`holds_the_meters_slot_while_a_started_threads_detail_loads` ほか）。追加: `formats_token_counts_like_the_meter`、`prefers_the_newest_attempt_usage_then_the_latest_compaction`、`the_meter_shows_percentage_with_a_known_window_and_tokens_otherwise`。 |
| mobile `thread-settings-options.ts`、`.test.ts`、web `runtimeModeConfig.ts`、`ChatComposer.tsx` の Build/Plan、`ChatView.logic.ts` の `resolveComposerInteractionMode` と test | `view::composer::controls`。`keeps_controls_usable_when_forward_compatible_decoding_removes_every_advertised_mode`、`resets_a_restored_plan_draft_when_the_selected_instance_does_not_support_plan_mode`、`keeps_legacy_plan_behavior_for_providers_that_omit_the_capability`、`resets_a_restored_plan_draft_when_the_beta_setting_is_off`、`disables_plan_mode_until_the_selected_provider_is_available`。追加: `offers_the_supported_modes_in_order_and_shows_an_unsupported_mode_as_the_first`、`the_controls_show_the_draft_model_mode_and_toggle`。 |
| mobile `use-composer-command-menu.ts`、`.test.ts`、shared `composerTrigger.test`、web `composer-logic.test`、`composerSlashSkillSearch.test`、`composerThreadItems.test`、`searchRanking.test` | `view::composer::commands`（`detect_composer_trigger`、`replace_text_range`、`parse_standalone_slash_command`、`slash_command_items`、`thread_items`、`composer_command_items`、`resolve_composer_command_selection`、`has_compactable_conversation`。offset は UTF-16）。移植: `detects_skill_prefixes_and_their_source_range`、`uses_the_basename_as_the_markdown_label`、`encodes_markdown_sensitive_destination_characters`、`supports_windows_paths`、`preserves_paths_that_legitimately_start_with_an_at_sign`、`detects_at_path_trigger_at_cursor`、`detects_slash_command_token_while_typing_command_name`、`detects_non_model_slash_commands_while_typing`、`keeps_slash_command_detection_active_for_provider_commands`、`detects_skill_trigger_at_cursor`、PR trigger の 5 件、`detects_at_path_trigger_in_the_middle_of_existing_text`、`detects_at_path_trigger_with_query_typed_mid_text`、`detects_trigger_with_true_cursor_even_when_mention_detection_would_false_match`、`replaces_a_text_range_and_returns_new_cursor`、`double_space_after_insertion_when_replacement_ends_with_space`、`parses_standalone_plan_command`、`parses_standalone_default_command`、`ignores_slash_commands_with_extra_message_text`、`keeps_native_plan_with_legacy_mode`、`does_not_offer_a_native_command_inside_the_message`、`still_applies_the_built_in_plan_command_for_supported_providers`、`matches_the_rendered_skill_prefix`、`offers_nothing_for_a_bare_at_so_the_picker_stays_a_file_picker`、`matches_titles_newest_first_skipping_self_and_archived`、searchRanking の 6 件。追加: `reports_the_model_command_and_its_query`、`offers_compact_only_for_a_compactable_conversation`、`compactable_conversation_needs_a_message_other_than_a_bare_compact`、`slash_menu_lists_commands_then_skills_and_hides_commands_shadowed_by_skills`、`ranks_skills_for_a_dollar_query`、`path_items_follow_thread_matches`、`choosing_a_thread_inserts_its_link_and_attaches_it_once`。 |
| web `promptStashStore.ts`、`.test.ts`、`ChatComposer.tsx` の stash 操作 | `view::composer::stash`（`PromptStash`、`stash_shortcut`、`new_stash_entry`、`restore_stash_entry`、`stash_menu`。20 件、2.7M 文字、100 添付）。移植: `keeps_attachments_within_the_budget_and_reports_dropped_names_in_order`、`admits_a_single_attachment_that_exactly_fits_the_budget`、`prepends_entries_so_the_newest_stash_is_first`、`evicts_the_oldest_entry_past_the_cap_and_returns_it`、`take_entry_removes_and_returns_the_entry_second_take_returns_none`、`finalize_entry_images_attaches_images_and_clears_the_pending_count`、`take_entry_returns_images_and_drop_metadata_finalized_after_a_menu_snapshot`、`preserves_uploaded_file_references_without_storing_file_contents`、`finalize_entry_images_reports_false_when_the_entry_was_already_taken`、`settles_a_pending_count_left_behind_by_a_crashed_or_closed_session`、`keeps_the_records_behind_a_stashed_prompts_chips`。追加: `stashes_a_composer_with_content_and_restores_the_only_saved_entry_when_empty`、`writes_the_trimmed_prompt_and_file_uploads_before_the_images`、`restores_text_after_a_blank_line_and_reports_what_did_not_come_back`、`an_image_only_entry_leaves_the_draft_text_alone`、`restores_up_to_the_attachment_limit`、`summarizes_entries_for_the_stash_menu`。 |
| web `DraftHeroHeadline.tsx`、`ChatView.logic.test` の draft hero submission transition | `view::composer::hero`（`draft_hero_state`、`dock_draft_hero_for_submission`、`draft_hero_headline`、`Snapshot::draft_hero_headline`）。`does_not_dock_the_composer_before_a_background_submission`、`leaves_the_hero_layout_while_a_worktree_setup_card_is_on_the_timeline`、`keeps_the_composer_in_the_hero_layout_until_navigation_after_server_promotion`。追加: `shows_the_hero_only_for_an_untouched_draft`、`asks_what_to_build_in_the_selected_project`、`a_draft_without_a_project_asks_what_to_work_on_and_moves_the_picker_below`、`asks_to_choose_or_add_a_project_when_the_target_is_unknown`。 |
| `composerContextRecords.test`、shared / web `composerContextReferences.test`、composer の context strip と送信済み発言の chip、mobile feed の未解決 link | `view::composer::chips`（`context_chips`、`standalone_attachment_ids`、`mobile_message_markdown`）。移植: `presents_review_range_consistently`、`distinguishes_a_pr_summary_from_a_comment_on_its_diff`、`labels_a_preview_annotation_by_its_comment`、`formats_images_with_the_image_form_and_everything_else_as_a_link`、`keeps_same_kind_raw_ids_distinct_when_one_contains_the_namespace_prefix`、`keeps_ids_that_already_fit_the_grammar_and_folds_the_rest_deterministically`、`tells_apart_producer_ids_that_agree_past_the_slugs_truncation_point`。追加: `resolves_each_link_to_a_chip_in_document_order`、`a_chip_whose_record_is_of_another_kind_or_lacks_its_attachment_is_unresolved`、`presents_pull_requests_by_state_and_review_comments_with_details`、`a_sent_message_shows_pictures_and_unchipped_files_outside_its_prose`、`marks_unavailable_links_in_the_mobile_feed`、`formats_attachment_sizes`、`recognizes_videos_without_a_recorded_type`。 |
| `state/threadWorkflows.ts`、mobile `threadQueueControlPresentation.ts`、`.test.ts`、web `QueuedRunsControl.tsx`、mobile `ThreadQueueControl.tsx` | `view::queue`（`queue_view`、`selected_queue_view`、`queue_row_controls`、`queue_preview_text`、drag と矢印キーの移動先）。`commands::workflows::queue_workflow` を再利用し、その test は上の表のとおり。移植: `preserves_queue_reorder_and_steer_controls_with_removal`、`disables_edge_reorder_controls_and_busy_dismissal`、`keeps_the_row_already_open_in_the_composer_from_being_reopened_or_steered`、`moves_between_variable_height_rows_and_to_either_end`、`opens_the_destination_gap_while_the_dragged_row_crosses_other_rows`、`does_not_send_a_reorder_for_an_unchanged_or_unmeasured_drop`。追加: `lists_confirmed_runs_then_unconfirmed_queued_sends`、`previews_context_links_by_label_and_drops_images_shown_as_thumbnails`、`compact_rows_fall_back_to_attachments_and_count_unshown_files`、`a_held_queue_offers_resume_until_the_resume_is_sent`、`a_waiting_queue_command_disables_every_row`、`marks_the_edited_row_and_names_the_shortcut_targets`、`rows_cannot_steer_without_an_active_run`、`arrow_keys_and_drops_anchor_before_the_following_row`。 |
| `state/threadRequests.ts`、`.test.ts`、web `pendingUserInput.ts`、`.test.ts`、mobile `pendingUserInputLayout.ts`、`.test.ts`、mobile `threadActivity.test` の回答部分、承認と質問の panel / card | `view::requests`（`approval_view`、`questions_view`、`requests_view`、`build_question_answers`、回答の下書き操作、`pending_input_max_height`）。threadRequests からの移植: `keeps_message_responses_available_after_the_originating_runtime_exits`、`only_marks_asynchronous_questions_as_dismissible`、`removes_answered_requests_from_the_composer_while_retaining_their_answers`。pendingUserInput からは 17 件（`prefers_a_custom_answer_over_selected_options` から `appends_the_typed_answer_after_an_existing_thread_draft` まで）、threadActivity からは 5 件（`replaces_single_select_options_and_toggles_multi_select_options` ほか）。pendingUserInputLayout からは `caps_a_tall_portrait_viewport`、`subtracts_the_keyboard_while_editing_a_custom_answer`、`keeps_the_fixed_action_area_usable_in_a_short_keyboard_open_viewport`。追加: `shows_the_oldest_approval_with_its_counter_and_default_choices`、`defaults_offer_only_the_decisions_the_request_accepts`、`app_access_requests_show_the_app_and_its_own_choices`、`an_approval_without_its_provider_process_cannot_be_answered`、`a_sent_reply_marks_its_request_responding`、`a_question_request_names_its_progress_options_and_shortcuts`。 |
| web `session-logic.ts` の `deriveActivePlanState` / `findLatestProposedPlan` / `hasActionableProposedPlan` / `isLatestRunSettled`、`ChatView.logic` の plan follow-up の条件、`ComposerPlanFollowUpBanner.tsx` | `view::plan`（`active_plan_state`、`latest_proposed_plan`、`should_show_plan_follow_up_prompt`、`plan_view`。題名は `commands::build::proposed_plan_title`）。移植: `selects_the_latest_proposed_plan_for_a_run`、`uses_run_status_as_the_settlement_boundary`、`keeps_task_progress_available_to_the_composer`、`shows_plan_actions_for_a_settled_actionable_plan_without_attachments`、`hides_plan_actions_while_the_composer_has_staged_attachments`、`preserves_the_existing_plan_follow_up_gates`。追加: `offers_the_plan_follow_up_once_the_planning_run_settles`、`a_pending_question_holds_back_the_plan_follow_up`、`a_bounded_snapshot_reads_the_plan_text_from_its_item`、`prefers_the_latest_runs_plan_and_orders_by_item_time`、`task_progress_follows_only_the_working_runs_own_list`。 |
| `state/threadSubagents.ts`、`subagentDisplay.ts`、web `agentSpawnSummary.ts`、mobile `threadAgentsPresentation.ts`、shared `orchestrationTiming.test`、`subagent-card-presentation.test` | `view::agents`（`agent_roster`、`agent_spawn_summary`、`subagent_group_summary`、`subagent_metadata`、`format_elapsed`、`format_subagent_display_title`、`subagent_card_detail`）。移植: threadSubagents の 7 件、subagentDisplay の 7 件（`keeps_a_mixed_group_live_while_a_member_is_active` ほか）、`reports_a_state_accurately_alongside_a_completed_agent`、`does_not_claim_completion_when_the_roster_is_missing_a_member`、threadAgentsPresentation の 6 件（duration の 2 件は `view::time::formats_durations` に統合）、`does_not_count_the_age_of_settled_work_with_unknown_completion_timing`、`counts_a_resumed_activation_despite_a_stale_previous_completion_timestamp`、`shows_readable_result_text_and_suppresses_generic_completion_messages`。追加: `live_rows_count_to_now_and_settled_rows_stop_at_completion`、`formats_reported_models_without_a_catalog`、`names_a_catalog_model_by_its_slug_name_or_alias`。 |
| `state/threadRelationships.ts`、`.test.ts`、web `ThreadRelationshipsControl.tsx`、`.test.tsx`、`.agents.test.tsx` | `view::relationships`（`relationship_graph`、`order_lineage_rows`、`merge_back_target`、`lineage_panel`、`lineage_window`、`merge_back_action`）。移植: threadRelationships の 13 件（`keeps_parent_status_independent_of_child_status` ほか）、`shows_six_rows_before_the_first_expansion`、`offers_one_page_at_a_time`、`omits_the_expansion_affordance_when_everything_fits`、`shows_the_matching_child_agent_details_and_refreshes_them_when_the_agent_settles`、`shows_readable_models_and_only_differing_workspace_details_in_agent_tooltips`、`shows_the_parents_own_visible_status_as_parent_and_child_activity_change`、`shows_transfer_lifecycle_states_when_viewing_the_target_thread`。追加: `offers_merge_back_into_the_fork_source_once_a_run_finished`。 |
| `state/threadCheckpoints.ts`、`checkpointDiff.ts`、contracts `checkpointDiff.ts`、`.test.ts`、web `checkpointDiffState.ts`、`diffPanelStore.ts`、`.test.ts`、`DiffPanel.tsx` の scope、session-logic の rollback 対象 | `view::checkpoints`（`checkpoint_summaries`、`revert_targets`、`turn_diff_request`、`DiffPanelSelection`、`diff_panel`、`DiffRequest::intent`。ignore whitespace の既定は true）。移植: checkpointDiff の 5 件、diffPanelStore の 8 件、`assigns_run_rollback_to_the_turn_start_message_instead_of_a_later_steer`。追加: `only_ready_checkpoints_offer_rollback`、`summarizes_run_checkpoints_with_their_files_and_last_assistant_message`、`orders_turns_newest_first_by_turn_count_then_completion`、`defaults_to_changes_with_whitespace_hidden`、`labels_the_latest_turn_and_requests_its_checkpoint_range`、`labels_an_older_turn_by_its_count_and_marks_only_the_submenu`、`shows_no_completed_turns_when_a_turn_is_selected_without_checkpoints`、`uncommitted_loads_the_workspace_review`。 |
| client-runtime `worktreeSetup.ts`、contracts `worktreeSetup.ts`、mobile `worktree-setup-state.ts`、`.test.ts`、web `WorktreeSetupCard.tsx`、`ChatView.logic.test` の worktree setup visibility | `view::setup_card`（`resolve_setup_snapshot`、`resolve_setup_progress`、`resolve_visible_setup`、`setup_card`、`setup_view`）。移植: `keeps_the_setup_identity_after_its_card_retires_so_the_working_header_can_take_over`、`retains_settled_details_after_the_stream_closes`、`does_not_carry_a_previous_threads_progress_across_navigation`、`keeps_setup_presentation_continuous_until_the_provider_handoff`、`uses_streamed_setup_progress_immediately_without_reverting_to_an_older_held_snapshot`、`does_not_keep_settled_setup_in_the_preparing_state`、`shows_a_running_setup_and_drops_a_clean_one_once_the_turn_started`、`keeps_a_failed_script_a_failed_setup_and_a_cancelled_setup_visible`、`prefers_whichever_snapshot_is_newer_by_sequence`。追加: card の 8 件と thread view の 2 件（`labels_stages_in_the_host_order` から `the_thread_view_retires_a_failed_setup_after_a_follow_up` まで）。 |
| mobile `floating-working-status.ts`、`.test.ts`、`floating-working-control.tsx`、threadExecution.test の `presentPendingBackgroundWork` | `view::working_status`（`floating_working_status`、`active_work_started_at`、`present_pending_background_work`、`working_control`、`queued_count`）。移植: `yields_the_pill_to_sync_and_working_state_once_connected`、`names_the_environment_it_is_retrying_and_says_so_only_after_a_failure`、`reports_why_the_environment_is_unreachable`、`falls_back_to_a_generic_name_when_the_environment_has_no_label`、`carries_the_reconnect_handler_so_the_pill_can_trigger_it`（callback の代わりに variant と操作可否を確かめる）、background work の 7 件。追加: `formats_the_working_duration`、`counts_the_timer_from_the_active_run_start`、`a_wake_keeps_the_start_of_the_work_it_continues`、`a_run_without_a_start_counts_from_its_request`、`compacting_lasts_until_the_compaction_finishes`、`a_compact_message_with_attachments_is_an_ordinary_turn`、`a_pending_request_hides_the_status`、`a_thread_being_created_shows_its_preparation`、`sync_precedes_work_and_work_waits_for_ready_content`、`a_settled_turn_reports_the_work_it_left_running`、`labels_the_sync_by_whether_messages_are_on_screen`、`the_control_labels_the_timer_queue_and_agents`、`the_control_hides_without_content_or_scroll_target`、`counts_only_messages_the_user_queued`。 |
| web `ChatHeader.tsx`、`ChatHeader.test.ts`、mobile `ThreadRouteScreen.tsx` の header 操作 | `view::header`（`resolve_rename_commit`、`thread_header`、`snapshot_thread_header`。Git / PR は段階 6）。移植: `commits_a_trimmed_changed_title`、`rejects_empty_and_whitespace_only_titles`、`no_ops_when_the_trimmed_title_is_unchanged`。追加: `the_project_leads_the_header_and_opens_a_new_thread`、`a_draft_title_has_no_action_menu`、`panels_need_a_project`、`files_open_in_the_thread_worktree`、`a_fork_with_a_finished_run_offers_merge_back`。 |
| server `orchestration-v2/UserFacingErrors.ts`、`.test.ts` | `view::rejection::rejection_message`、`provider_rejection_message`。Host が返す 94 の理由をすべて文にする（id は含めない）。すでに文になっている理由はそのまま返し、未知の code は "Failed to dispatch the command." にする。移植: `returns_the_deepest_actionable_domain_error_instead_of_generic_dispatch_wrappers`、`translates_policy_capability_rejections_into_provider_named_prose`、`uses_explicit_detail_fields_as_user_facing_messages`。Host の理由は平たい文字列なので、どれも文がそのまま返ることを確かめる形にした。追加: `unknown_codes_fall_back_to_a_generic_failure`、`words_every_host_rejection`（95 件）。 |

対象外にしたもの（理由）:

- `ComposerPrimaryActions.test.tsx` の stage artwork 2 件: 背景画像は環境の識別のための装飾で、表示用データではない。
- `ContextWindowMeter.logic.test.ts`:
  - provider instance ごとに model 名を解決する 1 件: model catalog に instance と shortName がないので、名前は呼び出し側が渡す。
  - `formatContextWindowCost` の 1 件: Codex と Claude の usage には費用がない（費用は ACP だけ）。
- `composerSubmission.test.ts`:
  - assistant citation を provider 向けに展開する 3 件: agent-core にまだ展開がない。
  - 送信境界で拒否する 1 件: 拒否は intent 側で扱う。
- `modelSelection.test.ts` の `deriveEffectiveComposerModelState` と `selectableChoices`: model catalog の解決なので、A4 の models の移植で扱う。
- composer の入力まわり。移植したのは mobile が使う共有 detector で、次は対象外:
  - web の composer-logic.test の `/model` 2 件
  - 展開・折り畳みの cursor と citation の cursor（web の rich-text editor）
  - `composerSubmissionIntentForKey`（keybinding）
  - `filterComposerPullRequestMatches`（PR は段階 6）
  - `use-composer-command-menu.test` の command 探索の再試行 6 件（React hook の timer）
- `promptStashStore.test` の localStorage の耐久性・v1 形式・citation 付きの復元: 保存形式も旧形式も持たない。citation の解析もまだ移植していない。
- `draftHeroTransition.test` の 5 件は browser の animation と view transition。server promotion の後に自動で遷移しない 1 件は、遷移を outbox の `StartedThread` が扱う。
- `composerContextRecords` の record の作成・import・legacy upgrade・clipboard: 下書きの context の管理にあたり、Draft はまだ context を持たない。legacy upgrade は旧形式。
- `threadQueueControlPresentation.test` の cancel の引数 1 件: 削除は intent 側が `QueueAction::Cancel` から `Command::CancelQueued` を作る。
- 質問への回答:
  - `threadRequests.test` の古い text 回答を補う 1 件: 旧形式の補完で、object identity を確かめている。
  - `pendingUserInput.test` の 5 件と threadActivity の 2 件: `agent_domain::Question` に option の value と自由回答の可否がない。これを使うのは OpenCode と Antigravity だけ。
- `session-logic.test` の plan step の所要時間 1 件: `PlanStep` に時間がない。
- agent まわり:
  - idle を使う 3 件（`threadSubagents`、`subagentDisplay`、`agentSpawnSummary`）: `ItemStatus` に idle がない。
  - agentSpawnSummary の native batch 2 件と workflow coordinator 2 件、provider alias 1 件、`threadAgentsPresentation` の thread を持たない agent 1 件: 対応する概念がない。task は必ず子 thread を持つ。
- `ThreadRelationshipsControl`:
  - scroll 領域の 1 件: markup だけを確かめている。
  - transfer の failed と resolved_native、source の thread から見る場合: domain の transfer にその状態がなく、transfer は target 側にだけ記録される。
  - fork source を lineage の親より優先する 1 件と、日付のない shell: thread の親は 1 つで、`created_at` は必ずある。
- `diffPanelStore.test` の thread を選ぶ前の既定 1 件: 選択は thread ごとの値なので、既定値の test と同じになる。
- ChatView.logic の setup を activity から読む 1 件: Host は setup を thread に記録しない。
- `UserFacingErrors.test` の Pi と Grok: 対応していない provider。

T3 と変えた点・近似:

- context window は attempt の `context_usage`（2026-10-06 の context occupancy）から取る。`totalProcessedTokens` は compaction 前の値だけ。
- 拒否の文面:
  - 1 つの Host code が T3 の複数の文に当たる場合は、1 つを選ぶか合わせた。`run-not-active`、`thread-not-active`、`maintenance-*`、`no-stable-source-run`、`request-not-ready` など。
  - T3 に対応がない約 20 の理由（`thread-already-exists`、`rollback-pending`、`invalid-attachment` など）は、同じ調子の短い文にした。
- 承認の選択肢: T3 と同じく、request 自身の選択肢を使うのは mcp-elicitation だけ。ほかは T3 の既定の選択肢を、request が受ける decision に絞る。
- domain にない時刻の代わり:
  - agent の「最後に更新された」: task に `updated_at` がないので、完了時刻か開始時刻で決める。
  - wake run の作業開始: 前の run から求める。
  - checkpoint の時刻: run の `completed_at`。
- 文字数の数え方: agent の 80 / 280 文字の上限は Unicode の文字で数える（T3 は UTF-16）。prompt の 120,000 文字と composer の offset は UTF-16 で数える。

A5 で接続した（2026-10-07、後述の「A5: 端末の状態・intent・UniFFI の公開」）:

- 拒否の文面は `Committed` の拒否にだけ使い、thread の provider の名前で書く。`NotSent`（transport の文）には使わない。
- setup card: stream の `None` で最後の snapshot を `held_setups` に残し、`resolve_setup_progress` に渡す。
- キューの移動は `QueueAction::Move` の 1 回の `ReorderQueued`。添付だけの複数選択の回答は空の text。Implement / Refine は `view::plan` の規則で判断する。
- 質問の回答の下書きと表示中の index、diff panel の選択と ignore whitespace、prompt stash、Draft の `MessageContext` は `Snapshot` に置いた。

未接続:

- thread の context record に入れる environment id（Host は1つなので持たない）。
- 「Changes」と Uncommitted、provider の skill と slash command、workspace の path 検索は 2026-10-08 に接続した（後述の「段階 4 の統合」）。
- 重複していた helper は 1 つにした（2026-10-07）: duration は `view::time::format_duration`、search ranking は `view::search_ranking`、JavaScript の文字列と数値の処理は `js_text`、名前の並びは `view::collation`、質問の回答の下書きは `view::requests`、一覧の行の状態は `thread_summary::thread_list_status`。

### 一覧・メニュー・検索・model・設定・project の view（2026-10-07）

表示用データは `Snapshot` と時刻（と明示した UI の選択）からの純粋関数にし、一覧は `Snapshot::shell_view()`（楽観的な lifecycle を重ねたもの）を読む。出力の record と enum は後の UniFFI 公開を前提にした形にした（bindings は A5）。期待値は T3 のまま。Host は 1 つなので T3 の environment の次元は持たない。

| T3 の原本 | agent-core の実装と検証 |
| --- | --- |
| client-runtime `state/models.ts`（行の runtime・provider stack・作業開始時刻） | `view::thread_summary::ThreadSummary`（shell 行を T3 の `EnvironmentThreadShell` の形で読む）。`the_provider_stack_ends_with_the_current_instance_and_keeps_the_newest_two_owners`、`working_time_counts_from_the_activity_run_and_falls_back_to_the_owning_run`、`archiving_waits_for_a_provider_turn_but_not_for_queued_work`、`a_shell_row_parks_at_idle_while_background_work_holds_its_completion`。 |
| client-runtime `state/threadSort.ts`、`threadSort.test.ts`、web `lib/threadSort.ts`、`lib/threadSort.test.ts` | `view::thread_sort`（pinned・active・settled の順序。鍵の計画は `ordering` を共有）。`uses_the_later_unsettle_time_when_an_old_thread_re_enters_the_active_list`、`prefers_the_persisted_settlement_stamp_over_later_activity`、`falls_back_to_the_latest_activity_when_the_stamp_is_missing`、`orders_by_settle_time_most_recently_settled_first`、`falls_back_to_last_activity_for_auto_settled_threads_without_a_settled_at_stamp`、`counts_a_turn_completion_as_activity_for_auto_settled_threads`、`breaks_timestamp_ties_by_id_so_the_order_is_stable`、`matches_the_per_comparison_order_on_a_shuffled_list_with_ties`、`keeps_input_order_and_descending_id_ties_for_both_sort_orders`、`sorts_threads_by_the_latest_user_message_in_recency_mode`、`falls_back_to_thread_timestamps_when_there_is_no_user_message`、`can_sort_threads_by_created_at_when_configured`、`returns_the_latest_active_thread_for_a_project`、`matches_the_first_sorted_eligible_thread_for_both_sort_orders`、`keeps_hidden_slots_available_when_inserting_between_visible_neighbors`、`materializes_keyless_rows_without_overwriting_hidden_slots`、`moves_a_thread_up_with_a_single_key_write`、`returns_none_when_the_move_falls_off_the_end_of_the_list`、`materializes_keys_for_the_whole_section_when_a_neighbor_is_keyless`、`breaks_equal_pin_keys_by_id`、`leaves_unique_insertable_keys_for_any_count`、`keeps_new_and_reopened_threads_ahead_of_the_saved_order`、`breaks_equal_order_keys_and_timestamps_by_thread`、`applies_every_move_across_a_mixed_keyless_and_keyed_section`、`moves_a_keyless_thread_into_the_arranged_run_with_one_write`、`materializes_a_large_active_list_without_changing_the_requested_order`。 |
| client-runtime `state/threadSettled.ts`、`threadSnoozed.test.ts`、`customSnooze.test.ts`、mobile `customSnoozeDate.ts`、`customSnoozeDate.test.ts`、web `Sidebar.snooze.ts`、`Sidebar.snooze.test.ts` | `view::snooze`。`hides_a_thread_whose_wake_time_is_in_the_future`、`stops_classifying_as_snoozed_once_the_wake_time_passes`、`never_snoozes_a_thread_with_no_snooze_state`、`wakes_early_when_the_agent_is_blocked_on_the_user`、`wakes_early_on_a_failure_that_happened_after_the_snooze`、`stays_snoozed_when_the_failure_predates_the_snooze`、`stays_snoozed_while_the_session_keeps_working`、`wakes_early_when_a_run_completes_after_the_snooze_was_set`、`ignores_runs_that_completed_before_the_snooze`、`a_quiet_snoozed_thread_does_not_raise_its_hand`、`approvals_input_and_failures_raise_the_hand`、`allows_snoozing_quiet_and_working_threads_alike`、`refuses_blocked_on_you_work`、`refuses_a_queued_turn_start`、`expires_queued_state_after_two_minutes`、`clears_queued_state_when_a_turn_adopts_the_message`、`bounds_future_client_clock_skew`、`woke_at_is_none_for_never_snoozed_and_still_snoozed_threads`、`reports_the_wake_time_for_a_timer_wake`、`reports_the_completion_time_for_an_early_run_completed_wake`、`falls_back_to_session_activity_for_blocked_or_failed_early_wakes`、`keeps_the_early_wake_authoritative_after_the_scheduled_time_passes`、`formats_remaining_time_coarsely_rounding_up`、`never_reads_zero_or_negative_while_still_snoozed`、`offers_the_shared_desktop_and_mobile_choices`、`when_labels_complement_the_label_instead_of_repeating_it`、`drops_the_evening_choice_once_evening_is_near_or_past`、`puts_next_week_a_full_week_out_when_today_is_monday`、`drops_next_week_on_sundays_when_it_matches_tomorrow`、`formats_preset_times_with_the_selected_clock_preference`、`describes_wakes_by_today_tomorrow_and_weekday`、`converts_local_date_and_time_to_an_absolute_wake_time`、`rejects_invalid_or_non_future_calendar_input`、`resolves_durations_from_the_confirmation_time`、`rejects_invalid_durations`、`rejects_nonexistent_local_times_at_the_spring_dst_transition`、`treats_duration_days_as_24_hours_across_dst`、`formats_local_input_values_without_converting_to_utc`、`passes_the_local_calendar_day_to_the_picker_without_shifting_it`、`applies_the_pickers_utc_calendar_day_while_retaining_the_local_time`、`applies_a_picked_local_time_without_changing_the_chosen_day`。 |
| client-runtime `state/threadInbox.ts`、`threadInbox.test.ts` | `view::inbox`（`InboxReturns`）。`stamps_a_thread_when_it_stops_working_but_never_on_the_first_observation`、`forgets_deleted_threads_and_resets_when_the_beta_turns_off`、`orders_working_threads_by_the_last_message_the_user_sent_not_by_later_runs`。 |
| mobile `lib/time.ts`、web の相対時刻と時刻表示（`timestampFormat.ts`） | `view::time`。`relative_time_never_counts_seconds`、`desktop_age_labels_say_just_now_under_a_minute_and_drop_ago_in_rows`、`formats_times_with_the_clock_preference`。 |
| web `components/Sidebar.logic.ts`、`Sidebar.logic.test.ts` | `view::sidebar::logic`。`row_accessibility_leads_with_the_title_without_folding_row_actions_into_its_name`、`a_pinned_thread_stays_in_the_pinned_shelf`、`lifecycle_shelves_are_authoritative_over_a_stale_pin`、`bulk_unpin_counts_only_the_pinned_rows_of_a_mixed_selection`、`bulk_unpin_is_omitted_when_nothing_selected_is_pinned`、`bulk_title_regeneration_counts_only_threads_that_can_start_a_new_regeneration`、`bulk_title_regeneration_shows_a_disabled_progress_item_when_every_thread_is_pending`、`bulk_title_regeneration_is_omitted_when_nothing_selected_supports_it`、`multi_select_offers_bulk_archive_with_the_selected_count`、`multi_select_disables_bulk_archive_when_a_selected_thread_is_running`、`the_project_scope_keeps_only_top_level_unarchived_threads`、`subagent_threads_are_hidden_from_the_sidebar`、`the_fork_parent_comes_from_the_fork_lineage`、`a_thread_completed_after_its_last_visit_is_unseen`、`a_missing_visit_marker_reads_as_seen`、`inactive_working_and_waiting_threads_recede_even_when_unread_and_woke`、`unread_ready_approval_and_input_threads_stay_prominent`、`active_and_selected_working_threads_stay_prominent`、`input_required_threads_stay_prominent_read_or_unread`、`prewarm_takes_the_first_visible_rows_up_to_the_limit`、`preferred_ids_lead_stale_ids_are_skipped_and_the_rest_keep_their_order`、`repeated_preferred_ids_do_not_duplicate_items`、`a_manual_project_order_keyed_by_location_is_honored`、`preference_aliases_resolve_to_their_items`、`preferred_ordering_is_a_permutation_that_leads_with_present_preferences`、`adjacent_thread_ids_follow_the_ordered_sidebar`、`approval_outranks_a_running_runtime`、`awaiting_input_outranks_a_running_runtime_below_approval`、`running_and_starting_runtimes_are_working`、`usage_limit_stops_stay_limited_and_visible_until_the_thread_recovers`、`failed_only_while_the_latest_run_failed`、`no_runtime_is_ready`、`a_waiting_runtime_shows_ahead_of_unread_and_woke`、`waiting_stays_static_while_working_shows_its_duration`、`the_default_scope_row_leads_while_the_query_is_empty`、`the_default_scope_row_hides_while_filtering`、`matching_projects_keep_source_order_and_a_miss_is_empty`、`closing_the_scope_menu_clears_the_query`、`the_scope_menu_stays_open_while_the_query_changes`、`working_time_uses_the_running_runs_start`、`working_time_uses_the_request_while_a_run_awaits_adoption`、`working_time_is_not_invented_when_the_newest_run_completed`、`working_time_shares_the_activity_start_when_a_newer_run_is_queued_or_cancelled`、`working_time_is_none_without_a_run_or_runtime`、`working_durations_read_in_seconds_minutes_and_hours`、`negative_working_durations_clamp_to_zero`、`row_ages_compact_the_relative_label`、`pending_approval_shows_before_every_other_pill`、`awaiting_input_shows_when_plan_mode_is_blocked_on_answers`、`a_running_thread_without_blockers_is_working`、`an_idle_thread_with_background_tasks_is_waiting`、`an_active_turn_stays_working_beside_background_tasks`、`waiting_ends_when_the_background_roster_clears`、`a_settled_plan_turn_with_a_proposed_plan_is_plan_ready`、`completion_is_not_manufactured_without_a_visit_marker`、`an_unseen_completion_without_blockers_is_completed`、`a_project_without_notable_threads_has_no_indicator`、`a_project_surfaces_its_most_urgent_thread`、`plan_ready_outranks_completed`、`waiting_ranks_below_active_work_and_above_plan_ready`、`delete_falls_back_to_the_top_remaining_thread_of_the_project`、`delete_fallback_skips_threads_deleted_in_the_same_action`、`projects_sort_by_the_latest_user_message_across_their_threads`、`projects_without_threads_sort_by_their_own_stamps`、`projects_without_stamps_sort_by_name_then_id`、`the_manual_project_order_is_kept`、`archived_threads_do_not_count_as_project_activity`、`project_sorting_matches_the_per_comparison_order_on_a_shuffled_list_with_ties`、`a_project_without_threads_reports_its_own_stamp`、`the_saved_project_order_applies_only_in_manual_mode`、`a_hidden_subagent_thread_does_not_reorder_projects`、`pin_order_keys_sort_between_their_bounds`、`pin_order_keys_extend_into_new_digits_between_adjacent_bounds`、`pin_order_keys_stay_ordered_under_repeated_top_insertion`、`pin_order_keys_stay_ordered_under_repeated_middle_insertion`、`pin_order_keys_refuse_corrupt_or_unordered_bounds`、`keyed_pins_sort_by_key_ahead_of_keyless_pins_newest_first`、`equal_pin_keys_break_by_id`、`a_sweep_covers_every_row_between_the_press_and_the_pointer_either_way`、`a_sweep_leaves_out_rows_that_cannot_apply_or_left_the_list`、`parking_the_open_thread_navigates_only_once_it_parked`、`a_completed_thread_reads_by_whether_its_background_roster_wakes_it`、`the_working_shelf_folds_away_running_threads_and_background_waits_only`、`a_ready_plan_stays_in_the_inbox_while_background_work_runs`、`the_inbox_puts_the_thread_that_finished_last_on_top_whatever_its_age`、`the_inbox_counts_a_return_the_host_does_not_stamp`、`drops_never_land_in_the_working_shelf_which_stays_out_of_the_inbox_order`、`a_time_ordered_inbox_drop_only_changes_lifecycle`、`drop_verbs_name_the_lifecycle_change`、`a_keyed_drop_into_the_pins_carries_the_moved_rows_key_on_the_pin`、`dropping_onto_the_settled_shelf_settles_and_a_settled_row_back_there_is_a_no_op`、`a_drop_preview_settles_or_resumes_the_thread_like_the_host`、`a_new_thread_in_the_current_project_needs_shift_unless_there_is_one_project`、`preferred_ordering_is_a_permutation_that_leads_with_present_preferences（proptest）`。 |
| web `components/Sidebar.tsx`（棚の組み立て、settled の 10 件と 25 件ずつの表示、card・slim、状態欄、時刻、recede、下書き、provider の重なり） | `view::sidebar::sidebar` → `SidebarView`。`threads_split_into_pinned_active_snoozed_and_settled_shelves`、`the_settled_header_stays_while_other_shelves_come_and_go`、`collapsed_shelves_keep_the_open_threads_row`、`snoozed_rows_sort_by_the_soonest_wake_and_show_when_they_return`、`the_settled_tail_pages_ten_then_twenty_five_and_keeps_the_open_thread`、`the_working_section_folds_inbox_work_but_leaves_pins_in_place`、`a_working_row_shows_its_status_and_duration_and_recedes_unless_open`、`an_unread_completion_reads_done_and_a_quiet_row_shows_its_age`、`a_woken_thread_carries_the_wake_until_visited`、`an_unsent_draft_marks_the_row_unless_it_is_open`、`new_thread_drafts_lead_the_sidebar_except_the_one_being_typed`、`the_project_scope_filters_rows_and_falls_back_to_all_when_the_project_is_gone`、`projects_follow_the_saved_order_in_manual_mode`、`an_empty_sidebar_explains_why`、`search_lists_title_matches_then_message_matches_across_every_shelf`、`the_bulk_menu_counts_only_rendered_selected_rows`、`parking_the_open_thread_moves_to_the_next_card_or_a_new_thread`、`a_settle_sweep_stays_in_the_pressed_rows_section`、`dropping_an_active_row_on_the_pins_plans_a_keyed_pin`、`inbox_returns_reorder_the_inbox_once_a_thread_stops_working`。 |
| mobile `features/threads/threadListV2.ts`、`threadListV2.test.ts`、`thread-list-v2-items.tsx`（swipe と長押しの項目）、`thread-title-regeneration-menu.ts`、`thread-title-rename.ts` と各 test | `view::thread_list`（`thread_list` → `ThreadListView`。長押しの項目は `view::thread_menu` の型を共有）。`accepts_a_displayed_evening_preset_while_its_wake_time_is_still_future`、`expires_a_displayed_preset_once_its_wake_time_has_passed`、`recomputes_presets_that_remain_available_instead_of_using_old_timestamps`、`distinguishes_usage_limits_from_ordinary_failures_and_clears_the_label_after_recovery`、`prioritizes_approval_over_a_running_runtime`、`reports_waiting_when_the_runtime_parks_idle_for_background_tasks`、`presents_an_unseen_completion_by_its_background_roster`、`resolves_ready_for_quiescent_threads`、`queued_messages_list_a_settled_thread_in_the_active_block`、`queued_messages_keep_a_settled_thread_in_the_reorderable_active_section`、`offers_settle_and_snooze_for_an_active_snoozable_thread`、`offers_unsettle_and_snooze_for_settled_history`、`omits_snooze_when_the_thread_does_not_allow_it`、`offers_wake_and_no_snooze_on_a_snoozed_row`、`reports_when_an_unadopted_turns_grace_window_lapses`、`has_no_gate_expiry_once_snoozable_or_when_only_data_can_unblock_it`、`honors_a_saved_active_order_and_leaves_new_threads_above_it`、`orders_active_threads_by_creation_time_newest_first_ignoring_activity`、`uses_each_saved_order_and_excludes_settled_snoozed_and_archived_rows`、`places_a_persisted_settled_thread_in_the_settled_shelf`、`hides_snoozed_threads_and_counts_them`、`moves_a_settled_pinned_thread_into_the_settled_shelf`、`keeps_active_pinned_threads_in_the_pinned_block`、`snooze_hides_a_pinned_thread_and_wake_restores_it_to_the_pinned_block`、`classifies_snooze_with_the_second_precise_clock_and_reports_the_next_wake`、`builds_snoozed_rows_between_active_and_settled_when_the_shelf_is_expanded`、`collapses_to_a_header_only_shelf`、`keeps_the_selected_thread_on_a_collapsed_shelf`、`partitions_settled_threads_into_a_slim_shelf`、`collapses_settled_threads_to_a_counted_shelf_header`、`keeps_the_selected_settled_thread_visible_when_its_shelf_is_collapsed`、`keeps_cards_in_creation_order_while_settled_sorts_by_recency`、`sorts_settled_threads_by_their_persisted_settlement_timestamp`、`keeps_settled_threads_in_the_tail_and_filters_by_search_query`、`includes_a_thread_matched_by_message_content`、`scopes_the_flat_list_to_one_project`、`caps_the_settled_tail_at_the_limit_and_reports_the_hidden_count`、`splices_queued_tasks_between_the_active_block_and_the_settled_tail`、`ends_the_list_with_queued_tasks_when_nothing_has_settled_yet`、`keeps_the_settled_shelf_between_active_and_settled_rows_when_nothing_is_queued`、`places_queued_tasks_before_a_collapsed_snoozed_shelf`、`holds_the_order_through_every_intermediate_key_upsert`、`keeps_the_action_guard_pending_when_receipts_precede_canonical_shells`、`keeps_search_results_in_the_full_pending_section_order`、`releases_for_real_section_membership_and_foreign_key_changes`、`does_not_hide_a_concurrent_return_to_a_previously_confirmed_key`、`preserves_the_hold_for_activity_but_releases_for_a_reopened_sort_anchor`、`excludes_subagents_from_navigation_search_and_ordering_while_retaining_user_forks`、`rebuilt_items_over_identical_rows_are_equal`、`notices_a_changed_wake_countdown_label`、`shelf_headers_differ_by_count_expansion_and_loading`、`flips_a_trailing_divider_when_a_neighbour_changes`、`carries_snooze_choices_on_every_row_whose_swipe_offers_them`、`blanks_the_time_for_rows_that_render_a_label_instead`、`a_minute_tick_changes_only_rows_whose_clock_driven_content_moved`、`keeps_unread_completion_labels_stable_across_a_minute_tick`、`keeps_hour_granularity_rows_stable_across_a_minute_tick_without_snooze_choices`、`changes_the_snoozed_countdown_row_when_the_wake_label_advances`、`trailing_dividers_follow_the_final_neighbour_order`、`stamps_queued_outbox_messages_onto_the_matching_row`、`stamps_move_availability_on_card_rows_only`、`keeps_the_settled_rows_swipe_snooze_choices_fresh_across_a_tick`、`stamps_the_shelf_loading_state_onto_headers`、`folds_unpinned_working_threads_into_a_collapsed_shelf`、`shows_working_rows_as_cards_when_expanded_or_only_the_selected_one_when_collapsed`、`orders_the_inbox_by_the_latest_return_this_device_observed`、`places_the_working_shelf_after_queued_tasks_and_before_snoozed_and_settled_threads`、`builds_each_rows_long_press_menu`、`checks_the_current_auto_settle_choice`、`offers_title_regeneration`、`shows_and_disables_the_pending_regeneration`、`trims_a_changed_title`、`rejects_empty_and_unchanged_titles`、`resolves_provider_drivers_only_when_the_current_instance_is_known`、`lists_the_shell_with_unsent_messages_and_new_threads_from_the_outbox`、`filters_rows_and_new_threads_by_project_and_search`、`offers_moves_by_the_section_order`。 |
| mobile `features/threads/threadOrder.ts`、`threadOrderAvailability.test.ts`、`state/thread-order.ts`、`thread-order.test.ts` | `view::thread_order`。`keeps_keyed_neighbors_as_usable_anchors`、`materializes_keyless_rows_for_a_move`、`reserves_snoozed_keys_when_moving_visible_rows`、`moves_a_keyed_row_with_one_write_despite_keyless_rows_elsewhere`、`moves_across_multiple_rows_while_keeping_hidden_anchors_in_place`、`rejects_missing_self_and_unchanged_destinations`、`persists_a_dropped_row_and_holds_its_order_until_confirmed`、`allows_a_long_drop_with_one_write`、`inserts_into_an_empty_section`、`places_an_incoming_row_between_existing_anchors_without_rewriting_them`、`rejects_removed_targets`、`clears_pinning_settlement_and_snooze_when_returning_a_parked_thread_to_active`、`does_not_send_lifecycle_commands_for_an_ordinary_active_reorder`、`locks_every_row_while_a_pending_reorder_is_in_flight`、`rewrites_skip_rows_that_already_hold_their_key`、`keeps_moves_available_for_ids_containing_colons`、`denies_single_row_sections_in_both_directions`、`walks_past_a_hidden_reserved_key_at_every_adjacency_midpoint`、`rewrites_a_section_of_consecutive_single_letter_keys`、`every_offered_move_realizes_the_adjacent_swap`、`blocks_another_pickup_after_receipts_and_clears_on_the_final_canonical_upsert`、`waits_for_receipts_when_shells_arrive_first`、`a_canonical_membership_change_invalidates_the_move`、`names_the_drag_action_for_each_destination_instead_of_its_section`、`does_not_offer_a_parked_section_reorder_or_a_snooze_without_a_wake_time`、`moves_the_active_header_and_intervening_rows_up_when_unpinning`、`opens_a_full_gap_below_the_pinned_header_when_pinning`、`leaves_the_source_gap_in_place_for_cancellation_or_its_current_destination`、`moves_only_crossed_rows_for_an_adjacent_reorder`、`every_offered_move_realizes_the_adjacent_swap（proptest）`。 |
| web `components/threadActionMenu.logic.ts`、`threadActionMenu.logic.test.ts`、mobile `useThreadHeaderOptions` | `view::thread_menu`（`thread_menu` → `ThreadMenuView`。`ThreadMenuAction` は `ThreadAction`・`Intent` に対応）。`groups_project_settings_with_utility_actions_before_archive`、`offers_project_filtering_only_for_surfaces_with_a_scoped_thread_list`、`offers_the_way_back_to_all_projects_once_the_list_is_scoped`、`includes_branch_items_only_for_threads_with_a_branch`、`flips_lifecycle_labels_with_thread_state`、`offers_auto_settle_as_a_submenu_with_the_current_option_checked`、`disables_snooze_when_the_thread_cannot_snooze_keeping_presets_visible`、`disables_title_regeneration_while_one_is_in_flight`、`marks_delete_as_destructive_and_keeps_it_last`、`offers_archive_as_a_non_destructive_action_right_before_delete`、`disables_archive_while_the_thread_is_running`、`draft_menu_offers_only_the_copy_values_the_draft_has`、`draft_menu_drops_project_settings_without_a_project_and_keeps_discard_last`、`unpin_asks_only_when_enabled_and_names_the_thread`、`delete_asks_by_default_and_archive_only_when_enabled`、`items_name_the_thread_actions_they_send`、`reads_the_thread_from_the_active_shell`、`the_list_reads_settlement_from_its_shelf_and_the_header_from_the_override`、`a_waiting_or_running_thread_cannot_snooze_or_archive`。 |
| client-runtime `state/threadSearch.ts`、`threadSearch.test.ts`、contracts `threadSearch.ts`（2〜200 文字）、web `searchSidebarThreads` | `view::search`（`search_view` → `SearchView`。sidebar と mobile の一覧もこの検索を使う）。`matches_thread_titles_case_insensitively_and_preserves_their_order`、`does_not_match_project_metadata`、`returns_no_results_for_an_empty_query`、`appends_content_only_matches_after_every_title_match`、`lists_a_thread_matching_both_title_and_content_once`、`ignores_content_matches_for_threads_outside_the_list`、`matches_linked_pull_request_terms`、`accepts_message_searches_at_the_maximum_query_length`、`ignores_message_searches_outside_the_query_bounds`、`highlights_every_occurrence_folding_ascii_case`、`names_who_wrote_the_matched_message`、`the_view_lists_title_then_message_matches_in_list_order_with_excerpts`、`an_empty_search_says_whether_message_matches_may_still_arrive`。 |
| client-runtime `state/archivedThreads.ts`、`archivedThreads.test.ts`、web・mobile のアーカイブ一覧 | `view::archived`（`archived_view` → `ArchivedView`）。`groups_archived_threads_by_project_and_sorts_newest_first`、`matches_project_thread_and_branch_text`、`ignores_non_archived_entries_returned_in_a_snapshot`、`screen_groups_order_by_their_newest_row_then_title`、`settings_groups_keep_project_order_and_list_newest_archives_first`、`rows_carry_labels_and_their_unarchive_and_delete_actions`、`does_not_expose_an_archived_snapshot_failure_message`、`empty_states_follow_loading_and_the_query`、`a_reload_over_shown_rows_is_refreshing`。 |
| client-runtime `state/providerInstanceDisplay.ts`、`providerInstanceDisplay.test.ts` | `view::models::display`。`keeps_a_snapshot_name_that_differs_from_the_brand_label`、`humanizes_a_custom_instance_id_when_the_snapshot_only_carries_the_brand_label`、`uses_the_brand_label_for_the_default_instance`、`initials_take_two_characters_of_one_word_or_the_first_of_two_words`、`initials_keep_an_emoji_whole`、`accent_colors_must_be_six_digit_hex`、`shows_the_badge_for_an_accent_color`、`shows_the_badge_when_two_entries_share_a_driver_even_without_an_accent`、`hides_the_badge_for_a_single_instance_of_a_driver_with_no_accent`、`labels_a_custom_instance_by_its_id_so_its_initials_differ_from_the_default`、`uses_the_current_runtime_owner_after_a_provider_handoff`、`a_thread_row_hides_the_badge_for_a_single_instance_with_no_accent_color`、`a_thread_row_resolves_unknown_instances_to_nothing`。 |
| client-runtime `state/models.ts`（provider instance と model の一覧） | `view::models`。`the_host_catalogue_lists_both_instances_and_formats_codex_names`。 |
| web `components/chat/TraitsPicker.tsx`、`TraitsPicker.test.ts`（option の記述と既定値） | `view::models::options`、`view::models::traits`。`keeps_the_codex_catalog_display_formatting`、`applies_selection_values_to_capability_descriptors`、`a_stored_prompt_injected_value_falls_back_to_the_default_choice`、`builds_wire_format_option_selections_from_descriptors`、`builds_dispatch_options_only_from_explicit_selections`、`keeps_slash_commands_intact_when_ultrathink_is_selected`、`still_adds_the_ultrathink_prefix_to_ordinary_prompts`、`ultrathink_is_detected_as_a_whole_word_in_any_case`、`maps_current_codex_model_capability_fields`、`uses_standard_routing_when_the_catalog_has_no_default_service_tier`、`the_flagship_codex_family_defaults_to_medium_reasoning`、`claude_efforts_are_labelled_and_ultrathink_goes_in_the_prompt`、`updates_generic_select_options_without_knowing_provider_specific_ids`、`updates_generic_boolean_options`、`prefers_an_exact_slug_and_returns_no_capabilities_for_an_unknown_one`、`omits_fast_mode_from_the_label_entirely_when_it_is_off`、`shows_the_bolt_instead_of_a_text_label_when_fast_mode_is_on`、`treats_codex_standard_and_fast_service_tiers_as_fast_mode_states`、`uses_a_distinct_double_bolt_for_codex_ultrafast`、`uses_ultrafast_without_requiring_a_fast_tier`、`keeps_other_codex_service_tiers_in_the_label`、`keeps_standard_as_text_for_models_without_speed_tiers`、`keeps_the_codex_service_tier_readable_when_it_is_the_only_trait`、`keeps_ultrafast_readable_when_it_is_the_only_trait`、`keeps_non_fast_mode_booleans_as_text_labels`、`falls_back_to_a_text_label_when_fast_mode_is_the_only_trait`、`stays_blank_when_descriptors_resolve_to_no_label_and_there_is_no_fast_mode`、`still_renders_the_prompt_controlled_ultrathink_label_alongside_the_bolt`、`the_traits_view_lists_selects_then_toggles_with_current_values`、`an_ultrathink_prompt_controls_the_effort_until_it_is_only_in_the_prefix`、`picking_ultrathink_prefixes_the_prompt_and_another_effort_removes_it`、`toggling_fast_mode_stores_every_current_option`。 |
| web `modelOrdering.ts`、`modelOrdering.test.ts` | `view::models::ordering`。`groups_favorites_first_while_preserving_provider_model_order_inside_each_group`、`sorts_the_favorites_view_by_provider_order_then_provider_model_order`、`toggling_a_favorite_adds_then_removes_it`。 |
| web `ProviderModelPicker.tsx`、`ModelPickerContent.tsx` の論理と test | `view::models::picker`（`model_picker` → `ModelPickerView`）、`view::models::search`。`keeps_model_and_legacy_section_keys_distinct_for_colliding_instance_names`、`wraps_through_favorites_and_ready_instances_skipping_unavailable_providers`、`keeps_thread_locks_and_the_selected_unavailable_catalog`、`handles_an_empty_catalog_and_a_removed_selection_in_either_direction`、`the_trigger_uses_the_first_option_label_for_a_missing_model`、`the_trigger_uses_the_first_option_when_the_active_instance_entry_is_missing`、`the_trigger_prefers_the_matching_model_and_prompts_without_a_catalogue`、`keeps_instance_initials_visible_in_the_resting_trigger`、`opens_on_favorites_when_there_are_any_and_groups_them_first_in_an_instance`、`a_locked_thread_disables_other_drivers_and_keeps_them_last`、`a_search_spans_instances_and_reports_when_nothing_matches`、`unavailable_instances_list_no_models_and_explain_themselves`、`a_started_thread_without_loaded_state_locks_to_its_driver`、`rows_carry_the_disabled_reason`、`builds_provider_agnostic_search_text_from_generic_fields`、`matches_typo_tolerant_multi_token_queries`、`rejects_results_when_any_query_token_does_not_match`、`ranks_exact_token_matches_ahead_of_fuzzier_matches`、`gives_favorite_models_a_strong_enough_ranking_boost_for_partial_queries`、`does_not_let_the_favorite_boost_outrank_clearly_better_textual_matches`、`matches_a_custom_instance_display_name_against_its_models`。 |
| mobile `features/threads/thread-provider-instance.ts`、`state/thread-provider-switching.ts` と各 test | `view::models::switching`。`offers_every_provider_when_the_session_can_hand_the_conversation_off`、`leaves_a_thread_that_never_ran_a_turn_unbound`、`allows_an_imported_thread_to_hand_off_before_opening_a_provider_session`、`keeps_a_preparing_turn_bound_before_its_provider_session_appears`、`keeps_a_started_thread_bound_until_its_projection_resolves_a_session`、`an_empty_loaded_state_reads_as_unknown`、`allows_model_changes_before_a_provider_session_has_started`、`allows_unchanged_model_selections`、`blocks_a_provider_switch_only_without_handoff`。 |
| web の会話の設定、mobile `SettingsRouteScreen.logic.ts`、`autoSettleSettingsSync.ts`、`settings-scoped-server.ts` と各 test | `view::settings`（`settings_view` → `SettingsView`）。`edits_a_project_override_without_changing_the_host_default`、`inheriting_removes_only_that_project_override`、`resets_only_the_selected_override_and_rejects_host_wide_writes_from_a_project`、`host_wide_changes_write_the_host_value`、`the_days_field_commits_only_whole_days_in_range`、`host_rows_wait_for_the_host_settings`、`the_host_page_shows_current_values_and_offers_resets`、`a_project_page_shows_effective_values_with_their_source`。 |
| web `projectScripts.ts`、`projectScripts.test.ts`、shared `projectScripts.ts` | `view::projects::scripts`。`builds_scripts_with_preview_settings`、`omits_preview_settings_when_no_preview_url_is_configured`、`only_records_async_false_for_setup_scripts_that_should_block_the_agent`、`builds_and_parses_script_run_commands`、`preserves_the_exact_id_at_the_shortcut_length_limit`、`slugifies_and_dedupes_project_script_ids`、`resolves_primary_and_setup_scripts`、`builds_default_runtime_env_for_scripts`、`allows_overriding_runtime_env_values`、`prefers_the_worktree_path_for_script_cwd_resolution`、`a_new_id_shortens_its_base_to_fit_the_suffix`、`the_form_requires_a_name_and_a_command_and_trims_its_fields`、`editing_starts_from_the_saved_script`、`a_new_setup_script_replaces_the_previous_setup`、`updates_and_deletes_scripts_by_id`、`the_view_lists_the_projects_scripts_and_prefers_the_last_run`。 |
| client-runtime `operations/projects.ts`、`operations/projects.test.ts`、`state/projectCommands.ts` | `view::projects::add`、`view::projects::paths`。`only_allows_project_creation_in_connected_environments`、`resolves_initial_browse_paths_from_settings`、`rejects_unsupported_windows_paths_on_non_windows_environments`、`resolves_relative_paths_from_the_active_project_cwd`、`needs_a_path_and_an_active_project_for_relative_paths`、`finds_existing_projects_by_normalized_path`、`derives_the_browse_target_and_navigation_state`、`filters_names_hidden_directories_and_exact_matches_consistently`、`the_listing_offers_only_folders_in_name_order`、`detects_windows_drive_paths`、`detects_unc_paths`、`detects_windows_absolute_paths`、`detects_explicit_relative_paths`、`normalizes_a_bare_windows_drive_root_the_same_as_one_with_a_trailing_separator`、`normalizes_trailing_separators_for_dispatch_and_comparison`、`normalizes_windows_style_paths_for_comparison`、`finds_existing_projects_even_when_the_input_formatting_differs`、`infers_project_titles_from_normalized_paths`、`detects_browse_queries_across_supported_path_styles`、`only_treats_windows_style_paths_as_browse_queries_on_windows`、`resolves_explicit_relative_paths_against_the_current_project`、`navigates_browse_paths_with_matching_separators`、`detects_browse_path_boundaries`、`only_allows_browse_up_after_entering_a_directory`、`normalizing_twice_changes_nothing`、`normalizing_twice_changes_nothing（proptest）`。 |
| mobile `features/threads/new-task-project-selection.ts`、`projectThreadCreationValidation.ts` と各 test | `view::projects::selection`。`preserves_an_explicit_project_selection`、`selects_the_only_physical_project_when_no_project_was_explicitly_selected`、`selects_one_logical_project_even_when_it_has_multiple_physical_workspaces`、`does_not_preserve_a_project_key_that_is_missing_from_the_catalog`、`asks_when_there_are_several_logical_projects`、`keeps_all_projects_for_an_empty_or_whitespace_only_query`、`matches_logical_names_and_workspace_names_or_paths_without_case_sensitivity`、`preserves_the_whole_logical_project_when_a_workspace_matches`、`uses_the_live_checkout_for_an_untouched_local_draft_label_and_recorded_branch`、`prefers_an_explicit_picker_choice_over_the_current_checkout`、`stays_none_when_no_ref_is_checked_out`、`never_borrows_the_current_checkout_for_a_worktree_draft`、`keeps_the_explicit_base_branch_for_a_worktree_draft`、`a_task_needs_text_and_a_worktree_needs_a_base_branch`。 |
| web `state/agentSessions.ts`、`onboarding/useProjectScans.ts`、`components/onboarding/WelcomeWizard.tsx`（`WelcomeWizard.test.tsx` の論理） | `view::projects::import`（`conversation/agentSessions/scan|import`）。`keeps_existing_projects_available_for_thread_history_import`、`keeps_projects_older_than_30_days_out_of_the_default_selection`、`keeps_future_activity_out_of_the_default_selection`、`keeps_non_git_folders_and_thin_histories_out_of_the_default_selection`、`groups_clones_by_origin_keeps_local_repos_separate_and_folds_non_git_folders_away`、`uses_the_scanned_project_id_before_the_project_reaches_the_client`、`uses_the_scanned_project_id_when_the_client_still_has_an_older_project_at_that_root`、`returns_none_to_create_a_project_when_neither_the_scan_nor_the_client_has_a_project_id`、`finds_an_existing_project_by_normalized_root`、`finds_an_alias_after_the_scanner_returns_its_persisted_project_root`、`finds_the_current_root_owner_when_the_scan_has_no_project_id`、`does_not_reuse_a_moved_project_when_the_scan_has_no_project_id`、`skips_a_failed_first_project_for_a_later_project_with_imported_history`、`prefers_a_partial_first_import_that_added_history`、`uses_a_completed_zero_history_project_when_no_import_added_history`、`keeps_an_earlier_successful_import_available_on_retry`、`ignores_cached_successes_outside_the_current_retry_selection`、`enters_the_workspace_after_a_partial_import_and_warns`、`preserves_the_import_warning_when_setup_is_finished_without_importing_again`、`a_retry_skips_projects_that_imported_completely_and_reports_failures`、`nothing_imported_reads_as_a_failure`、`the_step_lists_groups_folders_and_the_import_button`、`the_step_reports_loading_failures_and_empty_scans`。 |
| client-runtime `state/attachments.ts`、`attachments.test.ts`、web・mobile `state/attachments.ts` | `view::attachments`。`clamps_the_advertised_limit_to_the_turn_contract_cap`、`formats_attachment_row_sizes`、`formats_small_upload_limits_without_rounding_them_to_zero_mb`、`keeps_whole_mb_upload_limits_for_standard_server_caps`、`keeps_supported_images_and_heic_photos_on_the_image_path`、`rejects_unsupported_image_types_instead_of_attaching_them_as_generic_files`、`preserves_text_paste_when_an_application_adds_a_synthetic_generic_file`、`claims_unsupported_image_pastes_so_the_composer_can_report_them`、`claims_generic_file_only_pastes_so_the_composer_can_report_validation_errors`、`routes_empty_and_oversized_generic_files_to_composer_feedback`、`ignores_an_empty_clipboard`、`claims_image_pastes_even_when_clipboard_text_is_present`、`falls_back_to_the_extension_when_an_image_arrives_without_a_mime_type`、`infers_supported_image_types_from_octet_stream_files`、`does_not_infer_images_for_unknown_extensions_or_specific_conflicting_mime_types`、`admits_files_up_to_the_draft_limit_and_reports_each_refusal`、`a_full_draft_refuses_more_files`、`image_preparation_failures_name_their_cause`、`a_draft_over_the_send_limits_cannot_send`、`admission_never_overfills_a_draft`、`admission_never_overfills_a_draft（proptest）`。 |

対象外にしたもの（理由）:

- 不正な時刻文字列を扱う test（sort・snooze・settled の malformed / invalid の場合）: `Timestamp` は常に正しい時刻で、壊れた値が届かない。正しい値の部分は移植した。
- environment をまたぐ並びと絞り込み（id の後の environment での tiebreak、environment の選択）: store は Host ごとに 1 つ。id の部分は移植した。
- React・DOM・JavaScript の object identity を確かめる test（hook、class 名、pointer と DOM の問い合わせ、recycled list の等価、参照の保持）: 描画は各クライアントが行う。id・順序・内容の比較に置き換えられるものは置き換えた。
- ソース管理・PR・予定実行・usage・端末プレビュー・アカウント接続の設定と操作: 2026-10-07 の決定で段階 6。

T3 に合わせた判断と残る差:

- 時刻表示の `locale` は英語の既定（12 時間制）として扱う。snooze の preset と custom の日時は端末の時間帯（`chrono::Local`）で計算する。
- mobile の長押しの項目と web の thread menu は同じ `ThreadMenuItem` の型で返す（UniFFI の型名を 1 つにするため）。検索、名前の照合順、行の経過時間、drop の section も各 view で 1 つを共有する。
- `ThreadShell` に無い値は既定値で読む: `unsettled_at`（再開した thread の並び）、subagent の lineage、PR の一覧（linked PR のみ）、diff の統計、terminal の表示、project の favicon と作成・更新時刻。Working の beta の `InboxReturns`、移動中の並び（`PendingThreadOrder`）、model の favorites、設定、session scan の結果は A5 で `Snapshot` に置いた。model option の記述と instance の表示情報は Host から届く必要がある。

### agent-core の timeline・work log・markdown（A4、2026-10-07）

会話の timeline（desktop と mobile）、work log、markdown の補助を `crates/agent-core/src/view/` と `presentation/markdown/` に移植した。期待値は T3 のまま。T3 の item は domain の `Item` に、web の `WorkLogEntry` は `view::work_log::WorkLogEntry` に当たる。T3 が React component の中で決めている表示（icon、tone、展開、ラベル）も純粋関数に移し、client は描くだけにした。row は layout（desktop / mobile）で導出を選び、同じ `TimelineRow` 型を返す。thread ごとの cache は cursor・`history_revision`・`detail_revision`（`ThreadSync` に追加）と shell の行・outbox・setup・client の開閉状態で作り直しを決め、`timeline_update` が前の row からの splice を返す（desktop `app.rs` の `timeline_splice` を core へ移した）。

| T3 の原本 | agent-core の実装と検証 |
| --- | --- |
| web `MessagesTimeline.logic.ts`、`MessagesTimeline.logic.test.ts` | `view::timeline::desktop`（`derive_desktop_rows`、`DesktopRow`）、`desktop_folds`、`desktop_labels`、`desktop_layout`。`desktop_tests.rs`・`desktop_labels_tests.rs` に全件（下の対象外を除く）。preview の tool は `orchestration.thread_read`、`t3_project_clone` は `project_list` / `project_create`（期待値の文面は "Registered a project" など、同じ規則の結果）、「imported V1 turns」は `keeps_runless_turns_folded_once_the_threads_first_run_starts`、streaming の row parity は内容の比較 |
| web `session-logic.ts`（`deriveTimelineEntriesFromVisibleTurnItems`、`projectedWorkEntry`、`providerErrorPresentation`、`deriveRevertTurnCountByUserMessageId`、`timelineEntryIsPersistentResourceCard`）、`session-logic.test.ts` | `view::timeline::entries`。`entries_tests.rs` の `labels_provider_retry_progress_delay_recovery_and_exhaustion`、`assigns_run_rollback_to_the_turn_start_message_instead_of_a_later_steer`、`uses_visible_turn_item_order_and_keeps_provider_errors_in_the_work_log`、`keeps_task_progress_available_to_the_composer_and_out_of_the_timeline`（timeline の半分）、`keeps_failed_tool_items_tool_toned_so_groups_still_summarize`、`waits_for_a_dispatched_turn_item_before_adding_queued_input_to_the_timeline`、`appends_unsent_messages_after_committed_history_without_reordering_it`、`uses_projected_plan_status_and_file_contents_in_timeline_entries`、`resolves_the_attempt_identity_of_each_item`、`keeps_async_answers_in_the_question_row`、`keeps_completed_command_failures_visible_without_exposing_output`（3 件）、`retains_read_image_previews_without_tool_output`、`labels_a_read_of_a_bare_filename_from_its_structured_input`、`keeps_browser_identity_and_its_source_on_a_completed_tool_row`、`keeps_provider_returned_images_on_an_assistant_message`、`keeps_an_idle_provider_task_neutral_without_a_completion_mark`、work-log failure policy の 4 件、`deduplicates_unsent_messages_once_their_items_arrive`。`excludes_checkpoint_only_work_from_the_timeline` は `desktop_tests.rs` |
| web `session-logic.runtime-diagnostics.test.ts` | `desktop_tests.rs` の 4 件、`entries_tests.rs` の `shows_the_full_system_notice_without_a_detail`、`does_not_turn_an_unrelated_tool_payload_message_into_a_diagnostic` |
| web `MessagesTimeline.tsx`（`WorkEntryLogRow`、`workEntryIconName`、`workToneIcon`、`toolGroupSummaryIconName`、`buildToolCallExpandedBody`、`remarkThoughtPreview`）、`V2ItemInspector.tsx`、`MessagesTimeline.test.tsx` の work row の件 | `view::timeline::desktop_work_row`、`presentation::markdown::plain_text_preview`。`shows_dynamic_tool_input_without_cached_output_when_the_row_is_expanded`、`leads_an_unanswered_question_row_with_the_question_text`、`renders_provider_retries_in_the_normal_work_log`、`renders_app_tools_with_the_product_logo_and_pretty_name`、`formats_changed_file_paths_from_the_workspace_root`、`renders_a_muted_failure_marker_for_failed_tool_lifecycle_entries`、`keeps_the_red_treatment_for_severe_orchestration_failures`、`shows_plain_text_for_a_reasoning_preview`（7 件）、`expands_and_collapses_a_tool_call_through_its_header`、`loads_withheld_command_output_on_expansion_and_shows_it_once_loaded`、`a_failed_turn_draws_its_provider_failure_with_a_preparation_retry` |
| web `V2LifecycleRow.tsx`、`TimelineSystemDivider`、client-runtime `handoff.ts`（`handoff.test.ts`）、mobile `thread-handoff-row.tsx` | `view::timeline::lifecycle`。`handoff.test.ts` の 2 件、divider・subagent・通知・作成した thread の追加テスト。handoff は domain に item がないので `State.transfers` の provider handoff から divider を作り、その run の最初の entry の前に置く（T3 は ordinal `run * 100 - 1`）。`places_a_context_handoff_before_the_run_that_received_it` |
| web `ThreadErrorBanner.tsx`（test の 5 件）、`UsageLimitRecoveryBanner.tsx`、mobile `UsageLimitRecoveryCard.tsx` | `view::timeline::banners`。`orchestrationV2ThreadError.ts` は domain の `ThreadShell`（`last_error`、`last_error_class`、`usage_limit_reset_at`）が同じ値を持つので移植しない |
| web `lib/turnDiffTree.ts`（test 6 件）、`ChangedFilesTree.tsx`（test 3 件と 2 表） | `view::timeline::changed_files`（`Checkpoint.files` から） |
| web `MessagesTimeline.tsx` の user / assistant の行、mobile `userMessageIntentBadge.ts`（test 4 件）、`MessagesTimeline.test.tsx` の該当 8 件、`resolveAssistantMessageCopyState` の 5 件 | `view::timeline::message`（"Sent by another agent"、Queued / Steer、折りたたみ 600 UTF-16 / 8 行、"Edit from here"、Fork、copy）。directive の copy は `copies_the_rendered_representation_of_directives` |
| web `proposedPlan.ts` の表示部分（`stripDisplayedPlanMarkdown`、`buildCollapsedProposedPlanPreviewMarkdown`、`buildProposedPlanMarkdownFilename`）、`ProposedPlanCard.tsx` の折りたたみ（900 文字 / 20 行） | `view::timeline::plan_card`。`proposedPlan.test.ts` の該当 6 件と `a_long_plan_collapses_behind_its_preview` |
| mobile `lib/threadActivity.ts`、`threadActivity.test.ts` | `view::timeline::mobile`（`build_thread_feed`、`mobile_feed`、`FeedRow`）、`mobile_presentation`。回答の下書きは `view::requests`。`mobile_tests.rs` と `view::requests` の tests に全件（下の対象外を除く） |
| mobile `lib/threadActivityInspector.ts`（test） | `view::timeline::mobile_inspector`（4 件を調整、読み込んだ detail を使う 1 件を追加） |
| mobile `pending-thread-feed.ts`（test 3 件） | `view::timeline::pending`。行の assertion を移植した（outbox 側は前節） |
| mobile `thread-work-log.tsx`、`work-log-layout.tsx`、`thread-subagent-group.tsx`、`thread-activity-row-presentation.ts`、`threadContentPresentation.ts`、`threadActivityFileNavigation.ts`、`subagent-card-presentation`（elapsed）、`thread-feed-live-follow.ts` | `mobile_work_log`、`mobile_follow`、`work_row`（両 layout が使う work row の record）。`mobile_work_log_tests.rs`、`mobile_follow::tests` |
| client-runtime `work-log/commandLabel.ts`（test 37 件・293 case） | `view::work_log::command_label`、`command_label_tests.rs` |
| `work-log/presentation.ts`（test）、`markdownImages.ts`、`mediaSource.ts`、shared `usageFormat.ts` の `formatTokens` | `view::work_log::presentation`（29 件と追加 7 件）、`media_source` |
| `work-log/toolPresentation.ts`、`userInput.ts`、`scrollAnchor.ts`、`itemDetail.ts`、`state/itemSupport.ts`、`state/turnItemPresentation.ts` | `tool_presentation`（4 件 + 2）、`user_input`（3 件 + 1）、`scroll_anchor`（5 件）、`item_detail`（`item_detail_tests.rs` 10 件）、`item_support`（4 件 + 4）、`turn_item`（3 件） |
| shared `toolActivity.ts`、`t3McpToolPresentation.ts`、`toolOutput.ts`、client-runtime `t3ToolSummary.ts` | `tool_activity`（5 件）、`tool_catalog`（7 件。この Host が提供する tool だけ、server 名は `orchestration`）、`tool_output`（10 件）、`tool_summary`（11 件） |
| shared `orchestrationTiming.ts` の `formatDuration` / `deriveSubagentElapsedMs`、web `session-logic.ts` の `deriveActiveWorkStartedAt` と `state/models.ts` の `resolveThreadWorkingStartedAt` | `view::time`（`formats_durations`）、`view::agents`（subagent の 2 件）、`view::timeline::timing`（`shares_the_detail_timer_when_a_newer_run_is_queued_or_cancelled`、`a_local_send_counts_until_the_host_names_a_run`） |
| client-runtime `markdownLinks.ts`、mobile `markdownLinks.ts`、shared `path.ts`、`codexMarkdownDirectives.ts`、`codexArtifactTemplates.ts`、`codexFileCitations.ts`、shared `assistantCitations.ts` | `presentation::markdown::{links, directives, artifact_templates, citations, assistant_citations}`。各 `*_tests.rs` に全件（下の対象外を除く）。directive は micromark の文法を手で書き、remark の tree の assertion は match の内容で確かめる。`thread_title_seed` は citation を平文化する（`title_seeds_read_assistant_quotes_as_their_text_and_comment`） |
| desktop `app.rs` の `timeline_splice` | `view::timeline::splice`（`prepending_history_inserts_before_the_kept_rows` ほか、proptest `applying_the_splice_yields_the_new_ids`） |
| 両 layout の row と cache | `view::timeline::rows`（`TimelineRow`、`TimelineLayout`、`TimelineOptions`、`TimelineCache`、`timeline_update`）。`rows_tests.rs` 8 件。`sync::thread` の `the_row_key_changes_with_facts_history_pages_and_details_only`。outbox の `a_queued_send_is_marked_for_the_queue_instead_of_the_timeline` |

対象外にしたもの（理由）:

- 製品にない機能・形式: `userMessage.test.ts` 全 5 件（予定実行の旧 prefix）、automation の表示（予定実行は段階 6）、preview / device / browser の MCP tool と pull request の件（提供しない tool。共通の挙動は提供中の tool で確かめた）、`file_search` と `checkpoint` の item を前提にする件（domain に item がない）、idle の状態の件（domain にない）、sender thread の件（domain が記録しない）、anchored の local feedback 発言の件（この製品にない）、mobile の provider question values 2 件（option に raw value がなく、どの質問も自由回答を受ける）。
- JavaScript の object identity と memo: `computeStableMessagesTimelineRows` の 4 件、`...WithState` の再利用の件（session-logic 4 件、MessagesTimeline 3 件）。row は毎回作り直し、thread ごとの cache が同じ入力で再利用する。
- web の DOM だけの処理: CSS 文字列を返す minimap の 2 関数、asset URL の署名と preview URL（session-logic の image asset 4 件）。
- 旧い server・旧データの経路: `handoff.test.ts` の 2 件、`turnItemPresentation` の older server の assertion、`itemSupport` の node / provider session / thread / turn、`markdownLinks.test.ts` の `repairMarkdownFileLinks`（別 module）、`toolActivity.test` の ACP 用 4 件、`presentation.test` の旧 activity payload（`extractCommandOutputText`）。
- 他の実装者の範囲: plan（`findLatestProposedPlan`、`deriveActivePlanState`）、composer の停止（`deriveCanInterruptRunningThread`、`isLatestRunSettled`）、sidebar の timer の他の件。

T3 に合わせた判断と差:

- error item に title がないので、T3 の title 由来のラベル（"Runtime error" など）は Host と同じ "Provider error" / "Usage limit reached" になる。
- `parentItemId` と provider turn がないので、error は常に最上位として扱い、隣り合う subagent は attempt で束ねる。
- 「Worked」「You stopped this response」は経過時間が常にあるので出ない。
- desktop の展開時の inspector（`V2ItemInspector`）は work row の detail（call、取得した出力、項目の本文）で表す。出力は取得した detail からだけ出し、cache 済みの出力は出さない。
- mobile は T3 と同じく outbox の全発言を feed に足す。desktop は queue に入る送信を timeline に出さない（T3 web の optimistic と同じ）。
- handoff divider は provider の instance が変わったものだけ。T3 が出す fork / merge back の portable handoff の divider はない（domain の transfer の種類が違う）。
- 同じ判定の重複を一つにした: JavaScript の文字列の規則は `js_text`、media source は markdown link の関数を使う。`tool_output` は Host（`agent-runtime/src/sync/wire.rs`）と、citation の parser は `agent-runtime/src/title/citations.rs` と重複している（agent-core は agent-runtime に依存しない）。

### A5: 端末の状態・intent・UniFFI の公開（2026-10-07）

A4 の view が明示的な入力として受けていた端末側の状態を `Snapshot` に置き（`state/device.rs`）、intent で変える。ネイティブの3クライアントはこの節の getter と `Intent` だけを使い、表示用の規則を持たない。期待値は T3 のまま。

| T3 の原本 | agent-core の実装と検証 |
| --- | --- |
| client-runtime `state/threadInbox.ts`（戻った時刻の記録） | `Snapshot.inbox_returns`。shell か outbox が変わった publish のたびに `Owner::observe_list` が `InboxReturns::observe` を呼ぶ（Working の beta が off なら reset）。 |
| mobile `thread-order.ts`（保留中の並び） | `Intent::MoveThread` が `ThreadMovePlanner` で書き込みを作り、`Snapshot.thread_order`（`ThreadOrderHold`）に `PendingThreadOrder` と command id を持つ。publish のたびに `refresh`、書き込みが outbox から消えたら `complete`、どれかが拒否されたら解く。 |
| web `Sidebar.logic.ts` の drop | `Snapshot::sidebar_drop` が `plan_sidebar_drop` を返し、`Intent::DropThread` が pin・unpin・unsettle・unsnooze・settle と key の書き込みに変える。 |
| web `modelOrdering.ts`、settings の favorites | `Preferences.favorite_models`・`model_order`、`Intent::ToggleFavoriteModel`・`SetModelOrder`。instance の rail は `sort_models_for_provider_instance`（favorites、利用者の順、catalog の順）。`an_instance_lists_favorites_then_the_users_order_then_the_catalogue`。 |
| `ConversationSettings` の読み書き、web settings | `Intent::LoadConversationSettings`・`UpdateConversationSettings{scope, change}`（`plan_conversation_settings_update`）・`ResetProjectSettings`（`clear_project_overrides`）。`Snapshot::settings(scope)`。 |
| 時刻の表記・Working の beta・follow-up の設定 | `Preferences.timestamp_format`・`working_section`、`Snapshot.follow_up`。getter は一覧・menu・設定にこの値を渡す。`preferences_change_on_the_device_and_survive_a_restart`。 |
| `projectScripts.ts` の最後に実行した script、`runProjectScript` | `Preferences.last_run_scripts`、`Intent::RunProjectScript` は新しい terminal を開き、Host が開始を返してから command を書き込む。`Intent::UpdateProjectScripts` は `host/project/update`。 |
| 既存 session の取り込み（scan・選択・import・landing・toast） | `Snapshot.session_import`（`SessionImport`、`SessionImportProgress`）、`Intent::ScanSessions`・`SelectImportSessions`・`ImportSessions`（未登録の folder は `AddProject` してから `conversation/agentSessions/import`）・`CloseImport`。`Snapshot::session_import`・`import_toast`。 |
| web `pendingUserInput.ts`、mobile の回答の下書き | `Snapshot.question_drafts`、`Intent::EditAnswer`（選択肢で置き換えた入力は thread の下書きへ移す）・`ShowQuestion`・`SubmitAnswers`。送る値は `view::requests::question_answers`（入力と添付だけの回答は text、複数選択は choices）。回答の添付は `answer_draft_key` の下書き。`a_reply_sends_attachment_only_answers_as_text_and_chosen_options_as_choices`、`an_attachment_only_answer_is_empty_text_and_chosen_options_are_choices`、`a_typed_answer_moves_to_the_thread_draft_when_an_option_replaces_it`。 |
| web `diffPanelStore.ts` | `Snapshot.diff_panels`、`Preferences.diff_ignore_whitespace`、`Intent::SelectDiffScope`・`SelectDiffTurn`・`SelectDiffBaseRef`・`SetDiffIgnoreWhitespace`・`LoadDiff`（`DiffRequest::intent` で読み込む）。 |
| web `promptStashStore.ts` | `Snapshot.stash`（端末に保存）、`Intent::StashDraft`（`Outcome::Stashed`）・`FinalizeStashImages`・`RestoreStash`（`Outcome::StashRestored`）・`DeleteStash`。`a_stashed_draft_restores_its_text_and_uploaded_files`。 |
| composer の context record、`use-composer-command-menu.ts` | `Draft.context`、`Snapshot::composer_menu(text, cursor)`、`Intent::SelectComposerItem`（`Outcome::ComposerEdited{cursor}`）・`RemoveDraftContext`。送信・キューの編集・stash は context を運ぶ。 |
| `TraitsPicker` の選択 | `Intent::SelectTrait`・`ToggleTrait`（`select_trait`・`toggle_trait` の変更を下書きと thread の選択に適用）。 |
| client-runtime `worktreeSetup.ts` の保持 | stream の `None` で最後の snapshot を `Snapshot.held_setups` に残す。`a_closed_setup_stream_keeps_its_last_snapshot_for_the_card`。 |
| `ThreadErrorBanner` の非表示 | `Snapshot.error_dismissals`、`Intent::DismissThreadError{dismiss_key}`。 |
| `UsageLimitRecoveryBanner` | `Intent::LimitRecovery{thread_id, action}`（`toggle_limit_recovery` の `LimitRecoveryUpdate` を `UpdateMetadata` で送る）。 |
| server `UserFacingErrors.ts` の適用 | `Resolution::Failed{rejected}` の Committed の拒否だけを `provider_rejection_message` で文にする。`a_committed_refusal_reads_as_a_sentence_and_a_transport_failure_keeps_its_text`。 |
| web `QueuedRunsControl` の移動 | `QueueAction::Move{run_id, before_run_id}` が 1 回の `ReorderQueued`。`moving_a_queued_message_sends_one_reorder`。 |
| `state/attachments.ts` の受け入れ | `Intent::AttachFiles{draft_key, files}` が `admit_attachments` で選ぶ。縮小が必要な画像は拒否（縮小はネイティブ側）。 |
| `ThreadTerminalDrawer`、contracts `terminal.ts` の id | `view::terminals`（`terminal_tabs`、`next_terminal_id`、`terminal_view`）。handle は `thread_terminal_handle_for`（`terminal:{thread}:{id}`）。`Intent::OpenTerminal`・`NewTerminal`・`SplitTerminal`・`WriteTerminal`・`ResizeTerminal`・`DetachTerminal`・`CloseTerminal`。`lists_setup_terminals_first_and_numbers_the_next_terminal`、`a_thread_terminal_handle_extends_the_thread_prefix_with_its_id`。 |

UniFFI の公開（`bindings/views.rs`）:

- `Snapshot` の getter: `sidebar`、`sidebar_drop`、`thread_list`、`archived`、`thread_menu`、`thread`（`ThreadView`: header、rows と `rows_revision`、history、composer、queue、requests、plan、agents、lineage、setup、working、error banner、limit recovery、diff、terminals、scripts）、`selected_thread`、`new_thread`、`composer_menu`、`search`、`settings`、`model_picker`、`traits`、`terminals`、`terminal`、`diff`、`project_scripts`、`session_import`、`import_toast`、`stash`、`preferences`、`conversation_settings_loaded`。行は snapshot が共有する `TimelineCache` から作り、入力が変わらなければ作り直さない。
- 状態を持たない関数: `timeline_update`、`answer_draft_key`、`admit_attachments`、`snooze_presets`。
- 生成は `scripts/build-agent-bindings.sh`（`AgentCore.swift`、`AgentCoreFFI.h`、`AgentCoreFFI.modulemap`、`dev/remoteagent/core/agent_core.kt`）。

未接続:

- provider ごとの runtime mode は Host から届かないので、すべての mode を出す（plan toggle と model option は 2026-10-08 に接続した）。
- 接続状態は接続しているかどうかだけで、再接続中の環境名や理由は出ない。
- 新しい task の下書きの project 選択時刻。

### Android クライアント（段階 4、2026-10-07）

| T3 mobile | Android（`apps/mobile/src/main/kotlin/dev/remoteagent/mobile`） |
|---|---|
| `HomeScreen`、`HomeHeader.android`、`MaterialThreadListToolbar`、`AndroidHomeFab.android` | `HomeScreen.kt`（`Snapshot::thread_list`、空の状態は `ThreadListView.empty`） |
| `thread-list-v2-items`（card・slim・unsent 行、shelf、Show more）、`thread-list-v2-row-appearance.android`、`thread-swipe-actions` | `ThreadRows.kt` |
| long-press menu、`ThreadArrangementSheet` | `ThreadActions.kt`（`ThreadMenuItem`・`ThreadMenuAction`）、`HomeScreen.kt` の Arrange sheet（並べ替えは上下の移動） |
| `CustomSnoozeSheet.android` | `CustomSnoozeDialog.kt`（`custom_snooze_until`） |
| `ThreadRouteScreen`（Android header）、`ThreadDetailScreen`、`ThreadFeed`、`thread-work-log`、`thread-subagent-group`、`worktree-setup-card`、`floating-working-control` | `ThreadScreen.kt`、`FeedRows.kt`（`TimelineLayout::Mobile` の行） |
| `PendingApprovalCard`、`PendingUserInputCard`、`RequestActionButton` | `RequestCards.kt` |
| `ThreadComposer`、`ComposerToolbar`、`composerSendPresentation`、`ComposerCommandPopover` | `Composer.kt`（`ComposerView`、`Snapshot::composer_menu`） |
| `ThreadQueueControl`、`ThreadAgentsSheet`、`ThreadSettingsSheet`・`ThreadSettingsRows.android`、`worktree-setup-sheet.android` | `ThreadSheets.kt` |
| `NewTaskRouteScreen`・`NewTaskDraftScreen` | `NewTaskScreen.kt`（`Snapshot::new_thread`） |
| `ThreadTerminalRouteScreen` | `TerminalScreen.kt`（`(thread, terminal_id)`、setup card の「Open terminal」は `setup-<script id>`） |
| `SettingsRouteScreen`、`ArchivedThreadsScreen` | `SettingsScreen.kt`（`Snapshot::settings`・`setting_edit`、`Snapshot::archived`） |
| `composerImages.ts` の写真の再符号化、`ComposerAttachmentStrip` | `AttachmentFiles.kt`・`Attachments.kt`（`admit_attachments`） |
| `lib/mobileTheme`・既定の theme 変数 | `AppTheme.kt` |

未対応（2026-10-08 の統合の後、Android でまだ作っていないもの）:

- 音声入力（mic）、git 操作（段階 6）、端末 preview、Material You の配色、project の folder の閲覧（Add project は絶対 path の入力だけ）と「Choose project」の全画面（dropdown のまま）、既存 session の取り込み画面。
- terminal の「Text size」submenu と keyboard を閉じたときの bar（Attach output・Show keyboard）。
- SVG の project icon は Android で描けないので folder の glyph にする。
- model を変えたときの option は引き継がない。一覧の並べ替え（Arrange）は drag ではなく core の Move up / Move down を並べる。

### 段階 4 の統合: Host の data の接続（2026-10-08）

`stage4-int` で Host・core・iOS・Android・desktop の枝を統合し、core を Host の新しい protocol に載せた。期待値は T3 のまま。

| T3 の原本 | agent-core の実装と検証 |
| --- | --- |
| web `providerInstances.ts`（deriveProviderInstanceEntries、isProviderInstancePickerReady）、`ModelPickerSidebar.tsx` describeUnavailableInstance | `view::models::catalog`（`host/provider/list` の instance と model、Host の descriptor）。`the_host_instances_carry_their_display_metadata_and_descriptors`、`unavailable_instances_list_no_models_and_explain_themselves`。 |
| web `modelSelection.ts` の既定の instance と model | `view::models::default_model`。`a_new_draft_starts_on_the_first_ready_instances_default_model`。 |
| `shared/model.ts` resolveSelectableModel の alias | `view::models::options::resolve_selectable_model`。`prefers_an_exact_slug_and_returns_no_capabilities_for_an_unknown_one`、`names_a_catalog_model_by_its_slug_name_or_alias`。 |
| web `ModelPickerContent.tsx` legacySection | `ModelPickerView.legacy`（`ModelPickerOptions.toggled_legacy`）。`legacy_models_fold_under_a_row_that_opens_for_a_legacy_selection`。 |
| mobile `use-composer-command-menu.ts`（workspace snapshot の取得と 10 秒の再試行）、`queries.ts` useComposerPathSearch、web `queries.ts`、`ComposerCommandMenu.tsx` と mobile `ComposerCommandPopover.tsx` の文言、client-runtime `providerSkills.ts` | `Intent::UpdateComposerMenu`、`connection::workspace`（`ensure_provider_commands`、debounce した `host/workspace/searchEntries`）、`view::composer::menu`。`the_menu_lists_the_providers_skills_commands_and_found_paths`、`an_empty_menu_names_what_its_trigger_searched`。 |
| web `ChatView.tsx` の Compact context と `ContextWindowMeter.logic.ts` providerSupportsManualCompaction | `view::composer::view::compact_control`、`Intent::CompactContext`（`/compact` を送る）。 |
| web `DiffPanel.tsx`（diffPreview・vcs status・listRefs）、`lib/baseRefChoices.ts` | `Intent::LoadDiff` が `host/review/diffPreview` と `host/vcs/status` を読む。`view::checkpoints::{git_diff_view, build_base_ref_choices}`、`Intent::SearchDiffBaseRefs`。`base_choices_pair_local_branches_with_their_origin_twin`、`the_git_view_reads_the_preview_source_its_scope_picks`、`uncommitted_reads_the_working_tree_preview`。 |
| mobile `new-task-flow-provider.tsx`、`new-task-context-presentation.ts`、`projectThreadCreationValidation.ts` | `Draft.workspace`（`DraftWorkspace`）、`NewThreadView.workspace`、`view::new_thread::{new_thread_launch_workspace, branch_worktree_path, new_task_branch_label, new_task_workspace_label}`、`Intent::{SetNewThreadWorkspace, SelectNewThreadBranch, SetNewThreadStartFromOrigin, SearchNewThreadBranches, NewThreadOnBranch}`。`a_local_draft_works_on_the_checked_out_branch_and_a_worktree_starts_from_the_default`、`a_branch_checked_out_in_another_worktree_runs_there_locally`。 |
| mobile `checkout-new-task-branch.ts`、`queries.ts` usePaginatedBranches、server `GitVcsDriverCore.ts` switchRef | `host/vcs/switchRef`（`vcs::switch_ref`）、`Intent::LoadMoreNewThreadBranches`。`switching_refs_checks_out_local_and_remote_branches`、`picking_another_local_branch_switches_the_checkout_first`、`a_later_branch_page_joins_the_first`。 |
| mobile `ThreadRouteScreen.tsx` handleWorkLocally、web の setup card の Retry | `Intent::WorkLocally`（`CancelSetup` の結果で launch）、`Intent::RetryPreparation`（`Command::RetryPrepared`）。 |
| client-runtime terminal metadata（subscribeTerminalMetadata）、`shared/terminalLabels.ts`、mobile `terminalMenu.ts`、web `Sidebar.tsx` terminalProcessLabel・`ThreadStatusIndicators.tsx` | `StreamKey::TerminalMetadata`、`Snapshot.terminal_metadata`、`view::terminals::{terminal_tabs, next_terminal_id, running_terminal_ids, terminal_process_label, terminal_menu_status}`、`SidebarThreadRow.terminal_processes`、`Intent::{ClearTerminal, RestartTerminal}`、`host/terminal/closed`。`host_terminals_name_their_command_and_new_ids_fill_the_lowest_gap`、`lists_setup_terminals_first_and_takes_the_lowest_free_id`。 |
| `shared/projectScripts.ts` projectScriptRuntimeEnv、mobile `ThreadTerminalRouteScreen.tsx` の restartIfNotRunning | `Snapshot::terminal_location`、`StartTerminal { thread, terminal_id, cwd, worktree_path, size, env, restart_if_not_running }`。 |
| mobile `components/ProjectFavicon.tsx`、`state/assets` | `Owner::refresh_project_icons`（`host/project/favicon` と `known_hash`）、`Snapshot::project_icon`、`Intent::SetProjectIcon`。 |
| mobile `lib/mobileTheme`、既定の palette と状態色 | `presentation::theme` の `mobile*`・`status*`。`both_appearances_name_the_same_tokens_in_hex`。 |
| web `MessagesTimeline.tsx` formatWorkingTimer、mobile `floating-working-control.tsx` formatWorkingDuration | `view::time::format_working_timer`（`working_timer_label`）、`view::working_status::format_working_duration`（`working_duration_label`）。`working_timer_floors_to_whole_seconds`。 |

未接続（T3 にあって、まだ持たないもの）:

- diff panel の truncated な source の file ごとの遅延読み込み（diffFileContents）、window focus での再読み込み、環境 cwd での再試行。
- T3 の `newWorktreesStartFromOrigin` 設定。
- context meter の resume compaction の帯（`should_offer_resume_compaction` は core にあるが画面に出していない）。
- provider ごとの runtime mode。

### 段階 4 の統合: 3 クライアント（2026-10-08）

- iOS・Android・desktop は Host の data を core の view と intent だけで読む。配色は `theme()`、一覧の空の状態は `ThreadListView.empty`、設定の行は `setting_intent`・`setting_reset`、経過時間は `working_timer_label`・`working_duration_label`。
- mobile の thread 設定の model の一覧は T3 mobile ThreadSettingsSheet と同じ catalogue（`view::models::catalog_sheet`: provider ごとの開閉、Show legacy models、All providers / Favorites / provider の filter、favorites を先頭、staged と applied、空の文言、Default）。desktop は T3 web の picker（`ModelPickerView.legacy` の「Legacy models」の行、New badge）。`legacy_models_hide_behind_the_switch_and_primary_sections_start_open`、`a_selected_legacy_model_stays_listed`。
- composer の skill は T3 resolveProviderSkillSourceKind の出所（`ComposerCommandItem.skill_source`）で icon と badge を出す。`skill_sources_come_from_plugin_paths_then_the_scope`。
- mobile の review の見出しは T3 reviewModel gitSubtitle（`GitDiffView.subtitle`）。mobile は T3 mobile と同じく base の picker を持たない。
- mobile の setup card には T3 mobile と同じく「Open terminal」を出さない（desktop は T3 web と同じく出す）。
- desktop は T3 web と同じく右の panel に browser と terminal の tab をいくつでも置き、drawer と panel の terminal は別々に持ち、終わった terminal は閉じ、thread details は浮いた card（狭いときは popover）にし、既存 session の取り込みは初回の onboarding だけで出し、設定に Appearance と Keybindings を足した（2026-10-08 に desktop の未承認の差を T3 に戻した）。

未接続（2026-10-08 の時点）:

- desktop: Appearance と Keybindings の値の保存（いまは起動中だけ）、Themes の grid と組み込みの palette、Contrast・Composer context・Motion・Advanced typography、keybindings.json、terminal の「Add to chat」、thread details の Workspace の節、branch picker の「Create new ref」。
- iOS: composer の path の行の file 種別ごとの icon、icon の無い project の folder の symbol（いまは頭文字）。

## 段階 5: 旧ランタイムの削除（2026-10-08）

- `crates/orchestration` と `crates/provider-adapters` を workspace から削除した。上の生成表の翻訳先の列（すべて旧 crate への予定パスで、翻訳済みの行はなかった）と `PORT_MAP.json` の `rust` の欄、`inventory.py` の翻訳先の生成も消した。
- `agent-protocol` の `orchestration` module、旧 `orchestration/*` の Call と Body、cwd から作る `terminal_handle` を削除した。会話の Call は `SubscribeThread`・`SubscribeShell`・`GetTurnItem`・`GetTurnDiff` と呼ぶ。ALPN は `remote-agent/streams/12`。
- `crate_boundaries` は `agent-domain` の依存が純粋な crate だけであること、`agent-core`（bindings の有無とも）と `agent-ffi` が `agent-runtime`・`agent-providers`・`rusqlite` を含まないことを確かめる。
- 旧ランタイムを記述した文書（`SESSION_RUNTIME.md`、`BEX_PROTOCOL_DESIGN.md`、`BEX_PROTOCOL_NATIVE_CONTRACTS.md`、`CRATE_BOUNDARIES.md`、`IMPLEMENTATION.md`、`PLAN.md` の設計の節）を現在の設計に書き直し、旧コードの地図 `BEX_ARCHITECTURE_MAP.md` と、削除したテストを根拠にした `PR55_REVIEW.md`、この文書の「中断時の12ファイルの採否」を削除した。
- どこからも参照されない core と runtime の定数・関数を削除した。T3 から移植してテストだけが使う関数（minimap、drag、citation など）は、未接続の T3 の挙動として残す。


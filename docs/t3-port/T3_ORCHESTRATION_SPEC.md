I've written the spec below. Every contract type is listed with its fields; the server, adapter and sync sections are kept short and cite file:line. Paths are relative to `/tmp/t3code-ref` at commit 4ee6bfd.

**Before you start, four facts change the port plan:**
1. **One event log.** V2 events go into the shared `orchestration_events` table, marked by `application_event_version=2`. The `orchestration_v2_events` and `orchestration_v2_command_receipts` tables appear only in migrations. Nothing outside the migrations reads or writes them.
2. **Events carry whole records.** Nearly every event payload is the full entity, and applying it is an upsert by id. The client reducer `packages/client-runtime/src/state/orchestrationV2Projection.ts:153-286` is the shortest correct projection spec.
3. **Claude does not use a wire protocol.** The Claude adapter calls the TypeScript `@anthropic-ai/claude-agent-sdk` in-process. Rust has no equivalent SDK, so you will need to drive the `claude` CLI's stream-json mode yourself. Plan for this as separate work.
4. **The size is concentrated.** `Orchestrator.ts` is 10,272 lines and `ProjectionStore.ts` is 6,176.

---

## 0. Line counts

| File | Lines |
|---|---|
| `packages/contracts/src/orchestrationV2.ts` | 3405 |
| `packages/contracts/src/rpc.ts` | 1877 |
| `apps/server/src/orchestration-v2/Orchestrator.ts` (command handling) | 10272 |
| `.../ProjectionStore.ts` (SQLite projections, shell derivation) | 6176 |
| `.../Adapters/CodexAdapterV2.ts` | 6317 |
| `.../Adapters/ClaudeAdapterV2.ts` | 7859 |
| `.../ProviderSessionManager.ts` | 2078 |
| `.../RunExecutionService.ts` (consumes adapter events, ends runs) | 1467 |
| `.../ProviderTurnStartService.ts` | 1266 |
| `.../ThreadLaunchService.ts` | 966 |
| `.../EventSink.ts` (transaction + publish) | 891 |
| `.../EffectWorker.ts` | 834 |
| `.../ProviderRuntimeRecoveryService.ts` | 815 |
| `.../ThreadManagementService.ts` | 782 |
| `.../ProviderEventIngestor.ts` | 641 |
| `.../EffectOutbox.ts` | 636 |
| `.../ProviderAdapter.ts` (adapter interface) | 598 |
| `.../CheckpointService.ts` / `CheckpointRollbackService.ts` / `CheckpointCaptureService.ts` | 585 / 362 / 285 |
| `.../threadHistoryPaging.ts` | 532 |
| `.../CommandPolicy.ts` | 491 |
| `.../IdAllocator.ts` | 435 |
| `.../legacy/LegacyV1ThreadImporter.ts` | 833 |
| `.../EventStore.ts` / `CommandReceiptStore.ts` / `TurnItemPositionStore.ts` | 156 / 214 / 107 |
| `apps/server/src/ws.ts` (RPC handlers, subscribe logic) | 3883 |
| `apps/server/src/persistence/Layers/OrchestrationEventStore.ts` | 612 |
| `apps/server/src/persistence/Migrations/055_OrchestrationV2.ts` + `OrchestrationV2/*` | 332 + ~770 |
| `apps/server/src/project/AgentSessionScanner.ts` / `AgentSessionImporter.ts` | 1495 / 409 |
| `packages/effect-codex-app-server/src/client.ts` / `protocol.ts` | 275 / 485 |
| `packages/effect-codex-app-server/src/_generated/schema.gen.ts` (generated Codex protocol types) | 57042 |
| `packages/client-runtime/src/state/threads.ts` / `orchestrationV2Projection.ts` | 1009 / 286 |

All non-test files under `orchestration-v2/` total about 106k lines, including roughly 5k lines of test fixtures.

---

## 1. Contracts

### 1.0 Shared primitives

- **Ids.** All ids are branded non-empty trimmed strings (`baseSchemas.ts:129-222`): ThreadId, ProjectId, CommandId, EventId, MessageId, RunId, RunAttemptId, NodeId, ProviderSessionId, ProviderThreadId, ProviderTurnId, TurnItemId, RuntimeRequestId, ScheduledTaskId, CheckpointRef, CheckpointId, CheckpointScopeId, ContextHandoffId, ContextTransferId, RawEventId, PlanId.
- **Provider keys.** `ProviderDriverKind` and `ProviderInstanceId` are branded slugs (`providerInstance.ts:70,82`).
- **Scalars.** `IsoDateTime` is a plain string. `NonNegativeInt` is ≥0 and `PositiveInt` is ≥1. `DateTimeUtc` is serialized as an ISO string in the `*Json` variants.
- **Policy enums** (`providerPolicy.ts`):
  - `RuntimeMode` = approval-required | auto-accept-edits | auto | full-access (default full-access)
  - `ProviderInteractionMode` = default | plan
  - `ProviderRequestKind` = command | file-read | file-change | mcp-elicitation | permission
  - `ProviderApprovalDecision` = accept | acceptForSession | acceptAlways | decline | cancel
  - `ProviderApprovalOption` = {decision, label, warning?}
  - `ProviderUserInputAnswers` = Record<string, unknown>
  - `UserInputAttachments` = Record<string, (Image|File)[]≤100>
  - `UserInputAttachmentAnswerPayload` = {requestId, questionTextById?, answers, attachmentsByQuestionId}
- **`ModelSelection`** (`modelSelection.ts:16`) = {instanceId: ProviderInstanceId, model: string, options?}. The decoder also accepts the legacy `{provider, model}` shape.
- **`ChatAttachment`** (`chatAttachment.ts:149-217`) is a union of:
  - `{type:"image", id, name, mimeType, sizeBytes≤10MiB, source?: SnapShotSource}`
  - `{type:"file", id, name, mimeType, sizeBytes 1..50MiB, source?: {_tag:"pasted-text"}}`
  - `{type:<other string>, id, name, mimeType, sizeBytes}` (forward-compatible catch-all)
- **`OrchestrationMessageContext`** (`composerContext.ts:278`) = {version:1, records: ComposerContextRecord[] ≤200}. The record kinds are image, file, terminal, element, preview-annotation, review-comment, mention, skill, thread, and unknown.

### 1.1 Threads (read model)

**`OrchestrationV2AppThread`** (orchestrationV2.ts:357-432):
- Creation and identity: createdBy (user|agent|system), creationSource (web|mobile|mcp|provider|server), id, projectId, title.
- Provider and modes: providerInstanceId, modelSelection, runtimeMode, interactionMode.
- Workspace: branch|null, worktreePath|null.
- Pull requests: linkedPullRequest?|null, pullRequests?: ThreadPullRequestLink[], branchPullRequest?|null.
- Lineage: activeProviderThreadId|null, historyOrigin? (native|v1_import), lineage {parentThreadId|null, relationshipToParent: fork|subagent|null, rootThreadId}.
- forkedFrom|null, one of: {type:"run", threadId, runId} | {type:"node", nodeId} | {type:"provider_thread", providerThreadId, providerTurnId?}.
- Timestamps and status: createdAt, updatedAt, archivedAt|null, settledOverride (settled|active|null, defaults to null), settledAt|null, unsettledAt?, snoozedUntil?, snoozedAt?, limitRecovery?|null, pinnedAt?, autoSettleDisabledAt?, pinOrderKey?, activeOrderKey?, lastVisitedAt|null.
- Pending work markers: titleRegeneration?: {requestId, startedAt}|null, rollbackRequestId?: CommandId, rollbackFailure?: {requestId, message}|null, deletedAt|null.
- Related types:
  - `OrchestrationV2LimitRecovery` (L332) = {requestId?, runId, resetAt, autoResume, snooze?}
  - `LimitRecoveryUpdate` (L342) = {runId, resetAt, autoResume?, snooze?}, and at least one of the optional fields must be present.
  - `ThreadLinkedPullRequest` = {projectId, repository, number, url}
  - `ThreadPullRequestKey` = {host, repository, number}
  - `ThreadPullRequestLink` = Key + {url, source: manual|created|agent|stack|stack-dismissed, linkedAt, snapshot|null, stack|null, watch?} (`threadPullRequest.ts:23-128`)

**`OrchestrationV2ThreadShell`** (L1714-1804) is the sidebar row and is computed on read (`ProjectionStore.ts:1305 threadShellFromProjection`). It has the AppThread's display fields, plus:
- Latest run: latestRunId|null, latestRunRequestedAt?, latestRunStartedAt?, latestRunCompletedAt?.
- Activity: activeRunId|null, activityRunStartedAt?, activityRunStatus? (preparing|starting|running|waiting).
- status: idle | any RunStatus.
- Errors and limits: lastError?, lastErrorClass?, usageLimitResetAt?.
- pendingRuntimeRequest|null = {id, kind, createdAt}.
- latestVisibleMessage|null = {id, role, text, updatedAt}.
- latestUserMessageAt|null, latestUserAuthoredMessageAt?.
- hasActionableProposedPlan: bool.
- pendingBackgroundTasks: PendingBackgroundTask[] (default []), providerInstanceHistory: ProviderInstanceId[] (default []).
- itemCount, visibleItemCount.
- The remaining timestamp, settle, snooze, pin and order fields mirror AppThread.

Shell snapshots and streams:
- **`ThreadShellSnapshot`** (L1807) = {schemaVersion, snapshotSequence, threads[], archivedThreads[]}.
- **`ShellSnapshot`** (L1815) = ThreadShellSnapshot + projects: OrchestrationProjectShell[].
- **`ShellStreamItem`** (L1821) is one of:
  - {kind:"synchronized"}
  - {kind:"snapshot", snapshot, resolvedRepositoryIdentityRoots?}
  - {kind:"project.updated", sequence, project}
  - {kind:"project.removed", sequence, projectId}
  - {kind:"thread.updated", sequence, location: active|archive, thread}
  - {kind:"thread.removed", sequence, location, threadId}
- **`ArchivedShellSnapshot`** (L2987) = {schemaVersion, snapshotSequence, projects, threads}.
- **`ArchivedShellStreamItem`** (L2995) is snapshot | thread.updated{sequence, thread} | thread.removed{sequence, threadId}.

### 1.2 Runs, attempts, nodes, provider sessions/threads/turns

- **`RunStatus`** (L446) = preparing | queued | starting | running | waiting | completed | interrupted | failed | cancelled | rolled_back.
- **`Run`** (L534-577):
  - Identity and routing: id, threadId, ordinal: PositiveInt, providerInstanceId, modelSelection, providerThreadId|null, userMessageId, rootNodeId|null, activeAttemptId|null, status.
  - Queue: queuePosition?|null, queueHeld?: bool.
  - Timing: requestedAt, startedAt|null, completedAt|null.
  - Outputs: checkpointId|null, contextHandoffId|null.
  - Wake and restart: restartContinuationOfRunId?, workStartedAt?, restartCancelledBackgroundWork?: {kind: subagent|shell|monitor|task, label, id?}[].
  - Other: sourcePlanRef? {threadId, planId}, delegatedCompletion? (below), workspacePreparation?: ThreadLaunchWorkspaceStrategy.
- **`DelegatedCompletionCohort`** (L485) = {disposition: open|stopped|disposed, nextGeneration, delivery: {generation, messageId, taskIds: NodeId[]}|null}.
- **`ThreadLaunchWorkspaceStrategy`** (L511) is one of {type:"root", branch?} | {type:"existing_worktree", worktreePath, branch?} | {type:"worktree", baseRef, branch?, startFromOrigin?}.
- **`RunAttempt`** (L590) = {id, nativeThreadId?, runId, attemptOrdinal, rootNodeId, providerInstanceId, providerThreadId, providerTurnId|null, reason: initial|steering_restart|retry|provider_recovery, status: pending|running|completed|interrupted|failed|cancelled|superseded, startedAt|null, completedAt|null}.
- **`ExecutionNode`** (L615):
  - Fields: id, threadId, runId|null, parentNodeId|null, rootNodeId, kind, status, countsForRun, providerThreadId|null, providerTurnId|null, nativeItemRef: ProviderRef|null, runtimeRequestId|null, checkpointScopeId|null, startedAt|null, completedAt|null.
  - kind = root_turn | assistant_message | reasoning | plan | todo_list | tool_call | approval_request | user_input_request | subagent | hook | system.
  - status = idle | pending | running | waiting | completed | interrupted | failed | cancelled | rolled_back.
- **`ProviderRef`** (L94) = {driver, nativeId|null, strength: strong|weak|none, fingerprint?, ordinal?}.
- **`Subagent`** (L656):
  - Fields: id: NodeId, threadId, runId|null, parentNodeId, origin: provider_native|app_owned, createdBy, driver, providerInstanceId, providerThreadId|null, childThreadId|null, nativeTaskRef|null, prompt, title|null, model|null.
  - Completion: completionWake?: always|settled_only, completionDelivery?: {state: pending|claimed|acknowledged|delivered|disposed, observedByRunId|null}.
  - State: status (idle|pending|running|waiting|completed|failed|cancelled|interrupted), progress?, result|null, startedAt, completedAt, updatedAt.
- **`ProviderSession`** (L718) = {id, driver, providerInstanceId, status: starting|ready|running|waiting|stopped|error, cwd, model|null, capabilities, createdAt, updatedAt, lastError|null}.
- **`ProviderSessionDetached`** (L732) = {providerSessionId, detachedAt, reason?}.
- **`ProviderCapabilities`** (L188-329) has 12 sections. Each section's boolean flags are listed verbatim at the cited lines:
  - sessions, threads, turns, streaming, tools, approvals, planning, subagents, context, checkpointing.
  - identity: four strong/weak/none ref strengths.
  - runtimePolicy: {enforcement: native|client-boundary}.
- **`ProviderThread`** (L835):
  - Fields: id, driver, providerInstanceId, providerSessionId|null, appThreadId|null, ownerNodeId|null, nativeThreadRef|null, nativeConversationHeadRef|null.
  - status: not_loaded|idle|active|archived|closed|error.
  - Ordinals and history: firstRunOrdinal|null, lastRunOrdinal|null, handoffIds[], forkedFrom: {providerThreadId, providerTurnId?, checkpointId?}|null.
  - Extras: pendingBackgroundTasks (default []), contextUsage: ThreadTokenUsageSnapshot|null, nativeMetadata: {modelSelection?, title?, updatedAt?, itemIdentityVersion?:2}|null, createdAt, updatedAt.
- **`PendingBackgroundTask`** (L802) = {taskId, description?, kind: subagent(+childThreadId?)|command|monitor|background_task}. An unknown kind decodes as background_task.
- **`ProviderTurn`** (L944) = {id, providerThreadId, nodeId, runAttemptId|null, nativeTurnRef|null, ordinal, status: pending|running|completed|interrupted|failed|cancelled, startedAt, completedAt, tokenUsage?, turnTokenUsage?}.
  - `tokenUsage` = {usedTokens, maxTokens?, inputTokens?, cachedInputTokens?, outputTokens?, reasoningOutputTokens?, updatedAt: string} (L931).

### 1.3 Messages and turn items

- **`ConversationMessage`** (L1054) = {notification?, createdBy, creationSource, scheduledTaskId?, senderThreadId?, id, threadId, runId|null, nodeId|null, role: user|assistant|system, text, context?, attachments[], streaming, createdAt, updatedAt, delegatedCompletion?: {parentRunId, generation, taskIds}}.
- **`Notification`** (L1045) = {source, outcome: completed|failed|cancelled|updated|unknown, summary, detail?}.
  - `source` (L1006) is delegated_task{taskIds, childThreadId?} | subagent{childThreadId?} | command | monitor | background_task.
  - On the wire, subagent is encoded as `{kind:"background_task", work:"subagent"}` and command as `background_command`.
- **`UserMessageInputIntent`** (L1246) = turn_start | queued_turn | steer | promoted_queued_to_steer.
- **`TurnItemStatus`** (L1177) = idle | pending | running | waiting | completed | failed | cancelled | interrupted.
- **`TurnItem`** (L1290-1491) is a union on `type`. Every variant has these base fields (L1255): toolSurface?, toolIcon?, toolSource?, id, threadId, runId|null, nodeId|null, providerThreadId|null, providerTurnId|null, nativeItemRef|null, parentItemId|null, ordinal, status, title|null, startedAt|null, completedAt|null, updatedAt.

  | type | extra fields |
  |---|---|
  | notification | Notification fields |
  | user_message | createdBy, creationSource, messageId, scheduledTaskId?, senderThreadId?, inputIntent, text, context?, attachments[] |
  | assistant_message | messageId, text, attachments?, streaming |
  | reasoning | text, streaming |
  | proposed_plan | planId, markdown, streaming |
  | todo_list | planId, steps: PlanStep[], explanation? |
  | user_input_request | requestId, questions: UserInputQuestion[], questionAnswer?, responseMode?: "message" |
  | file_change | fileName, additions?, deletions?, diffStr?, oldStr?, newStr?, changes?: {operation, path, oldPath?, fileType?, mimeType?}[] |
  | command_execution | input, output?, outputOmitted?, outputIndicatesFailure?, exitCode? |
  | file_search | pattern?, results?: {fileName, line?, column?, preview?}[] |
  | web_search | patterns?, results?: {title?, url?, snippet?}[] |
  | approval_request | requestId, requestKind, prompt?, appName?, options?: ProviderApprovalOption[] |
  | checkpoint | checkpointId, scopeId, files: CheckpointFileSummary[] |
  | run_interrupt_request / run_interrupt_result / system_notice | message |
  | error | failure: ProviderFailure, retry?: {attempt, maxAttempts|null, retryDelayMs|null} |
  | compaction | driver|null, summary?, beforeTokenCount?, afterTokenCount? |
  | handoff | contextHandoffId, fromProviderThreadIds[], toProviderThreadId, fromProviderInstanceIds[], toProviderInstanceId, fromModelSelections?, toModel?, strategy, summary? |
  | fork | source (run/node/provider_thread union), targetThreadId, providerThreadId? |
  | thread_created | targetThreadId, targetRunId|null, targetProviderInstanceId, targetModel |
  | subagent | subagentId, origin, driver, providerInstanceId, childThreadId|null, prompt, progress?, result|null |
  | dynamic_tool | toolName|null, viewedImagePath?, input: unknown, output?: unknown, outputOmitted? |

- Supporting types:
  - `PlanStep` (L1081) = {id, text, status: pending|running|completed, durationAnchorAt?, durationMs?}.
  - `UserInputQuestion` (L1092) = {id, header, question, options: {label, description, value?}[], multiSelect?, allowCustomAnswer?, required?}.
  - `ProviderFailure` (L1220) = {class: usage_limit|provider_error|transport_error|permission_error|validation_error|unknown, message≤4096, code≤128|null, retryable|null, resetAt?}.
  - `ProjectedTurnItem` (L1494) = {position, visibility: local|inherited|synthetic, sourceThreadId, sourceItemId, item}.
  - `PlanArtifact` (L1118) shares {id: PlanId, threadId, runId|null, nodeId, status: draft|active|completed|superseded, detailInTurnItem?} and is either {kind:"proposed_plan", markdown} or {kind:"todo_list", steps, explanation?}.

### 1.4 Approvals and questions

- **`RuntimeRequest`** (L966) = {id, nodeId, providerTurnId|null, nativeRequestRef|null, kind, status, responseCapability, createdAt, resolvedAt|null, decision?, answers?}.
  - kind = ProviderRequestKind | dynamic_tool_call | user_input | auth_refresh.
  - status = pending | resolved | expired | cancelled.
  - responseCapability = {type:"live", providerSessionId} | {type:"message"} | {type:"not_resumable", reason}.

### 1.5 Checkpoints and rollback

- **`CheckpointScope`** (L703) = {id, threadId, runId|null, nodeId, parentScopeId|null, providerThreadId|null, kind: root_run|subagent|tool|provider_thread|manual, ordinalWithinParent, advancesAppRunCount, cwd, createdAt}.
- **`Checkpoint`** (L1141) = {id, threadId, scopeId, runId|null, nodeId, parentCheckpointId|null, ordinalWithinScope, appRunOrdinal|null, ref: CheckpointRef, status: ready|missing|error|stale, files: {path, kind, additions, deletions}[], capturedAt}.
- **`CheckpointRollbackRequest`** (L1157) = {scopeId, checkpointId, requestedAt}.
- **Error:** `CheckpointUnavailableError` {threadId, target} (L1165).

### 1.6 Context transfer, handoff and fork

- **`ContextTransfer`** (L161) = {id, type, sourceThreadId, targetThreadId, sourcePoint, basePoint|null, sourceProviderInstanceId|null, targetProviderInstanceId|null, targetRunId|null, status, resolution|null, createdBy, error|null, createdAt, updatedAt, consumedAt|null}.
  - type = fork | provider_handoff | merge_back | subagent_spawn | subagent_result.
  - status = pending | resolved_native | resolved_portable | failed | consumed | superseded.
  - resolution = native_fork{providerThreadRef} | portable_context | delta_context | fork_delta_context | checkpoint_context, the last four each carrying {contextHandoffId}.
- **`ContextSourcePoint`** (L119) = {threadId, runId?, checkpointId?, turnItemId?, providerThreadRef?, providerTurnRef?}.
- **`ThreadForkSourcePoint`** (L129) = latest_stable | run{runId} | checkpoint{checkpointId}.
- **`ContextHandoff`** (L883) = {id, transferId?, threadId, targetRunId, fromProviderThreadIds[], toProviderThreadId, coveredRunOrdinals {from, to}, strategy, status, summaryMessageId|null, summaryText, history?, delivery?, detailInTurnItem?, createdByProviderInstanceId|null, createdAt, updatedAt}.
  - strategy = delta_since_target_last_seen | fork_delta_summary | full_thread_summary | checkpoint_summary | manual_context.
  - status = pending | ready | failed | superseded.
  - history = {messages: HistoricalMessage[], coverage, omittedItems, omittedItemIds?}.
  - delivery = {nativeThreadId, status: pending|injected|inline, itemIds, omittedItemIds?}.
  - `HistoricalMessage` (L870) = {role: user|assistant, text, runStatus?, threadId, runId|null, itemId, providerThreadId|null, status, kind}.

### 1.7 Domain events (L1518-1666)

The base shape (L1518) is {id: EventId, threadId, runId?, nodeId?, driver?, providerInstanceId?, rawEventId?, occurredAt, type, payload}. Each event type carries:

| type(s) | payload |
|---|---|
| thread.created | AppThread |
| thread.archived, .unarchived, .deleted, .settled, .unsettled, .snoozed, .unsnoozed, .pinned, .auto-settle-set, .unpinned, .pin-reordered, .active-reordered, .visited, .marked-unread, .metadata-updated, .pull-request-synced, .runtime-mode-updated, .interaction-mode-updated, .model-selection-updated, .provider-switched | AppThread (full replacement) |
| run.created, run.updated | Run |
| run.background-work-cancelled | {runId, restartCancelledBackgroundWork[]} (patches one field) |
| run-attempt.created, run-attempt.updated | RunAttempt |
| node.updated | ExecutionNode |
| subagent.updated | Subagent |
| provider-session.attached, provider-session.updated | ProviderSession |
| provider-session.detached | ProviderSessionDetached (deletes the row) |
| provider-thread.updated | ProviderThread |
| provider-turn.updated | ProviderTurn (the client keeps the previous tokenUsage when this one omits it) |
| runtime-request.updated | RuntimeRequest |
| message.updated | ConversationMessage |
| turn-item.updated | TurnItem |
| plan.updated | PlanArtifact |
| checkpoint-scope.created | CheckpointScope |
| checkpoint.captured | Checkpoint |
| checkpoint.rollback-requested | CheckpointRollbackRequest (no change to the projection) |
| context-handoff.updated | ContextHandoff |
| context-transfer.created, context-transfer.updated | ContextTransfer |

Envelopes:
- **`StoredEvent`** (L1860) = {sequence: NonNegativeInt, commandId|null, event}.
- `*Json` variants (L1867-2475) are the same shapes with DateTime fields as ISO strings.
- **`RawProviderEvent`** (L1503) = {id, driver, providerInstanceId, providerSessionId, sequence, direction: incoming|outgoing, messageKind: request|response|notification|error, method|null, jsonRpcId: string|number|null, payload, observedAt}.

### 1.8 Thread projection

**`ThreadProjection`** (L1669) = {thread, runs[], attempts[], nodes[], subagents[], providerSessions[], providerThreads[], providerTurns[], runtimeRequests[], messages[], plans[], turnItems[], checkpointScopes[], checkpoints[], contextHandoffs[], contextTransfers[], visibleTurnItems: ProjectedTurnItem[], updatedAt}.

### 1.9 Commands

Every command carries `commandId` and a thread id, either `threadId`, or `sourceThreadId`/`parentThreadId` for fork, merge-back and delegated-task commands.

**Client commands** (`OrchestrationV2Command`, L2477-2915), grouped by area:

Thread lifecycle:
- `thread.create` {createdBy, creationSource, threadId, projectId, title, modelSelection, runtimeMode, interactionMode, branch|null, worktreePath|null, importedNativeThread?: {ref {driver, nativeId, strength:"strong"}, metadata?}}
- `thread.archive`, `thread.unarchive`, `thread.delete` {threadId}
- `thread.settle` {settledAt?}
- `thread.auto-settle` {snapshotAt, settledAt?} (sent only by the server-side sweep)
- `thread.unsettle` {reason:"user"}
- `thread.snooze` {snoozedUntil: IsoDateTime}
- `thread.unsnooze` {reason:"user"}
- `thread.auto-settle.set` {enabled}
- `thread.pin` {orderKey?}, `thread.unpin`, `thread.pin.reorder` {orderKey}, `thread.active.reorder` {orderKey}
- `thread.visit` {visitedAt: IsoDateTime} (the server keeps the maximum seen)
- `thread.mark-unread`

Thread settings and metadata:
- `thread.metadata.update` {title?, regenerateTitle?, branch?|null, worktreePath?|null, expectedWorktreePath?|null, expectedEmpty?, limitRecovery?: LimitRecoveryUpdate|null, linkedPullRequest?|null}
- `thread.title.regeneration.complete` {requestId, title?}
- `thread.runtime-mode.set` {runtimeMode}, `thread.interaction-mode.set` {interactionMode}, `thread.model-selection.set` {modelSelection}
- `provider.switch` {threadId, modelSelection}
- `provider-session.detach` {providerSessionId, reason?}

Pull requests:
- `thread.pull-request.link` {host, repository, number, url, source}
- `thread.pull-request.unlink` {host, repository, number}
- `thread.pull-request-link.sync` {key fields, snapshot, stack|null}
- `thread.pull-request.watch` {key fields, watching, link?: {url, source}}
- `thread.pull-request.sync` {projectId, snapshotSequence, expected {workspaceRoot, branch, worktreePath, linkedPullRequest, branchPullRequest}, branchPullRequest|null, linkedPullRequest?}

Messages and runs:
- `message.dispatch`:
  - Fields: {notification?, createdBy, creationSource, scheduledTaskId?, senderThreadId?, threadId, messageId, text, context?, attachments[], titleSeed?, modelSelection?, sourcePlanRef?, restartContinuationOfRunId?, usageLimitContinuationOfRunId?, manualContinuationOfRunId?, usageLimitRecoveryRequestId?, deliveryIntent?: auto|steer|restart, delegatedCompletion?: {parentRunId, generation, taskIds}, dispatchMode}.
  - dispatchMode is one of: `defer_start{workspaceStrategy?}`, `steer_active{targetRunId}`, `restart_active{targetRunId}`, `queue_after_active`, `start_immediately`.
- `notification.delivery.accept` {messageId}
- `prepared-run.release` {runId}, `prepared-run.progress` {runId, phase: worktree|setup}, `prepared-run.fail` {runId, failure}, `prepared-run.retry` {runId}
- `run.interrupt` {runId, reason?, holdQueue?}

Queue:
- `queued-message.promote-to-steer` {queuedRunId, targetRunId}
- `queue.resume` {threadId}
- `queued-run.reorder` {runId, beforeRunId|null}
- `queued-run.cancel` {runId}
- `queued-run.edit` {runId, text, context?, attachments?} (omitting attachments keeps the existing ones)

Approvals and questions:
- `runtime-request.respond` {requestId, decision?, answers?, attachmentsByQuestionId?}
- `thread.user-input.dismiss` {requestId}

Checkpoints:
- `checkpoint.rollback` {scopeId, checkpointId, restoreFiles?}

Fork and delegation:
- `thread.fork` {createdBy, creationSource, sourceThreadId, targetThreadId, sourcePoint, title?, createdAt?}
- `thread.merge_back` {creation fields, sourceThreadId, targetThreadId, sourcePoint, createdAt?}
- `delegated_task.request` {creation fields, parentThreadId, parentRunId, parentNodeId, task, title?, modelSelection, runtimeMode, interactionMode, completionWake?, createdAt?}
- `delegated_task.wake-policy` {parentThreadId, taskId, completionWake}
- `delegated_task.completion-delivery.acknowledge` {parentThreadId, taskId, observedByRunId|null}
- `delegated_task.completion-delivery.dispose` {parentThreadId, taskId}
- `thread.created.record` {parentThreadId, parentRunId, parentNodeId, targetThreadId, targetRunId|null}

**Server-only commands** (`OrchestrationV2InternalCommand`, L2923-2966):
- `thread.pull-request-watch.sync` {key fields, startedAt, watch|null, wake?: {messageId, text, notification}}
- `checkpoint.rollback.fail` {requestId: CommandId, message}
- `thread.background-work.settle` {providerThreadId, providerTurnId}

### 1.10 Orchestration RPC methods (L2972, schemas L3314-3359)

| method | request | response | stream |
|---|---|---|---|
| `orchestration.dispatchCommand` | OrchestrationV2Command | {sequence} | no |
| `orchestration.getTurnDiff` | {threadId, fromTurnCount, toTurnCount, ignoreWhitespace?} (`checkpointDiff.ts:29`) | {threadId, fromTurnCount, toTurnCount, diff} | no |
| `orchestration.getFullThreadDiff` | {threadId, toTurnCount, ignoreWhitespace?} | same as above | no |
| `orchestration.searchThreads` | {query (2..200 chars), limit? 1..50} (`threadSearch.ts:16`) | {matches: {threadId, projectId, source: user|assistant, snippet≤240, messageCreatedAt|null}[]} | no |
| `orchestration.getArchivedShellSnapshot` | {} | ArchivedShellSnapshot | no |
| `orchestration.getThreadProjection` | {threadId} | ThreadProjection | no |
| `orchestration.getWorkflowScript` | {threadId, scriptPath} | {scriptPath, contents, truncated} | no |
| `orchestration.getTurnItem` | {threadId, itemId, revision?} | {item: TurnItem|null} | no |
| `orchestration.launchThread` | ThreadLaunchInput (below) | {threadId, projection, resumed} | no |
| `orchestration.subscribeArchivedShell` | {} | ArchivedShellStreamItem | yes |
| `orchestration.subscribeShell` | {afterSequence?, requestCompletionMarker?} | ShellStreamItem | yes |
| `orchestration.subscribeThread` | {threadId, afterSequence?, requestCompletionMarker?, acceptBoundedSnapshot?} | ThreadStreamItem | yes |

- **`ThreadLaunchInput`** (L3014) = {commandId, creationSource?, threadId?, reuseExistingThread?, projectId, title, generateTitle?, modelSelection, runtimeMode, interactionMode, workspaceStrategy, initialMessage?: {messageId?, text, context?, attachments}}.
- **`ThreadStreamItem`** (L3177) is one of:
  - {kind:"synchronized"}
  - {kind:"snapshot", snapshotSequence, projection, historyCursor?, hasMoreHistory?, latestLocalTurnOrdinal?, payloadBudgetExceeded?}
  - {kind:"event", sequence, event}
  - Decode-only: {kind:"unknown-event", sequence, eventType}, used when the event type is unknown to the client.
- **Errors** (L3204-3247):
  - DispatchCommandError {commandId, commandType, message, detail?}
  - GetThreadProjectionError {threadId, message}
  - GetShellSnapshotError {message}
  - ThreadLaunchError {commandId, projectId, message}
  - Workflow script error reasons (L3292): invalid-path, root-unavailable, not-found, outside-root, not-js, not-regular-file, changed-during-read, read-failed.
- **HTTP endpoints** (`packages/contracts/src/environmentHttp.ts:526-558`), all with the orchestration protocol header:
  - `GET /api/orchestration/shell` → ShellSnapshot
  - `GET /api/orchestration/threads/:threadId` → ThreadDetailSnapshot {snapshotSequence, projection, historyCursor?, hasMoreHistory?, latestLocalTurnOrdinal?}
  - `GET .../bounded` → ThreadBoundedSnapshot {snapshotSequence, projection, historyCursor|null, hasMoreHistory, latestLocalTurnOrdinal|null, payloadBudgetExceeded?}
  - `GET .../history?cursor` → ThreadHistoryPage {snapshotSequence, items: ProjectedTurnItem[], nextCursor|null, hasMoreHistory}
- **Test-replay types** (L3361-3405):
  - ProviderReplayEntry is expect_outbound{frame} | emit_inbound{frame, afterMs?} | runtime_exit{status, error?}.
  - ProviderReplayTranscript and its NDJSON header follow the entry type.

### 1.11 Other RPC methods in `rpc.ts`, by area

`WS_METHODS` is at L339-536 and the registered group at L1702-1877. Every method's error union also includes `EnvironmentAuthorizationError`. The rows show request → response, with "stream" marking streaming methods.

**Projects:**
- `projects.mutate` ProjectMutation → Project.
  - ProjectMutation (`project.ts:192`) is project.create {commandId, projectId, title, workspaceRoot, createWorkspaceRootIfMissing?, defaultModelSelection?, scripts?}, project.update {commandId, projectId, any of title/workspaceRoot/defaultModelSelection/autoPull/projectIcon/faviconPath/defaultThreadEnvMode/scripts}, or project.delete {commandId, projectId, force?}.
  - Project = {id, title, workspaceRoot, repositoryIdentity?, faviconPath?, projectIcon?, defaultModelSelection|null, defaultThreadEnvMode?, autoPull?, scripts: {id, name, command, icon, runOnWorktreeCreate, async?, previewUrl?, autoOpenPreview?}[], createdAt, updatedAt, deletedAt|null}.
- `projects.ensureScratch` {} → {projectId}.
- `projects.createNew` {name} → {projectId, workspaceRoot, commitError?}.
- `projects.listEntries`, `projects.readFile`, `projects.writeFile`, `projects.searchEntries`, `projects.searchContents` use the `Project*Input`/`*Result` types in `project.ts`.
- `projects.list`, `projects.add` and `projects.remove` are named in `WS_METHODS` (L341-343) but are not in `WsRpcGroup`. They are dead names.
- `projectClone.start` / `.cancel` / `.retry`; `subscribeProjectClones` (stream).
- The shell also carries `OrchestrationProjectShell` (`orchestrationProject.ts:9`) = {id, title, workspaceRoot, repositoryIdentity?, defaultModelSelection|null, defaultThreadEnvMode?, autoPull?, faviconPath?, projectIcon?, scripts, createdAt, updatedAt}.

**Session history import:**
- `agentSessions.scan` {} → {candidates: {path, title, projectId?, sources: (claudeAgent|codex)[], threadCount, lastActiveAt|null, alreadyImported, git?: {remoteKey|null, repository|null}|null}[], scannedAt, truncated?}.
- `agentSessions.import` {projectId, expectedWorkspaceRoot?} → {importedCount, skippedCount} (`agentSessions.ts`).

**Settings and server:**
- `server.getSettings` {} → ServerSettings.
- `server.updateSettings` {patch: ServerSettingsPatch, providerInstanceMutation?} → ServerSettings (`settings.ts`, 1807 lines).
- `server.getConfig` → ServerConfig; `subscribeServerConfig` {environmentThemes?, usageLimitSources?, usageLimitsCommand?} → ServerConfigStreamEvent (stream).
- `server.probe`, `server.upsertKeybinding`, `server.removeKeybinding`, `server.updateServer` (+`WithProgress` stream), `server.commitDesktopUpdate`, `subscribeServerLifecycle` (stream).
- Diagnostics: getTraceDiagnostics, getProcessDiagnostics, getHostResources, getProcessResourceHistory, getResourceTelemetryHistory, retryResourceTelemetry, signalProcess, subscribeResourceTelemetry (stream).
- Client state and usage: reportClientActivity, reportHostPowerState, getBackgroundPolicy, subscribeBackgroundPolicy (stream), getUsageSummary, refreshUsageRates.

**Providers:**
- `server.refreshProviders` {instanceId?, cwd?, fresh?, refreshModels?} → ServerProviderUpdatedPayload; `server.updateProvider`.
- Auth: `provider.auth.start`, `.respond`, `.complete`, `.cancel`, `.logout` → ProviderAuthState; `provider.auth.subscribe` (stream).
- Install: `provider.install.start`, `.cancel`, `.remove`; `provider.install.subscribe` (stream).
- ChatGPT and Codex: `provider.chatgpt.reconnect-profile`, `.import-profile`, `.handoff.subscribe` (stream); `provider.codex.auth-callback.subscribe` (stream).
- `provider.consumeResetCredit`, `provider.uploadFeedback`.
- ACP registry: search, prepare, uninstall binary, accept URL auth, list/import/delete sessions, list/set/disable providers, logout.

**Git / VCS** (`git.ts`):
- `vcs.pull` {cwd} → {status: pulled|skipped_up_to_date, refName, upstreamRef|null}.
- `vcs.refreshStatus` {cwd} → VcsStatusResult; `subscribeVcsStatus` {cwd} → snapshot{local, remote|null} | localUpdated{local} | remoteUpdated{remote} (stream).
- `vcs.listRefs` {cwd, query?, cursor?, includeMatchingRemoteRefs?, refKind? all|local|remote, refresh?, limit?}.
- `vcs.createWorktree` {cwd, refName, newRefName?, baseRefName?, path|null} → {worktree}.
- `vcs.removeWorktree` {cwd, path, force?}.
- `vcs.createRef` {cwd, refName, switchRef?}; `vcs.switchRef` {cwd, refName}; `vcs.init` {cwd, kind?}.
- `git.runStackedAction` {actionId, cwd, action, commitMessage?, featureBranch?, filePaths?, threadId?, projectId?} → GitActionProgressEvent (stream).
- `git.resolvePullRequest` {cwd, reference}.
- `git.preparePullRequestThread` {cwd, reference, mode, threadId?} → {pullRequest, branch, worktreePath|null, isOnPullRequestHead}.
- `subscribeWorktreeSetup` (stream), `worktreeSetup.cancel`.
- Review and pull requests: `review.getDiffPreview`, `review.getDiffFileContents`, and 29 `pullRequests.*` methods (L483-511).
- Source control: `sourceControl.lookupRepository`, `.cloneRepository`, `.publishRepository`.

**Terminal** (`terminal.ts`):
- `terminal.open` {threadId, terminalId, cwd, worktreePath?, cols?, rows?, env?, providerInstanceId?} → TerminalSessionSnapshot.
  - TerminalSessionSnapshot = {threadId, terminalId, cwd, worktreePath|null, status: starting|running|exited|error, pid|null, history, exitCode|null, exitSignal|null, label, updatedAt, sequence?}.
- `terminal.attach` {…, restartIfNotRunning?} → snapshot | output{data} | exited | closed | error | cleared | restarted | activity (stream).
- `terminal.write` {threadId, terminalId, data≤64KiB}; `terminal.resize` {cols, rows}; `terminal.clear`; `terminal.restart` → snapshot; `terminal.close` {threadId, terminalId?, deleteHistory?}.
- `subscribeTerminalEvents`, `subscribeTerminalMetadata` (both stream).

**Other:**
- `shell.openInEditor`, `filesystem.browse`.
- Assets and attachments: `assets.createUrl`, `assets.persistChatAttachments` {threadId, messageId, attachments: {id?, type:"image", name, mimeType, sizeBytes, dataUrl, source?}[]} → {attachments}, `attachments.createUploadUrl`, `attachments.delete`.
- `scheduledTasks.list` / `.subscribe` / `.upsert` / `.setEnabled` / `.delete` / `.runNow`.
- `cloud.getRelayClientStatus`, `cloud.installRelayClient`.
- Preview and devices: `preview.*`, `previewAutomation.*`, `device.*`, `subscribePreviewEvents`, `subscribeDiscoveredLocalServers`, `subscribeDeviceState`, `subscribeAuthAccess`.

---

## 2. Server architecture (`apps/server/src/orchestration-v2/`)

### Command path: plan, then one transaction, then side effects

1. **Entry.** `dispatchWithReceipt` takes a per-thread lock (`Orchestrator.ts:9868`, `threadDispatch.withLock`, built on `KeyedSerialExecutor.ts`).
2. **Idempotency** (L9690-9741). If a receipt already exists for the command id:
   - A `rejected` receipt fails with `OrchestratorCommandPreviouslyRejectedError`.
   - A receipt for a different thread fails with `OrchestratorCommandIdConflictError`.
   - Otherwise the stored events are replayed and the stored result sequence is returned.
3. **Plan** (`dispatchOnce`, L9404-9679). One switch dispatches on command type and fills two accumulators: domain events and pending effects. A Stop also returns `cancelUnsettledEffects`. Planning reads projections and writes nothing.
   - A planning failure calls `commitRejectedCommand` (EventSink L586-609), which inserts a `rejected` receipt in its own transaction.
   - A plan with zero events is an error, except for `thread.background-work.settle`.
4. **Commit** (`EventSink.commitCommand`, L518-584). Inside one `sql.withTransaction`:
   1. Insert the receipt if absent, as status accepted with sequence 0.
   2. Normalize turn-item ordinals with `TurnItemPositionStore`. Each run gets a band of `run.ordinal*1_000_000`, tracked in `orchestration_v2_turn_item_positions`.
   3. Append events to `orchestration_events` (EventStore L326-387: `aggregate_kind='thread'`, `stream_version=MAX+1`, `application_event_version=2`, `RETURNING sequence`).
   4. Apply each event through `ProjectionStore.apply` (L1679-2470, one upsert into the matching projection table).
   5. Update `orchestration_v2_projection_metadata.last_sequence`.
   6. Insert the pending effects into `orchestration_v2_effect_outbox`.
   7. Update the receipt with the final sequence.
   8. Optionally cancel unsettled effects.
5. **Publish** (`commitThenPublish`, L239-261). The writer takes a single-permit "publish lane" semaphore as the last step of the transaction and releases it only after publishing to the PubSubs. This keeps live events ordered by sequence.
6. **After commit** (L9852-9860): `queue.resume` calls `startNextQueuedRun`, and notification and wake-policy commands offer delegated deliveries.
7. **Non-command writes.** Provider ingestion and the services use other EventSink entry points:
   - `write` / `writeWithEffects` (L361) append without a receipt.
   - `writeIfRunCurrent` (L395) checks the run's status and `activeAttemptId` inside the transaction and drops the write if either changed (optimistic concurrency).
   - `writeIfProviderThreadOwner` (L447) does the same check against `last_run_ordinal`.

### Side effects (outbox and worker)

- **Effect types** (`EffectOutbox.ts:26-105`): provider-runtime.continue, provider-session.detach, provider-turn.start, provider-turn.interrupt, provider-turn.steer, provider-turn.restart, runtime-request.respond, provider-thread.rollback, checkpoint.capture, terminal.cleanup, attachment.cleanup, thread-title.generate.
- **Claiming** (L287-326). Effects run one at a time per thread in rowid order. An earlier pending effect blocks later ones even while it waits out a retry backoff. `thread-title.generate` has its own per-thread lane.
- **Leases.** Claims are SQL `UPDATE … RETURNING` statements with `lease_owner`/`lease_expires_at` (L491-525).
- **Worker** (`EffectWorker.ts:107-466`). Each effect type dispatches to a service:
  - provider-turn.start → `ProviderTurnStartService.start`
  - provider-turn.interrupt → `ProviderTurnControl.interrupt`, then dispatches `thread.background-work.settle`
  - provider-turn.steer → `steer`. If the turn already completed, it re-dispatches `message.dispatch` as a new turn with id `command:steer-follow-up:<effectId>`.
  - provider-turn.restart → interrupt and await the terminal event, optionally detach the session, then start the turn.
  - runtime-request.respond → `RuntimeRequestService.respond`
  - provider-thread.rollback → `CheckpointRollbackService.execute`. The final failure dispatches `checkpoint.rollback.fail`.
  - checkpoint.capture → `RunFinalizationService.finalize`
- **Worker settings.** Concurrency 4, lease 30s, at most 5 attempts. Backoff is `min(30s, 100ms·2^(n-1))` (L515, L704, L756).
- **After a crash** (`reconcileAfterProcessLoss`, L108-124, L458-485): process-bound effect types (turn start, interrupt, steer, restart, request respond) are cancelled; replay-safe types are requeued.
- **Startup order** (`serverRuntimeStartup.ts:384-413`): import legacy shells, then `ProviderRuntimeRecovery.recover`, then `recoverDelegatedTasks`, then start the effect worker, then auto-bootstrap.
  - Recovery (`ProviderRuntimeRecoveryService.ts`) terminalizes live runs, sets `queueHeld=true` on queued runs (L281-289), cancels pending requests (L304), and enqueues restart continuations.

### Reactors

- **Queue promotion.** The Orchestrator subscribes to terminal `run.updated` events (completed, interrupted, failed, cancelled, rolled_back), skipping commands that start with `command:runtime-reconcile:`. Each one triggers `handleTerminalRun`: finalize the app-owned subagent's parent, finalize delegated delivery, then `startNextQueuedRun` (`Orchestrator.ts:9871-9930`).
- **Provider events.** `RunExecutionService` consumes the adapter's event stream. `ProviderEventIngestor.normalize` (L389-547) maps adapter events to domain events 1:1:
  - `app_thread.created` → `thread.created`
  - `provider_session.updated` → `provider-session.updated`
  - `provider_thread.updated` → `provider-thread.updated`
  - `provider_turn.updated` → `provider-turn.updated`, plus dismissal of native user-input requests when the turn is terminal
  - `node.updated`, `subagent.updated`, `message.updated` → same-named events
  - `turn_item.updated` → `turn-item.updated`
  - `runtime_request.updated` → `runtime-request.updated`
  - `plan.updated` → `plan.updated`, with todo step durations computed
  - `turn.terminal` failed → an error turn item
- **Run ending.** `RunExecutionService.finalize` (L560-700) runs on `turn.terminal`:
  - completed → run and root node become `waiting`, `completedAt` stays null, and a `checkpoint.capture` effect is enqueued.
  - interrupted or cancelled → terminal status plus capture.
  - failed → `failed`.
  - The capture (`CheckpointCaptureService.ts:149-232`) emits `checkpoint.captured` and moves a `waiting` run to `completed`.
- Other reactors: `PullRequestSyncReactor.ts`, `PullRequestWatchReactor.ts`, `UsageLimitRecoveryWorker.ts`, `ProviderContinuationService.ts` (background wake turns), `ThreadSettlementService.ts` (auto-settle sweep).

### Run state machine

- **Blocking statuses** (a new message queues behind these) are preparing, starting, running and waiting (`Orchestrator.ts:460`). Only preparing, starting and running count as live for wakes (L474).
- **Creation** (`dispatchMessage`, L4310-6318):
  - No blocking run: `defer_start` creates a `preparing` run (workspace setup); otherwise the run is `starting` and a `provider-turn.start` effect is enqueued (L5157).
  - A blocking run exists and the mode is defer/start/queue: the run is `queued` with `queuePosition = max(queued positions)+1`. It inherits `queueHeld=true` if any queued run is held (L4797-4829).
- **Mode choice when `deliveryIntent` is set** (`CommandPolicy.ts:120-165`):
  - No active run → `start_immediately`.
  - Intent `steer` → `steer_active`; intent `restart` → `restart_active`.
  - Active run is preparing or starting → `queue_after_active`.
  - Otherwise, by provider capability, in this order: active steering → `steer_active`, queued messages → queue, interrupt-restart → `restart_active`, and the fallback is queue.
- **Transitions:**
  - preparing → starting via `prepared-run.release` (L7661); preparing → failed via `prepared-run.fail` (L7768); failed → preparing via `prepared-run.retry` (L7852).
  - starting → running is written by `ProviderTurnStartService` through `writeIfRunCurrent` (L838-933).
  - running → waiting → completed, as described under run ending.
  - running → interrupted / failed / cancelled on `turn.terminal`.
  - queued → starting via `startNextQueuedRun` (L1210-1816, effect at L1810); queued → cancelled by cancel or promote; queued → failed via `failQueuedRunStart` (L1105-1205).
  - completed → rolled_back via rollback.
- **Attempts.** A new attempt with reason `steering_restart` is created for interrupt-restart steering (L4143-4148).

### Queue operations

- **Delivery order** (`QueuedRunOrder.ts:12-31`): delegated-completion runs first, then by `queuePosition ?? ordinal`, then by ordinal.
- **Hold.** A Stop with `holdQueue` sets `queueHeld=true` on queued runs (L8069-8085). Restart recovery holds them too.
- **Resume.** `queue.resume` (L9577-9633) rejects archived or usage-limited threads, clears `queueHeld`, and then starts the next run.
- **Reorder** (L7217-7306) recomputes `queuePosition` from index+1. Delegated-completion runs cannot be moved, and nothing can be placed ahead of them.
- **Cancel** (L7308-7400) marks the run, its attempt and its root node `cancelled`.
- **Edit** (L7402-7490) rewrites `message.updated` and the `turn-item.updated` for the user message. Empty text is rejected.
- **Promote to steer** (L7077-7215) cancels the queued run, then steers its message into the target run with `inputIntent` `promoted_queued_to_steer`.

### Steer and Stop

- **Steer** (`dispatchSteerIntoRun`, L3585-4310):
  - If the provider supports active steering, it emits a user turn item with intent `steer` and enqueues `provider-turn.steer` (L3885).
  - Otherwise it creates a new attempt and enqueues `provider-turn.restart` (L4296).
  - If the target already completed, the message becomes a new turn (L4497-4515).
- **Stop** (`run.interrupt`, L8000-8391):
  - It always emits a `run_interrupt_request` turn item.
  - If no provider turn exists yet, the attempt, root node and run are marked `interrupted` in the same commit. A `run_interrupt_result` item is added, and pending start/restart effects are cancelled (L8110-8228).
  - If the session is gone, it only settles background work (L8298-8327).
  - Otherwise it enqueues `provider-turn.interrupt` for the run, plus one for each other provider thread that still has background work (L8243-8293, L8375-8389).

### Approvals and questions

- **Adapter side.** The adapter emits `node.updated` (approval_request or user_input_request), `runtime_request.updated` (pending, `responseCapability: live`) and the matching turn item. It then waits on an internal Deferred.
- **Respond** (`runtime-request.respond`, `Orchestrator.ts:6810-7037`):
  - Rejects if the request is missing, not pending, or `not_resumable`.
  - Emits resolved request, node and turn-item events.
  - For live requests it enqueues the `runtime-request.respond` effect, which calls `adapter.respondToRuntimeRequest` and resolves the Deferred.
  - For `message`-capable user-input requests (the live callback is gone), the answers become a `message.dispatch` that queues or steers (L6942-7007).
- **Dismiss.** `thread.user-input.dismiss` (L7039-7076) applies only to message-mode user input.

### Rollback and checkpoints

- **Command** (`checkpoint.rollback`, L8393-8568). It validates:
  - the active provider thread and session exist and the provider supports rollback;
  - the checkpoint is `ready` and its scope matches;
  - the workspace restore is isolated when files are restored;
  - the target provider turn is on the same provider thread.
  It then emits `thread.metadata-updated` (rollbackRequestId, rollbackFailure null) and `checkpoint.rollback-requested`, and enqueues `provider-thread.rollback`.
- **Execution** (`CheckpointRollbackService.ts:212-342`): `adapter.rollbackThread` with target `thread_start` or `provider_turn`, then `checkpoints.restore` (unless `restoreFiles` is false), then events for the provider thread, `checkpoint.captured`, and runs and nodes marked `rolled_back`.
- **Storage.** Checkpoints are git refs `refs/t3/orchestration-v2/checkpoints/<b64url(sha256(scopeId)[:32])>/ordinal/<n>` (`CheckpointService.ts:26,161-169`), written through `checkpointing/CheckpointStore.ts`.

### Fork and merge-back

- **Command.** `thread.fork` (L3354-3440) emits `thread.created` for the target, with fork lineage and `forkedFrom`, and `context-transfer.created` with status `pending`. Nothing is sent to the provider yet.
- **Resolution** happens at the target's first turn in `ProviderTurnStartService`:
  - `session.forkThread` → `resolved_native` / `native_fork` (L632, L893), or
  - `ensureThread` plus a portable handoff → `resolved_portable` / `portable_context` (L650-762).
  - History injection goes through `injectHistory` (L1136).
- **Merge-back.** `thread.merge_back` (L3442) works the same way.

### SQLite schema

Base tables come from `Migrations/001,002,004`. The V2 setup is `055_OrchestrationV2.ts` plus the files under `Migrations/OrchestrationV2/`.

**Event log and receipts (live):**
- **`orchestration_events`**:
  - Columns: sequence INTEGER PK AUTOINCREMENT, event_id UNIQUE, aggregate_kind, stream_id, stream_version, event_type, occurred_at, command_id, causation_event_id, correlation_id, actor_kind (server|provider), payload_json, metadata_json, application_event_version INTEGER DEFAULT 1.
  - V2 rows use aggregate_kind='thread' and version 2. `metadata_json` = {runId?, nodeId?, driver?, providerInstanceId?, rawEventId?} (OrchestrationEventStore.ts:95).
  - Indexes: UNIQUE(aggregate_kind, stream_id, stream_version); (aggregate_kind, stream_id, sequence); command_id; correlation_id; (application_event_version, sequence); partial indexes from ApplicationEventSequenceIndexes.ts.
- **`orchestration_command_receipts`**: command_id PK, aggregate_kind, aggregate_id, accepted_at, result_sequence, status (accepted|rejected), error, command_type.

**Projection tables.** Each is `payload_json` plus indexed columns.

| table | key and columns |
|---|---|
| `orchestration_v2_projection_threads` | thread_id PK; project_id, title, default_provider, runtime_mode, interaction_mode, active_provider_thread_id, created_at, updated_at, archived_at, deleted_at, provider_instance_id |
| `…_runs` | run_id PK; thread_id, ordinal (UNIQUE with thread_id), provider, provider_thread_id, status, requested_at, completed_at, provider_instance_id |
| `…_run_attempts` | attempt_id PK; thread_id, run_id, attempt_ordinal (UNIQUE with run_id), root_node_id, provider, provider_thread_id, provider_turn_id, status, provider_instance_id |
| `…_nodes` | node_id PK; thread_id, run_id, parent_node_id, root_node_id, kind, status, provider_thread_id, provider_turn_id, runtime_request_id, checkpoint_scope_id, started_at, completed_at |
| `…_subagents` | subagent_id PK; thread_id, run_id, parent_node_id, provider, provider_thread_id, child_thread_id, origin, status, started_at, completed_at, updated_at, driver, provider_instance_id |
| `…_provider_sessions` | provider_session_id PK; thread_id, provider, status, model, updated_at, driver, provider_instance_id |
| `…_provider_session_bindings` | PK(provider_session_id, thread_id) |
| `…_provider_threads` | provider_thread_id PK; thread_id, owner_node_id, provider, provider_session_id, status, first_run_ordinal, last_run_ordinal, updated_at, driver, provider_instance_id |
| `…_provider_turns` | provider_turn_id PK; thread_id, provider_thread_id, node_id, run_attempt_id, ordinal (UNIQUE with provider_thread_id), status, started_at, completed_at |
| `…_runtime_requests` | runtime_request_id PK; thread_id, node_id, provider_turn_id, kind, status, created_at, resolved_at |
| `…_messages` | message_id PK; thread_id, run_id, node_id, role, streaming, created_at, updated_at |
| `…_plans` | plan_id PK; thread_id, run_id, node_id, kind, status |
| `…_turn_items` | turn_item_id PK; thread_id, run_id, node_id, provider_thread_id, provider_turn_id, parent_item_id, ordinal, type, status, updated_at |
| `…_checkpoint_scopes` | scope_id PK; thread_id, run_id, node_id, parent_scope_id, provider_thread_id, kind, ordinal_within_parent, advances_app_run_count, created_at |
| `…_checkpoints` | checkpoint_id PK; thread_id, scope_id, run_id, node_id, parent_checkpoint_id, ordinal_within_scope (UNIQUE with scope_id), app_run_ordinal, status, captured_at |
| `…_context_handoffs` | context_handoff_id PK; thread_id, target_run_id, to_provider_thread_id, strategy, status, updated_at |
| `…_context_transfers` | context_transfer_id PK; source_thread_id, target_thread_id, target_run_id, type, status, source_provider, target_provider, updated_at, source/target_provider_instance_id |

**Supporting tables:**
- `orchestration_v2_turn_item_positions`: PK(thread_id, turn_item_id), UNIQUE(thread_id, ordinal).
- `orchestration_v2_projection_metadata`: projection_name PK, schema_version (currently 2, ProjectionStore.ts:490), last_sequence, updated_at. `ProjectionMaintenance.ts` rebuilds all projections from the log when these are stale.
- `orchestration_v2_effect_outbox` (EffectCancellation.ts:9): effect_id PK, command_id, thread_id, effect_type, payload_json, status CHECK(pending|running|succeeded|failed|cancelled), attempt_count, available_at, lease_owner, lease_expires_at, created_at, updated_at, completed_at, last_error.
- `orchestration_v2_thread_launch_workflows`: command_id PK, thread_id, project_id, status, title, worktree_path, branch, setup_committed, thread_committed, message_committed, last_error, created_at, updated_at.
- `orchestration_v2_legacy_imports`: thread_id PK, source_updated_at, shell_imported_at, transcript_imported_at, imported_message_count, last_error.
- `provider_session_runtime`: thread_id PK, provider_name, adapter_key, runtime_mode, status, last_seen_at, resume_cursor_json, runtime_payload_json, provider_instance_id.
- `scheduled_tasks`: see `ScheduledTasks.ts:9`.
- Projects are stored in `projection_projects`, written as project events in the same log.

---

## 3. Provider adapters

**Adapter interface** (`ProviderAdapter.ts`):
- `getCapabilities`, `planSelectionTransition`, `openSession(input) → SessionRuntime` (L581-594).
- The session runtime (L484-579) provides: `events: Stream<ProviderAdapterV2Event>`, `ensureThread`, `resumeThread`, `injectHistory?`, `startTurn`, `compactThread?`, `steerTurn`, `interruptTurn`, `unloadThread?`, `respondToRuntimeRequest`, `readThreadSnapshot`, `uploadFeedback?`, `rollbackThread`, `forkThread`, `hasPendingBackgroundWork[ForThread]?`.
- **Adapter events** (L78-154): app_thread.created, provider_session.updated, provider_thread.updated, provider_turn.updated, node.updated, subagent.updated, message.updated, turn_item.updated, runtime_request.updated, plan.updated, and turn.terminal.
  - turn.terminal = {providerThreadId, providerTurnId, runOrdinal, status: completed|interrupted|cancelled|failed, failure, failureItemOrdinal?, retry?, threadDisposition: reusable|broken}.
- Adapters build complete entity records themselves, with ids from `IdAllocator.derive.*` (`IdAllocator.ts:71-180`). The ingestor only wraps them in envelopes.

### Codex (`CodexAdapterV2.ts`)

The adapter spawns `codex app-server` (`codexLaunchArgs.ts:13`) and speaks JSON-RPC over stdio through `packages/effect-codex-app-server`.

**Requests T3 sends:**
- Handshake: `initialize` {clientInfo {name:"T3 Code", title, version}, capabilities {experimentalApi:true, optOutNotificationMethods:["turn/diff/updated"]}}, then the `initialized` notification (L232-235, L1616-1622, `CodexProvider.ts:347`).
- `thread/start` with `codexThreadRuntimeParams` = {cwd?, model?, config: {...CODEX_THREAD_CONFIG, mcp_servers?: {"t3-code": {url, http_headers}}}} (L1199, L5401).
- `thread/resume` {threadId, excludeTurns:true, ...params}. If the session is archived, it calls `thread/unarchive` and retries (L5433-5461).
- `turn/start` (L696-774, L5578) = {threadId, input: [{type:"text"}, image items], additionalContext?, cwd, model, summary:"detailed", approvalsReviewer, approvalPolicy, sandboxPolicy, effort?, serviceTier?, collaborationMode? {mode: plan|default, settings {model, reasoning_effort, developer_instructions?}}}.
- **Runtime-mode mapping** (L655-694):

  | runtimeMode | approvalPolicy | approvalsReviewer | sandbox |
  |---|---|---|---|
  | approval-required | untrusted | user | readOnly |
  | auto-accept-edits | on-request | user | workspaceWrite |
  | auto | on-request | auto_review | workspaceWrite |
  | full-access | never | user | dangerFullAccess |

- `turn/steer` {threadId, expectedTurnId, input} (L5626).
- `turn/interrupt` {threadId, turnId} (L5904).
- `thread/compact/start` {threadId} (L5498).
- `thread/inject_items` {threadId, items} (L5523). Error -32601 means the call is unsupported.
- `thread/unsubscribe` (L5648).
- `thread/backgroundTerminals/list` and `/terminate` (L2046-2063).
- `thread/fork` {threadId, lastTurnId?, ...params} (L6226).
- Rollback (`provider/CodexThreadRevert.ts`): page through `thread/turns/list` {cursor, limit, sortDirection:"desc", itemsView:"summary"}, then `thread/revert` {threadId, beforeTurnId}. For zero turns it calls `thread/read` instead.
- `thread/read` {threadId, includeTurns}, `feedback/upload`.

**Notifications Codex sends, and what T3 emits for each:**
- `turn/started` registers the root turn context, or a subagent turn (L3882). `turn/completed` calls `finalizeCodexTurn`, which emits `provider_turn.updated` and `turn.terminal`. A completed status is reported as interrupted if T3 had requested an interrupt (L5317-5346).
- `item/agentMessage/delta` is coalesced into the assistant message's `message.updated` and `turn_item.updated` (L3711).
- `item/reasoning/summaryTextDelta` and `item/reasoning/textDelta` become reasoning items (L3725-3730).
- `item/plan/delta` becomes `node.updated`, `plan.updated` (proposed_plan) and `turn_item.updated`. `turn/plan/updated` produces the same three for a todo_list (L3732-3801).
- `item/started` and `item/completed` (L4035-4470), by item type:
  - commandExecution → command_execution
  - mcpToolCall / dynamicToolCall → dynamic_tool
  - fileChange → file_change
  - webSearch → web_search
  - plan → proposed_plan
  - contextCompaction → compaction
  - reasoning completes the reasoning item
  - userMessage → a subagent's user message
  - subAgentActivity / collabAgentToolCall → `subagent.updated` and subagent items
- `thread/tokenUsage/updated` → `provider_turn.updated` carrying tokenUsage (L3834).
- `thread/settings/updated` and `model/rerouted` → the subagent's model.
- `account/rateLimits/updated` → fills `resetAt` on usage-limit error items.
- `error`: with willRetry it becomes a retry error item; otherwise it is stored as the turn's failure (L3913).

**Requests Codex sends to T3** (approvals). Each becomes a node, a pending `runtime_request.updated` and an `approval_request` item, then waits for the decision:
- `item/commandExecution/requestApproval` (L4473) answers {decision}, with acceptAlways mapped to acceptForSession.
- `item/fileChange/requestApproval`, `item/permissions/requestApproval`, and the legacy `execCommandApproval` / `applyPatchApproval`.
- `mcpServer/elicitation/request` (L4658) becomes an mcp-elicitation approval.
- `item/tool/requestUserInput` (L4861) becomes a `user_input_request` and answers with answers.

**Capabilities** (L237-330): every flag is true, all identity strengths are strong, and enforcement is native.

### Claude (`ClaudeAdapterV2.ts`)

The adapter uses `@anthropic-ai/claude-agent-sdk` `query()` in streaming-input mode, wrapped by `ClaudeAgentSdkQueryRunner` (L311-373, L627). It keeps one live query per native session.

- **Opening a query** (options built at L862-905): model, tools, permissionMode, `includePartialMessages: true`, effort, `sessionId` or `resume`, `resumeSessionAt` (set after a rollback), allowed/disallowed tools, `canUseTool`, `allowDangerouslySkipPermissions`, thinking {type:"adaptive", display:"summarized"}, settings, `onUserDialog`, `pathToClaudeCodeExecutable`, env, mcpServers.
- **Permission mode** (L1440-1487):
  - Interaction mode plan → `plan`.
  - Otherwise by runtime mode: approval-required → `default`, auto-accept-edits → `acceptEdits`, auto → `auto`, full-access → `bypassPermissions`.
  - A read-only sandbox gives `dontAsk` with read-only tools.
  - The permission callback is installed only for approval-required and auto-accept-edits (L1511-1515).
- **Turn control:**
  - startTurn: open or reuse the query and `offer(SDKUserMessage)` (L7185-7202).
  - steer: `offer` with `priority:"now"` (L7367-7399).
  - interrupt: `query.interrupt` then `close`, wait 10s, then force-finalize as interrupted (L7279-7352).
  - Model and mode changes call `setModel` / `setPermissionMode`.
- **Approvals** (`canUseTool`, L6567-6800):
  - AskUserQuestion becomes a `user_input_request` (L6623).
  - ExitPlanMode is answered immediately and yields a proposed plan (L6689).
  - Any other tool becomes an approval. The request kind comes from `CLAUDE_KNOWN_TOOL_CLASSIFICATIONS` (L1592-1611): bash→command, edit/write/multiedit/notebookedit→file-change, read/grep/glob/ls→file-read, everything else→command.
  - The result is `permissionResultFromDecision` → allow {updatedInput, updatedPermissions?} or deny.
- **SDK messages → events** (`handleSdkMessageFrame`, L5414-6460):
  - `stream_event` message_start, content_block_start/delta/stop (thinking) → streamed reasoning items.
  - `assistant` → `message.updated` and `turn_item.updated` (assistant text), tool_use items per the classification above (command_execution, file_change, web_search, dynamic_tool), and `provider_turn.updated` usage.
  - `user` tool_result → completes the tool item; a subagent tool_result terminalizes the subagent.
  - `result` → `finalizeActiveTurn` → `turn.terminal`; `success` with `is_error` counts as failed (L6299-6352).
  - `system`: compact_boundary → compaction item; api_retry → retry item; model_refusal_fallback → system notice; task_started / task_progress / task_notification → background-task roster, subagent updates and wake notifications; init.
  - `rate_limit_event` → usage-limit `system_notice` and the reset time.
  - Subagent frames are routed by `parent_tool_use_id` to the child thread.
- **Rollback** (L7615-7688): `thread_start` allocates a new session id; otherwise it sets `nativeConversationHeadRef` to the `resumeSessionAt` message uuid.
- **Fork** (L7690-7746): SDK `forkSession(sessionId, {dir, upToMessageId})`.
- **Snapshots:** `readThreadSnapshot` is not implemented.
- **Capability differences from Codex** (L178): no multiple threads per session, no native turn id, no interrupt-restart steering, no tool-output streaming, no thread snapshot, no apply-patch approvals.

---

## 4. Client subscription and sync

- **Sequence.** The global sequence is `orchestration_events.sequence`. Thread subscriptions use a per-thread high-water mark (`latestAgentSequence(threadId)`); the shell uses the application-wide one (project events plus V2 thread events).
- **subscribeThread** (`ws.ts:742-932`):
  - Without `afterSequence`: send a snapshot frame (full or bounded, wire-projected), then `synchronized` if a completion marker was requested, then live events after `snapshotSequence`.
  - With `afterSequence`, it falls back to a snapshot when any of these hold:
    - `afterSequence` is above the high-water mark;
    - more than 128 events would be replayed;
    - the raw payload exceeds 1 MiB;
    - the gap contains a `thread.created` event and the thread still exists;
    - the encoded replay exceeds 1 MiB (`ThreadStream.ts:10-95`).
  - Otherwise it replays the persisted events up to the high-water mark, then sends `synchronized`, then live events.
  - The live stream subscribes to the PubSub before reading the high-water mark, so nothing is lost (`EventSink.ts:707-745`). Live tool updates are coalesced (`ThreadLiveEventCoalescer.ts`).
  - `WireProjection.ts` truncates large fields: strings over 32 KB, dynamic values over 16 KB. It sets `outputOmitted`, and clients fetch the full content with `getTurnItem`.
- **subscribeShell** (`ws.ts:934-1155`):
  - Snapshot built inside one SQL transaction, or a replay of application events when the gap is ≤1000 events and ≤8 MiB.
  - Events are batched (512 events or 50 ms), coalesced per thread, and each affected thread's shell is re-read. Output items are `thread.updated` / `thread.removed` / `project.updated` / `project.removed`.
  - Project repository-identity enrichment refreshes are merged into the stream.
- **Client side** (`client-runtime/src/state/threads.ts`):
  - Keeps `lastSequence` and drops items with `sequence <= applied` (L446-468). Unknown events still advance the cursor.
  - A snapshot resets the cursor to `snapshotSequence` (L584).
  - Resubscribes with `afterSequence` (L907-920). A cache keeps idle threads for 5 minutes (`docs/internals/connection-runtime.md:62-80`).
  - The reducer is `orchestrationV2Projection.ts:153`. It upserts by id and maintains `visibleTurnItems` sorted by (ordinal, id) using `@t3tools/shared/orchestrationV2Timeline` visibility rules.
- **Protocol gate.** The socket URL carries `orchestrationProtocol=2`. A mismatch is rejected with HTTP 426 (`docs/internals/legacy-orchestration-migration.md:33-42`).

---

## 5. Importing existing history

1. **Native Codex and Claude sessions.** This is the `agentSessions.scan` / `agentSessions.import` pair.
   - **Scanner** (`project/AgentSessionScanner.ts`):
     - Claude: `<claudeHome>/projects/**/*.jsonl` (L926-947). It uses `type` user/assistant records, skips `isSidechain`, and takes `cwd` from the records.
     - Codex: `<codexHome>/sessions/**/rollout-*.jsonl` (L978-1014). It uses `session_meta` (cwd), `turn_context` (model), `event_msg.user_message`, and `response_item` with role user or assistant, with dedupe (L287-494).
     - Limits: a 30-day window, the first user message plus the last 200 messages, 4 GiB per transcript, 100 transcripts per import.
   - **Importer** (`project/AgentSessionImporter.ts:175-397`). For each session it writes, through `eventSink.write` with no receipt:
     - `thread.created` with id `import:<instanceId>:<sessionId>`, `historyOrigin:"v1_import"`, settled;
     - a `message.updated` and `turn-item.updated` pair per message, as user_message or assistant_message with runId null;
     - a `provider-thread.updated` whose `nativeThreadRef` is the strong native session id, so the next turn resumes the native session (codex `thread/resume` or claude `resume`).
     - It also upserts `provider_session_runtime` with the resume cursor. Tools, approvals and plans are not imported.
2. **Legacy T3 V1 threads** (`orchestration-v2/legacy/LegacyV1ThreadImporter.ts`, 833 lines; doc `docs/internals/legacy-orchestration-migration.md`):
   - Shells are imported at startup. Transcripts are imported lazily from `projection_thread_messages` when a thread is first read (`threadManagement.ensureLegacyTranscript`, ws.ts:755).
   - The first continuation sends a 32,000-character legacy handoff.
3. **ACP registry sessions:** `server.importAcpRegistrySession` (`acpRegistry.ts`).
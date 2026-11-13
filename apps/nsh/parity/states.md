# Conversation screen states (T3 Code) and their parity coverage

Every visual state of T3 Code's conversation screen, walked from the chat
routes (`apps/web/src/routes/_chat.*.tsx`) through `components/ChatView.tsx`
and each component it renders, transitively (`components/chat/*`, the panels
and dialogs ChatView opens). Paths are relative to T3's `apps/web/src`. The
sidebar is compared with the GPUI client instead (last section).

Mac captures use the installed T3 app and the same Codex fixture. The default
comparison checks element presence; `--appearance` also checks visual metrics.
Each run records the exact binaries and T3 version in `manifest.json` and
`reference-manifest.json`. A mapped state can fail to
reach its target or fail to measure an element; consult `report.md` and
`summary.json`, never this inventory, for verification. iPhone and Windows
currently have no T3 comparison capture in this loop.

Status: **mapped** (`map.toml` state that attempts to compare it; not a passing result), **not yet** (with the
reason). Reasons recur:

- *fixture*: the Codex fixture (`crates/host-fixture/src/fixture/scenario.rs`)
  has no scenario tag that produces it; both clients render fixture turns
  only, so a tag has to be added first.
- *service*: T3 needs something the loop does not run for it (a GitHub or
  GitLab remote, a running dev server, a terminal's output, a paired device).
- *pointer*: it needs a pointer gesture the NSH driver does not post (drag,
  resize); `hover` is posted.
- *not mapped*: both clients can reach it; the map has no state for it yet.

## Route and page states

| State | Source | Status |
|---|---|---|
| New chat in a project (hero, project choice, start without a project) | `chat/DraftHeroHeadline.tsx`, `ChatView.tsx` | mapped: `draft` |
| New chat without a project | `routes/_chat.draft.$draftId.tsx` | not yet: not mapped |
| No active thread / no projects | `NoActiveThreadState.tsx`, `NoProjectsHero.tsx` | not yet: not mapped (NSH opens its new-chat hero instead) |
| Environment unavailable / disconnected (WifiOff state) | `ChatView.tsx` `EnvironmentUnavailableState` | not yet: needs the fixture Host stopped mid-run (both sides) |
| Thread loading, history loading and "load earlier" controls | `chat/MessagesTimeline.tsx` `TimelineHistoryControl`, `MessagesTimelineHistoryControls` | not yet: fixture `[gallery]` pages history, not mapped |
| Scroll to bottom button | `ChatView.tsx` (`showScrollToBottom`) | mapped: `turn-expanded` (`scroll-to-bottom`) |
| Workspace drop overlay | `chat/workspaceFileDrop.ts`, `data-chat-workspace-drop-overlay` | not yet: pointer (drag) |

## Header

| State | Source | Status |
|---|---|---|
| Breadcrumb: project / chat title | `chat/ChatHeader.tsx`, `WorkspaceBreadcrumb.tsx` | mapped: `turn-finished`, `turn-live`, `turn-thinking`, `draft` (`header-project`, `header-title`, `header-new-chat`) |
| Title actions menu (rename, fork, archive…) | `ChatHeader.tsx` thread actions | not yet: not mapped |
| Panel layout controls: details, terminal, right panel, maximize | `chat/PanelLayoutControls.tsx` | mapped: `turn-finished` (details, terminal and right panel toggles); maximize not yet: it shows only with the right panel open |
| Thread relationships (parent/child, agents) | `chat/ThreadRelationshipsControl.tsx` | not yet: not mapped (NSH has a `subagent` acceptance fixture) |

## Banners above the timeline and composer

| State | Source | Status |
|---|---|---|
| Thread error banner (dismissible) | `chat/ThreadErrorBanner.tsx` | mapped: `turn-failed` (`error-banner`) |
| Provider status banner (provider missing, signed out, outdated) | `chat/ProviderStatusBanner.tsx` | not yet: fixture (an account-less fixture Codex) |
| Usage limits and recovery banners | `chat/ComposerUsageLimits.tsx`, `chat/UsageLimitRecoveryBanner.tsx` | not yet: fixture (rate-limit numbers below any threshold) |
| Feedback banner | `chat/ComposerFeedback.tsx` | not yet: not mapped |
| Server update status | `chat/ComposerServerUpdateStatus.tsx` | not yet: service (needs a newer server version) |
| Plan follow-up banner | `chat/ComposerPlanFollowUpBanner.tsx` | not yet: fixture (no proposed plan) |
| Provider subagent bar | `chat/ProviderSubagentBar.tsx` | not yet: fixture |
| Queued runs control | `chat/QueuedRunsControl.tsx` | not yet: not mapped (fixture `queue` input exists; NSH `queued-input` scenario reaches it) |

## Timeline rows (`chat/MessagesTimeline.tsx`, `MessagesTimeline.logic.ts`)

| Row kind / state | Source | Status |
|---|---|---|
| `message` user: bubble, text | `UserTimelineRow`, `UserMessageBody` | mapped: `turn-finished`, `turn-thinking`, `turn-live`, `turn-failed`, `history-minimap` |
| user: hover-only actions (copy, edit from here, time) | `UserTimelineRow` | mapped: `turn-finished` (hidden), `turn-hover` (shown) |
| user: long message collapsed / expanded with fade | `CollapsibleUserMessageBody` | not yet: not mapped |
| user: attachments (images, files, video), context chips (file, terminal, PR, review comment, skill, mention), intent marker | `UserMessageContextChip`, `ChatImageAttachment`, `UserVideoAttachment`, `UserMessageIntentMarker` | not yet: images via `[images]` not mapped to the user row; chips need composer input |
| `message` assistant: Markdown text | `AssistantTimelineRow`, `ChatMarkdown.tsx` | mapped: `turn-finished`, `markdown-document` |
| assistant: copy, fork, time | `AssistantCopyButton`, `AssistantForkButton`, `AssistantMessageMeta` | mapped: `turn-finished`, `markdown-document` |
| assistant Markdown: headings, emphasis, quote, ordered and task lists | `ChatMarkdown.tsx` | covered as one block: `markdown-document` (`markdown-body`; NSH draws one text view) |
| assistant Markdown: code block with language, wrap and copy | `ChatMarkdown.tsx`, `HighlightedCodeLines.tsx` | mapped: `markdown-document` |
| assistant Markdown: table | `ChatMarkdown.tsx` | mapped: `markdown-table` |
| assistant Markdown: images (loaded, unavailable) | `ChatMarkdownAssetImage` | mapped: `images`, `markdown-document` (`markdown-image-error`) |
| assistant Markdown: links, citations, mermaid | `AssistantCitationChip.tsx`, `MermaidDiagram.tsx` | not yet: not mapped (NSH has Mermaid fixtures; citations still need a shared fixture) |
| assistant selection toolbar | `AssistantSelectionToolbar.tsx` | not yet: pointer (text selection by drag; NSH `select` is accessibility-only) |
| assistant changed files section | `AssistantChangedFilesSection`, `ChangedFilesTree.tsx` | mapped: `changed-files`, `turn-finished` |
| `assistant-meta` row | `AssistantMetaTimelineRow` | mapped: `changed-files` |
| `working` header (Working for …, timer) | `WorkingTimelineRow`, `WorkingTimer` | mapped: `turn-thinking`, `turn-live`, `approval`, `question`, `turn-retry` |
| `thinking` row with its moving highlight | `ThinkingTimelineRow` | mapped: `turn-thinking` |
| `work-live` row with its shimmer (running tool, input requested) | `LiveWorkEntryTimelineRow`, `LiveActivityRow` | mapped: `turn-live`, `approval`, `question` |
| `turn-fold` (Worked for …) folded and expanded | `TurnFoldTimelineRow` | mapped: `turn-finished`, `turn-expanded` |
| `work-toggle` group (Ran N commands) folded and expanded | `WorkGroupToggleTimelineRow`, `WorkGroupHeader` | mapped: `turn-expanded`, `turn-live` |
| `work` rows: command | `SimpleWorkEntryRow`, `WorkLog.tsx`, `ToolActivityIcon` | mapped: `turn-expanded`, `turn-failed` |
| `work` rows: file change, MCP, dynamic tool, agent tool, web search, plan, review mode, compaction | `SimpleWorkEntryRow`, `WorkLog.tsx`, `ToolActivityIcon` | not yet: `turn-items` is mapped, but T3 fails the fixture's `[items]` turn ("Provider turn failed."): the fixture sends item kinds T3's Codex adapter rejects (`sleep`, `subAgentActivity`) |
| viewed screenshots | T3's Codex adapter omits `imageView`; BEX displays it as a conversation image | intentional difference; core tests cover live and reopened turns |
| `work` row expanded output (shell command block, fetched output, file diff) | `ShellCommandBlock.tsx`, `FetchedToolOutput`, `FileDiff` | not yet: not mapped |
| `event` rows: provider error, run interrupted, retrying | `V2EventTimelineRow`, `TimelineSystemDivider.tsx` | mapped: `turn-failed`, `turn-interrupted`, `turn-retry` |
| `event` rows: handoff, fork, compaction, todo list | `V2EventTimelineRow` | not yet: fixture |
| `attempt-fold` (superseded attempts) | `AttemptFoldTimelineRow` | not yet: fixture |
| `context-compaction` row (live compacting label) | `ContextCompactionTimelineRow` | not yet: fixture (`[items]` sends a completed compaction only) |
| `proposed-plan` card | `ProposedPlanCard.tsx` | not yet: fixture (plan mode) |
| `html-render` frame | `HtmlRenderFrame.tsx` | not yet: fixture `[visualize]` exists, not mapped |
| `worktree-setup` card | `WorktreeSetupCard.tsx` | not yet: fixture |
| subagent rows and avatars | `SubagentAvatar`, `SubagentElapsed` | not yet: not mapped (`subagent` acceptance fixture exists on NSH) |
| secret request card | `SecretRequestCard.tsx` | not yet: fixture |
| question answer history | `QuestionAnswerHistory` | not yet: not mapped (after answering `[request]`) |

## Minimap and gutter

| State | Source | Status |
|---|---|---|
| Hidden below two user turns | `TIMELINE_MINIMAP_MIN_ITEMS` | mapped: `turn-finished` (absent on both) |
| Persistent side gutter with strips | `TimelineMinimap`, `resolveTimelineMinimapHasPersistentGutter` | mapped: `history-minimap` |
| Hover-only on a narrow window; hover preview; previous/next buttons | `TimelineMinimap`, `TimelineMinimapNavigationButton` | not yet: needs a second viewport width and pointer moves over the gutter |
| Timeline side gutter (content column inset) | `chat-content-lane`, `--chat-content-max-width` | mapped: `history-minimap` (`timeline-gutter-text`), every state's column position |

## Composer (`chat/ChatComposer.tsx`, `chat/ComposerSurface.tsx`)

| State | Source | Status |
|---|---|---|
| Resting: surface, placeholder, model, attach, send | `ComposerFooterModeControls`, `ComposerPrimaryActions.tsx` | mapped: `draft`, `turn-finished` |
| Draft typed, grown to three lines | `useComposerMultilinePrompt.ts` | mapped: `composer-draft` |
| Running: stop button | `ComposerPrimaryActions.tsx` | mapped: `turn-thinking`, `turn-live` |
| Context strip: environment, branch, worktree mode | `ComposerSurface.ContextStrip`, `BranchToolbar*.tsx` | not compared: NSH leaves it out by product decision; the project comes from the draft hero and the sidebar, the Host from the connection list |
| Model picker open (providers, models, search, reasoning, speed, permissions, account) | `ProviderModelPicker.tsx`, `ModelPickerContent.tsx`, `ModelPickerAccount.tsx` | mapped: `model-picker` |
| Permissions menu open inside the model picker | `ProviderModelPicker.tsx` | not yet: menu choices are exercised before capture, but the open menu is not mapped |
| Compact controls menu (narrow composer) | `CompactComposerControlsMenu.tsx` | not yet: needs a narrower viewport |
| Slash command, @mention and $skill menus | `ComposerCommandMenu.tsx` | not yet: not mapped |
| Attachments: image thumbnails, files, video, upload failure | `ComposerImageThumbnail.tsx`, `ComposerFileAttachment` | not yet: not mapped (NSH `attachments` scenario attaches through the open panel) |
| Pending approval in its work row and its actions | `TimelinePendingApprovalRow.tsx`, `ComposerPendingApprovalActions.tsx` | mapped: `approval` |
| Pending question panel, options, custom answer, skip | `ComposerPendingUserInputPanel.tsx` | mapped: `question` |
| Terminal contexts attached | `ComposerPendingTerminalContexts.tsx` | not yet: service (terminal) |
| Stash badge and menu, tasks badge and drawer, context window meter, prompt length warning | `ComposerStashBadge.tsx`, `ComposerTasksBadge.tsx`, `ContextWindowMeter.tsx`, `ComposerPromptLengthValidation.tsx` | not yet: not mapped |
| Activity status row | `ComposerActivityStatus.tsx` | not yet: not mapped |
| Dictation | NSH only (`Transcribe audio with Codex`) | mapped: `draft` (reported as NSH-only) |

## Panels and dialogs opened from the conversation

| State | Source | Status |
|---|---|---|
| Thread details card: inline and popover | `chat/ThreadDetailsPanel.tsx`, `ThreadDetailsCard.tsx` | mapped: `details-inline`, `details-popover` (surface, editor and changes row); NSH retains sources; project scripts, Git actions and thread relationships remain absent |
| Diff / review panel | `DiffPanel.tsx`, `DiffPanelShell.tsx` | not yet: not mapped (NSH `review-panel` scenario reaches it) |
| File preview and file browser | `files/FilePreviewPanel.tsx`, `files/FileBrowserPanel.tsx` | not yet: not mapped |
| Browser preview panel and mini player | `preview/PreviewPanel.tsx`, `preview/ThreadPreviewMiniPlayer.tsx` | not yet: service (a page to load) |
| Terminal drawer | `ThreadTerminalDrawer.tsx` | not yet: service (the terminal's screen is not in NSH's accessibility tree) |
| Pull request panels and dialogs | `pullRequest/*` | not yet: service (a Git host) |
| Device panel and setup wizard | `device/*` | not yet: service (a paired device) |
| Right panel tabs, sheet, maximize | `RightPanelTabs.tsx`, `RightPanelSheet.tsx` | not yet: not mapped |
| Expanded image dialog | `chat/ExpandedImageDialog.tsx` | not yet: not mapped |
| Confirm dialogs (delete, discard) | `ConfirmDialogHost.tsx` | not yet: not mapped |
| Automations panel | `chat/ThreadAutomationsPanel.tsx` | not yet: service (scheduled tasks) |

## Sidebar header (compared with T3 Code)

| State | T3 source | Status |
|---|---|---|
| Search field and icon, project filter, add project and new thread buttons | `sidebar/SidebarThreadHeader.tsx` | mapped: `sidebar-header` |

## Sidebar (compared with the GPUI client, `b539d6069^:apps/desktop`)

| State | GPUI source | Status |
|---|---|---|
| Width | `app/view.rs:426` | mapped: `sidebar` |
| Settings button | `sidebar.rs:778` | mapped: `sidebar` (presence) |
| Project row height and icon | `sidebar.rs:176-186, 268` | mapped: `sidebar` |
| Chat rows (active spinner, unread dot, worktree badges), section headers, "show more", errors | `sidebar.rs:88-130, 880-950` | not yet: not mapped |
| Connection menu | `sidebar.rs:808` | not yet: NSH has no connection menu above the search field |

# Desktop conversation display contract

Originally audited against the locally installed Codex Desktop bundle in ChatGPT
`26.818.61809` (`7019`) and the `codex app-server` v2 JSON schema shipped in
that bundle on 2026-08-25. The turn lifecycle and selection contracts below
record Bex's current product requirements, updated on 2026-09-11; the original
reference app's behavior does not override them. It is not a second wire protocol.

Claude Code conversations use the same lifecycle, activity, draft and error
contracts through the Host adapter. Streamed and completed blocks replace by
stable block ID, so thinking cannot overwrite text and final text is not
duplicated. Approvals and questions remain actionable after reconnect. Failed
submission retains the draft; retry clears the previous submission notice.
Switching between Claude and Codex requires a new conversation. Claude does not
yet support active-turn steering or fork-based side chats; those requests fail
explicitly instead of creating a Codex conversation or dropping the draft.

## Turn lifecycle

| Input state | Expanded work | Header / divider | Transition |
| --- | --- | --- | --- |
| No work item yet | n/a | Thinking | Replaced as the first renderable work item arrives |
| `inProgress` | collapsed by default | Activity summary | Commentary stays visible; each work group opens only on explicit action |
| Waiting on approval | collapsed by default | Awaiting approval | The pending request stays visible and actionable outside work |
| Waiting on user input or MCP elicitation | collapsed by default | Waiting for your answer | The request stays outside a hidden collapsed body |
| Final assistant output starts | collapsed by default | Past-message count | The answer stays visible outside work |
| `completed` without final text | collapsed when work exists | Worked for _duration_ | Generated output and end resources remain visible |
| `interrupted` by this client | collapsed by default | You stopped after _duration_ | Partial work can be explicitly opened |
| `failed` | collapsed by default | Error-specific presentation | The error remains visible outside work |
| History reopened | collapsed by default | Same terminal divider | Hydrating server items must not auto-expand work |

Manual expansion applies to the selected group while its status is unchanged;
it must not expand adjacent groups. A status transition resets expansion to the
collapsed default. Activity wording, counts, streaming updates, and history
hydration must not change this default.

## History pagination and refresh

- Load the latest bounded page first; request older pages using the server's
  opaque cursor. A refresh must not fetch the entire conversation.
- Retain cached history only when its suffix matches the refreshed page's
  prefix in order. Apply the same rule to turns and to items within a turn,
  preserving repeated occurrences from the server page. A shared ID elsewhere
  is not enough; repeated boundary IDs cannot establish which cached occurrence
  the page continues, so they must not retain a cached prefix.
- When the windows do not overlap, replace the displayed window with the new
  page and its cursor. Previously cached messages remain on the server and
  can be loaded again; never present disconnected windows as complete history.
- Keep the cursor belonging to the oldest retained boundary. A late older-page
  reply for a replaced cursor must not alter the new window.
- `notLoaded` means the Host omitted item hydration, not that the turn is empty.
  Retain cached items and their deferred-detail markers in that case. If no
  items are cached, preserve the server's additional-loading state.
- Failed additional loading preserves the window and cursor. Retrying clears
  the error and successful loading restores every message in order. Reopening
  must retain fetched history when its boundary still overlaps.

Acceptance: `refresh_keeps_only_contiguous_history_and_its_cursor`,
`refresh_gap_recovers_missing_history_through_store_after_retry`, and
`refreshed_history_pages_recover_every_turn_and_item_through_store` cover the
window rules, RPC failure/recovery, and bounded Host hydration through complete
retrieval and reopening.

## Selection and copying

- Mac text selection exposes **チャットに追加**, **詳細を表示**, and
  **サイドチャットで質問**. Right-click exposes **コピー** and **Googleで検索**
  for only the selected text. Adding a quote preserves the existing draft and
  persists it through the Store. **詳細を表示** creates a new side conversation
  and immediately asks the AI to explain the selection; it does not merely
  display the selected text in a dialog. Existing side-chat drafts remain saved.
  Selection must work in the real virtual list,
  including mouse-move and release before the next paint.
- iPhone assistant text uses selection handles in the conversation. Copy and
  chat actions operate on the selected range. Own-message long press is a
  separate menu for complete copying; it must not replace assistant selection.
- Side-chat submission must retain the selected input, complete the turn, clear
  the sent draft, and persist the conversation. Closing the iPhone sheet restores
  the original conversation and draft, including after a preparation retry.

## Regression history and required checks

The September 11 investigation found two different failures:

- Commit `3a38181` changed `initially_expanded: false` to `!completed` while
  revising activity presentation, and added a Rust assertion expecting live
  expansion. The existing Simulator assertion that live commands start closed
  was later changed to expect expansion in `53a3d41`. The old lifecycle table
  above also contradicted the closed default. Local post-commit quality ran
  lint checks, not these behavioral tests. Updating assertions to match the
  changed implementation masked the product regression.
- The prior Mac selection toolbar and iPhone `UITextView` selection/own-message
  copy changes remained uncommitted in worktree `bex/session-yDLr3l` based on
  `dcc6799`. They were absent from current branch `997b22c` and all available
  committed/reflog snapshots. This was missing integration, not a deletion
  identified in a later commit. The old worktree was left intact.

Enabling the Simulator gate in the immutable quality checkout also exposed
hard-coded `target/debug` and iOS library paths in the build scripts. The worker
uses a shared `CARGO_TARGET_DIR`; binding generation, Swift compilation, and
Xcode now consume that same configured directory. Generated binding sources
remain local to each checkout so Gradle and Xcode keep their existing inputs.

The reopened-history gate also exposed a fixture mismatch: the completed live
command emitted `passed`, but history rewrote the same item to a different full
body. The Store correctly retained the already loaded body. The fixture now
returns identical completed content in live and persisted reads; the Simulator
case checks both cached reopening and an unseen persisted conversation that
must fetch deferred details. The full-body assertion remains required.

`just quality rust` runs the core library and desktop tests as well as lint.
`just quality swift` runs `just conversation-ui`, which exercises selection,
copying, side-chat completion/recovery, and live/failed/interrupted/reopened work
through an isolated Host and Simulator. Missing, failed, or skipped cases fail
the check. A successful build or selectable flag alone does not verify these
contracts. A completed feature must be tied to a commit in its integration
target; worktree-local behavior is not proof that another branch contains it.

## Server `ThreadItem` coverage

All current app-server items must survive parsing, caching, live replacement,
and history reload:

- `userMessage`: text plus image, local image, audio, local audio, skill, and
  mention inputs. Empty text does not hide attachments.
- `hookPrompt`: rendered as hook feedback/user context, not an assistant answer.
- `agentMessage`: `commentary` belongs to work activity; `final_answer` is the
  response. A missing phase uses the last renderable assistant message as the
  compatibility final response.
- `plan`: a proposed plan, separate from ordinary activity and collapsed after
  completion.
- `reasoning`: retained in the source data but hidden from conversation rows.
- `commandExecution`: in-progress, completed, failed, and declined states;
  output, exit code, cwd, duration, and parsed action type remain inspectable.
- `fileChange`: in-progress, completed, failed, and declined states; add,
  update/move, and delete changes remain inspectable.
- `mcpToolCall`: in-progress, completed, and failed states; app widgets and
  special renderers may be standalone or persistent while collapsed.
- `dynamicToolCall`: in-progress, completed, and failed states. Renderer policy
  may make it standalone, summary-only, persistent, or continuous across calls.
- `collabAgentToolCall`: spawn/send-input/resume/close state plus target agent
  pending, running, interrupted, completed, errored, shutdown, and not-found
  states.
- `subAgentActivity`: started, interacted/updated, and interrupted activity;
  adjacent activity for an agent is grouped.
- `webSearch`: grouped when the query is non-empty; search, open-page, and
  find-in-page actions are distinguishable.
- `imageView`: standalone, with adjacent views summarized as an image count.
- `sleep`: progress-only activity; not rendered as a historical standalone row.
- `imageGeneration`: pending placeholder, generated output, and
  `usageLimitExceeded` failure. Mac and iPhone keep these rows outside collapsed
  work. Render `savedPath` on the selected Host, or decode the base64 `result`
  when no saved path is present; history detail deferral must preserve both.
- `enteredReviewMode` / `exitedReviewMode`: state transitions, not ordinary
  history rows.
- `contextCompaction`: manual/automatic in-progress and completed divider.

Unknown future items remain cached and receive a safe fallback row; they are
never dropped solely because the mobile build does not know their type.

Markdown HTTP/HTTPS links open in the system browser. File links resolve on the
selected Host, including relative paths, URL-escaped spaces and line suffixes.
Mac opens images in its image viewer and other files in the Files panel with
the parent directory and editor, preserving unsaved file drafts and revision checks.
Side Chat uses its own Files/Diff panel and returns to the same conversation draft.
iPhone uses a
temporary download and a Quick Look sheet with an explicit close action;
closing the sheet removes the temporary copy.

## Requests and approvals

Pending server requests are part of the live conversation state, even though
they are not persisted as ordinary `ThreadItem` values:

- command execution approval;
- file-change approval;
- permission-profile approval;
- `request_user_input` questions and answers;
- MCP server elicitation, including form, URL action, connector auth, tool
  suggestion/plugin installation, and MCP tool permission;
- dynamic tool call requests.

`serverRequest/resolved` removes the pending request but retains its completed
summary when the corresponding response item is available. Guardian automatic
review has `inProgress`, `approved`, `denied`, `timedOut`, and `aborted` states.
Approved review rows disappear; in-progress reviews are grouped; denied,
timed-out, and aborted reviews remain standalone. Repeated denials may emit a
standalone "turn ended by Auto-review" warning.

## Errors and exceptional states

The app-server error families are:

- context window exceeded;
- session budget exceeded;
- usage limit exceeded;
- server overloaded;
- cyber-policy and misalignment-policy failures;
- internal server error;
- unauthorized and bad request;
- thread rollback failure;
- sandbox error;
- HTTP connection failure;
- response-stream connection failure or mid-stream disconnection;
- too many failed response attempts;
- active turn not steerable;
- other/unknown.

An `error` notification with `willRetry=true` is a `stream-error`: it remains in
the activity with reconnect attempt/max-attempt progress and optional details.
HTTP 429 or server overload uses the busy/reconnecting presentation. A final
non-retrying error is a standalone `system-error`. Interrupted turns and
transient connection/server failures are retryable; quota/rate-limit wording is
not treated as a generic retryable outage. Image-generation usage limits use a
special output failure instead of a generic turn error.

Thread-level exceptional state also includes not-loaded, idle, system-error,
active/waiting-on-approval, active/waiting-on-user-input, remote connection
unavailable, archived, active in another writer, resume/config failure,
worktree missing/cleaned up, queue changes, and realtime close/error.

## Grouping and collapse policy

Desktop first separates a turn into user items, agent activity, assistant final
item, automation updates, post-assistant items, plan, todo, diff, approval,
input request, MCP elicitation, permission request, model/personality/fork
events, remote task links, subagent groups, and a trailing system error.

Within activity:

- command, patch, MCP, web search, dynamic tool, reasoning, and in-progress
  automatic review are normally groupable;
- assistant messages, stream/system errors, image views, context compaction,
  multi-agent actions, subagent activity, user-input responses, worktree init,
  and failed automatic reviews are standalone;
- empty web searches, completed approved automatic reviews, and display-only
  state items are omitted from the generic group because they render elsewhere;
- a steering user message/hook feedback, eligible dynamic-tool renderer, and
  expanded MCP app can remain visible while the rest of completed work is
  collapsed;
- a lone context-compaction event does not get a redundant outer collapse row;
- completed activity groups use a bounded scrolling detail body rather than
  eagerly expanding an unbounded transcript.

The mobile implementation should derive this projection from state on every
snapshot/live update. UI components must not independently guess item order,
final-message identity, terminal state, or collapse eligibility.

## Shared native layout

`RenderedTurn` stores one flat layout, shared by Desktop and the mobile
`conversation_rows` getter, with core-owned row order and identities,
including partial-history boundaries, activity membership, pending requests,
errors, and the last response eligible for a fork. Activity rows follow their
header consecutively; clients filter them using that header's expansion state.
Native clients cache rows while the rendered turn is unchanged. `activity_is_expanded` applies a
user choice only while its status matches the current activity status; otherwise
it uses the core default. Filtering activity rows must leave requests and errors
visible. Only visible items receive rendered data; cached rows retain those
items across updates. iOS keeps parsed Markdown beside each cached message row.

Desktop file-change headers and patches, and the mobile expanded text, read
`presentation::body::file_changes`. The wire model stores `changes` as a named
field, so it must never be looked up in `Item::extra`. Deferred items retain
file headers while their diff bodies are fetched separately.

When a provider is unavailable, the model menu displays the remaining catalog and the provider error. An existing draft keeps its saved model and settings until the user changes them; a new draft selects an available default. iOS exposes the model catalog without requiring a Codex account. Codex exit fails its active turn but leaves the Host connection and Claude approvals/conversations usable.

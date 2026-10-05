# Native T3 conversation display contract

The product requirement is T3 Code commit
`4ee6bfd50ef4a089440d5c3662db2298da9cc50e`. Colors, dimensions and interactions
follow [T3_UI_SPEC.md](t3-port/T3_UI_SPEC.md). This replaces the retired Bex
conversation requirements as authorized by [PLAN.md](t3-port/PLAN.md).

## Common behavior

Clients render agent-core shelves, timeline, requests and composer capabilities.
Host orchestration determines queue, steer, restart and stop. Native widgets do
not infer execution from text or model names. Five shelves use T3 ordering:
Pinned, Active, Working, Snoozed, Settled. Settled shows 10 then 25 more. Search,
project filtering, archived threads, pin ordering, visit/unread, rename, settle,
snooze and delete are available. Rows show branch/worktree/run/waiting/unread.

User messages use muted bubbles. Assistant Markdown is native selectable text
with links, tables and copyable code. Plans have cards; consecutive work items
form disclosure groups. Request cards preserve warnings and choices. Core
validates structured answers; custom input takes precedence. Only message
response requests offer Dismiss.

Normal send queues during an active run. Steer, restart and stop are explicit.
Queue UI offers held resume, edit, cancel, reorder and promote-to-steer. Composer
selects model, effort, tier, interaction/runtime mode and defaults. Late receipts
preserve newer typing/navigation. History prepend preserves visible anchors;
streaming follows only when already near the bottom.

## Desktop

GPUI uses a 256 px sidebar, 52 px header, chat column up to 736 px and 540 px
right tools. Pin drag uses shared fractional keys. Enter sends, Shift+Enter adds
a newline, Alt+Enter steers. Diff, terminal, files and browser remain native.
The Diff panel selects workspace changes, individual completed turns or all
available turns. Shared core supplies ready checkpoint choices and file rows;
late results cannot overwrite a different thread or turn-range selection.

## Mobile

SwiftUI/Compose use native list/navigation, conversation scrolling, growing
composers, queue sheets, request cards and settings. DM Sans Regular/Medium/Bold
is bundled with OFL. QR pairing, keys, dictation and workspace tools use platform
services. Transcription appends to the original draft, preserving new typing.

## Verification

Changed-crate unit/property tests verify common behavior. GPUI, UniFFI, iOS and
both Android ABIs must compile. This port runs no CI, live-provider E2E or
Simulator UI tests. Build success does not claim pixel or device acceptance.

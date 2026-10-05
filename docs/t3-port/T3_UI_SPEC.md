# T3 Code UI spec for native clients (desktop GPUI, iOS SwiftUI, Android Compose)

This is based on commit 4ee6bfd in `/tmp/t3code-ref`. Three path prefixes are used throughout:
- `WEB` = `/tmp/t3code-ref/apps/web/src`
- `MOB` = `/tmp/t3code-ref/apps/mobile/src`
- `PKG` = `/tmp/t3code-ref/packages`

Web numbers assume a 16px root, where 1 Tailwind spacing unit is 4px. Mobile is different: Uniwind sets the rem to **14px** (`/tmp/t3code-ref/apps/mobile/metro.config.js:127`), so a spacing unit there is 3.5px. Mobile text sizes are written in px, so they are not affected.

Things to know before reading the details:
- The default web theme is the stock `:root` set in `index.css`. Built-in themes (t3-chat, grove, ocean, ember, iris) only apply when `data-theme-id` is set.
- The sidebar has two implementations. The current one groups threads into status shelves and filters by project with a picker. The older per-project tree is behind `legacySidebarEnabled`, which is off by default.
- The send-while-running default is **queue**, not steer.
- Mobile has no rollback / "Edit from here" UI.

---

## 1. Theme tokens

### 1.1 Ready-to-use hex palette (best source)

`PKG/shared/src/themePalettes.ts` holds the stock look already flattened to opaque hex:
- Light: lines 131-189 (`T3_CODE_LIGHT_THEME_COLORS`)
- Dark: lines 191-249 (`T3_CODE_DARK_THEME_COLORS`)

The comment at 124-130 says these were captured from `index.css`. The role list is at lines 47-104.

| Role | Light | Dark |
|---|---|---|
| canvas / chrome / toolbar | #fcfcfc | #0a0a0a |
| surface (card) / surfaceOverlay (popover) | #ffffff | #111111 |
| surfaceRaised | #fcfcfc | #111111 |
| text | #27272a | #f5f5f5 |
| textMuted / mutedForeground / placeholder / secondaryLabel / iconMuted | #71717b | #818181 |
| border / toolbarBorder | #e4e4e7 | #191919 |
| input | #d4d4d8 | #1e1e1e |
| accent / focus / messageAction / update | #1b4ed8 | #346bf1 |
| messageActionHover | #3160db | #3061d9 |
| secondary / muted | #fafafa | #111111 |
| accentSurface / messageSurface (user bubble) / toolbarControlHover | #f4f4f5 | #141414 |
| accentSurfaceForeground | #18181b | #f5f5f5 |
| error / errorForeground / errorSurface | #fb2c36 / #c10007 / #fcebec | #fb414a / #ff6467 / #301214 |
| warning / warningForeground / warningSurface | #fe9a00 / #bb4d00 / #fcf4e8 | #fe9a00 / #ffb900 / #312108 |
| updateForeground / updateSurface | #1b4ed8 / #e0e6f7 | #51a2ff / #121b34 |
| codeBackground | #ffffff | #111111 |
| sidebar | #fafafa | #000000 |
| sidebarForeground / sidebarMutedForeground | #27272a / #71717b | #f1f3f7 / #a3a3a3 |
| sidebarRowHover / Active / Selected | #fcfcfc / #ffffff / #ffffff | #131313 / #1a1b1b / #111111 |
| sidebarBorder / sidebarControlSurface | #e4e4e7 / #f4f4f5 | #141414 / #0a0a0a |
| terminal bg / fg / cursor / selection | #fcfcfc / #27272a / #26384e / #d0d6dd | #0a0a0a / #f5f5f5 / #b4cbff / #343a47 |
| terminalScrollbar / hover | #d6d6d6 / #bdbdbd | #222222 / #363636 |

### 1.2 Source CSS variables (web)

All in `WEB/index.css`.

**Light `:root`** (1062-1130):
- `--radius: 0.625rem`
- `--background: zinc-25`, defined as `oklch(99.2% 0 0)` at 215
- `--foreground: zinc-800`
- `--card` and `--popover`: white
- `--primary: oklch(0.488 0.217 264)`
- `--secondary` and `--muted`: zinc-50
- `--muted-foreground: zinc-500`
- `--accent: zinc-100`; user message surface = accent (1088)
- `--border: zinc-200`, `--input: zinc-300`
- error red-500/700, info blue-500/700, success emerald-500/700, warning amber-500/700
- `--code-background` = card 90% mixed with background
- terminal cursor `rgb(38 56 78)`, selection `rgb(37 63 99 / 20%)`

**Dark** (1132-1170):
- `--background: neutral-950`
- card, popover, surface-raised = background 97% mixed with white
- `--foreground: neutral-100`
- `--primary: oklch(0.571 0.21 264)`
- secondary and muted = white 3%; accent = white 4%
- border = white 6%; input = white 8%
- `--tool-error-icon: #fca5a5`
- terminal cursor `rgb(180 203 255)`, selection 25%

**Sidebar overrides** (`[data-app-sidebar]`, 1176-1218). The dark sidebar is pure black:
- background `#000`, foreground `#f1f3f7`, accent `#191a1d`, muted `#0a0a0a`, muted-foreground `#a3a3a3`
- border `rgb(255 255 255/8%)`
- row hover / active / selected = foreground at 8% / 11% / 7%

**Diff colors** (2024-2029):
- addition = success, deletion = destructive
- foregrounds: emerald-600 / emerald-400 and red-600 / red-400
- Optional blue-orange scheme at 2041-2054

**Accents and effects:**
- Status hues: sidebar v1 pill colors are in `WEB/components/Sidebar.logic.ts:1158-1235`; v2 colors are in section 2.2 below.
- Glass:
  - Light: `--glass-blur: 12px`, `--glass-opacity: 80%`, saturation 1.14
  - Dark: blur 16px, saturation 1.08
  - Defined at 116-118 and 142-148
  - `surface-glass` and `dialog-glass` utilities at 334-393
- Composer shadow (193-194):
  - Light: `0 12px 28px -18px rgb(0 0 0/40%)`
  - Dark: `0 14px 32px -18px rgb(0 0 0/75%)`
- Grain texture overlay at 0.035 opacity (1658-1675). Optional for native.

### 1.3 Fonts and sizes (web)

**Font families:**
- Sans: `-apple-system, BlinkMacSystemFont, "Segoe UI", system-ui, sans-serif` (`index.css:166-170`)
- Mono: `"SF Mono", "SFMono-Regular", Menlo, Consolas, "Liberation Mono", monospace` (`WEB/appearanceFonts.ts:20-26`)

**Default sizes** (`PKG/contracts/src/settings.ts:117-150`):

| Setting | Default | Range |
|---|---|---|
| Interface (root) | 16px | 12-20 |
| Prompt | 14px | 12-20 |
| Code | 13px | 10-18 |
| Terminal | 12px | 8-20 |

**Extra text steps** (`index.css:178-185`):
- 2xs = 11px, line-height 16px
- 3xs = 10px, line-height 14px
- 4xs = 8px
- 5xs = 7px

Standard Tailwind steps also apply: xs 12/16, sm 14/20, base 16/24, lg 18/28, 2xl 24/32, 3xl 30/36.

**Radii** (`index.css:277-282`, based on `--radius` 10px):

| Token | Value |
|---|---|
| sm | 6px |
| md | 8px |
| lg | 10px |
| xl | 14px |
| 2xl | 18px |
| 3xl | 22px |
| `--control-radius` | 8px (101) |

**Layout constants** (`index.css`):
- `--workspace-topbar-height: 52px` (120)
- `--chat-content-max-width: 46rem` = 736px (128). Wide is 72rem; full is 100% (2033-2039).
- `--thread-details-panel-width: 17.5rem` (129)
- Gutter: 12px, or 20px at `sm` (134-140)
- Scrollbar: 6px wide (90)

**Chat markdown** (`index.css:1745-2022`):
- Base text: `text-sm leading-relaxed` (14px, line-height about 22.75px). Color is foreground at 80% (`WEB/components/ChatMarkdown.tsx:3515`).
- Block margins: 0.65rem.
- Headings: h1 20px, h2 18px, h3 16px, h4-h6 14px. Weight 600, line-height 1.3, margin 1.25rem top and 0.5rem bottom.
- Inline code: 12px, 1px border, 6px radius, muted background, padding 0.1rem 0.35rem.
- `pre`: 12px radius, padding 0.8rem 0.9rem.
- Fenced code block: `rounded-lg border-border/70 bg-secondary`, dark `bg-input/32`. Header row is `pl-3 pt-1.5` (`ChatMarkdown.tsx:1085-1089`).
- Links: info-foreground color, dotted underline on hover.
- Tables: 12px text; cells padded 0.45rem by 0.75rem.

### 1.4 Mobile tokens

**Fonts** (`/tmp/t3code-ref/apps/mobile/global.css:15-19`):
- DM Sans: `DMSans-Regular`, `DMSans-Medium`, `DMSans-Bold` (assets listed in `/tmp/t3code-ref/apps/mobile/app.config.ts:118-120`)
- Mono: Menlo on iOS, `monospace` on Android (`MOB/features/threads/thread-list-v2-row-appearance.ts:4` and its `.android.ts` sibling)

**Type scale** (`MOB/lib/typography.ts:1-11`, mirrored in `global.css:21-40`):

| Role | Size / line-height | Tailwind class |
|---|---|---|
| micro | 11/14 | 3xs |
| caption | 12/16 | 2xs |
| label | 13/17 | xs |
| footnote | 14/19 | sm |
| body | 16/23 | base |
| headline | 18/23 | lg |
| title | 21/28 | xl |
| largeTitle | 26/32 | 2xl |
| display | 30/36 | 3xl |

- The base font size is user-adjustable from 11 to 22 (`MOB/lib/appearancePreferences.ts:10-13`).
- Markdown sizes (`appearancePreferences.ts:154-172`): body 16/23, h1 21, h2 19, h3 17, h4-h6 15, code 13 with line-height 19.

**Palette** (`/tmp/t3code-ref/apps/mobile/generated-uniwind-default-theme-variables.json`, light at lines 2-90, dark at 91-180). Same hues as web, plus mobile-specific roles:

| Role | Light | Dark |
|---|---|---|
| user-bubble | #efeff1 | #161616 |
| composer-surface | rgba(244,244,245,.94) | rgba(26,27,27,.9) |
| composer-border | rgba(228,228,231,.8) | rgba(25,25,25,.8) |
| grouped-card | #f4f4f5 | #1a1b1b |
| drawer | #fafafa | #000 |
| thread-selected | #fff | #1a1b1b |
| primary-text | #1b4ed8 | #4b7cf3 |
| md-link | #1b4ed8 | #3b70f1 |
| danger / danger-foreground | #fcebec / #c10007 | #301214 / #ff6467 |
| backdrop | rgba(0,0,0,.22) | rgba(0,0,0,.48) |

- Platform tweaks are in `MOB/lib/mobileThemeVariables.ts:19-51`. On iOS light, the header and drawer use the row-hover tone.
- A Material You variant exists for Android (`MOB/lib/mobileTheme.ts:30`).

**Mobile metrics** (`MOB/lib/layoutMetrics.ts`):
- Home horizontal inset 20
- iOS nav bar 44
- App bar 48
- Code surface (`typography.ts:14-21`): row 22, gutter 46, font 12

---

## 2. Desktop/web layout

### 2.1 Shell

**Sidebar width:**
- Default 256px, minimum 208px; the main area keeps at least 640px (`WEB/components/threadSidebarWidth.ts:1-4`).
- The minimum grows if needed so the "T3 Code" brand never clips.
- Resizable from the rail; double-click resets.
- Wiring is in `WEB/components/AppSidebarLayout.tsx:255-334`.
- On macOS desktop the left controls are inset 90px for the traffic lights (line 60).
- Base primitives: `WEB/components/ui/sidebar.tsx:27-30, 222`.

**Chat header** (`WEB/components/ChatView.tsx:10874-10910`, `WEB/components/chat/ChatHeader.tsx:231-337`):
- Height is `--workspace-topbar-height` (52px), padding `px-3` / `sm:px-5`; the bar is draggable on desktop.
- Breadcrumb: project favicon (14px) plus muted project name (max 160px). Clicking it creates a new thread in that project.
- Then a `/` separator and the thread title (`text-sm font-medium`). Clicking the title opens the action menu; double-clicking renames.
- 96px of right padding is reserved for the panel buttons (thread details, terminal drawer, right panel) in `WEB/components/chat/PanelLayoutControls.tsx:56,101,123`.

**Thread action menu** (`WEB/components/threadActionMenu.logic.ts:115-233`):
New thread on branch, Pin/Unpin, Settle/Un-settle, Snooze (presets or Custom…), Rename, Regenerate title, Mark unread, Auto-settle, Copy (path / branch / thread ID), Project settings, Archive, Delete.

**Right panel tabs** (`WEB/components/RightPanelTabs.tsx:336-384`):
- Browser, Terminal, Files, Diff, Pull request, Device.
- Preview panel width: default 540px, minimum 360px, at most 70% (`WEB/hooks/usePreviewPanelInlineSize.ts:11-23`).

### 2.2 Sidebar (v2, the default) — `WEB/components/Sidebar.tsx`

**Header** (`WEB/components/sidebar/SidebarThreadHeader.tsx:80-150`):
- Search field: `h-8`, `rounded-md`, placeholder "Search", 16px search icon.
- Then 28px (`size-7`) icon buttons:
  - Project filter: shows a folder icon, or the selected project's favicon
  - Add project: FolderPlus
  - New thread: SquarePen; Shift+click creates it in the current project
- The project filter is a combobox ("Search projects...", "All projects"), max width 18rem (`Sidebar.tsx:4841-4970`).

**Grouping:**
- Threads form one flat list, filtered by project scope, split into shelves:
  - Pinned (drag boundary)
  - Active
  - Working (collapsible)
  - Snoozed (collapsible)
  - Settled (collapsible; shows 10 rows, then pages of 25)
- Shelves are rendered at `Sidebar.tsx:5261-5362`; paging constants at 288-293.
- Shelf headers are 32px (`h-8`), built from `CollapsibleSectionHeader` (`Sidebar.tsx:759-794`).
- Row shape: Active, Pinned and Working use "card" rows; Snoozed and Settled use "slim" rows (5113-5115).
- Rows sit 1px apart (`gap-px`, 5091).

**Card row** (1906-2158):
- Content box is 78px tall (`h-[4.875rem]`), plus 2px top and bottom padding. Padding is 10px horizontal and 8px vertical.
- Line 1 (20px tall):
  - Draft pen, if there is an unsent draft
  - 16px project favicon
  - Project name: `text-xs`, secondary-label color, medium weight unless the row is receding
  - Pin icon
  - Status slot on the right
- Line 2 (`mt-1`): title, `text-sm`, truncated; color rules at 1602-1640.
- Line 3 (`mt-0.5`, `text-xs`, secondary-label):
  - Worktree indicator and branch (muted at 40%, middle-truncated)
  - Terminal icon (teal, pulsing)
  - PR badge
  - Diff stat `+N −N` in mono, addition / deletion foreground colors
  - Remote machine icon
  - Provider icon stack: current icon 14px at 60% opacity; earlier owners 12px, grayscale, 35% opacity (351-398)

**Slim row** (1742-1904):
- `h-9` (36px), `px-2.5`, `gap-2.5`.
- Favicon at 40% opacity and grayscale until hover.
- Title `text-sm`, with the time label on the right.
- Hover actions swap in where the time was: Settle (check), Un-settle (Undo2), Wake (AlarmClockOff).

**Row surface** (1534-1561):
- `rounded-md` (8px).
- Background by state:
  - Active route: `bg-sidebar-row-active`
  - Selected: `bg-sidebar-row-selected`
  - Unsent draft: `bg-warning/4`, hover `/8`
  - Otherwise: transparent, `hover:bg-sidebar-row-hover`
- Background work that is not selected "recedes": muted color, and working rows drop to 70% opacity.

**Status slot** (1258-1311). Icons are 16px, labels medium weight:

| Status | Color | Icon / extra |
|---|---|---|
| Working | info | CircleDashed, plus a live duration (`Ns` / `Nm` / `Nh Nm`, `Sidebar.logic.ts:1150-1156`) |
| Waiting | muted | none |
| Approval | warning-foreground | ShieldQuestion |
| Input | indigo-600, dark indigo-300 | MessageCircleQuestion |
| Limited | warning | CircleAlert |
| Failed | error | CircleAlert |
| Woke | warning | AlarmClock, clickable to dismiss |
| Done (unread) | success | CircleCheck |
| none | — | relative time ("now", "5m", …) |

On hover the status is replaced by: Discard draft (X), Snooze (clock menu), and "Settle" (check + label).

**Legacy per-project tree:** `WEB/components/LegacySidebar.tsx`. Rows are `h-8 text-xs` (line 724), with a "Projects" label (3025) and "Show more" (1104-1116). It uses the v1 status pills from `Sidebar.logic.ts:1158-1235`:
- Pending Approval: amber
- Awaiting Input: indigo
- Working / Connecting: sky, pulsing
- Plan Ready: violet
- Completed: emerald

### 2.3 Chat view — `WEB/components/chat/MessagesTimeline.tsx`

**Column and spacing:**
- The column is centered at `--chat-content-max-width` (`.chat-content-lane`, `index.css:901-906`), with side padding 12px, or 20px at `sm` (`index.css:2270-2308`).
- 12-16px spacers at the top and bottom of the timeline (369-379).
- Space below each row (1757-1790): 16px (`pb-4`) normally; 8px (`pb-2`) for assistant commentary, events and folds; 6px (`pb-1.5`) for working and turn-fold rows.

**User message** (2108-2307):
- Right-aligned bubble: `max-w-[80%] rounded-2xl` (18px), `bg-message`, `p-3`.
- Long text collapses after 8 lines or 600 characters, with a 1.75rem fade (4320-4323).
- Images: 2-column grid, max 210px wide, 4:3, `rounded-lg`.
- Above the bubble, optionally:
  - "Sent by automation" / "Sent by another agent" (`text-2xs`)
  - Intent marker "Queued" or "Steer" (Redo2 icon, `text-xs` muted; 2311-2354)
- Below, revealed on hover: timestamp (`text-xs`), "Edit from here" (Undo2), copy.

**Assistant message** (2495-2542):
- No bubble; `px-1 py-0.5`, rendered with ChatMarkdown.
- Then a "changed files" card (`WEB/components/chat/ChangedFilesTree.tsx:46-110`):
  - `mt-4 rounded-lg bg-secondary`, dark `bg-input/20`
  - Sticky header `px-3 py-2`: "N changed files" and the +/− stat
  - Buttons: expand/collapse all folders, and "Open diff"
- Then a meta row, revealed on hover (2609-2661): Fork (GitFork) button, status chip if not completed, copy, timestamp.

**Work log / tool calls:**
- Row primitives are in `WEB/components/chat/WorkLog.tsx:47-62`:
  - Rows at least 24px tall, `text-sm leading-relaxed`
  - 24px icon slot holding a 16px icon (`text-icon-muted`)
  - Truncated label in secondary-label color
  - Trailing hover timestamp and a 12px chevron that rotates 90° when expanded (5310-5324)
- Expanded detail is mono at the code size, `max-h-64` (4879-4880). Detail text is indented 28px (`ms-7`, `WorkLog.tsx:116-134`).
- Expanded groups scroll inside `max-height: min(18rem, 50dvh)` (3366).
- Group summary icons: read = eye, edit = square-pen, command = terminal, search = globe/search, and so on (3660-3701).
- Live rows (3509-3611): "Thinking" shimmer, using the `live-tool-shine` 2.2s animation (`index.css:502-529`).
- "Working for 12s" row (3403-3436): `text-sm` muted, bottom border `border-border/60`.
- Failed tool rows: icon at `tool-error-icon/40`. Severe failures: CircleAlert in destructive red; usage limits in warning (5028-5086).
- Turn folds: label plus chevron, bottom border (2449-2471).

**Plan card** (`WEB/components/chat/ProposedPlanCard.tsx:149-212`):
- `rounded-3xl` (22px), `border-border/80 bg-card/70`, padding `p-4` / `sm:p-5`.
- Header: "Plan" badge, title `text-sm font-medium`, and a "…" menu: Copy to clipboard, Download as markdown, Save to workspace.
- Collapses when the plan is over 900 characters or 20 lines: `max-h-104` (416px), 96px fade, and an "Expand plan" button.

**Diff panel** (`WEB/components/DiffPanel.tsx`):
- Scope dropdown: Uncommitted / Latest turn / Turn N (664-710).
- Branch base-ref combobox, 18rem wide (720-830).
- Stacked/Split toggle (886-900) and line-wrap toggle (908).
- Rendering uses `@pierre/diffs` with `diffStyle` unified or split and `lineDiffType: "none"` (1146-1148), on `--code-background` (`index.css:2056-2073`).

### 2.4 Composer — `WEB/components/chat/ChatComposer.tsx` + `ComposerSurface.tsx`

**Placement** (`ChatView.tsx:11083-11100`):
- A new draft is the "hero" state: composer vertically centered, headline above it — "What should we build in {project}?", `text-2xl` / `sm:text-3xl` (`WEB/components/chat/DraftHeroHeadline.tsx:326-369`).
- After sending, the composer docks to the bottom with 6-8px top padding.

**Shape** (`ComposerSurface.tsx:5-84`):
- `rounded-3xl` (22px), max width = chat column width.
- Glass background: card (light) or surface-raised (dark) at 80% opacity with blur.
- 1px outline: `rgb(0 0 0/8%)` light, white 5% dark.
- Composer shadow in light mode only.
- An optional "context strip" sits under it, inset 22px (`--chat-composer-drawer-inset: 1.375rem`).

**Body and editor:**
- Body padding (6859-6869): 12px horizontal (16px at `sm`), 14px top (16px at `sm`), 8px bottom.
- Editor (`WEB/components/ComposerPromptEditorTiptap.tsx:780, 1354`): minimum 78px tall (`min-h-19.5`), maximum 208px (`max-h-52`), `leading-relaxed`, font = prompt size (14px default).
- Placeholder: "Ask anything, @tag files/folders, $use skills, or / for commands" (7369-7385).

**Footer** (7434-7558): `px-3 pb-3` (16px at `sm`). Left side, controls are 28px (`h-7 px-2.5`, `text-sm` medium; `WEB/components/chat/ComposerControl.tsx:17-24`) with 16px separators:
- Model picker
  - Trigger at 5401-5488: provider icon plus model name
  - Popover (`ModelPickerContent.tsx:832-907`): max width 360px (`max-w-90`), max height about 346px (`max-h-86.5`), provider sidebar and "Search models..."
- Traits (`WEB/components/chat/TraitsPicker.tsx`): Effort / Reasoning / Thinking / Fast mode / Context window
- Runtime mode select (1296-1342), options from `runtimeModeConfig.ts`: Supervised (Lock), Auto-accept edits (PenLine), Auto (Sparkles), Full access (LockOpen)
- Build/Plan toggle (1254-1290): Bot icon / PencilRuler icon
- Controls that do not fit move into an overflow menu (`CompactComposerControlsMenu.tsx`)

**Footer, right side:**
- Paperclip "Attach files" (32px, 28px at `sm`; 7469-7505)
- Context-window ring (28px button, 20px ring; `ContextWindowMeter.tsx:51-75`)
- Primary action, from `WEB/components/chat/ComposerPrimaryActions.tsx`:
  - **Send:** circle, 36px (32px at `sm`), `bg-message-action`, up-arrow icon (14px). While a turn runs, the icon becomes ListPlus (queue) or CornerUpRight (steer). Editing a queued message shows Check. A thread that can resume shows Play. (282-327)
  - **Stop:** 32px circle, `bg-destructive/90`, 8×8 square with 1.5 radius, tooltip "Interrupt" (123-145). Shown when the run can be interrupted and the prompt is empty.
  - **Labeled pills** (Submit / Next question / Implement / Refine): `rounded-full bg-message-action` (74-75), 32/28px tall for question answers and 36/32px for Implement/Refine. "Implement" is a split button whose menu has "Implement in a new thread" (196-247).

**Banners above the composer** (`ComposerBanner.tsx`):
- Attached glass strip; overlaps the composer by 1rem+1px; `rounded-t-2xl`; text `text-xs/4`; 28px icon column (24px at `sm`) (148-176).
- Warning variant: tint warning 8%, outline warning 28%.
- Order, top to bottom (6606-6780):
  1. Queued list
  2. Banner stack
  3. Top drawer: approval, question, or "Plan ready" (`ComposerPlanFollowUpBanner.tsx`)
  4. Tasks drawer
- A stash badge sits beside them.

**Queue UI** (`WEB/components/chat/QueuedRunsControl.tsx:246-496`):
- Header row: ListOrdered icon, "Queued", count, chevron.
- Expanded list is at most 128px (`max-h-32`).
- Each row: drag-to-reorder (arrow keys also work), 16px thumbnails, truncated preview, Edit (pencil), "Steer" (CornerUpRight, `xs` ghost-muted), Remove (X).

**Approval** (`ComposerPendingApprovalPanel.tsx:38-66`, `ComposerPendingApprovalActions.tsx:24-107`):
- Label, e.g. "Command approval", `text-2xs` warning, with a "1/N" counter.
- Detail in mono `text-xs`, max height 80px.
- Buttons (`xs`): Decline (outline) and Approve (primary). A "…" menu holds Cancel and "Always allow this session".
- While an approval is pending, the editor is disabled with the placeholder "Resolve this approval request to continue".

**Questions** (`ComposerPendingUserInputPanel.tsx:180-299`):
- Header text with "i/N", a collapse chevron, and dismiss.
- Question `text-sm`.
- Options: `rounded-md px-2.5 py-2`, label `text-sm` medium, description `text-2xs`. The right side shows a 1-9 key hint, or a primary-colored check when selected. Selected background is `bg-muted/55`.

### 2.5 Terminal (web)

`WEB/components/ThreadTerminalDrawer.tsx`:
- Bottom drawer, default 280px tall (`WEB/types.ts:34`), minimum 180px, maximum 75% of the window (93-94).
- Actions: split, split vertical, new, close (1440-1490).
- Tab rows `h-6 text-xs` (1682).
- Ghostty surface, 12px font (`WEB/terminal/ghostty/surface.ts:20`).

---

## 3. Mobile (apps/mobile)

### 3.1 Navigation — `MOB/Stack.tsx`

**Root native stack** (670-899):

| Route | Presentation |
|---|---|
| Home | glass header (route `""`) |
| Thread (`threads/:environmentId/:threadId`) | glass header |
| ThreadTerminal | solid header |
| ThreadReview | solid header |
| ThreadReviewComment | sheet, detents 0.55 / 0.92 |
| ThreadFiles, ThreadFile, ThreadAttachment | push |
| ThreadDevicePreview | full-screen modal |
| ThreadSettingsSheet | form sheet, detent 1 |
| ThreadQueue | sheet, detents **0.65 / 0.95** |
| ThreadAgents | sheet, 0.5 / 0.9 |
| GitOverview / GitCommit / GitBranches | sheet, 0.55 / 0.92 |
| GitConfirm | sheet, 0.45 / 0.7 |
| SettingsSheet | nested stack, `SettingsContentStack` at 182-396 |
| SettingsLegal | full-screen modal |
| ConnectOnboarding | sheet, 0.6 / 0.95 |
| Connections | sheet 0.55 / 0.7 on iOS; full page on Android |
| ConnectionsNew | sheet |
| NewTaskSheet | nested stack (436-531): NewTask "Choose project" → NewTaskDraft → NewTaskEnvironment / NewTaskBranch / ThreadSettings / AddProject* |

- Settings and New task become cards on Android or in split view, and a 0.92 form sheet on compact iOS (928-952).
- Header style: title 18px, weight 800, back button minimal (136-170).

**Adaptive layout** (`MOB/lib/layout.ts:15-22, 90-113`):
- Split view when width ≥ 720 and height ≥ 600.
- List pane = 32% of width, clamped to 280-380.
- Chat column max width 960.
- Auxiliary pane 260-480.
- Implementation: `MOB/features/layout/AdaptiveWorkspaceLayout.tsx`.

### 3.2 Thread list (Home)

**Header** (`MOB/features/home/HomeHeader.tsx:30-160`):
- Right: settings (ellipsis).
- Search "Search".
- Filter menu: Environment (All environments / each environment) and Project (All projects / each project).
- Compose button `square.and.pencil` = New task. It sits in a bottom toolbar before iOS 26 and in the toolbar on Liquid Glass iOS.

**Sections** (`MOB/features/threads/thread-list-v2-items.tsx:102-175`; Working / Snoozed / Settled at 194):
- Label `text-xs` medium, tertiary color, `mt-4 mb-1.5 px-5`, followed by a 1px hairline and a 10px chevron.

**Card row** (922-1144):
- Padding `px-5 py-2.5` (17.5px / 8.75px).
- Line 1:
  - 15px favicon
  - Project title, `text-sm` medium, muted
  - Queued icon
  - 11px pin
  - Status or time, `text-xs` tabular
- Line 2: title, `text-base` (16/23) medium, up to 2 lines.
- Line 3:
  - Branch in Menlo `text-xs`, then " · " and the environment label with an 11px machine glyph
  - PR icon + number (emerald open / rose closed / violet merged)
  - Provider icons 14px, earlier owners at 30%
- Divider: inset 17.5px (`ml-5`), 1px, `bg-border-subtle`.

**Slim row** (1146-1220): at least 44px tall, `px-5`, favicon at 40%, title `text-base` muted on one line, time `text-sm` in Menlo.

**Status colors** (68-72, 590-592):

| Status | Color |
|---|---|
| Approval | warning-foreground |
| Input | indigo 600 / 300 |
| Working | sky 600 / 400 |
| Failed | danger-foreground |
| Limited | warning-foreground |
| Done | emerald 700 / 300 |

**Interactions** (1228-1277):
- Swipe left: primary Settle / Un-settle / Wake (a full swipe commits it), secondary Snooze menu.
- Long-press menu: New thread on branch, Copy thread ID, lifecycle actions.

**iPad sidebar rows** (`thread-list-v2-row-appearance.ts:19-61`): 12px radius, padding 12 / 10, background drawer or thread-selected.

### 3.3 Thread screen — `MOB/features/threads/ThreadDetailScreen.tsx` + `ThreadFeed.tsx`

**Feed geometry:**
- Side padding 16px (compact) or 20px (split); content centered at max 960 (`ThreadFeed.tsx:2218-2227`).

**User bubble** (1690-1700):
- Right-aligned, max width = 85% of the content width.
- `rounded-[20px]`, padding 12.25px / 8.75px (`px-3.5 py-2.5`), `--color-user-bubble`, 17.5px gap below (`mb-5`).
- Images 180×140, radius 14.

**Assistant rows** (1855-1862): `px-1`; bottom gap 17.5px with the meta row (copy, fork at 312-360, time), else 3.5px. No bubble.

**Work log** (`MOB/features/threads/work-log-layout.tsx`, `thread-work-log.tsx`): rows at least 28px (`layout.ts:24`), 24px icon slot, label `text-sm` muted on one line, rose for danger and warning-foreground for warnings.

**Other surfaces:**
- Floating working control (`floating-working-control.tsx`): "Working 12m 04s", queue count (opens the ThreadQueue sheet), agents, scroll to end.
- Approval and question cards sit above the composer (`ThreadDetailScreen.tsx:1233-1240`):
  - **PendingApprovalCard** (`PendingApprovalCard.tsx:35-74`): `rounded-[20px] border bg-card-alt p-4`; eyebrow "Approval needed" (12px bold uppercase, tracking 1.1); title `text-lg` bold; detail `text-sm` secondary; buttons "Allow once" (primary), "Allow session" (secondary), "Decline" (danger). Buttons are `rounded-[14px]`, `px-3.5 py-3`, text bold `text-sm` (`RequestActionButton.tsx:25-40`).
  - **PendingUserInputCard**: collapsed form is a pill (line 173); expanded form uses the same card style (228).
- Header actions: terminal, files, git controls ("Review changes", "More") in `ThreadGitControls.tsx:259-568` and `ThreadRouteScreen.tsx:106-134`.

### 3.4 Composer — `MOB/features/threads/ThreadComposer.tsx:746-1173`

**Wrapper:** 12px side padding; top padding 8 (expanded) or 6 (collapsed). On iOS, a backdrop gradient fades from screen 0% to 90%.

**Surface** (300-360): Liquid glass, falling back to `--color-composer-surface` with a `composer-border` 1px border; iOS shadow `0 6px 28px`, black at 15% (light) / 35% (dark).

**Collapsed (capsule):**
- Radius 27, vertical padding 2.
- Attach button, one-line editor 36px tall, up to 3 thumbnails (30px, radius 8), mic, then send or stop.

**Expanded:**
- Radius 26, minimum height 140, top padding 14, bottom padding 6.
- Editor at least 72 and at most 160 tall, 14px side padding.
- Toolbar row: attach, model control (`h-11 rounded-xl px-2`, provider icon + model label; opens ThreadSettingsSheet), dictation, send/stop.

**Buttons** (`MOB/components/ComposerToolbar.tsx:229-276`):
- 44px hit area holding a 30px circle.
- `bg-primary`; disabled `bg-primary/15`; stop uses the `stop.fill` symbol on `bg-danger`.
- Send icon and label by state (`composerSendPresentation.ts:19-62`):
  - Idle: arrow.up "Send" (or "Queue" when delivery is deferred)
  - Steer: arrow.turn.left.up
  - Queue: list.number
  - Editing a queued message: checkmark "Update queued message"
- A long-press menu picks the alternate follow-up action; Cmd-Return does the same on a hardware keyboard.

**Thread settings sheet** (`ThreadSettingsSheet.tsx`):
- Title "Thread settings".
- Model list with a provider filter (All providers / Favorites).
- "Options" (trait rows), "Runtime" (Supervised / Auto-accept edits / Auto / Full access; `thread-settings-options.ts:19-34`), "Legacy models" switch.

**Plan mode:** `/plan` and `/default` slash commands (`use-composer-command-menu.ts:79-93`). A Plan/Build toggle exists only in the new-task draft (`NewTaskDraftScreen.tsx:1764-1776`).

### 3.5 Queue sheet — `MOB/features/threads/ThreadQueueControl.tsx:53-470`

- Title "Queued", 18px weight 800 (433-436).
- Rows at least 56px (`min-h-14`, line 368), bottom border, `bg-sheet`.
- "Editing" tag: `text-2xs` uppercase, primary color.
- "Steer" pill: 32px tall, `rounded-full bg-primary px-3 text-xs` (395).
- Row menu: Steer now / Edit / Move up / Move down / Remove (330-354).
- Swipe reveals a red "Remove" (514).
- After a restart, a held queue shows "Queue held after restart" with a "Resume queue" button.

### 3.6 Pairing and connections

- Connections sheet "Environments": `MOB/features/connection/ConnectionsRouteScreen.tsx`. Rows (label / URL editing) are in `ConnectionEnvironmentRow.tsx`.
- `ConnectionsNewRouteScreen.tsx:192-270`:
  - Title "Add Environment" or "Scan QR Code"
  - Fields "Host" (placeholder `192.168.1.100:8080`) and "Pairing code" (placeholder `abc-123-xyz`)
  - Button "Add environment" / "Pairing..."
  - Camera-permission prompt
- `pairing.ts:30-90`: builds `host#token=<code>`; parses `#token` / `?token`; reads the `pairingUrl` parameter from QR payloads.
- T3 Connect cloud onboarding: `MOB/features/cloud/ConnectOnboardingRouteScreen.tsx`.
- Web equivalent: `WEB/components/auth/PairingRouteSurface.tsx:93-110`.

### 3.7 Settings — `MOB/features/settings/SettingsRouteScreen.tsx:74-211`

| Section | Rows |
|---|---|
| Connections | T3 Account, Environments, Notifications |
| Interface | Appearance, Keyboard |
| Automations | Scheduled tasks |
| Projects & threads | Overview, Organization, Thread behavior, Follow-ups, Archived Threads |
| Server settings | Provider accounts, New threads, Source control, Agent behavior, Maintenance |
| App | Usage, About T3 Code |

- Section cards (`components/SettingsSection.tsx:37-44`): `rounded-[24px]` on iOS, `[28px]` on Android, `bg-grouped-card`.
- Rows (`SettingsRow.tsx:62-88`): `p-4 gap-4`, 22-24px icon, label `text-lg`, 16px chevron.

---

## 4. Conversation-flow features and where they live

| Feature | Web | Mobile |
|---|---|---|
| Create thread | Sidebar SquarePen (`SidebarThreadHeader.tsx:133-150`); header project crumb (`ChatHeader.tsx:248-266`); draft route `WEB/routes/_chat.draft.$draftId.tsx` with hero headline | Compose button (`HomeHeader.tsx:55-63`) → `NewTaskSheet` (`Stack.tsx:436-531`, `NewTaskDraftScreen.tsx`) |
| Send | `ComposerPrimaryActions.tsx:282-327` | `ThreadComposer.tsx:1054-1068, 1148-1162` |
| Steer / queue while running | Send icon flips; default `followUpBehavior: "queue"` (`PKG/contracts/src/settings.ts:453-455`); Ctrl/⌘-click or alternate shortcut does the other (276-280); "Steer" marker on the user message (`MessagesTimeline.tsx:2311-2354`) | `composerSendPresentation.ts`; long-press menu; queue sheet "Steer" |
| Stop | Red circle "Interrupt" (`ComposerPrimaryActions.tsx:123-145`) | `stop.fill` danger button (`ThreadComposer.tsx:1055-1060`) |
| Queue management | `QueuedRunsControl.tsx` (reorder, edit, steer, remove) | `ThreadQueueControl.tsx` (sheet) |
| Approvals | Composer top drawer (`ChatComposer.tsx:6628-6653`) | `PendingApprovalCard.tsx` |
| Questions | `ComposerPendingUserInputPanel.tsx` | `PendingUserInputCard.tsx` |
| Model / provider switch | `ProviderModelPicker.tsx`, `ModelPickerContent.tsx`, `TraitsPicker.tsx`; handoff history shown in sidebar tooltip (`Sidebar.tsx:501-508`) | ThreadSettingsSheet via the model chip (`ThreadComposer.tsx:1122-1135`); `MOB/state/thread-provider-switching.ts` |
| Runtime mode | `ChatComposer.tsx:1296-1342`, `runtimeModeConfig.ts` | ThreadSettingsSheet "Runtime" |
| Plan mode | Build/Plan toggle (`ChatComposer.tsx:1254-1290`); `ProposedPlanCard.tsx`; "Plan ready" banner; Implement / Refine / "Implement in a new thread" (`ComposerPrimaryActions.tsx:196-247`) | `/plan` and `/default` slash commands; Plan/Build in new-task draft |
| Attachments | Paperclip (`ChatComposer.tsx:7469-7505`); drag-drop overlay "Drop files to attach" (`ChatView.tsx:~10920`); paste; `@` files, `$` skills, `/` commands | `ComposerAttachmentButton`, `ComposerAttachmentStrip`, native paste |
| Fork | Per assistant response, GitFork "Fork from this response" (`MessagesTimeline.tsx:2544-2588`); handler creates "<title> fork" (`ChatView.tsx:8150-8191`) | `ThreadFeed.tsx:312-360` |
| Rollback | User message Undo2 "Edit from here" → dialog "Revert files too" / "Revert and keep changes" (`MessagesTimeline.tsx:2378-2407`, `ChatView.tsx:11597-11626`); checkpoint "Roll back" in item inspector (`V2ItemInspector.tsx:342-354`, confirm at `ChatView.tsx:8088-8108`) | No UI found |
| Diff view | Per-turn changed-files card → `DiffPanel.tsx` right-panel tab | `ReviewSheet` (`ThreadReview` route), native `t3-review-diff` module |
| Terminal | `ThreadTerminalDrawer.tsx` and right-panel Terminal tab | `ThreadTerminalRouteScreen.tsx`, native `t3-terminal` module |
| Thread lifecycle | Settle / Snooze / Pin / Archive / Rename / Mark unread (`threadActionMenu.logic.ts`, sidebar hover actions) | Swipe actions and long-press menu (`thread-list-v2-items.tsx:1228-1277`) |
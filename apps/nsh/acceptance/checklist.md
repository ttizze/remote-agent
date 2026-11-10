# Mac NSH acceptance checklist

The features of the original clients and the Mac scenario that checks each.
Run with `scripts/dev-env.sh just nsh-acceptance [scenario-filter...]`; the step
language is documented in `apps/nsh/src/acceptance/script.rs`.

Status: **covered** (a scenario checks it), **partial** (a scenario checks part of
it; the note says what is missing), **not yet** (applies to the Mac, no scenario
yet; the note says why), **blocked** (needs a change outside this repository
first; the note says which), **n/a** (no Mac equivalent).

## iOS acceptance tests (`apps/mobile/iosApp/UITests`)

| iOS test | Status | Mac scenario / note |
|---|---|---|
| AccountUITests.testSimulatorSwitchesCodexAccountsAndForksConversation | covered | account-sign-in (adds a second Codex account and switches back), account-switch (switching keeps the open conversation; forking after it) |
| AccountUITests.testSimulatorAddsClaudeAccountAndKeepsCodexSelected | covered | account-claude |
| AccountUITests.testSimulatorGoesBackFromAccountLoginAndCanStartAgain | covered | accounts (Codex sign-in stopped and started again), account-claude (Claude's code entry stopped, then completed) |
| AccountUITests.testSimulatorSignsInDirectlyFromModelSettings | covered | account-claude (from the model picker's account row), account-sign-in (sign-out confirmation) |
| AccountUITests.testSimulatorOpensAccountManagementFromSettingsAndModelPicker | covered | settings, accounts, model-picker (account row opens the provider's settings) |
| AccountUITests.testSimulatorAutomaticallyShowsModelControlsInExistingAndRunningConversations | covered | live-work (model in a running conversation), long-history (sends in an existing one) |
| AccountUITests.testSimulatorComposerOffersFastModelAndEffortBeforeMicrophone | covered | model-effort (the Mac shows depth and speed in T3 Code's picker, not the composer's bar, so their order beside the microphone is n/a) |
| ActivityUITests.testSimulatorSendsWithOneTapWhileReadingHistory | covered | long-history (sends after scrolling into earlier turns) |
| ActivityUITests.testSimulatorComposerGrowsToFitWrappedDraft | covered | composer (`expect_height`: one line keeps the field's least height, a wrapped draft grows it) |
| ActivityUITests.testSimulatorCanStartAConversationInAProject | covered | folded-work, approval, tool-panel |
| ActivityUITests.testSimulatorGroupsLiveCommandsBetweenCommentaryAndExpandsOnTap | covered | live-work |
| ActivityUITests.testSimulatorShowsRetryingStreamErrorThenRecovers | covered | retry-stream |
| ActivityUITests.testSimulatorKeepsFailedWorkCollapsedWithVisibleTerminalError | covered | failed-turn |
| ActivityUITests.testSimulatorKeepsInputRequestVisibleUntilResolved | covered | approval |
| ActivityUITests.testSimulatorSendsQuestionChoiceOnTap | covered | question-choice |
| ActivityUITests.testSimulatorCanSkipQuestion | covered | question-skip |
| AttachmentsUITests.testSimulatorCanAddASecondPhoto | n/a | iOS photo picker; the Mac attaches files (attachments) |
| AttachmentsUITests.testSimulatorCanAttachPhotosAndVideos | n/a | as above |
| AttachmentsUITests.testSimulatorRetriesPhotoUploadAfterStorageRecovery | covered | upload-failure (a file; the Mac attaches again rather than retrying in place) |
| AttachmentsUITests.testSimulatorCanAttachDownloadAndPrepareAIEdit | covered | attachments (attach, remove, send), file-actions (the attachment survives `relaunch`; Quick Look from the editor, whose share and open buttons are the Mac's download; AI edit fills the composer). Quick Look's button titles follow the system language (English here), as in images |
| DeviceNavigationUITests.testSimulatorWorkbenchStartsEmptyAndOwnsOnlyOpenedTabs | covered | tool-panel, tools |
| DeviceNavigationUITests.testSimulatorBrowserUsesOneSharedImportMenu | covered | browser (a page from the fixture Host, typing into it, tabs and the data import from one menu); the Mac page takes typing itself, so the iPhone's input row is n/a |
| DeviceNavigationUITests.testSimulatorNativeTerminalCopiesTextOnLongPress | n/a | long press |
| DeviceNavigationUITests.testSimulatorNativeTerminalPastesMultilineText | covered | terminal-paste |
| DeviceNavigationUITests.testSimulatorNativeTerminalDoesNotDuplicateQueryResponses | covered | terminal-query |
| DeviceNavigationUITests.testSimulatorNativeTerminalRetainsShellAfterReopening | covered | terminal-reopen (the shell keeps its variable while another tool is in front and the panel closes and opens; the draft stays), tools |
| DeviceNavigationUITests.testSimulatorUsesNativeProjectDisclosureAndWorkspaceTree | covered | open-conversation (project disclosure), tool-panel (file tree) |
| DeviceNavigationUITests.testPhysicalDeviceCanPairWithManualPayload | n/a | physical device |
| DictationUITests.testSimulatorDictationContinuesPastThirtySecondsAndReachesHost | not yet | recording needs the system's microphone permission, whose prompt reaches the user running the scenarios; the driver can only decline it |
| DictationUITests.testSimulatorDictationPermissionDenialPreservesDraftAndSend | covered | dictation-denied (`microphone deny`) |
| HistoryUITests.testSimulatorReopensRunningLongHistoryWithoutBlankViewport | covered | history-running (the long history with its latest turn still working opens at its end) |
| HistoryUITests.testSimulatorKeepsSmallOlderScrollDuringLiveUpdate | covered | history-live-scroll (a four-line wheel up while a turn streams stops following, as Core's `follows_end` decides; back at the end it follows again) |
| HistoryUITests.testSimulatorRendersVisualizationAndReopensIt | partial | visualization (the Mac offers the file and opens it in the tool panel, as the desktop client did; only the iPhone renders it inline) |
| HistoryUITests.testSimulatorRendersMarkdownTableAndReopensIt | covered | markdown-table |
| HistoryUITests.testSimulatorKeepsDraftDuringLongMarkdownStreamAndReopensFinalText | covered | long-markdown |
| HistoryUITests.testSimulatorReopensCompletedHistoryCollapsed | covered | folded-work, markdown-table (reopened collapsed) |
| ImagesUITests.testSimulatorShowsGeneratedImagesAndOpensFileLinksAfterReopening | partial | images (a file link opens in the tool panel); generated images are screenshot evidence only |
| ImagesUITests.testSimulatorOpensOnlyTheTappedImageAndSavesIt | covered | image-viewer (the Mac's own viewer: the pressed image beside the conversation's generated images, saved through the save panel) |
| ImagesUITests.testSimulatorDisplaysImagesInMessagesAndMarkdownAfterReopening | covered | images |
| ModelSelectionUITests.testSimulatorModelPickerUsesNativeFormAndAgentPicker | covered | model-picker |
| NavigationUITests.testSimulatorRemovesHostAndRequiresPairingAfterRelaunch | covered | saved-connections (pairs the fixture Host as a PC, removes it, and after a `relaunch` it is still gone) |
| NavigationUITests.testSimulatorStartsOnListAndPreservesDetailOnForeground | partial | open-conversation; iOS list/detail navigation and foregrounding are n/a |
| NavigationUITests.testSimulatorOpensQrScannerWhileAddingHost | n/a | camera QR scanner; the Mac shows its own QR code for phones (phone-pairing) and pastes another PC's connection info (saved-connections) |
| NavigationUITests.testSimulatorUsesNativeHostNavigationAndPairingDismissal | covered | environment-settings |
| NavigationUITests.testSimulatorSearchesFromBottomBarAndCreatesInCollapsedProject | covered | sidebar-search; new chats start in the collapsed project in most scenarios |
| NavigationUITests.testSimulatorUsesClaudeWithoutCodexAndRestoresConversation | covered | claude-only (`host without-codex`, `relaunch`) |
| NavigationUITests.testSimulatorRepeatedlyReopensTasksAndNewDraftsAfterBackNavigation | covered | reopen-drafts (two rounds between a conversation and new chats in and outside the project, each keeping its draft) |
| PaginationUITests.testSimulatorOpensLongInterruptedHistoryAtLatestMessage | covered | long-history |
| PaginationUITests.testSimulatorLoadsEarlierHistoryOnTapAtTheReadingPosition | covered | long-history (position by screenshot) |
| PaginationUITests.testSimulatorLoadsLatestFiveTitlesPerProjectAndExpandsOneProject | covered | list-pagination |
| PaginationUITests.testSimulatorPaginatesRecentProjectsAndUnassignedChats | covered | list-pagination |
| SynchronizationUITests.testSimulatorShowsHistoryLoadingBeforeHostReplies | covered | history-loading |
| SynchronizationUITests.testSimulatorRetriesAFailedTaskOpenWithoutLosingItsDraft | covered | history-retry |
| SynchronizationUITests.testSimulatorReconnectClearsHistoryFailureAndPreservesDraft | covered | reconnect (the environment's connection switch reconnects; the failed read shows its error until the next reconnect clears it) |
| SynchronizationUITests.testSimulatorUpdatesAnOpenConversationFromAnotherClient | covered | other-client |
| SynchronizationUITests.testSimulatorKeepsOpenTaskAndFetchesLatestReplyAfterForeground | covered | reconnect (the reply another client added arrives with the reconnect, the draft stays) |
| SynchronizationUITests.testSimulatorFetchesNewTaskAfterForeground | covered | other-client-new (the Mac stays connected behind other apps, so the new conversation arrives without a return) |
| SynchronizationUITests.testSimulatorFetchesNewTaskWhenReturningToList | n/a | the Mac sidebar is always shown |
| SynchronizationUITests.testSimulatorMarksUnseenCompletionUntilOpened | covered | unseen-completion |
| SynchronizationUITests.testSimulatorShowsAcceptedAdditionalInputBeforeCodexProcessesIt | covered | queued-input |
| TurnUITests.testSimulatorKeepsLatestVisibleAcrossRepeatedLongHistorySubmissions | covered | history-position |
| TurnUITests.testSimulatorSelectsPluginAndSkillFromComposer | covered | composer (skills; the fixture lists no plugins) |
| TurnUITests.testSimulatorSelectsAssistantTextInPlaceAndAddsOnlySelectionToDraft | covered | message-actions |
| TurnUITests.testSimulatorOpensSideChatWithoutLosingOriginalDraft | covered | side-chat |
| TurnUITests.testSimulatorCopiesSelectedOrWholeUserMessageText | covered | message-actions (whole messages, and a selection in the user's message with ⌘C) |
| TurnUITests.testSimulatorKeepsChatUnassignedAfterSendingAndReopening | covered | send-message |
| TurnUITests.testSimulatorApprovalEditorAndDraftSurviveReconnect | covered | relaunch-draft (approval, a saved edit in the diff, and the draft after `relaunch`) |
| WorktreeUITests.testSimulatorShowsChangesMarkToTheRightOfRunningStatus | covered | worktree-changes |
| WorktreeUITests.testSimulatorEditsHostWorktreeSettingsFromTaskMenu | covered | worktree-settings (from the settings; the Mac has no task menu) |
| WorktreeUITests.testSimulatorReviewsTheOpenSessionsWorktree | covered | worktree-review, review-panel |

## Desktop client features

| Feature | Status | Mac scenario / note |
|---|---|---|
| Sidebar: projects, chats, new chat, new chat in a project | covered | send-message, open-conversation, folded-work, list-pagination, sidebar-title (the provider's title arrives live and survives `relaunch`), project-expansion (unfolded projects survive `relaunch`) |
| Sidebar: search, project menu, add/hide/rename project | covered | sidebar-search, projects |
| Sidebar: a conversation's Copy submenu (path, branch, title, ID) | covered | thread-copy |
| Sidebar: unseen completions, changes from the source branch | covered | unseen-completion, worktree-changes |
| New-chat hero: project choice, start without a project | covered | new-chat-hero |
| Thread details: inline visibility, narrow-window popover, tool panel and Escape | mapped | thread-details (Native clients CI) |
| Conversation: messages, folded work, approvals, questions | covered | send-message, folded-work, live-work, approval, question-choice, question-skip, question-back |
| Conversation: errors, reconnecting, opening and its failure | covered | failed-turn, retry-stream, history-loading, history-retry |
| Conversation: copy, edit in composer, fork from a message, quote a selection | covered | message-actions, fork, claude-fork-while-running, fork-failure |
| Conversation: a selection's bar of actions (add to chat, explain in a side chat, ask in one), gone on scroll or deselect; the right-click menu in the UI language | covered | selection-bar, selection-bar-scroll |
| Conversation: a message the Host may not have received goes back to the composer or is discarded | covered | unresolved-submission |
| Conversation: a turn's work that fails to load shows why under the opened row, with a retry | not yet | the Host reads turn details from its transcript database, which it holds open in one SQLite connection for its lifetime; making the file unreadable from outside (chmod, swapping it) does not fail the read, and the provider is asked only for conversations never saved. The core context test covers the state; the view was checked by hand with a temporary Host failure |
| Conversation: steering a running turn, minimap beside the lane | covered | steering, queued-input, minimap |
| Conversation: live Thinking and work rows (T3 Code's shimmer), expandable thoughts | covered | thinking, live-work; the shimmer itself is screenshot evidence only |
| Conversation: one without a message yet (a side chat's) opens empty and sends its first message | covered | empty-conversation |
| Conversation: waiting on background work (Claude, Codex), its report and notification after reopening and in English, Stop, refused model change | covered | background-waiting, background-model-change, codex-background-waiting, codex-background-report |
| Composer: model picker | covered | model-picker |
| Composer: why a send waits (another agent chosen while the conversation works) | covered | provider-switch |
| Composer: Japanese input method (confirming return, sending while composing) | covered | input-method |
| Composer: DeepSeek's OpenRouter route, Flash 4.1 pick and models | covered | openrouter |
| Composer: approval mode menu | covered | permission-mode |
| Composer: controls fit narrow windows, none cut off | covered | composer-narrow |
| Sidebar: a sheet over the content below 768 points, closing on its toggle, Escape or a choice | covered | composer-narrow |
| Composer: attach files, dictation, completions | partial | attachments, upload-failure, composer, dictation-denied; recording needs the system's microphone permission |
| Composer: paste text at the selection and undo it (Edit > Undo and ⌘Z), paste an image without the text copied with it | covered | composer-paste |
| Composer: Escape discards a recording | not yet | recording needs the system's microphone permission |
| Thread details card (workspace, editor, changes, sources) | partial | attachments (sources); the editor button opens an external app |
| Changed-files bar and review | covered | review-panel, worktree-review, diff-files (the badge opens the diff and its file menu jumps to a file) |
| Diff: binary files, file menu, copying the diff | covered | diff-files |
| Image viewer: zoom by steps, pinch and ⌘ + wheel, reset, save, the conversation's generated images | covered | image-viewer (⌘ + wheel; a trackpad pinch is not posted, and Core's `ImageZoom::gestured` tests both) |
| An image's menu: save as | covered | image-save-as |
| Command-click (Control-click on Windows) on a web link opens the in-app browser | covered | link-browser (the browser loads the fixture Host's page, whose title its tab menu lists) |
| Tool panel: files, file preview, diff, close | covered | tool-panel |
| File list beside an open file: workspace tree, drag into the composer, Shift+F10 menu, resizing, ⌘/Shift-click selection | partial | explorer, file-tree-selection (⌘-click adds rows, Shift-click extends to a row, and the selection drags together); the width surviving a relaunch is unit-tested, and the handle takes no arrow keys (NSH handles take only drags and VoiceOver steps) |
| File chips: glyph, accent, caption, menu, opening, size tooltip, ⌘-click to the editor | partial | file-chips (⌘-click opens the fixture Host's only editor, a fake one the runner gives it); the size tooltip is a text attribute that AppKit's link elements do not expose to accessibility (MessageTests checks it) |
| Tool panel: layout, browser, terminal, agents, side chat | covered | review-panel (maximize), tools, browser, terminal-paste, terminal-query, terminal-reopen, side-chat |
| Settings: general, providers, environment, worktrees | covered | settings, environment-settings |
| Settings: display language | covered | display-language |
| The menus NSH makes (app, Edit, Window) in the UI language | covered | menu-labels (Japanese, then English as soon as English is chosen, without a relaunch) |
| Settings: accounts, updates, worktree editing | partial | accounts, account-sign-in, account-claude, account-switch, worktree-settings; updates not yet: アップデートを確認 opens the releases page in the default browser, outside the app |
| Accessibility: the settings overlay is modal, message roles | covered | settings-modal, send-message |
| Accessibility: VoiceOver reads in the visual order | covered | reading-order (`expect_reading_order`: the conversation's texts, folds and buttons read top to bottom after scrolling 2000 lines back and forth; the rows the list left behind stay unread) |
| Accessibility: a scrolled list says where it is | covered | reading-order, history-position, history-live-scroll (`expect_scrolled`: the conversation is an AXScrollArea whose vertical scroll bar gives its position) |
| Onboarding without a Host | n/a | the desktop app always lists this PC's Host and starts it when needed, so the Mac never lacks one; pairing another PC is in the environment settings (environment-settings, saved-connections) |
| First-run setup: environment and AI, then importing projects from history | covered | onboarding |
| Settings: importing existing projects | partial | onboarding (the same import as the first run); opening it from the settings has no scenario |
| Settings: phone connection QR code, copying the invitation, allowed devices | covered | phone-pairing |
| Settings: adding a PC over SSH or a pasted invitation, removing a saved connection | partial | saved-connections (an SSH destination that would be an option is refused; no SSH server runs) |
| Keyboard shortcuts | partial | composer (Return sends), composer-completion-keys (arrows, Return and Escape in the completion list), tool-keys (the empty tool panel's keys), terminal-paste (⌘V), model-picker (Escape closes it); ⌘, does nothing |
| Markdown tables, diagrams, images, file links | covered | markdown-table, markdown-document, mermaid, images, visualization, file-chips |

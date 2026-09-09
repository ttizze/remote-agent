package dev.remoteagent.mobile

internal fun reduceProfile(state: AppState, action: AppAction.Profile): AppState =
    when (action) {
        is AppAction.TurnOptionsChanged ->
            state.updateProfile(action.hostIdentity) {
                state.copy(turnChoices = state.turnChoices + (action.hostIdentity to action.options))
            }

        AppAction.PairingOpened -> state.copy(showingPairing = true, pairingError = null)

        AppAction.PairingDismissed ->
            if (state.profiles.isEmpty()) state else state.copy(showingPairing = false, pairingError = null)

        AppAction.ProfileSelectionOpened -> state.copy(selectedProfileId = null, showingPairing = false)

        is AppAction.ProfilePaired -> {
            val profiles = state.profiles.filterNot { it.id == action.profile.id } + action.profile
            state.copy(
                profiles = profiles,
                selectedProfileId = action.profile.id,
                profileViews = state.profileViews + (action.profile.id to ProfileViewState()),
                showingPairing = false,
                pairingError = null,
            )
        }

        is AppAction.PairingFailed -> state.copy(pairingError = action.message)

        is AppAction.ProfileSelected ->
            if (state.hasProfile(action.hostIdentity)) {
                state.copy(selectedProfileId = action.hostIdentity, pairingError = null).updateView(
                    action.hostIdentity
                ) {
                    it.copy(loadingHistory = false)
                }
            } else {
                state
            }

        is AppAction.AddressesDiscovered ->
            state.updateProfile(action.hostIdentity) { profile ->
                state.copy(
                    profiles =
                        state.profiles.map {
                            if (it.id == profile.id) {
                                action.addresses.firstOrNull()?.let { relayUrl -> profile.copy(relayUrl = relayUrl) }
                                    ?: profile
                            } else it
                        }
                )
            }
    }

internal fun reduceConnection(state: AppState, action: AppAction.Connection): AppState =
    when (action) {
        is AppAction.ConnectStarted ->
            state.updateView(action.hostIdentity) {
                it.copy(connection = ConnectionPhase.Connecting, loadingHistory = false, notice = null)
            }

        is AppAction.ConnectSucceeded ->
            state.updateView(action.hostIdentity) {
                it.copy(
                    connection = ConnectionPhase.Connected,
                    threadList = LoadPhase.Idle,
                    threadDetail = LoadPhase.Idle,
                    notice = null,
                )
            }

        is AppAction.ConnectFailed ->
            state.updateView(action.hostIdentity) {
                it.copy(
                    connection = ConnectionPhase.Failed(action.message),
                    loadingHistory = false,
                    notice = action.message,
                )
            }

        is AppAction.Disconnected ->
            state.updateView(action.hostIdentity) {
                it.copy(
                    connection = ConnectionPhase.Disconnected,
                    loadingHistory = false,
                    loadingMoreThreads = false,
                    threadList = LoadPhase.Idle,
                    threadDetail = LoadPhase.Idle,
                    interruptingTurnId = null,
                )
            }
    }

internal fun reduceNavigation(state: AppState, action: AppAction.Navigation): AppState =
    when (action) {
        is AppAction.WorkingDirectoryChanged ->
            state.updateViewIfConnected(action.hostIdentity) {
                it.copy(
                    workingDirectoryPath = action.path,
                    newThreadCwd = null,
                    threadList = LoadPhase.Idle,
                    selectedThreadId = null,
                    threadDetail = LoadPhase.Idle,
                )
            }

        is AppAction.NewThreadOpened ->
            state.updateView(action.hostIdentity) {
                it.copy(
                    loadingHistory = false,
                    selectedThreadId = null,
                    newThreadCwd = action.cwd.trim(),
                    threadDetail = LoadPhase.Ready,
                    notice = null,
                )
            }

        is AppAction.ThreadSelected ->
            state.updateViewIfConnected(action.hostIdentity) {
                it.copy(
                    loadingHistory = false,
                    selectedThreadId = action.threadId,
                    newThreadCwd = null,
                    threadDetail = LoadPhase.Idle,
                    notice = null,
                )
            }

        is AppAction.ThreadListOpened ->
            state.updateView(action.hostIdentity) {
                it.copy(
                    loadingHistory = false,
                    selectedThreadId = null,
                    newThreadCwd = null,
                    threadDetail = LoadPhase.Idle,
                    notice = null,
                )
            }
    }

internal fun reduceThreadList(state: AppState, action: AppAction.ThreadList, cacheLimits: MobileCacheLimits): AppState =
    when (action) {
        is AppAction.ThreadListLoading ->
            state.updateViewIfConnected(action.hostIdentity) {
                if (action.append) it.copy(loadingMoreThreads = true, notice = null)
                else it.copy(threadList = LoadPhase.Loading, loadingMoreThreads = false, notice = null)
            }

        is AppAction.ThreadListLoaded ->
            if (state.isConnected(action.hostIdentity)) {
                val cache = reconcileProjectList(state.cache, action.hostIdentity, action.projects, cacheLimits)
                state
                    .copy(cache = reconcileThreadList(cache, action.hostIdentity, action.threads, cacheLimits))
                    .updateView(action.hostIdentity) {
                        it.copy(
                            threadList = LoadPhase.Ready,
                            loadingMoreThreads = false,
                            moreProjectIds = action.moreProjectIds,
                            hasMoreChats = action.hasMoreChats,
                            hasMoreProjects = action.hasMoreProjects,
                        )
                    }
            } else state

        is AppAction.ThreadListExpanded ->
            state.updateView(action.hostIdentity) {
                if (action.projectId != null)
                    it.copy(
                        projectThreadLimits =
                            it.projectThreadLimits +
                                (action.projectId to ((it.projectThreadLimits[action.projectId] ?: 5) + 10))
                    )
                else if (action.projects) it.copy(visibleProjectCount = it.visibleProjectCount + 10)
                else it.copy(visibleChatCount = it.visibleChatCount + 10)
            }

        is AppAction.ThreadListSearchChanged ->
            state.updateView(action.hostIdentity) {
                it.copy(
                    threadSearchTerm = action.term,
                    visibleProjectCount = 5,
                    visibleChatCount = 5,
                    projectThreadLimits = emptyMap(),
                )
            }

        is AppAction.ThreadListFailed ->
            state.updateViewIfConnected(action.hostIdentity) {
                it.copy(
                    threadList = LoadPhase.Failed(action.message),
                    loadingMoreThreads = false,
                    notice = action.message,
                )
            }
    }

internal fun reduceHistory(state: AppState, action: AppAction.History, cacheLimits: MobileCacheLimits): AppState =
    when (action) {
        is AppAction.ThreadReadLoading ->
            state.updateViewIfConnected(action.hostIdentity) {
                if (it.selectedThreadId == action.threadId) it.copy(threadDetail = LoadPhase.Loading) else it
            }

        is AppAction.SnapshotReceived ->
            if (state.isConnected(action.hostIdentity)) {
                state
                    .copy(cache = reconcileThreadRead(state.cache, action.hostIdentity, action.result, cacheLimits))
                    .updateView(action.hostIdentity) {
                        if (!action.select) it
                        else
                            it.copy(
                                selectedThreadId = action.result.thread.summary.id,
                                unreadCompletedThreadIds =
                                    it.unreadCompletedThreadIds - action.result.thread.summary.id,
                                newThreadCwd = null,
                                threadDetail = LoadPhase.Ready,
                                notice = null,
                            )
                    }
            } else state

        is AppAction.HistoryLoading ->
            state.updateViewIfConnected(action.hostIdentity) {
                it.copy(loadingHistory = action.loading, notice = action.error)
            }

        is AppAction.HistoryReceived ->
            if (state.isConnected(action.hostIdentity)) {
                val profile = state.cache.profile(action.hostIdentity)
                state.copy(
                    cache =
                        state.cache.copy(
                            profiles =
                                state.cache.profiles +
                                    (action.hostIdentity to
                                        profile.copy(
                                            snapshots = profile.snapshots + (action.thread.summary.id to action.thread)
                                        ))
                        )
                )
            } else state

        is AppAction.ThreadReadFailed ->
            state.updateViewIfConnected(action.hostIdentity) {
                it.copy(threadDetail = LoadPhase.Failed(action.message), notice = action.message)
            }
    }

internal fun reduceTurn(state: AppState, action: AppAction.Turn, cacheLimits: MobileCacheLimits): AppState =
    when (action) {
        is AppAction.ThreadStartFailed ->
            state.updateViewIfConnected(action.hostIdentity) {
                // Keep the empty chat and its draft visible when creation fails.
                it.copy(notice = action.message)
            }

        is AppAction.MessageAccepted ->
            if (state.isConnected(action.hostIdentity)) {
                state
                    .copy(
                        cache =
                            acknowledgeMessage(
                                state.cache,
                                action.hostIdentity,
                                action.threadId,
                                action.submission,
                                cacheLimits,
                            )
                    )
                    .updateView(action.hostIdentity) {
                        it.copy(notice = if (action.submission.turnId == null) QUEUED_TURN_DELIVERY_NOTICE else null)
                    }
            } else state

        is AppAction.TurnFailed -> state.updateViewIfConnected(action.hostIdentity) { it.copy(notice = action.message) }

        is AppAction.InterruptStarted ->
            state.updateViewIfConnected(action.hostIdentity) {
                it.copy(interruptingTurnId = action.turnId, notice = null)
            }

        is AppAction.InterruptFinished -> state.updateView(action.hostIdentity) { it.copy(interruptingTurnId = null) }

        is AppAction.HostEventReceived -> reduceHostEvent(state, action, cacheLimits)
    }

private fun reduceHostEvent(
    state: AppState,
    action: AppAction.HostEventReceived,
    cacheLimits: MobileCacheLimits,
): AppState =
    if (state.isConnected(action.hostIdentity)) {
        val cache = applyLiveEvent(state.cache, action.hostIdentity, action.event, cacheLimits)
        val updated = if (cache === state.cache) state else state.copy(cache = cache)
        val event = action.event
        if (event.kind != ConversationEventKind.TurnStarted && event.kind != ConversationEventKind.TurnCompleted)
            updated
        else
            updated.updateViewIfConnected(action.hostIdentity) { view ->
                when {
                    event.kind == ConversationEventKind.TurnStarted ->
                        view.copy(unreadCompletedThreadIds = view.unreadCompletedThreadIds - event.threadId)
                    event.kind == ConversationEventKind.TurnCompleted &&
                        codexTurnStatus(event.paramsObject.childObject("turn")?.string("status")) ==
                            TurnStatus.Completed -> {
                        val isViewingThread =
                            state.selectedProfileId == action.hostIdentity &&
                                !state.showingPairing &&
                                view.selectedThreadId == event.threadId &&
                                view.threadDetail == LoadPhase.Ready
                        if (isViewingThread) view
                        else view.copy(unreadCompletedThreadIds = view.unreadCompletedThreadIds + event.threadId)
                    }
                    else -> view
                }
            }
    } else {
        state
    }

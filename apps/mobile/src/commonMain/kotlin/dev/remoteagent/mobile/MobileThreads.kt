package dev.remoteagent.mobile

internal suspend fun MobileController.showThreadList(profile: HostProfile) {
    val generation = sessions.currentGeneration(profile.id) ?: return
    dispatchIfCurrent(profile.id, generation) { AppAction.ThreadListOpened(profile.id) }
    listThreads(profile, generation)
}

internal suspend fun MobileController.listThreads(profile: HostProfile) {
    val generation = sessions.currentGeneration(profile.id) ?: return
    listThreads(profile, generation)
}

internal suspend fun MobileController.listThreads(profile: HostProfile, generation: Long, refresh: Boolean = true) {
    if (!isConnected(profile.id, generation)) return
    if (listLoads[profile.id] == generation) {
        pendingListRefresh += profile.id
        return
    }
    listLoads[profile.id] = generation
    try {
        do {
            pendingListRefresh.remove(profile.id)
            if (!refreshThreadList(profile, generation, refresh)) break
        } while (isConnected(profile.id, generation) && profile.id in pendingListRefresh)
    } finally {
        if (listLoads[profile.id] == generation) listLoads.remove(profile.id)
    }
}

internal suspend fun MobileController.expandTaskList(
    profile: HostProfile,
    projects: Boolean,
    projectId: String? = null,
) {
    dispatch(AppAction.ThreadListExpanded(profile.id, projects, projectId))
    val generation = sessions.currentGeneration(profile.id) ?: return
    listThreads(profile, generation, refresh = false)
}

internal suspend fun MobileController.searchTaskList(profile: HostProfile, term: String) {
    if (state.profileViews[profile.id]?.threadSearchTerm == term.trim()) return
    dispatch(AppAction.ThreadListSearchChanged(profile.id, term.trim()))
    val generation = sessions.currentGeneration(profile.id) ?: return
    listThreads(profile, generation, refresh = false)
}

internal fun MobileController.openNewThread(profile: HostProfile, cwd: String) {
    dispatch(AppAction.NewThreadOpened(profile.id, cwd))
}

internal suspend fun MobileController.refreshVisibleState(profile: HostProfile, generation: Long) {
    listThreads(profile, generation)
    state.profileViews[profile.id]?.selectedThreadId?.let { threadId -> readThread(profile, threadId, generation) }
}

internal fun MobileController.threadListQuery(hostIdentity: String): ThreadListQuery {
    val view = state.profileViews[hostIdentity] ?: ProfileViewState()
    return ThreadListQuery(
        view.visibleProjectCount,
        view.visibleChatCount,
        view.projectThreadLimits,
        view.threadSearchTerm,
    )
}

private suspend fun MobileController.refreshThreadList(
    profile: HostProfile,
    generation: Long,
    refresh: Boolean,
): Boolean {
    val query = threadListQuery(profile.id)
    dispatchIfCurrent(profile.id, generation) { AppAction.ThreadListLoading(profile.id, append = !refresh) }
    val result = gateway.codex.listThreads(profile, query)
    if (query != threadListQuery(profile.id)) return true
    return when (result) {
        is GatewayResult.Failure -> {
            dispatchIfCurrent(profile.id, generation) { AppAction.ThreadListFailed(profile.id, result.message) }
            false
        }
        is GatewayResult.Success -> {
            val page = result.value
            dispatchIfCurrent(profile.id, generation) {
                AppAction.ThreadListLoaded(
                    profile.id,
                    page.threads,
                    page.projects,
                    page.moreProjectIds,
                    page.hasMoreChats,
                    page.hasMoreProjects,
                )
            }
            true
        }
    }
}

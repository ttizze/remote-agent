package dev.remoteagent.mobile

import kotlinx.atomicfu.AtomicRef
import kotlinx.atomicfu.getAndUpdate
import kotlinx.atomicfu.update

internal suspend fun AtomicRef<MobileApp>.showThreadList(profile: HostProfile) {
    // Native back navigation must commit even while the transport is disconnected.
    dispatch(AppAction.ThreadListOpened(profile.id))
    val generation = value.effects.sessions.currentGeneration(profile.id) ?: return
    listThreads(profile, generation)
}

internal suspend fun AtomicRef<MobileApp>.listThreads(profile: HostProfile) {
    val generation = value.effects.sessions.currentGeneration(profile.id) ?: return
    listThreads(profile, generation)
}

internal suspend fun AtomicRef<MobileApp>.listThreads(profile: HostProfile, generation: Long, refresh: Boolean = true) {
    if (!isConnected(profile.id, generation)) return
    val previous = getAndUpdate {
        if (it.listLoads[profile.id] == generation) it.copy(pendingListRefresh = it.pendingListRefresh + profile.id)
        else it.copy(listLoads = it.listLoads + (profile.id to generation))
    }
    if (previous.listLoads[profile.id] == generation) return
    try {
        do {
            update { it.copy(pendingListRefresh = it.pendingListRefresh - profile.id) }
            if (!refreshThreadList(profile, generation, refresh)) break
        } while (isConnected(profile.id, generation) && profile.id in value.pendingListRefresh)
    } finally {
        update { if (it.listLoads[profile.id] == generation) it.copy(listLoads = it.listLoads - profile.id) else it }
    }
}

internal suspend fun AtomicRef<MobileApp>.expandTaskList(
    profile: HostProfile,
    projects: Boolean,
    projectId: String? = null,
) {
    dispatch(AppAction.ThreadListExpanded(profile.id, projects, projectId))
    val generation = value.effects.sessions.currentGeneration(profile.id) ?: return
    listThreads(profile, generation, refresh = false)
}

internal suspend fun AtomicRef<MobileApp>.searchTaskList(profile: HostProfile, term: String) {
    if (state.profileViews[profile.id]?.threadSearchTerm == term.trim()) return
    dispatch(AppAction.ThreadListSearchChanged(profile.id, term.trim()))
    val generation = value.effects.sessions.currentGeneration(profile.id) ?: return
    listThreads(profile, generation, refresh = false)
}

internal fun AtomicRef<MobileApp>.openNewThread(profile: HostProfile, cwd: String) {
    dispatch(AppAction.NewThreadOpened(profile.id, cwd))
}

internal suspend fun AtomicRef<MobileApp>.refreshVisibleState(profile: HostProfile, generation: Long) {
    listThreads(profile, generation)
    state.profileViews[profile.id]?.selectedThreadId?.let { threadId -> readThread(profile, threadId, generation) }
}

internal fun AtomicRef<MobileApp>.threadListQuery(hostIdentity: String): ThreadListQuery {
    val view = state.profileViews[hostIdentity] ?: ProfileViewState()
    return ThreadListQuery(
        view.visibleProjectCount,
        view.visibleChatCount,
        view.projectThreadLimits,
        view.threadSearchTerm,
    )
}

private suspend fun AtomicRef<MobileApp>.refreshThreadList(
    profile: HostProfile,
    generation: Long,
    refresh: Boolean,
): Boolean {
    val query = threadListQuery(profile.id)
    dispatchIfCurrent(profile.id, generation) { AppAction.ThreadListLoading(profile.id, append = !refresh) }
    val result =
        value.effects.gateway.command(profile, AgentCommand.ListThreads(query)).mapGateway(::parseThreadListPage)
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

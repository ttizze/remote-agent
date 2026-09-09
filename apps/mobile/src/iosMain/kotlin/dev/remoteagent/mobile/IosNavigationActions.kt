package dev.remoteagent.mobile

import kotlinx.atomicfu.AtomicRef
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.launch

class IosNavigationActions
internal constructor(private val controller: AtomicRef<MobileApp>, private val scope: CoroutineScope) {
    fun refreshTaskList() = withSelectedProfile { profile -> controller.listThreads(profile) }

    fun expandTaskList(projects: Boolean, projectId: String?) = withSelectedProfile { profile ->
        controller.expandTaskList(profile, projects, projectId)
    }

    fun searchTaskList(term: String) = withSelectedProfile { profile -> controller.searchTaskList(profile, term) }

    fun openNewThread(cwd: String) {
        controller.state.selectedProfile?.let { controller.openNewThread(it, cwd) }
    }

    fun openThread(threadId: String) = withSelectedProfile { profile -> controller.readThread(profile, threadId) }

    fun loadOlderHistory(turnId: String?) = withSelectedProfile { profile ->
        controller.loadOlderHistory(profile, turnId)
    }

    fun showThreadList() = withSelectedProfile { profile -> controller.showThreadList(profile) }

    private fun withSelectedProfile(block: suspend (HostProfile) -> Unit) {
        val profile = controller.state.selectedProfile ?: return
        scope.launch { block(profile) }
    }
}

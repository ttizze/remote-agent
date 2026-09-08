package dev.remoteagent.mobile

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch

/**
 * The single interface SwiftUI needs from the shared application module. Calls are intent-level and state is published
 * as immutable view snapshots.
 */
class IosAppController {
    private val dependencies = IosMobileDependencies()
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main)
    private val controller =
        MobileController(dependencies.gateway, dependencies.repository, scope, deferHistoryItemDetails = true)
    private val viewProjector = IosViewStateProjector()
    private var observation: HostEventSubscription? = null
    private var connectionObservation: HostEventSubscription? = null
    private var rendering: Job? = null

    val hosts = IosHostActions(controller, scope)
    val navigation = IosNavigationActions(controller, scope)
    val conversation = IosConversationActions(controller, scope, dependencies)
    val workspace = IosWorkspaceActions(controller, scope, dependencies)

    init {
        IosLifecycleBridge.onRestoreAfterForeground = { controller.restoreConnection(scope) }
        IosLifecycleBridge.onPersistBeforeBackground = { completion ->
            scope.launch {
                try {
                    controller.flushPersistence()
                } finally {
                    completion()
                }
            }
        }
        controller.openApp(scope)
        connectionObservation = controller.maintainConnection(scope)
    }

    fun currentState(): IosAppViewState = viewProjector.project(controller.state)

    fun currentThread(): IosThreadView? {
        viewProjector.project(controller.state)
        return viewProjector.thread
    }

    fun observe(observer: (IosAppViewState, IosThreadView?) -> Unit) {
        observation?.cancel()
        rendering?.cancel()
        val updates = Channel<AppState>(Channel.CONFLATED)
        observation = controller.observe { updates.trySend(it) }
        rendering = scope.launch {
            var previousApp: IosAppViewState? = null
            var previousThread: IosThreadView? = null
            for (first in updates) {
                // Reconcile every event in common state; project only the latest frame.
                delay(VIEW_FRAME_INTERVAL_MS)
                val app = viewProjector.project(updates.tryReceive().getOrNull() ?: first)
                val thread = viewProjector.thread
                if (app !== previousApp || thread !== previousThread) {
                    previousApp = app
                    previousThread = thread
                    observer(app, thread)
                }
            }
        }
    }

    fun close() {
        observation?.cancel()
        observation = null
        rendering?.cancel()
        rendering = null
        connectionObservation?.cancel()
        connectionObservation = null
        IosLifecycleBridge.onRestoreAfterForeground = null
        IosLifecycleBridge.onPersistBeforeBackground = null
        scope.launch {
            try {
                controller.flushPersistence()
            } finally {
                scope.cancel()
            }
        }
    }
}

private const val VIEW_FRAME_INTERVAL_MS = 16L

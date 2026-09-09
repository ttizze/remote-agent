package dev.remoteagent.mobile

import kotlinx.atomicfu.AtomicRef
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext

/** Retires the active transport generation before reducing Disconnected. */
internal fun AtomicRef<MobileApp>.disconnect(hostIdentity: String) {
    value.effects.sessions.currentGeneration(hostIdentity)?.let { generation -> disconnect(hostIdentity, generation) }
}

internal suspend fun AtomicRef<MobileApp>.disconnect(profile: HostProfile) {
    withContext(NonCancellable) {
        val hostIdentity = profile.id
        value.effects.sessions.currentGeneration(hostIdentity) ?: return@withContext
        // Invalidate callbacks immediately, then serialize the native
        // close behind any connection attempt already in flight.
        value.effects.sessions.retireHost(hostIdentity)
        value.effects.sessions.withHostConnection(hostIdentity) {
            // A reconnect requested after this disconnect is the newer
            // user intent and owns the newly installed handle.
            if (value.effects.sessions.currentGeneration(hostIdentity) != null) return@withHostConnection
            value.effects.gateway.disconnect(profile)
            value.effects.eventMutex.withLock { publish(AppAction.Disconnected(hostIdentity)) }
        }
    }
}

internal fun AtomicRef<MobileApp>.disconnect(hostIdentity: String, generation: Long) {
    if (!value.effects.sessions.isCurrent(hostIdentity, generation)) return
    // Retire first: retireHost cancels the subscription before the
    // Disconnected action can reduce UI state or notify observers.
    dispatch(AppAction.Disconnected(hostIdentity))
}

internal suspend fun AtomicRef<MobileApp>.disconnect(profile: HostProfile, generation: Long) {
    if (!value.effects.sessions.isCurrent(profile.id, generation)) return
    value.effects.sessions.retireHost(profile.id)
    value.effects.gateway.disconnect(profile)
    value.effects.eventMutex.withLock { publish(AppAction.Disconnected(profile.id)) }
}

/** Connect, subscribe, and reconcile the visible Host state before returning. */
internal suspend fun AtomicRef<MobileApp>.connect(profile: HostProfile, scope: CoroutineScope) {
    value.effects.sessions.withHostConnection(profile.id) {
        if (
            state.profileViews[profile.id]?.connection == ConnectionPhase.Connected &&
                value.effects.sessions.currentGeneration(profile.id) != null
        )
            return@withHostConnection
        // Start the generation before touching the transport. A previous
        // subscription is retired by the coordinator before connect can
        // deliver callbacks for the new generation.
        val generation =
            value.effects.eventMutex.withLock {
                value.effects.sessions.beginConnection(profile.id).also {
                    publish(AppAction.ConnectStarted(profile.id))
                }
            }
        try {
            val connectionProfile = discoveredConnectionProfile(profile)
            if (!value.effects.sessions.isCurrent(profile.id, generation)) return@withHostConnection

            when (val result = value.effects.gateway.connect(connectionProfile)) {
                is GatewayResult.Failure ->
                    ifCurrent(profile.id, generation) {
                        value.effects.sessions.retireHost(profile.id)
                        value.effects.gateway.disconnect(connectionProfile)
                        dispatch(AppAction.ConnectFailed(profile.id, result.message))
                    }

                is GatewayResult.Success -> {
                    finishConnection(connectionProfile, generation, scope)
                }
            }
        } catch (cancelled: CancellationException) {
            // A blocking platform connect may install its handle just as
            // the caller is cancelled. Close it before the per-Host
            // connection mutex admits a reconnect.
            withContext(NonCancellable) {
                value.effects.sessions.retireHost(profile.id)
                value.effects.gateway.disconnect(profile)
                value.effects.eventMutex.withLock { publish(AppAction.Disconnected(profile.id)) }
            }
            throw cancelled
        }
    }
}

private val THREAD_WATCH_METHODS = setOf("host/thread/changed", "host/thread/watchFailed")
private val THREAD_LIST_INVALIDATING_METHODS =
    setOf("thread/started", "thread/name/updated", "thread/project/updated", "thread/archived", "thread/unarchived")

private suspend fun AtomicRef<MobileApp>.discoveredConnectionProfile(profile: HostProfile): HostProfile {
    return when (val discovered = value.effects.gateway.discover(profile)) {
        is GatewayResult.Success ->
            discovered.value
                .map(String::trim)
                .filter(String::isNotEmpty)
                .distinct()
                .takeIf(List<String>::isNotEmpty)
                ?.let { relayUrls ->
                    dispatch(AppAction.AddressesDiscovered(profile.id, relayUrls))
                    profile.copy(relayUrl = relayUrls.first())
                } ?: profile
        is GatewayResult.Failure -> profile
    }
}

private fun AtomicRef<MobileApp>.receiveHostMessage(
    profile: HostProfile,
    generation: Long,
    scope: CoroutineScope,
    message: RawCodexMessage,
) {
    val notification = message as? RawCodexMessage.Notification
    val refreshProjects = notification?.method == "project/changed"
    val refreshThreads = notification?.method in THREAD_LIST_INVALIDATING_METHODS
    val threadWatchEvent = notification?.method in THREAD_WATCH_METHODS
    val event = message.takeUnless { refreshProjects || refreshThreads || threadWatchEvent }
    // Capture the generation and read barrier before
    // yielding. Native transports may invoke this
    // callback synchronously while a read is in flight.
    val current = value.effects.sessions.isCurrent(profile.id, generation)
    val buffered =
        if (current) {
            event?.let { value.effects.sessions.bufferEvent(profile.id, it).isHeld } == true
        } else {
            false
        }
    // Platform controllers provide their serialized
    // application/UI scope. Never reduce state
    // inline on a native poller thread.
    scope.launch {
        value.effects.eventMutex.withLock {
            if (value.effects.sessions.isCurrent(profile.id, generation) && event != null && !buffered) {
                dispatch(AppAction.HostEventReceived(profile.id, event))
            }
        }
        if (value.effects.sessions.isCurrent(profile.id, generation)) {
            if (refreshProjects || refreshThreads) listThreads(profile, generation)
            if (threadWatchEvent && notification != null) receiveThreadWatchNotification(profile.id, notification)
        }
    }
}

private suspend fun AtomicRef<MobileApp>.finishConnection(
    profile: HostProfile,
    generation: Long,
    scope: CoroutineScope,
) {
    if (!value.effects.sessions.isCurrent(profile.id, generation)) {
        value.effects.gateway.disconnect(profile)
        return
    }
    dispatch(AppAction.ConnectSucceeded(profile.id))
    val subscription =
        try {
            value.effects.gateway.subscribeRaw(
                profile = profile,
                onMessage = { message -> receiveHostMessage(profile, generation, scope, message) },
                onClosed = { scope.launch { disconnect(profile, generation) } },
            )
        } catch (cancelled: CancellationException) {
            throw cancelled
        } catch (_: Throwable) {
            disconnect(profile, generation)
            null
        }
    if (subscription != null) {
        if (value.effects.sessions.installSubscription(profile.id, generation, subscription)) {
            refreshVisibleState(profile, generation)
        } else value.effects.gateway.disconnect(profile)
    }
}

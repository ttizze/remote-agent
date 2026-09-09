package dev.remoteagent.mobile

import kotlinx.atomicfu.AtomicRef
import kotlinx.atomicfu.atomic
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock

/** Mobile lifecycle state. A new value invalidates old callbacks before resources are cancelled. */
internal data class HostSessions(
    val maxReadBytes: Int,
    val hosts: Map<String, HostSession> = emptyMap(),
    val nextReadToken: Long = 0,
)

internal data class HostSession(
    val generation: Long = 0,
    val active: Boolean = false,
    val subscription: HostEventSubscription? = null,
    val connectionMutex: Mutex = Mutex(),
    val reads: Map<String, HostReadBuffer> = emptyMap(),
)

internal fun hostSessions(cacheLimits: MobileCacheLimits = MobileCacheLimits()): AtomicRef<HostSessions> {
    val bytes = cacheLimits.maxApproximateBytes.coerceAtMost(MAX_READ_BUFFERED_BYTES)
    require(bytes > 0) { "maxCacheBytes must be positive" }
    return atomic(HostSessions(bytes))
}

/** Only this effect seam publishes a transition. The transition may be retried and cannot perform I/O. */
internal inline fun <T> AtomicRef<HostSessions>.changeSessions(transition: (HostSessions) -> Pair<HostSessions, T>): T {
    while (true) {
        val before = value
        val (after, result) = transition(before)
        if (after === before || compareAndSet(before, after)) return result
    }
}

internal fun AtomicRef<HostSessions>.beginConnection(hostIdentity: String): Long {
    val previous = changeSessions { state ->
        val host = state.hosts[hostIdentity] ?: HostSession()
        state.withHost(
            hostIdentity,
            host.copy(
                generation = nextGeneration(host.generation),
                active = true,
                subscription = null,
                reads = emptyMap(),
            ),
        ) to host
    }
    previous.subscription?.cancel()
    return nextGeneration(previous.generation)
}

internal fun AtomicRef<HostSessions>.retireHost(hostIdentity: String) {
    val previous = changeSessions { state ->
        val host = state.hosts[hostIdentity]
        if (host == null) state to null
        else
            state.withHost(
                hostIdentity,
                host.copy(
                    generation = nextGeneration(host.generation),
                    active = false,
                    subscription = null,
                    reads = emptyMap(),
                ),
            ) to host
    }
    previous?.subscription?.cancel()
}

internal fun AtomicRef<HostSessions>.isCurrent(hostIdentity: String, generation: Long): Boolean =
    value.hosts[hostIdentity]?.let { it.active && it.generation == generation } == true

internal fun AtomicRef<HostSessions>.currentGeneration(hostIdentity: String): Long? =
    value.hosts[hostIdentity]?.takeIf { it.active }?.generation

internal fun AtomicRef<HostSessions>.installSubscription(
    hostIdentity: String,
    generation: Long,
    subscription: HostEventSubscription,
): Boolean {
    val previous = changeSessions { state ->
        val host = state.hosts[hostIdentity]?.takeIf { it.active && it.generation == generation }
        if (host == null) state to null
        else state.withHost(hostIdentity, host.copy(subscription = subscription)) to host
    }
    // Cancellation may call back synchronously. State is already installed and no lock is held.
    if (previous == null) subscription.cancel()
    else if (previous.subscription !== subscription) previous.subscription?.cancel()
    return previous != null
}

internal suspend fun <T> AtomicRef<HostSessions>.withHostConnection(hostIdentity: String, block: suspend () -> T): T {
    val host = changeSessions { state ->
        val existing = state.hosts[hostIdentity]
        if (existing != null) state to existing else HostSession().let { state.withHost(hostIdentity, it) to it }
    }
    return host.connectionMutex.withLock { block() }
}

internal fun HostSessions.withHost(id: String, host: HostSession): HostSessions = copy(hosts = hosts + (id to host))

private const val MAX_READ_BUFFERED_BYTES = 256 * 1024

internal fun nextGeneration(previous: Long): Long =
    if (previous == Long.MAX_VALUE) {
        error("Host session generation exhausted")
    } else {
        previous + 1L
    }

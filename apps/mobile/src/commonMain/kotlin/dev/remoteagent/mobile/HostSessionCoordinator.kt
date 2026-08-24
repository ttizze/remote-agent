package dev.remoteagent.mobile

import kotlinx.atomicfu.locks.SynchronizedObject
import kotlinx.atomicfu.locks.synchronized
import kotlinx.coroutines.sync.Mutex

/**
 * Coordinates the short-lived state associated with one mobile/Host session.
 *
 * The coordinator deliberately does not own transport or UI state.  A caller
 * obtains a generation from [beginConnection], uses that generation at every
 * callback boundary, and discards work for generations for which [isCurrent]
 * is false.  Maps are protected with an atomicfu lock because subscriptions
 * can invoke their callback synchronously from a native transport thread.
 * Connect operations use a separate coroutine [Mutex] per host, so a slow
 * connection to one Host cannot serialize a connection to another Host.
 */
internal class HostSessionCoordinator(
    cacheLimits: MobileCacheLimits = MobileCacheLimits(),
) {
    private val coordinationLock = SynchronizedObject()
    private val hosts = mutableMapOf<String, HostState>()
    private val readBuffers = mutableMapOf<ReadKey, ReadBuffer>()
    private var nextReadToken = 0L
    private val maxReadBytes = cacheLimits.maxApproximateBytes.coerceAtMost(MaxReadBufferedBytes)

    init {
        require(maxReadBytes > 0) { "maxCacheBytes must be positive" }
    }

    /**
     * Starts a new connection generation for [hostIdentity].  The previous
     * subscription is detached while the coordination lock is held and
     * cancelled only after that lock has been released.
     */
    fun beginConnection(hostIdentity: String): Long {
        var previousSubscription: HostEventSubscription? = null
        val generation = synchronized(coordinationLock) {
            val host = hosts.getOrPut(hostIdentity) { HostState() }
            host.generation = nextGeneration(host.generation)
            host.active = true
            previousSubscription = host.subscription
            host.subscription = null
            // A new generation makes all old read completions stale. Remove
            // them now so a long-lived coordinator cannot retain old events.
            readBuffers.keys.removeAll { it.hostIdentity == hostIdentity }
            host.generation
        }
        previousSubscription?.cancel()
        return generation
    }

    /** Invalidates the current generation and detaches its subscription. */
    fun retireHost(hostIdentity: String) {
        var previousSubscription: HostEventSubscription? = null
        synchronized(coordinationLock) {
            val host = hosts[hostIdentity] ?: return@synchronized
            host.generation = nextGeneration(host.generation)
            host.active = false
            previousSubscription = host.subscription
            host.subscription = null
            readBuffers.keys.removeAll { it.hostIdentity == hostIdentity }
        }
        previousSubscription?.cancel()
    }

    /**
     * Returns true only while [generation] is the active generation for the
     * Host. A retired Host is never current, even if its generation matches a
     * token captured before retirement.
     */
    fun isCurrent(hostIdentity: String, generation: Long): Boolean = synchronized(coordinationLock) {
        hosts[hostIdentity]?.let { it.active && it.generation == generation } == true
    }

    /** Installs a subscription if [generation] is still current. */
    fun installSubscription(
        hostIdentity: String,
        generation: Long,
        subscription: HostEventSubscription,
    ): Boolean {
        var subscriptionToCancel: HostEventSubscription? = null
        val installed = synchronized(coordinationLock) {
            val host = hosts[hostIdentity]
            if (host == null || !host.active || host.generation != generation) {
                subscriptionToCancel = subscription
                false
            } else {
                subscriptionToCancel = host.subscription
                host.subscription = subscription
                true
            }
        }
        // This is intentionally outside synchronized(coordinationLock). A
        // transport's cancel callback may synchronously call back into this
        // coordinator.
        if (!installed) {
            subscription.cancel()
        } else if (subscriptionToCancel !== subscription) {
            subscriptionToCancel?.cancel()
        }
        return installed
    }

    /**
     * Serializes connection work for one Host. Different Host identities get
     * different Mutex instances and can therefore progress independently.
     */
    suspend fun <T> withHostConnection(
        hostIdentity: String,
        block: suspend () -> T,
    ): T = hostConnectionMutex(hostIdentity).withSuspendingLock(block)

    /**
     * Starts a read barrier for the current Host generation. A second read of
     * the same Host/thread/generation replaces the first barrier; the first
     * token is consequently stale and cannot consume the second completion.
     */
    /** Starts a read barrier only when [generation] is still current. */
    fun beginRead(hostIdentity: String, threadId: String, generation: Long): HostReadToken? =
        synchronized(coordinationLock) {
            val host = hosts[hostIdentity] ?: return@synchronized null
            if (!host.active || host.generation != generation) return@synchronized null
            beginReadLocked(hostIdentity, threadId, generation)
        }

    /**
     * Buffers an event for a read barrier. [HostReadBufferResult.Overflowed]
     * is the retry signal: the caller must discard the read result and issue a
     * fresh read instead of applying an incomplete event stream.
     */
    fun bufferEvent(token: HostReadToken, event: ThreadEvent): HostReadBufferResult =
        synchronized(coordinationLock) {
            val key = ReadKey(token.hostIdentity, token.threadId, token.generation)
            val host = hosts[key.hostIdentity]
            val read = readBuffers[key]
            if (host == null || !host.active || host.generation != key.generation) {
                if (read?.token == token) readBuffers.remove(key)
                return@synchronized HostReadBufferResult.Stale
            }
            if (read == null || read.token != token) return@synchronized HostReadBufferResult.Stale
            if (event.threadId != key.threadId) return@synchronized HostReadBufferResult.NotBuffered
            if (read.overflowed) return@synchronized HostReadBufferResult.Overflowed

            val eventBytes = event.sessionApproximateBytes()
            if (
                read.events.size >= MaxReadBufferedEvents ||
                eventBytes > maxReadBytes - read.approximateBytes
            ) {
                read.overflowed = true
                return@synchronized HostReadBufferResult.Overflowed
            }
            read.events += event
            read.approximateBytes += eventBytes
            HostReadBufferResult.Buffered
        }

    /**
     * Convenience form for callbacks that identify an event by Host/thread.
     * It resolves the active token synchronously, then applies the same stale
     * generation checks as the token form.
     */
    fun bufferEvent(hostIdentity: String, event: ThreadEvent): HostReadBufferResult {
        val token = synchronized(coordinationLock) {
            val host = hosts[hostIdentity]
            if (host == null || !host.active) {
                null
            } else {
                readBuffers[ReadKey(hostIdentity, event.threadId, host.generation)]?.token
            }
        } ?: return HostReadBufferResult.NotBuffered
        return bufferEvent(token, event)
    }

    /** Completes a read and drains its held events, or returns null if stale. */
    fun finishRead(token: HostReadToken): HostReadCompletion? = synchronized(coordinationLock) {
        val key = ReadKey(token.hostIdentity, token.threadId, token.generation)
        val read = readBuffers[key]
        if (read?.token != token) return@synchronized null
        readBuffers.remove(key)
        val host = hosts[key.hostIdentity]
        if (host == null || !host.active || host.generation != key.generation) {
            return@synchronized null
        }
        HostReadCompletion(read.events.toList(), read.overflowed)
    }

    /** Returns the per-Host Mutex used by [withHostConnection]. */
    internal fun hostConnectionMutex(hostIdentity: String): Mutex = synchronized(coordinationLock) {
        hosts.getOrPut(hostIdentity) { HostState() }.connectionMutex
    }

    private fun beginReadLocked(
        hostIdentity: String,
        threadId: String,
        generation: Long,
    ): HostReadToken {
        val key = ReadKey(hostIdentity, threadId, generation)
        readBuffers.remove(key)
        nextReadToken = nextGeneration(nextReadToken)
        val token = HostReadToken(hostIdentity, threadId, generation, nextReadToken)
        readBuffers[key] = ReadBuffer(token)
        return token
    }

    private fun nextGeneration(previous: Long): Long =
        if (previous == Long.MAX_VALUE) {
            error("Host session generation exhausted")
        } else {
            previous + 1L
        }

    private suspend fun <T> Mutex.withSuspendingLock(block: suspend () -> T): T {
        lock()
        return try {
            block()
        } finally {
            unlock()
        }
    }

    private class HostState(
        val connectionMutex: Mutex = Mutex(),
        var generation: Long = 0L,
        var active: Boolean = false,
        var subscription: HostEventSubscription? = null,
    )

    private data class ReadKey(
        val hostIdentity: String,
        val threadId: String,
        val generation: Long,
    )

    private data class ReadBuffer(
        val token: HostReadToken,
        val events: MutableList<ThreadEvent> = mutableListOf(),
        var approximateBytes: Int = 0,
        var overflowed: Boolean = false,
    )

    private companion object {
        const val MaxReadBufferedEvents = 256
        const val MaxReadBufferedBytes = 256 * 1024
    }
}

/** Opaque identity for one Host/thread read within one connection generation. */
internal class HostReadToken internal constructor(
    val hostIdentity: String,
    val threadId: String,
    val generation: Long,
    val sequence: Long,
)

/** Result of attempting to hold a live event behind a read barrier. */
internal enum class HostReadBufferResult(
    /** Whether a caller should retry the native read. */
    val retryRequired: Boolean,
    /** Whether the event was held (including an overflowed barrier). */
    val isHeld: Boolean,
) {
    Buffered(retryRequired = false, isHeld = true),
    NotBuffered(retryRequired = false, isHeld = false),
    Overflowed(retryRequired = true, isHeld = true),
    Stale(retryRequired = false, isHeld = false),
}

/** Events held while a native thread/read response is in flight. */
internal data class HostReadCompletion(
    val events: List<ThreadEvent>,
    val overflowed: Boolean,
) {
    val retryRequired: Boolean get() = overflowed
}

private fun ThreadEvent.sessionApproximateBytes(): Int =
    (toString().length.toLong() * 2L + 8L).coerceAtMost(Int.MAX_VALUE.toLong()).toInt()

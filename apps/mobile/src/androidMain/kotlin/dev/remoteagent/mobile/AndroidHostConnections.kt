package dev.remoteagent.mobile

/** Pollers and subscriptions are retired atomically with their native handle. */
internal class AndroidHostConnections {
    private val stateLock = Any()
    private val handles = mutableMapOf<String, AndroidNativeHandle>()
    private val subscriptions = mutableMapOf<String, MutableSet<(RawCodexMessage) -> Unit>>()
    private val pollers = mutableMapOf<String, Thread>()

    fun replace(hostIdentity: String, handle: AndroidNativeHandle) {
        synchronized(stateLock) { handles[hostIdentity] = handle }
    }

    fun retire(hostIdentity: String) {
        val (poller, handle) =
            synchronized(stateLock) {
                val currentHandle = handles.remove(hostIdentity)
                subscriptions.remove(hostIdentity)
                pollers.remove(hostIdentity) to currentHandle
            }
        poller?.interrupt()
        handle?.close()
    }

    fun <T> withHandle(profile: HostProfile, block: (Long) -> T): T {
        val handle = synchronized(stateLock) { handles[profile.id] } ?: error("Host is not connected")
        val readLock = handle.lifetime.readLock()
        readLock.lock()
        return try {
            check(synchronized(stateLock) { handles[profile.id] } === handle && !handle.closed) {
                "Host is not connected"
            }
            block(handle.pointer)
        } finally {
            readLock.unlock()
        }
    }

    /** One poller drains the ordered native event queue and fans messages out to the current subscribers. */
    fun subscribe(
        profile: HostProfile,
        onMessage: (RawCodexMessage) -> Unit,
        onClosed: (String) -> Unit,
    ): HostEventSubscription {
        val subscribed =
            synchronized(stateLock) {
                val handle = handles[profile.id] ?: return@synchronized false
                subscriptions.getOrPut(profile.id) { linkedSetOf() }.add(onMessage)
                if (pollers[profile.id] == null) {
                    pollers[profile.id] =
                        Thread { pollLoop(profile.id, handle, onClosed) }
                            .apply {
                                isDaemon = true
                                start()
                            }
                }
                true
            }
        if (!subscribed) return HostEventSubscription {}
        return HostEventSubscription {
            synchronized(stateLock) {
                subscriptions[profile.id]?.remove(onMessage)
                if (subscriptions[profile.id].isNullOrEmpty()) {
                    subscriptions.remove(profile.id)
                    pollers.remove(profile.id)?.interrupt()
                }
            }
        }
    }

    private fun pollLoop(hostIdentity: String, handle: AndroidNativeHandle, onClosed: (String) -> Unit) {
        while (!Thread.currentThread().isInterrupted) {
            if (!pollOnce(hostIdentity, handle, onClosed)) break
        }
    }

    private fun pollOnce(hostIdentity: String, handle: AndroidNativeHandle, onClosed: (String) -> Unit): Boolean {
        if (synchronized(stateLock) { handles[hostIdentity] } !== handle) return false
        var active = true
        val readLock = handle.lifetime.readLock()
        readLock.lock()
        val current =
            try {
                active = synchronized(stateLock) { handles[hostIdentity] } === handle && !handle.closed
                if (active) NativeHostTransport.nextEvent(handle.pointer) else null
            } catch (failure: IllegalStateException) {
                runCatching { onClosed(failure.message ?: "PC Host connection closed") }
                active = false
                null
            } finally {
                readLock.unlock()
            }
        if (active) {
            if (current != null) publish(hostIdentity, handle, current)
            else
                try {
                    Thread.sleep(POLL_INTERVAL_MS)
                } catch (_: InterruptedException) {
                    active = false
                }
        }
        return active
    }

    private fun publish(hostIdentity: String, handle: AndroidNativeHandle, raw: String) {
        parseRawCodexMessage(raw)?.let { message ->
            val listeners =
                synchronized(stateLock) {
                    if (handles[hostIdentity] === handle) subscriptions[hostIdentity]?.toList().orEmpty()
                    else emptyList()
                }
            listeners.forEach { listener -> runCatching { listener(message) } }
        }
    }
}

private const val POLL_INTERVAL_MS = 50L

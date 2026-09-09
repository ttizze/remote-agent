package dev.remoteagent.mobile

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.Job
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.delay
import kotlinx.coroutines.ensureActive
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.getAndUpdate
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch

/** Mobile subscriptions are immutable values; Rust owns each native handle's lifetime. */
internal data class NativeHostConnection(
    val handle: Long,
    val listeners: Set<NativeHostListener> = emptySet(),
    val poller: Job? = null,
)

internal data class NativeHostListener(val message: (RawCodexMessage) -> Unit, val closed: (String) -> Unit)

internal fun MutableStateFlow<Map<String, NativeHostConnection>>.replaceNativeHost(
    host: String,
    handle: Long?,
    close: (Long) -> Unit,
) {
    val previous = getAndUpdate { if (handle == null) it - host else it + (host to NativeHostConnection(handle)) }[host]
    previous?.poller?.cancel()
    previous?.let { close(it.handle) }
}

internal fun MutableStateFlow<Map<String, NativeHostConnection>>.nativeHandle(host: String): Long =
    value[host]?.handle ?: error("Host is not connected")

/** One poller drains the wire queue, then fans out to the current immutable listener set. */
internal fun MutableStateFlow<Map<String, NativeHostConnection>>.subscribeNativeHost(
    host: String,
    scope: CoroutineScope,
    nextEvent: (Long) -> String?,
    onMessage: (RawCodexMessage) -> Unit,
    onClosed: (String) -> Unit,
): HostEventSubscription {
    val handle = value[host]?.handle ?: return HostEventSubscription {}
    val listener = NativeHostListener(onMessage, onClosed)
    val candidate = scope.launch(start = CoroutineStart.LAZY) { pollNativeHost(host, handle, nextEvent) }
    val previous =
        getAndUpdate { state ->
            val current = state[host]
            if (current?.handle != handle) state
            else
                state +
                    (host to
                        current.copy(listeners = current.listeners + listener, poller = current.poller ?: candidate))
        }[host]
    if (previous?.handle == handle && previous.poller == null) candidate.start() else candidate.cancel()
    return HostEventSubscription {
        update { state ->
            val current = state[host]
            if (current?.handle != handle) state
            else {
                val listeners = current.listeners - listener
                state + (host to current.copy(listeners = listeners))
            }
        }
    }
}

private suspend fun MutableStateFlow<Map<String, NativeHostConnection>>.pollNativeHost(
    host: String,
    handle: Long,
    nextEvent: (Long) -> String?,
) {
    try {
        while (value[host]?.handle == handle) {
            currentCoroutineContext().ensureActive()
            val raw = if (value[host]?.listeners?.isNotEmpty() == true) nextEvent(handle) else null
            if (raw == null) delay(POLL_INTERVAL_MS) else publishNativeMessage(host, handle, raw)
        }
    } catch (cancelled: CancellationException) {
        throw cancelled
    } catch (failure: IllegalStateException) {
        nativeHostClosed(host, handle, failure)
    } catch (failure: IllegalArgumentException) {
        nativeHostClosed(host, handle, failure)
    } finally {
        val job = currentCoroutineContext()[Job]
        update { state ->
            val current = state[host]
            if (current?.handle == handle && current.poller === job) state + (host to current.copy(poller = null))
            else state
        }
    }
}

private fun MutableStateFlow<Map<String, NativeHostConnection>>.publishNativeMessage(
    host: String,
    handle: Long,
    raw: String,
) {
    val message = parseRawCodexMessage(raw) ?: return
    value[host]?.takeIf { it.handle == handle }?.listeners?.forEach { runCatching { it.message(message) } }
}

private fun MutableStateFlow<Map<String, NativeHostConnection>>.nativeHostClosed(
    host: String,
    handle: Long,
    failure: Exception,
) {
    value[host]
        ?.takeIf { it.handle == handle }
        ?.listeners
        ?.forEach { runCatching { it.closed(failure.message ?: "PC Host connection closed") } }
}

private const val POLL_INTERVAL_MS = 50L

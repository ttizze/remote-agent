package dev.remoteagent.mobile

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.delay
import kotlinx.coroutines.ensureActive
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.getAndUpdate
import kotlinx.coroutines.launch

internal fun MutableStateFlow<Map<String, Long>>.replaceNativeHost(
    host: String,
    handle: Long?,
    close: (Long) -> Unit,
) {
    getAndUpdate { if (handle == null) it - host else it + (host to handle) }[host]?.let(close)
}

internal fun MutableStateFlow<Map<String, Long>>.nativeHandle(host: String): Long =
    value[host] ?: error("Host is not connected")

/** HostSessions owns the single subscription for each Host and cancels it before replacement. */
internal fun MutableStateFlow<Map<String, Long>>.subscribeNativeHost(
    host: String,
    scope: CoroutineScope,
    nextEvent: (Long) -> String?,
    onMessage: (RawCodexMessage) -> Unit,
    onClosed: (String) -> Unit,
): HostEventSubscription {
    val handle = value[host] ?: return HostEventSubscription {}
    fun closed(message: String?) {
        if (value[host] == handle) runCatching { onClosed(message ?: "PC Host connection closed") }
    }
    val job = scope.launch {
        try {
            while (value[host] == handle) {
                currentCoroutineContext().ensureActive()
                val raw = nextEvent(handle)
                if (raw == null) delay(POLL_INTERVAL_MS)
                else if (value[host] == handle) {
                    parseRawCodexMessage(raw)?.let { message -> runCatching { onMessage(message) } }
                }
            }
        } catch (cancelled: CancellationException) {
            throw cancelled
        } catch (failure: IllegalStateException) {
            closed(failure.message)
        } catch (failure: IllegalArgumentException) {
            closed(failure.message)
        }
    }
    return HostEventSubscription { job.cancel() }
}

private const val POLL_INTERVAL_MS = 50L

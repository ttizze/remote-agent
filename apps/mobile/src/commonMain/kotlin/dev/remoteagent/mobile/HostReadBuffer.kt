package dev.remoteagent.mobile

import kotlinx.atomicfu.AtomicRef
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive

internal data class HostReadBuffer(
    val token: HostReadToken,
    val latest: HeldHostEvent? = null,
    val count: Int = 0,
    val approximateBytes: Int = 0,
    val overflowed: Boolean = false,
)

/** Structural sharing appends a notification without copying the already buffered events or their bodies. */
internal data class HeldHostEvent(val event: RawCodexMessage, val previous: HeldHostEvent?)

internal fun AtomicRef<HostSessions>.beginRead(
    hostIdentity: String,
    threadId: String,
    generation: Long,
): HostReadToken? = changeSessions { state ->
    val host = state.hosts[hostIdentity]?.takeIf { it.active && it.generation == generation }
    if (host == null) state to null
    else {
        val sequence = nextGeneration(state.nextReadToken)
        val token = HostReadToken(hostIdentity, threadId, generation, sequence)
        state.copy(
            nextReadToken = sequence,
            hosts = state.hosts + (hostIdentity to host.copy(reads = host.reads + (threadId to HostReadBuffer(token)))),
        ) to token
    }
}

internal fun AtomicRef<HostSessions>.bufferEvent(token: HostReadToken, event: RawCodexMessage): HostReadBufferResult =
    changeSessions {
        it.holdEvent(token, event)
    }

internal fun AtomicRef<HostSessions>.bufferEvent(hostIdentity: String, event: RawCodexMessage): HostReadBufferResult =
    changeSessions { state ->
        val token = state.hosts[hostIdentity]?.takeIf { it.active }?.reads?.get(event.threadId)?.token
        if (token == null) state to HostReadBufferResult.NotBuffered else state.holdEvent(token, event)
    }

private fun HostSessions.holdEvent(
    token: HostReadToken,
    event: RawCodexMessage,
): Pair<HostSessions, HostReadBufferResult> {
    val host = hosts[token.hostIdentity]
    val read = host?.reads?.get(token.threadId)
    return when {
        host?.active != true || host.generation != token.generation || read?.token !== token ->
            this to HostReadBufferResult.Stale
        event.threadId != token.threadId -> this to HostReadBufferResult.NotBuffered
        read.overflowed -> this to HostReadBufferResult.Overflowed
        else -> {
            val bytes = event.sessionApproximateBytes()
            val overflowed = read.count >= MAX_READ_BUFFERED_EVENTS || bytes > maxReadBytes - read.approximateBytes
            val updated =
                if (overflowed) read.copy(overflowed = true)
                else
                    read.copy(
                        latest = HeldHostEvent(event, read.latest),
                        count = read.count + 1,
                        approximateBytes = read.approximateBytes + bytes,
                    )
            withHost(token.hostIdentity, host.copy(reads = host.reads + (token.threadId to updated))) to
                if (overflowed) HostReadBufferResult.Overflowed else HostReadBufferResult.Buffered
        }
    }
}

internal fun AtomicRef<HostSessions>.finishRead(token: HostReadToken): HostReadCompletion? {
    val read =
        changeSessions { state ->
            val host = state.hosts[token.hostIdentity]
            val pending = host?.reads?.get(token.threadId)
            if (host?.active != true || host.generation != token.generation || pending?.token !== token) state to null
            else state.withHost(token.hostIdentity, host.copy(reads = host.reads - token.threadId)) to pending
        } ?: return null
    val events = ArrayList<RawCodexMessage>(read.count)
    var held = read.latest
    while (held != null) {
        events.add(held.event)
        held = held.previous
    }
    events.reverse()
    return HostReadCompletion(events, read.overflowed)
}

/** Opaque identity for one Host/thread read within one connection generation. */
internal class HostReadToken
internal constructor(val hostIdentity: String, val threadId: String, val generation: Long, val sequence: Long)

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
internal data class HostReadCompletion(val events: List<RawCodexMessage>, val overflowed: Boolean) {
    val retryRequired: Boolean
        get() = overflowed
}

private fun RawCodexMessage.sessionApproximateBytes(): Int {
    val extensions =
        when (this) {
            is RawCodexMessage.Notification -> extensions
            is RawCodexMessage.ServerRequest -> extensions
        }
    val idBytes = if (this is RawCodexMessage.ServerRequest) id.approximateBytes() else 0
    return (params.approximateBytes() +
            extensions.approximateBytes() +
            idBytes +
            method.length * CHAR_BYTES +
            EVENT_OVERHEAD_BYTES)
        .coerceAtMost(Int.MAX_VALUE.toLong())
        .toInt()
}

private fun JsonElement.approximateBytes(): Long =
    EVENT_OVERHEAD_BYTES +
        when (this) {
            is JsonObject -> entries.sumOf { (key, value) -> key.length * CHAR_BYTES + value.approximateBytes() }
            is JsonArray -> sumOf { it.approximateBytes() }
            is JsonPrimitive -> content.length * CHAR_BYTES
        }

private const val MAX_READ_BUFFERED_EVENTS = 256
private const val CHAR_BYTES = 2L
private const val EVENT_OVERHEAD_BYTES = 8L

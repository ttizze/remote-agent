package dev.remoteagent.mobile

import kotlinx.cinterop.ByteVar
import kotlinx.cinterop.CPointerVar
import kotlinx.cinterop.ExperimentalForeignApi
import kotlinx.cinterop.alloc
import kotlinx.cinterop.memScoped
import kotlinx.cinterop.ptr
import kotlinx.cinterop.value
import mobile_client.mobile_client_account_transition
import mobile_client.mobile_client_classify_event
import mobile_client.mobile_client_conversation_transition
import mobile_client.mobile_client_present_conversation

@OptIn(ExperimentalForeignApi::class)
internal actual fun nativeConversationPresentation(request: String): String = memScoped {
    val failure = alloc<CPointerVar<ByteVar>>()
    failure.value = null
    val result = mobile_client_present_conversation(request, failure.ptr) ?: error(takeError(failure.value))
    takeString(result)
}

@OptIn(ExperimentalForeignApi::class)
internal actual fun nativeClassifyEvent(method: String): Int = mobile_client_classify_event(method, 0).toInt()

@OptIn(ExperimentalForeignApi::class)
internal actual fun nativeConversationTransition(
    kind: Int,
    status: Int,
    currentStatus: Int,
    item: Int,
    flags: Int,
): Int =
    mobile_client_conversation_transition(
            kind.toUInt(),
            status.toUInt(),
            currentStatus.toUInt(),
            item.toUInt(),
            flags.toUInt(),
        )
        .toInt()

@OptIn(ExperimentalForeignApi::class)
internal actual fun nativeAccountTransition(event: Int, flags: Int): Int =
    mobile_client_account_transition(event.toUInt(), flags.toUInt()).toInt()

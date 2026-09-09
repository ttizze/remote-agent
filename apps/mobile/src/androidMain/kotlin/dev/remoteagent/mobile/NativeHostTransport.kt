package dev.remoteagent.mobile

/** JNI declarations for the Rust mobile-client. */
object NativeHostTransport {
    init {
        System.loadLibrary("mobile_client")
    }

    external fun generateDeviceKey(): String

    external fun connect(configJson: String, devicePkcs8: ByteArray): Long

    external fun transfer(handle: Long, paramsJson: String): String

    external fun agentCommand(handle: Long, commandJson: String): String

    external fun nextEvent(handle: Long): String?

    external fun close(handle: Long)
}

internal object NativeConversation {
    init {
        NativeHostTransport
    }

    external fun presentConversation(requestJson: String): String

    external fun classifyEvent(method: String): Int

    external fun conversationTransition(kind: Int, status: Int, currentStatus: Int, item: Int, flags: Int): Int
}

internal actual fun nativeConversationPresentation(request: String): String =
    NativeConversation.presentConversation(request)

internal actual fun nativeClassifyEvent(method: String): Int = NativeConversation.classifyEvent(method)

internal actual fun nativeConversationTransition(
    kind: Int,
    status: Int,
    currentStatus: Int,
    item: Int,
    flags: Int,
): Int = NativeConversation.conversationTransition(kind, status, currentStatus, item, flags)

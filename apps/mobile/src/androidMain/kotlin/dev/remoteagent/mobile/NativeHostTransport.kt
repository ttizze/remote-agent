package dev.remoteagent.mobile

/** JNI declarations for the Rust mobile-client. */
object NativeHostTransport {
    init {
        System.loadLibrary("mobile_client")
    }

    external fun presentConversation(requestJson: String): String

    external fun generateDeviceKey(): String

    external fun connect(configJson: String, devicePkcs8: ByteArray): Long

    external fun transfer(handle: Long, paramsJson: String): String

    external fun request(handle: Long, method: String, paramsJson: String): String

    external fun nextNotification(handle: Long): String?

    external fun nextServerRequest(handle: Long): String?

    external fun respondResult(handle: Long, requestIdJson: String, resultJson: String): Boolean

    external fun respondError(handle: Long, requestIdJson: String, errorJson: String): Boolean

    external fun close(handle: Long)
}

internal actual fun nativeConversationPresentation(request: String): String =
    NativeHostTransport.presentConversation(request)

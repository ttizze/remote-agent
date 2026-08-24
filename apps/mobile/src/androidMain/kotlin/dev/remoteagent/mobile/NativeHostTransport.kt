package dev.remoteagent.mobile

/** JNI declarations for the Rust mobile-client. No secret is retained in this layer. */
object NativeHostTransport {
    init { System.loadLibrary("mobile_client") }

    external fun generateDeviceKey(): String
    external fun connect(configJson: String, keyBase64: String): Long
    external fun request(handle: Long, method: String, paramsJson: String): String
    external fun nextNotification(handle: Long): String?
    external fun nextServerRequest(handle: Long): String?
    external fun respondResult(handle: Long, requestIdJson: String, resultJson: String): Boolean
    external fun respondError(handle: Long, requestIdJson: String, errorJson: String): Boolean
    external fun close(handle: Long)
}

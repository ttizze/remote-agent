package dev.remoteagent.mobile

import java.util.concurrent.locks.ReentrantReadWriteLock
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put

internal class AndroidNativeHandle(
    val pointer: Long,
    val lifetime: ReentrantReadWriteLock = ReentrantReadWriteLock(true),
    var closed: Boolean = false,
) {
    fun close() {
        val writeLock = lifetime.writeLock()
        writeLock.lock()
        try {
            if (!closed) {
                closed = true
                if (pointer != 0L) runCatching { NativeHostTransport.close(pointer) }
            }
        } finally {
            writeLock.unlock()
        }
    }
}

/** Opens the configured Phoenix relay path. */
internal fun openAndroidHost(
    profile: HostProfile,
    relayToken: String,
    key: ByteArray,
    ticket: String?,
): AndroidNativeHandle {
    val config =
        buildJsonObject {
                put("relayUrl", profile.relayUrl)
                put("runnerId", profile.runnerId)
                put("hostIdentity", profile.hostIdentity)
                put("deviceName", android.os.Build.MODEL)
                put("relayToken", relayToken)
                ticket?.let { put("pairingTicket", it) }
                put("requestTimeoutMs", REQUEST_TIMEOUT_MS)
            }
            .toString()
    val handle = NativeHostTransport.connect(config, key)
    check(handle != 0L) { "Native connect returned no handle" }
    return AndroidNativeHandle(handle)
}

private const val REQUEST_TIMEOUT_MS = 30_000

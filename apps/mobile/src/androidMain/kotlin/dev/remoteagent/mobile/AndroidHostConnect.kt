package dev.remoteagent.mobile

import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put

/** Opens the configured Phoenix relay path. */
internal fun openAndroidHost(profile: HostProfile, relayToken: String, key: ByteArray, ticket: String?): Long {
    val config = buildJsonObject {
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
    return handle
}

private const val REQUEST_TIMEOUT_MS = 30_000

package dev.remoteagent.mobile

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith

class NativeHostTransportTest {
    @Test
    fun byte_array_key_reaches_native_validation_before_connection() {
        val config =
            """
            {"relayUrl":"ws://127.0.0.1:1/socket/websocket","runnerId":"fixture",
             "relayToken":"fixture-token","hostIdentity":"AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE",
             "deviceName":"JNI fixture","requestTimeoutMs":10}
        """
        val error = assertFailsWith<IllegalStateException> { NativeHostTransport.connect(config, byteArrayOf(1, 2, 3)) }
        assertEquals("device identity is not a valid Ed25519 private key", error.message)
    }
}

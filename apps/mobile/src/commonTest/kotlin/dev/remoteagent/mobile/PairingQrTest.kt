package dev.remoteagent.mobile

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertIs

class PairingQrTest {
    private val fixed32Bytes = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"

    private val hostGeneratedV2Payload = """
        {
          "protocolVersion": 2,
          "hostIdentity": "$fixed32Bytes",
          "addresses": ["192.0.2.1:49152"],
          "ticket": "$fixed32Bytes",
          "expiresAtMs": 200
        }
    """.trimIndent()

    @Test
    fun accepts_current_unexpired_payload() {
        val result = parsePairingQr(payload(expiresAtMs = 200), nowMs = 100)

        val valid = assertIs<PairingQrResult.Valid>(result)
        assertEquals(listOf("192.0.2.1:49152"), valid.payload.addresses)
    }

    @Test
    fun accepts_host_generated_v2_payload() {
        val result = parsePairingQr(hostGeneratedV2Payload, nowMs = 100)

        assertIs<PairingQrResult.Valid>(result)
    }

    @Test
    fun rejects_expired_or_unknown_protocol_payloads() {
        assertEquals(
            PairingQrResult.Invalid(PairingQrFailure.Expired),
            parsePairingQr(payload(expiresAtMs = 100), nowMs = 100),
        )
        assertEquals(
            PairingQrResult.Invalid(PairingQrFailure.UnsupportedProtocol),
            parsePairingQr(payload(expiresAtMs = 200, protocolVersion = 1), nowMs = 100),
        )
    }

    @Test
    fun rejects_wrong_key_lengths_and_missing_addresses() {
        assertEquals(
            PairingQrResult.Invalid(PairingQrFailure.InvalidHostIdentity),
            parsePairingQr(payload(expiresAtMs = 200, hostIdentity = "AA"), nowMs = 100),
        )
        assertEquals(
            PairingQrResult.Invalid(PairingQrFailure.InvalidAddresses),
            parsePairingQr(payload(expiresAtMs = 200, addresses = ""), nowMs = 100),
        )
    }

    private fun payload(
        expiresAtMs: Long,
        protocolVersion: Int = 2,
        hostIdentity: String = fixed32Bytes,
        addresses: String = "\"192.0.2.1:49152\"",
    ): String = """
        {
          "protocolVersion": $protocolVersion,
          "hostIdentity": "$hostIdentity",
          "addresses": [$addresses],
          "ticket": "$fixed32Bytes",
          "expiresAtMs": $expiresAtMs
        }
    """.trimIndent()
}

package dev.remoteagent.mobile

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertIs

class PairingQrTest {
    private val relayUrl = "wss://relay.example.test/socket/websocket"
    private val runnerId = "runner-1"
    private val relayToken = "relay-token-for-fixture"

    private val hostGeneratedPayload =
        """
        {
          "protocolVersion": 4,
          "relayUrl": "$relayUrl",
          "runnerId": "$runnerId",
          "relayToken": "$relayToken",
          "hostIdentity": "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
          "hostName": "Mac",
          "ticket": "AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE",
          "expiresAtMs": 200
        }
    """
            .trimIndent()

    @Test
    fun accepts_current_unexpired_payload() {
        val result = parsePairingQr(payload(expiresAtMs = 200), nowMs = 100)

        val valid = assertIs<PairingQrResult.Valid>(result)
        assertEquals(relayUrl, valid.payload.relayUrl)
        assertEquals(runnerId, valid.payload.runnerId)
        assertEquals(relayToken, valid.payload.relayToken)
    }

    @Test
    fun accepts_host_generated_payload() {
        val result = parsePairingQr(hostGeneratedPayload, nowMs = 100)

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
    fun rejects_invalid_relay_url_runner_id_and_token() {
        assertEquals(
            PairingQrResult.Invalid(PairingQrFailure.InvalidRelayUrl),
            parsePairingQr(payload(expiresAtMs = 200, relayUrl = "ssh://host:22"), nowMs = 100),
        )
        assertEquals(
            PairingQrResult.Invalid(PairingQrFailure.InvalidRelayUrl),
            parsePairingQr(
                payload(expiresAtMs = 200, relayUrl = "wss://relay.example.test/socket#fragment"),
                nowMs = 100,
            ),
        )
        assertEquals(
            PairingQrResult.Invalid(PairingQrFailure.InvalidRunnerId),
            parsePairingQr(payload(expiresAtMs = 200, runnerId = " "), nowMs = 100),
        )
        assertEquals(
            PairingQrResult.Invalid(PairingQrFailure.InvalidRelayToken),
            parsePairingQr(payload(expiresAtMs = 200, relayToken = ""), nowMs = 100),
        )
    }

    private fun payload(
        expiresAtMs: Long,
        protocolVersion: Int = 4,
        relayUrl: String = this.relayUrl,
        runnerId: String = this.runnerId,
        relayToken: String = this.relayToken,
    ): String =
        """
        {
          "protocolVersion": $protocolVersion,
          "relayUrl": "$relayUrl",
          "runnerId": "$runnerId",
          "relayToken": "$relayToken",
          "hostIdentity": "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
          "hostName": "Mac",
          "ticket": "AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE",
          "expiresAtMs": $expiresAtMs
        }
    """
            .trimIndent()
}

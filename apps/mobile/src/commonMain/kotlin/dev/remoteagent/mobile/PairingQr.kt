package dev.remoteagent.mobile

import kotlinx.serialization.Serializable
import kotlinx.serialization.SerializationException
import kotlinx.serialization.json.Json

/** Must stay aligned with host_protocol::CURRENT_PROTOCOL_VERSION. */
private const val CURRENT_PROTOCOL_VERSION = 4
private const val MAXIMUM_QR_CHARACTERS = 8 * 1024
private const val MAXIMUM_RELAY_URL_CHARACTERS = 2 * 1024
private const val MAXIMUM_RUNNER_ID_CHARACTERS = 256
private const val MAXIMUM_RELAY_TOKEN_CHARACTERS = 4 * 1024

/** The only connection material carried by a mobile pairing QR. */
@Serializable
data class PairingQrPayload(
    val protocolVersion: Int,
    val relayUrl: String,
    val runnerId: String,
    val relayToken: String,
    val expiresAtMs: Long,
    val hostIdentity: String,
    val hostName: String,
    val ticket: String,
)

sealed interface PairingQrResult {
    data class Valid(val payload: PairingQrPayload) : PairingQrResult

    data class Invalid(val reason: PairingQrFailure) : PairingQrResult
}

enum class PairingQrFailure {
    TooLarge,
    InvalidJson,
    UnsupportedProtocol,
    Expired,
    InvalidRelayUrl,
    InvalidRunnerId,
    InvalidRelayToken,
    InvalidHostIdentity,
    InvalidTicket,
}

private val pairingJson = Json {
    ignoreUnknownKeys = true
    isLenient = false
}

fun parsePairingQr(contents: String, nowMs: Long): PairingQrResult {
    if (contents.length > MAXIMUM_QR_CHARACTERS) {
        return PairingQrResult.Invalid(PairingQrFailure.TooLarge)
    }
    val payload =
        try {
            pairingJson.decodeFromString<PairingQrPayload>(contents)
        } catch (_: SerializationException) {
            null
        } catch (_: IllegalArgumentException) {
            null
        }
    return if (payload == null) PairingQrResult.Invalid(PairingQrFailure.InvalidJson)
    else validatePairingPayload(payload, nowMs)
}

private fun validatePairingPayload(payload: PairingQrPayload, nowMs: Long): PairingQrResult =
    when {
        payload.protocolVersion != CURRENT_PROTOCOL_VERSION ->
            PairingQrResult.Invalid(PairingQrFailure.UnsupportedProtocol)
        nowMs >= payload.expiresAtMs -> PairingQrResult.Invalid(PairingQrFailure.Expired)
        !isRelayUrl(payload.relayUrl) -> PairingQrResult.Invalid(PairingQrFailure.InvalidRelayUrl)
        !isBoundedToken(payload.runnerId, MAXIMUM_RUNNER_ID_CHARACTERS) ->
            PairingQrResult.Invalid(PairingQrFailure.InvalidRunnerId)
        !isBoundedToken(payload.relayToken, MAXIMUM_RELAY_TOKEN_CHARACTERS) ->
            PairingQrResult.Invalid(PairingQrFailure.InvalidRelayToken)
        !isCanonicalKey(payload.hostIdentity) -> PairingQrResult.Invalid(PairingQrFailure.InvalidHostIdentity)
        !isCanonicalKey(payload.ticket) -> PairingQrResult.Invalid(PairingQrFailure.InvalidTicket)
        else -> PairingQrResult.Valid(payload)
    }

private fun isRelayUrl(value: String): Boolean {
    if (!hasValidRelayCharacters(value)) return false
    val authorityStart =
        when {
            value.startsWith(WS_PREFIX) -> WS_PREFIX.length
            value.startsWith(WSS_PREFIX) -> WSS_PREFIX.length
            else -> 0
        }
    return authorityStart > 0 && authorityStart < value.length && value[authorityStart] !in ":/?#"
}

private fun hasValidRelayCharacters(value: String): Boolean =
    value.length <= MAXIMUM_RELAY_URL_CHARACTERS && value.none(::isInvalidRelayCharacter)

private fun isInvalidRelayCharacter(value: Char): Boolean = value.isWhitespace() || value.isISOControl() || value == '#'

private fun isBoundedToken(value: String, maximumCharacters: Int): Boolean =
    value.isNotBlank() && value.length <= maximumCharacters && !value.any(Char::isISOControl)

private fun isCanonicalKey(value: String): Boolean =
    try {
        val codec = kotlin.io.encoding.Base64.UrlSafe.withPadding(kotlin.io.encoding.Base64.PaddingOption.ABSENT)
        val bytes = codec.decode(value)
        bytes.size == KEY_BYTES && codec.encode(bytes) == value
    } catch (_: IllegalArgumentException) {
        false
    }

private const val WS_PREFIX = "ws://"
private const val WSS_PREFIX = "wss://"
private const val KEY_BYTES = 32

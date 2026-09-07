package dev.remoteagent.mobile

import kotlinx.serialization.Serializable
import kotlinx.serialization.SerializationException
import kotlinx.serialization.json.Json

/** Must stay aligned with host_protocol::CURRENT_PROTOCOL_VERSION. */
private const val CurrentProtocolVersion = 4
private const val MaximumQrCharacters = 8 * 1024
private const val MaximumRelayUrlCharacters = 2 * 1024
private const val MaximumRunnerIdCharacters = 256
private const val MaximumRelayTokenCharacters = 4 * 1024

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
    if (contents.length > MaximumQrCharacters) {
        return PairingQrResult.Invalid(PairingQrFailure.TooLarge)
    }
    val payload = try {
        pairingJson.decodeFromString<PairingQrPayload>(contents)
    } catch (_: SerializationException) {
        return PairingQrResult.Invalid(PairingQrFailure.InvalidJson)
    } catch (_: IllegalArgumentException) {
        return PairingQrResult.Invalid(PairingQrFailure.InvalidJson)
    }
    if (payload.protocolVersion != CurrentProtocolVersion) {
        return PairingQrResult.Invalid(PairingQrFailure.UnsupportedProtocol)
    }
    if (nowMs >= payload.expiresAtMs) {
        return PairingQrResult.Invalid(PairingQrFailure.Expired)
    }
    if (!isRelayUrl(payload.relayUrl)) {
        return PairingQrResult.Invalid(PairingQrFailure.InvalidRelayUrl)
    }
    if (!isBoundedToken(payload.runnerId, MaximumRunnerIdCharacters)) {
        return PairingQrResult.Invalid(PairingQrFailure.InvalidRunnerId)
    }
    if (!isBoundedToken(payload.relayToken, MaximumRelayTokenCharacters)) {
        return PairingQrResult.Invalid(PairingQrFailure.InvalidRelayToken)
    }
    if (!isCanonicalKey(payload.hostIdentity)) return PairingQrResult.Invalid(PairingQrFailure.InvalidHostIdentity)
    if (!isCanonicalKey(payload.ticket)) return PairingQrResult.Invalid(PairingQrFailure.InvalidTicket)
    return PairingQrResult.Valid(payload)
}

private fun isRelayUrl(value: String): Boolean {
    if (
        value.length > MaximumRelayUrlCharacters ||
        value.any(Char::isWhitespace) ||
        value.any(Char::isISOControl) ||
        '#' in value
    ) {
        return false
    }
    val schemeEnd = value.indexOf("://")
    if (schemeEnd <= 0) return false
    val scheme = value.substring(0, schemeEnd)
    if (scheme != "ws" && scheme != "wss") return false
    val authority = value.substring(schemeEnd + 3).substringBeforeAny('/', '?', '#')
    return authority.isNotEmpty() && !authority.startsWith(':')
}

private fun isBoundedToken(value: String, maximumCharacters: Int): Boolean =
    value.isNotBlank() && value.length <= maximumCharacters && !value.any(Char::isISOControl)

private fun String.substringBeforeAny(vararg delimiters: Char): String {
    val end = indexOfFirst { it in delimiters }
    return if (end < 0) this else substring(0, end)
}

private fun isCanonicalKey(value: String): Boolean = try {
    val codec = kotlin.io.encoding.Base64.UrlSafe.withPadding(kotlin.io.encoding.Base64.PaddingOption.ABSENT)
    val bytes = codec.decode(value)
    bytes.size == 32 && codec.encode(bytes) == value
} catch (_: IllegalArgumentException) { false }

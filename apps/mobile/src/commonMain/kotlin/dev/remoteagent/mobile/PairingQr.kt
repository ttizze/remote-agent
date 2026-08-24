package dev.remoteagent.mobile

import kotlin.io.encoding.Base64
import kotlin.io.encoding.ExperimentalEncodingApi
import kotlinx.serialization.Serializable
import kotlinx.serialization.SerializationException
import kotlinx.serialization.json.Json

/** Must stay aligned with host_protocol::CURRENT_PROTOCOL_VERSION. */
private const val CurrentProtocolVersion = 2
private const val MaximumQrCharacters = 8 * 1024
private const val IdentityBytes = 32
private const val PairingTicketBytes = 32
private const val MaximumAddresses = 16
private const val MaximumAddressCharacters = 512

@Serializable
data class PairingQrPayload(
    val protocolVersion: Int,
    val hostIdentity: String,
    val addresses: List<String>,
    val ticket: String,
    val expiresAtMs: Long,
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
    InvalidHostIdentity,
    InvalidTicket,
    InvalidAddresses,
}

private val pairingJson = Json {
    ignoreUnknownKeys = true
    isLenient = false
}

@OptIn(ExperimentalEncodingApi::class)
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
    if (!isFixedBase64Url(payload.hostIdentity, IdentityBytes)) {
        return PairingQrResult.Invalid(PairingQrFailure.InvalidHostIdentity)
    }
    if (!isFixedBase64Url(payload.ticket, PairingTicketBytes)) {
        return PairingQrResult.Invalid(PairingQrFailure.InvalidTicket)
    }
    if (
        payload.addresses.isEmpty() ||
        payload.addresses.size > MaximumAddresses ||
        payload.addresses.any { it.isBlank() || it.length > MaximumAddressCharacters }
    ) {
        return PairingQrResult.Invalid(PairingQrFailure.InvalidAddresses)
    }
    return PairingQrResult.Valid(payload)
}

@OptIn(ExperimentalEncodingApi::class)
private fun isFixedBase64Url(encoded: String, expectedBytes: Int): Boolean = try {
    !encoded.contains('=') &&
        Base64.UrlSafe.withPadding(Base64.PaddingOption.ABSENT).decode(encoded).size == expectedBytes
} catch (_: IllegalArgumentException) {
    false
}

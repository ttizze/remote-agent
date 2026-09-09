@file:OptIn(kotlinx.cinterop.ExperimentalForeignApi::class)

package dev.remoteagent.mobile

import kotlin.io.encoding.Base64
import kotlinx.cinterop.ByteVar
import kotlinx.cinterop.CPointer
import kotlinx.cinterop.CPointerVar
import kotlinx.cinterop.ExperimentalForeignApi
import kotlinx.cinterop.addressOf
import kotlinx.cinterop.alloc
import kotlinx.cinterop.memScoped
import kotlinx.cinterop.ptr
import kotlinx.cinterop.reinterpret
import kotlinx.cinterop.usePinned
import kotlinx.cinterop.value
import mobile_client.mobile_client_connect
import mobile_client.mobile_client_generate_device_key
import mobile_client.mobile_client_next_event

internal fun callConnect(config: String, key: ByteArray): GatewayResult<Long> = memScoped {
    val error = alloc<CPointerVar<ByteVar>>()
    error.value = null
    key.usePinned { pinned ->
        val value = mobile_client_connect(config, pinned.addressOf(0).reinterpret(), key.size.toULong(), error.ptr)
        if (value == 0UL) GatewayResult.Failure(takeError(error.value)) else GatewayResult.Success(value.toLong())
    }
}

internal fun nextEvent(current: Long): String? = memScoped {
    val error = alloc<CPointerVar<ByteVar>>()
    error.value = null
    val value = mobile_client_next_event(current.toULong(), error.ptr)
    if (value == null) {
        if (error.value != null) error(takeError(error.value))
        return null
    }
    takeString(value)
}

internal fun generatedPkcs8(): ByteArray = memScoped {
    val error = alloc<CPointerVar<ByteVar>>()
    error.value = null
    val encoded = mobile_client_generate_device_key(error.ptr) ?: error(takeError(error.value))
    val base64Url = takeString(encoded)
    Base64.UrlSafe.withPadding(Base64.PaddingOption.ABSENT).decode(base64Url)
}

internal fun takeResult(value: CPointer<ByteVar>?, error: CPointer<ByteVar>?): GatewayResult<String> {
    if (value != null) return GatewayResult.Success(takeString(value))
    val message = takeError(error)
    val rawError = runCatching { iosJson.parseToJsonElement(message) }.getOrNull()
    return GatewayResult.Failure(message, rawError)
}

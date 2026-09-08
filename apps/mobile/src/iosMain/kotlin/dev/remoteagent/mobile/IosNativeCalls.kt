@file:OptIn(kotlinx.cinterop.ExperimentalForeignApi::class)

package dev.remoteagent.mobile

import cnames.structs.MobileClientHandle
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
import kotlinx.serialization.json.JsonElement
import mobile_client.mobile_client_connect
import mobile_client.mobile_client_generate_device_key
import mobile_client.mobile_client_next_notification
import mobile_client.mobile_client_next_server_request
import mobile_client.mobile_client_request
import mobile_client.mobile_client_respond_error
import mobile_client.mobile_client_respond_result

internal fun callConnect(config: String, key: ByteArray): GatewayResult<CPointer<MobileClientHandle>> = memScoped {
    val error = alloc<CPointerVar<ByteVar>>()
    error.value = null
    key.usePinned { pinned ->
        val value = mobile_client_connect(config, pinned.addressOf(0).reinterpret(), key.size.toULong(), error.ptr)
        if (value == null) GatewayResult.Failure(takeError(error.value)) else GatewayResult.Success(value)
    }
}

internal fun nextNotification(current: CPointer<MobileClientHandle>): String? = memScoped {
    val error = alloc<CPointerVar<ByteVar>>()
    error.value = null
    val value = mobile_client_next_notification(current, error.ptr)
    if (value == null) {
        if (error.value != null) error(takeError(error.value))
        return null
    }
    takeString(value)
}

internal fun nextServerRequest(current: CPointer<MobileClientHandle>): String? = memScoped {
    val error = alloc<CPointerVar<ByteVar>>()
    error.value = null
    val value = mobile_client_next_server_request(current, error.ptr)
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

internal fun nextRawMessage(current: CPointer<MobileClientHandle>): RawCodexMessage? {
    val raw = nextNotification(current) ?: nextServerRequest(current)
    return raw?.let(::parseRawCodexMessage)
}

internal fun nativeRequest(
    current: CPointer<MobileClientHandle>,
    method: String,
    params: JsonElement,
): GatewayResult<String> = memScoped {
    val error = alloc<CPointerVar<ByteVar>>()
    error.value = null
    val result = mobile_client_request(current, method, params.toString(), error.ptr)
    takeResult(result, error.value)
}

internal fun nativeResponse(
    current: CPointer<MobileClientHandle>,
    requestId: JsonElement,
    payload: JsonElement,
    isError: Boolean,
): GatewayResult<Unit> = memScoped {
    val error = alloc<CPointerVar<ByteVar>>()
    error.value = null
    val success =
        if (isError) {
            mobile_client_respond_error(current, requestId.toString(), payload.toString(), error.ptr)
        } else {
            mobile_client_respond_result(current, requestId.toString(), payload.toString(), error.ptr)
        }
    if (success != 0) GatewayResult.Success(Unit) else GatewayResult.Failure(takeError(error.value))
}

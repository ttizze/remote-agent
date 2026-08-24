package dev.remoteagent.mobile

import kotlinx.cinterop.ByteVar
import kotlinx.cinterop.CPointed
import kotlinx.cinterop.CPointer
import kotlinx.cinterop.CPointerVar
import kotlinx.cinterop.COpaquePointer
import kotlinx.cinterop.COpaquePointerVar
import kotlinx.cinterop.CValuesRef
import kotlinx.cinterop.ExperimentalForeignApi
import kotlinx.cinterop.alloc
import kotlinx.cinterop.addressOf
import kotlinx.cinterop.memScoped
import kotlinx.cinterop.ptr
import kotlinx.cinterop.reinterpret
import kotlinx.cinterop.toKString
import kotlinx.cinterop.toCValues
import kotlinx.cinterop.usePinned
import kotlinx.cinterop.value
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put
import cnames.structs.MobileClientHandle
import mobile_client.mobile_client_close
import mobile_client.mobile_client_connect
import mobile_client.mobile_client_generate_device_key
import mobile_client.mobile_client_next_notification
import mobile_client.mobile_client_next_server_request
import mobile_client.mobile_client_request
import mobile_client.mobile_client_respond_error
import mobile_client.mobile_client_respond_result
import mobile_client.mobile_client_string_free
import platform.CoreFoundation.CFDictionaryRef
import platform.CoreFoundation.CFDataCreate
import platform.CoreFoundation.CFDataGetBytePtr
import platform.CoreFoundation.CFDataGetLength
import platform.CoreFoundation.CFDictionaryCreate
import platform.CoreFoundation.CFTypeRefVar
import platform.Foundation.NSData
import platform.Foundation.NSFileManager
import platform.Foundation.NSHomeDirectory
import platform.Foundation.dataWithBytes
import platform.Foundation.dataWithContentsOfFile
import platform.Foundation.writeToFile
import platform.CoreFoundation.kCFBooleanTrue
import platform.Security.SecItemAdd
import platform.Security.SecItemCopyMatching
import platform.Security.SecItemDelete
import platform.Security.errSecItemNotFound
import platform.Security.errSecSuccess
import platform.Security.kSecAttrAccessible
import platform.Security.kSecAttrAccessibleWhenUnlockedThisDeviceOnly
import platform.Security.kSecAttrAccount
import platform.Security.kSecAttrService
import platform.Security.kSecClass
import platform.Security.kSecClassGenericPassword
import platform.Security.kSecReturnData
import platform.Security.kSecValueData
import kotlin.io.encoding.Base64
import kotlin.io.encoding.ExperimentalEncodingApi

private const val DeviceKeyService = "dev.remoteagent.mobile.pkcs8"
private const val DefaultMaxFrameBytes = 64 * 1024
private const val DefaultRequestTimeoutMs = 30_000L

private val iosJson = Json {
    ignoreUnknownKeys = true
    isLenient = false
    classDiscriminator = "type"
}

/** iOS composition root: common UI owns no platform default implementation. */
internal class IosMobileDependencies {
    val repository = IosMobileRepository()
    val gateway = IosHostGateway()
}

/** Called by the Swift application lifecycle observer. */
object IosLifecycleBridge {
    /** Installed by the one live iOS application controller. */
    internal var onRestoreAfterForeground: (() -> Unit)? = null

    fun restoreAfterForeground() = onRestoreAfterForeground?.invoke()
    fun didEnterBackground() = Unit
}

/** Bonjour answers are hints only; Rust still pins and verifies Host identity. */
object IosBonjourBridge {
    private var addresses: List<String> = emptyList()

    fun update(addresses: List<String>) {
        this.addresses = addresses
    }

    fun candidates(): List<String> = addresses
}

/**
 * All C calls are made on Dispatchers.Default. Handle leases keep a retired
 * native client alive until every concurrent request/poll/response finishes.
 */
@OptIn(ExperimentalForeignApi::class, ExperimentalEncodingApi::class)
internal class IosHostGateway : HostGateway {
    private val codexClient = CommonCodexClient(this)
    private val handleMutex = Mutex()
    private var handle: NativeHandle? = null
    private var lastConnectedProfile: HostProfile? = null
    private val subscriptions = mutableSetOf<Job>()
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)

    override suspend fun pair(payload: PairingQrPayload): GatewayResult<HostProfile> = withContext(Dispatchers.Default) {
        val identityReference = payload.hostIdentity
        val addresses = (payload.addresses + IosBonjourBridge.candidates()).filter(String::isNotBlank).distinct()
        val key = try {
            IosPkcs8KeyStore.loadOrGenerate(identityReference) { generatedPkcs8() }
        } catch (error: Throwable) {
            return@withContext GatewayResult.Failure(error.message ?: "端末鍵を保存できませんでした")
        }
        try {
            when (val result = connect(addresses, payload.hostIdentity, payload.ticket, key)) {
                is GatewayResult.Success -> GatewayResult.Success(HostProfile(
                    hostIdentity = payload.hostIdentity,
                    name = payload.hostIdentity.take(12),
                    addresses = addresses,
                    deviceIdentityReference = identityReference,
                ))
                is GatewayResult.Failure -> result
            }
        } finally {
            key.fill(0)
        }
    }

    override suspend fun discover(profile: HostProfile): GatewayResult<List<String>> =
        GatewayResult.Success((profile.addresses + IosBonjourBridge.candidates()).distinct())

    override suspend fun connect(profile: HostProfile): GatewayResult<Unit> = withContext(Dispatchers.Default) {
        val key = try {
            IosPkcs8KeyStore.load(profile.deviceIdentityReference)
                ?: return@withContext GatewayResult.Failure("端末鍵が見つかりません。再ペアリングしてください。")
        } catch (error: Throwable) {
            return@withContext GatewayResult.Failure(error.message ?: "端末鍵を読み出せませんでした")
        }
        try {
            connect((profile.addresses + IosBonjourBridge.candidates()).filter(String::isNotBlank).distinct(), profile.hostIdentity, null, key)
        } finally {
            key.fill(0)
        }
    }

    override suspend fun rawRequest(
        profile: HostProfile,
        method: String,
        params: JsonElement,
    ): GatewayResult<JsonElement> = withContext(Dispatchers.Default) {
        request(method, params).mapGateway { response -> iosJson.parseToJsonElement(response) }
    }

    override suspend fun listThreads(profile: HostProfile, cwd: String): GatewayResult<List<ThreadSummary>> =
        codexClient.listThreads(profile, cwd)

    override suspend fun readThread(profile: HostProfile, threadId: String): GatewayResult<ThreadReadResult> =
        codexClient.readThread(profile, threadId)

    override suspend fun startThread(profile: HostProfile, cwd: String): GatewayResult<ThreadSnapshot> =
        codexClient.startThread(profile, cwd)

    override suspend fun startTurn(profile: HostProfile, threadId: String, text: String): GatewayResult<String> =
        codexClient.startTurn(profile, threadId, text)

    override suspend fun interrupt(profile: HostProfile, threadId: String, turnId: String): GatewayResult<Unit> =
        codexClient.interrupt(profile, threadId, turnId)

    override suspend fun respondResult(
        profile: HostProfile,
        requestId: JsonElement,
        result: JsonElement,
    ): GatewayResult<Unit> = respond(profile, requestId, result, isError = false)

    override suspend fun respondError(
        profile: HostProfile,
        requestId: JsonElement,
        error: JsonElement,
    ): GatewayResult<Unit> = respond(profile, requestId, error, isError = true)

    override fun subscribeRaw(profile: HostProfile, onMessage: (RawCodexMessage) -> Unit): HostEventSubscription {
        val job = scope.launch {
            while (true) {
                nextRawMessage()?.let(onMessage)
                delay(50)
            }
        }
        subscriptions += job
        return HostEventSubscription { subscriptions.remove(job); job.cancel() }
    }

    private suspend fun connect(
        addresses: List<String>,
        hostIdentity: String,
        pairingTicket: String?,
        key: ByteArray,
    ): GatewayResult<Unit> {
        var lastFailure = "接続先アドレスがありません"
        for (address in addresses.filter(String::isNotBlank).distinct()) {
            val config = buildJsonObject {
                put("address", address)
                put("serverName", "bex-host")
                put("hostIdentity", hostIdentity)
                put("deviceName", "Bex iOS")
                pairingTicket?.let { put("pairingTicket", it) }
                put("maxFrameBytes", DefaultMaxFrameBytes)
                put("requestTimeoutMs", DefaultRequestTimeoutMs)
            }.toString()
            when (val result = callConnect(config, key)) {
                is GatewayResult.Success -> {
                    val retired = handleMutex.withLock {
                        val previous = handle
                        previous?.retired = true
                        handle = NativeHandle(result.value)
                        lastConnectedProfile = HostProfile(
                            hostIdentity = hostIdentity,
                            name = hostIdentity.take(12),
                            addresses = addresses,
                            deviceIdentityReference = hostIdentity,
                        )
                        previous?.pointer?.takeIf { previous.borrowers == 0 }
                    }
                    retired?.let { mobile_client_close(it) }
                    return GatewayResult.Success(Unit)
                }
                is GatewayResult.Failure -> lastFailure = result.message
            }
        }
        return GatewayResult.Failure(lastFailure)
    }

    private suspend fun request(method: String, params: JsonElement): GatewayResult<String> {
        return withHandle { current -> memScoped {
            val error = alloc<CPointerVar<ByteVar>>()
            error.value = null
            val result = mobile_client_request(
                current,
                method,
                params.toString(),
                error.ptr,
            )
            takeResult(result, error.value)
        } } ?: GatewayResult.Failure("PC Hostへ接続されていません")
    }

    private suspend fun respond(
        profile: HostProfile,
        requestId: JsonElement,
        payload: JsonElement,
        isError: Boolean,
    ): GatewayResult<Unit> {
        return withHandle { current -> memScoped {
            val error = alloc<CPointerVar<ByteVar>>()
            error.value = null
            val success = if (isError) {
                mobile_client_respond_error(current, requestId.toString(), payload.toString(), error.ptr)
            } else {
                mobile_client_respond_result(current, requestId.toString(), payload.toString(), error.ptr)
            }
            if (success != 0) GatewayResult.Success(Unit) else GatewayResult.Failure(takeError(error.value))
        } } ?: GatewayResult.Failure("PC Hostへ接続されていません")
    }

    private fun callConnect(config: String, key: ByteArray): GatewayResult<CPointer<MobileClientHandle>> = memScoped {
        val error = alloc<CPointerVar<ByteVar>>()
        error.value = null
        key.usePinned { pinned ->
            val value = mobile_client_connect(
                config,
                pinned.addressOf(0).reinterpret(),
                key.size.toULong(),
                error.ptr,
            )
            if (value == null) GatewayResult.Failure(takeError(error.value)) else GatewayResult.Success(value)
        }
    }

    private fun nextNotification(current: CPointer<MobileClientHandle>): String? = memScoped {
        val error = alloc<CPointerVar<ByteVar>>()
        error.value = null
        val value = mobile_client_next_notification(current, error.ptr)
        if (value == null) {
            if (error.value != null) error(takeError(error.value))
            return null
        }
        takeString(value)
    }

    private suspend fun nextRawMessage(): RawCodexMessage? = withHandle { current ->
        val raw = nextNotification(current) ?: nextServerRequest(current) ?: return@withHandle null
        parseRawCodexMessage(raw)
    }

    private fun nextServerRequest(current: CPointer<MobileClientHandle>): String? = memScoped {
        val error = alloc<CPointerVar<ByteVar>>()
        error.value = null
        val value = mobile_client_next_server_request(current, error.ptr)
        if (value == null) {
            if (error.value != null) error(takeError(error.value))
            return null
        }
        takeString(value)
    }

    private suspend fun <T> withHandle(block: (CPointer<MobileClientHandle>) -> T): T? {
        val lease = handleMutex.withLock {
            handle?.also { it.borrowers += 1 }
        } ?: return null
        return try {
            block(lease.pointer)
        } finally {
            val retired = handleMutex.withLock {
                lease.borrowers -= 1
                lease.pointer.takeIf { lease.retired && lease.borrowers == 0 }
            }
            retired?.let { mobile_client_close(it) }
        }
    }

    private class NativeHandle(
        val pointer: CPointer<MobileClientHandle>,
        var borrowers: Int = 0,
        var retired: Boolean = false,
    )

    private fun generatedPkcs8(): ByteArray = memScoped {
        val error = alloc<CPointerVar<ByteVar>>()
        error.value = null
        val encoded = mobile_client_generate_device_key(error.ptr)
            ?: error(takeError(error.value))
        val base64Url = takeString(encoded)
        Base64.UrlSafe.withPadding(Base64.PaddingOption.ABSENT).decode(base64Url)
    }

    private fun takeResult(value: CPointer<ByteVar>?, error: CPointer<ByteVar>?): GatewayResult<String> {
        if (value != null) return GatewayResult.Success(takeString(value))
        val message = takeError(error)
        val rawError = runCatching { iosJson.parseToJsonElement(message) }.getOrNull()
        return GatewayResult.Failure(message, rawError)
    }

    private fun takeString(value: CPointer<ByteVar>): String = value.toKString().also { mobile_client_string_free(value) }
    private fun takeError(value: CPointer<ByteVar>?): String = value?.let(::takeString) ?: "mobile-client call failed"
}

/** Keychain owns opaque Rust PKCS#8 bytes, never a Swift CryptoKit key. */
@OptIn(ExperimentalForeignApi::class)
private object IosPkcs8KeyStore {
    fun loadOrGenerate(reference: String, generate: () -> ByteArray): ByteArray = load(reference) ?: generate().also {
        save(reference, it)
    }

    fun load(reference: String): ByteArray? = memScoped {
        val result = alloc<CFTypeRefVar>()
        result.value = null
        when (SecItemCopyMatching(query(reference, true), result.ptr)) {
            errSecSuccess -> result.value?.reinterpret<CPointed>()?.toByteArray()
            errSecItemNotFound -> null
            else -> null
        }
    }

    private fun save(reference: String, key: ByteArray) {
        withQuery(reference, returnsData = false, key = null) { SecItemDelete(it) }
        withQuery(reference, returnsData = false, key = key) { attributes ->
            check(SecItemAdd(attributes, null) == errSecSuccess) { "Keychain write failed" }
        }
    }

    private fun query(reference: String, returnsData: Boolean): CFDictionaryRef =
        withQuery(reference, returnsData, null) { it }

    private fun <T> withQuery(
        reference: String,
        returnsData: Boolean,
        key: ByteArray?,
        block: (CFDictionaryRef) -> T,
    ): T = memScoped {
        val service = cfString(DeviceKeyService)
        val account = cfString(reference)
        val keyData = key?.toCFData()
        val keys = mutableListOf<COpaquePointer?>(
            requireNotNull(kSecClass).reinterpret<CPointed>(),
            requireNotNull(kSecAttrService).reinterpret<CPointed>(),
            requireNotNull(kSecAttrAccount).reinterpret<CPointed>(),
        )
        val values = mutableListOf<COpaquePointer?>(
            requireNotNull(kSecClassGenericPassword).reinterpret<CPointed>(),
            service.reinterpret<CPointed>(),
            account.reinterpret<CPointed>(),
        )
        if (returnsData) {
            keys += requireNotNull(kSecReturnData).reinterpret<CPointed>()
            values += requireNotNull(kCFBooleanTrue).reinterpret<CPointed>()
        }
        if (keyData != null) {
            keys += requireNotNull(kSecValueData).reinterpret<CPointed>()
            values += keyData.reinterpret<CPointed>()
            keys += requireNotNull(kSecAttrAccessible).reinterpret<CPointed>()
            values += requireNotNull(kSecAttrAccessibleWhenUnlockedThisDeviceOnly).reinterpret<CPointed>()
        }
        val dictionary = CFDictionaryCreate(null, keys.toOpaqueCValues(), values.toOpaqueCValues(), keys.size.toLong(), null, null)
            ?: error("Keychain query could not be created")
        block(dictionary)
    }
}

@Suppress("UNCHECKED_CAST")
@OptIn(ExperimentalForeignApi::class)
private fun List<COpaquePointer?>.toOpaqueCValues(): CValuesRef<COpaquePointerVar> =
    map { it?.reinterpret<CPointed>() }.toTypedArray().toCValues() as CValuesRef<COpaquePointerVar>

@OptIn(ExperimentalForeignApi::class)
private fun cfString(value: String) = platform.CoreFoundation.CFStringCreateWithCString(null, value, platform.CoreFoundation.kCFStringEncodingUTF8)
    ?: error("CFString could not be created")

@OptIn(ExperimentalForeignApi::class)
private fun ByteArray.toCFData() = usePinned {
    CFDataCreate(null, it.addressOf(0).reinterpret(), size.toLong()) ?: error("CFData could not be created")
}

@OptIn(ExperimentalForeignApi::class)
private fun CPointer<CPointed>.toByteArray(): ByteArray {
    val data = reinterpret<cnames.structs.__CFData>()
    val length = CFDataGetLength(data).toInt()
    return ByteArray(length).also { bytes ->
        bytes.usePinned { pinned ->
            CFDataGetBytePtr(data)?.let { platform.posix.memcpy(pinned.addressOf(0), it, length.toULong()) }
        }
    }
}

@OptIn(ExperimentalForeignApi::class)
private fun ByteArray.toNSData(): NSData = usePinned { NSData.Companion.dataWithBytes(it.addressOf(0), size.toULong()) }

@OptIn(ExperimentalForeignApi::class)
private fun NSData.toByteArray(): ByteArray = ByteArray(length.toInt()).also { bytes ->
    bytes.usePinned { pinned -> platform.posix.memcpy(pinned.addressOf(0), this.bytes, length) }
}

/** Durable profile/cache repository. File writes replace whole JSON atomically. */
@OptIn(ExperimentalForeignApi::class)
internal class IosMobileRepository : MobileRepository {
    private val path = "${NSHomeDirectory()}/Library/Application Support/Bex/mobile-state.json"
    private var state = loadState()

    override fun load(): AppState = state
    override fun save(state: AppState) {
        this.state = state
        NSFileManager.defaultManager.createDirectoryAtPath(
            "${NSHomeDirectory()}/Library/Application Support/Bex", true, null, null,
        )
        val bytes = MobileStateCodec.encode(state)
        check(bytes.toNSData().writeToFile(path, atomically = true))
    }

    private fun loadState(): AppState = try {
        val bytes = NSData.Companion.dataWithContentsOfFile(path)?.toByteArray() ?: return AppState()
        when (val result = MobileStateCodec.decode(bytes)) {
            is MobileStateDecodeResult.Success -> result.value
            is MobileStateDecodeResult.Failure -> AppState()
        }
    } catch (_: Throwable) {
        AppState()
    }
}

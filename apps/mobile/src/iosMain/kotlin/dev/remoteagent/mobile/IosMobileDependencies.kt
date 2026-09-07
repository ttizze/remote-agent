package dev.remoteagent.mobile

import kotlinx.cinterop.ByteVar
import kotlinx.cinterop.CPointer
import kotlinx.cinterop.CPointerVar
import kotlinx.cinterop.ExperimentalForeignApi
import kotlinx.cinterop.alloc
import kotlinx.cinterop.addressOf
import kotlinx.cinterop.memScoped
import kotlinx.cinterop.ptr
import kotlinx.cinterop.toKString
import kotlinx.cinterop.usePinned
import kotlinx.cinterop.value
import kotlinx.atomicfu.locks.SynchronizedObject
import kotlinx.atomicfu.locks.synchronized
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put
import cnames.structs.MobileClientHandle
import mobile_client.mobile_client_transfer
import mobile_client.mobile_client_close
import mobile_client.mobile_client_connect
import mobile_client.mobile_client_next_notification
import mobile_client.mobile_client_next_server_request
import mobile_client.mobile_client_request
import mobile_client.mobile_client_respond_error
import mobile_client.mobile_client_respond_result
import mobile_client.mobile_client_string_free
import platform.Foundation.NSData
import platform.Foundation.NSFileManager
import platform.Foundation.NSHomeDirectory
import platform.Foundation.dataWithBytes
import platform.Foundation.dataWithContentsOfFile
import platform.Foundation.writeToFile
import kotlinx.cinterop.CPointed
import kotlinx.cinterop.COpaquePointer
import kotlinx.cinterop.COpaquePointerVar
import kotlinx.cinterop.CValuesRef
import kotlinx.cinterop.reinterpret
import kotlinx.cinterop.toCValues
import mobile_client.mobile_client_generate_device_key
import platform.CoreFoundation.CFDictionaryRef
import platform.CoreFoundation.CFDataCreate
import platform.CoreFoundation.CFDataGetBytePtr
import platform.CoreFoundation.CFDataGetLength
import platform.CoreFoundation.CFDictionaryCreate
import platform.CoreFoundation.CFTypeRefVar
import platform.CoreFoundation.kCFBooleanTrue
import platform.Security.SecItemAdd
import platform.Security.SecItemCopyMatching
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
import platform.CoreFoundation.CFRelease
import platform.Security.SecItemUpdate
private const val DeviceKeyService = "app.bex.mobile.credentials.v4"

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

/**
 * All C calls are made on Dispatchers.Default. Handle leases keep a retired
 * native client alive until every concurrent request/poll/response finishes.
 */
@OptIn(ExperimentalForeignApi::class)
internal class IosHostGateway : HostGateway {
    private val codexClient = CommonCodexClient(this, deferItemDetails = true)
    fun setTurnOptions(hostIdentity: String, options: CodexTurnOptions) = codexClient.setTurnOptions(hostIdentity, options)
    suspend fun listModels(profile: HostProfile): GatewayResult<List<CodexModel>> = codexClient.listModels(profile)

    /** Owns both handles and subscriptions so registration cannot cross a replacement. */
    private val handleLock = SynchronizedObject()
    private val handles = mutableMapOf<String, NativeHandle>()
    private val subscriptions = mutableMapOf<String, MutableSet<Job>>()
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)

    override suspend fun pair(payload: PairingQrPayload): GatewayResult<HostProfile> = withContext(Dispatchers.Default) {
        try {
            val reference = payload.hostIdentity
            val key = IosCredentialStore.loadOrGenerate("key:$reference") { generatedPkcs8() }
            IosCredentialStore.save("relay:$reference", payload.relayToken.encodeToByteArray())
            val profile = HostProfile(payload.runnerId, payload.hostName, payload.relayUrl, payload.hostIdentity, reference)
            when (val result = connect(profile, payload.relayToken, key, payload.ticket)) {
                is GatewayResult.Success -> GatewayResult.Success(profile)
                is GatewayResult.Failure -> result
            }
        } catch (failure: Throwable) {
            GatewayResult.Failure(failure.message ?: "Keychainへの保存に失敗しました")
        }
    }

    override suspend fun discover(profile: HostProfile): GatewayResult<List<String>> =
        GatewayResult.Success(listOf(profile.relayUrl))

    override suspend fun connect(profile: HostProfile): GatewayResult<Unit> = withContext(Dispatchers.Default) {
        try {
            val key = IosCredentialStore.load("key:${profile.deviceIdentityReference}") ?: error("秘密鍵がありません。再ペアリングしてください")
            val token = IosCredentialStore.load("relay:${profile.deviceIdentityReference}") ?: error("接続資格情報がありません。再ペアリングしてください")
            connect(profile, token.decodeToString(), key, null)
        } catch (failure: Throwable) {
            GatewayResult.Failure(failure.message ?: "Keychainを読み出せません")
        }
    }

    override suspend fun transfer(profile: HostProfile, params: JsonElement): GatewayResult<JsonElement> = withContext(Dispatchers.Default) {
        withHandle(profile.id) { current -> memScoped {
            val error = alloc<CPointerVar<ByteVar>>()
            error.value = null
            takeResult(mobile_client_transfer(current, params.toString(), error.ptr), error.value)
                .mapGateway { iosJson.parseToJsonElement(it) }
        } } ?: GatewayResult.Failure("PC Hostへ接続されていません")
    }

    override suspend fun rawRequest(
        profile: HostProfile,
        method: String,
        params: JsonElement,
    ): GatewayResult<JsonElement> = withContext(Dispatchers.Default) {
        request(profile.id, method, params).mapGateway { response -> iosJson.parseToJsonElement(response) }
    }

    override suspend fun disconnect(profile: HostProfile): GatewayResult<Unit> = withContext(Dispatchers.Default) {
        val hostIdentity = profile.id
        val (jobs, retired) = synchronized(handleLock) {
            val jobs = subscriptions.remove(hostIdentity)?.toList().orEmpty()
            val pointer = handles.remove(hostIdentity)?.also { it.retired = true }?.let { handle ->
                handle.pointer.takeIf { handle.borrowers == 0 }
            }
            jobs to pointer
        }
        jobs.forEach(Job::cancel)
        retired?.let { mobile_client_close(it) }
        GatewayResult.Success(Unit)
    }

    override suspend fun listThreads(profile: HostProfile, query: ThreadListQuery): GatewayResult<ThreadListPage> =
        withContext(Dispatchers.Default) { codexClient.listThreads(profile, query) }

    suspend fun readItemDetails(profile: HostProfile, threadId: String, turnId: String, itemId: String): GatewayResult<String> =
        withContext(Dispatchers.Default) { codexClient.readItemDetails(profile, threadId, turnId, itemId) }

    override suspend fun readThread(profile: HostProfile, threadId: String): GatewayResult<ThreadReadResult> =
        withContext(Dispatchers.Default) { codexClient.readThread(profile, threadId) }

    override suspend fun startThread(profile: HostProfile, cwd: String): GatewayResult<ThreadSnapshot> =
        codexClient.startThread(profile, cwd)

    override suspend fun startTurn(profile: HostProfile, threadId: String, cwd: String, text: String, attachments: List<CodexAttachment>, resume: Boolean, clientUserMessageId: String): GatewayResult<String> =
        codexClient.startTurn(profile, threadId, cwd, text, attachments, resume, clientUserMessageId)

    override suspend fun steerTurn(profile: HostProfile, threadId: String, turnId: String, text: String, attachments: List<CodexAttachment>, clientUserMessageId: String): GatewayResult<Unit> =
        codexClient.steerTurn(profile, threadId, turnId, text, attachments, clientUserMessageId)

    override suspend fun queueTurn(profile: HostProfile, threadId: String, text: String, attachments: List<CodexAttachment>, clientUserMessageId: String): GatewayResult<String> =
        codexClient.queueTurn(profile, threadId, text, attachments, clientUserMessageId)

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

    override fun subscribeRaw(profile: HostProfile, onMessage: (RawCodexMessage) -> Unit): HostEventSubscription =
        subscribeRaw(profile, onMessage) {}

    override fun subscribeRaw(
        profile: HostProfile,
        onMessage: (RawCodexMessage) -> Unit,
        onClosed: (String) -> Unit,
    ): HostEventSubscription {
        val hostIdentity = profile.id
        lateinit var job: Job
        val registered = synchronized(handleLock) {
            val subscribedHandle = handles[hostIdentity] ?: return@synchronized false
            job = scope.launch(start = CoroutineStart.LAZY) {
                try {
                    while (true) {
                        val message = nextRawMessage(hostIdentity, subscribedHandle)
                        if (message != null && isCurrentHandle(hostIdentity, subscribedHandle)) {
                            onMessage(message)
                        }
                        delay(50)
                    }
                } catch (cancelled: CancellationException) {
                    throw cancelled
                } catch (failure: Throwable) {
                    if (isCurrentHandle(hostIdentity, subscribedHandle)) {
                        runCatching { onClosed(failure.message ?: "PC Hostとの接続が切れました") }
                    }
                } finally {
                    currentCoroutineContext()[Job]?.let { removeSubscription(hostIdentity, it) }
                }
            }
            subscriptions.getOrPut(hostIdentity) { mutableSetOf() }.add(job)
            true
        }
        if (!registered) return HostEventSubscription {}
        job.start()
        return HostEventSubscription {
            removeSubscription(hostIdentity, job)
            job.cancel()
        }
    }

    private suspend fun connect(
        profile: HostProfile,
        relayToken: String,
        key: ByteArray,
        ticket: String?,
    ): GatewayResult<Unit> {
        val runnerId = profile.id
        val config = buildJsonObject {
            put("relayUrl", profile.relayUrl)
            put("runnerId", profile.runnerId)
            put("hostIdentity", profile.hostIdentity)
            put("deviceName", platform.UIKit.UIDevice.currentDevice.name)
            put("relayToken", relayToken)
            ticket?.let { put("pairingTicket", it) }
            put("requestTimeoutMs", DefaultRequestTimeoutMs)
        }.toString()
        return when (val result = callConnect(config, key)) {
            is GatewayResult.Success -> {
                val (jobs, retired) = synchronized(handleLock) {
                    val jobs = subscriptions.remove(runnerId)?.toList().orEmpty()
                    val previous = handles[runnerId]
                    previous?.retired = true
                    handles[runnerId] = NativeHandle(result.value)
                    jobs to previous?.pointer?.takeIf { previous.borrowers == 0 }
                }
                jobs.forEach(Job::cancel)
                retired?.let { mobile_client_close(it) }
                GatewayResult.Success(Unit)
            }
            is GatewayResult.Failure -> result
        }
    }

    private suspend fun request(hostIdentity: String, method: String, params: JsonElement): GatewayResult<String> {
        return withHandle(hostIdentity) { current -> memScoped {
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
        return withHandle(profile.id) { current -> memScoped {
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

    private fun nextRawMessage(
        hostIdentity: String,
        expectedHandle: NativeHandle,
    ): RawCodexMessage? = withHandle(hostIdentity, expectedHandle) { current ->
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

    private fun <T> withHandle(
        hostIdentity: String,
        expectedHandle: NativeHandle? = null,
        block: (CPointer<MobileClientHandle>) -> T,
    ): T? {
        val lease = synchronized(handleLock) {
            handles[hostIdentity]
                ?.takeIf { expectedHandle == null || it === expectedHandle }
                ?.also { it.borrowers += 1 }
        } ?: return null
        return try {
            block(lease.pointer)
        } finally {
            val retired = synchronized(handleLock) {
                lease.borrowers -= 1
                lease.pointer.takeIf { lease.retired && lease.borrowers == 0 }
            }
            retired?.let { mobile_client_close(it) }
        }
    }

    private fun isCurrentHandle(hostIdentity: String, expectedHandle: NativeHandle): Boolean =
        synchronized(handleLock) { handles[hostIdentity] === expectedHandle }

    private fun removeSubscription(hostIdentity: String, job: Job) {
        synchronized(handleLock) {
            subscriptions[hostIdentity]?.let { jobs ->
                jobs.remove(job)
                if (jobs.isEmpty()) subscriptions.remove(hostIdentity)
            }
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
private object IosCredentialStore {
    fun loadOrGenerate(reference: String, generate: () -> ByteArray): ByteArray = load(reference) ?: generate().also {
        save(reference, it)
    }

    fun load(reference: String): ByteArray? = memScoped {
        val result = alloc<CFTypeRefVar>()
        result.value = null
        withQuery(reference, true, null) { query ->
            when (val status = SecItemCopyMatching(query, result.ptr)) {
                errSecSuccess -> {
                    val data = requireNotNull(result.value)
                    try { data.reinterpret<CPointed>().toByteArray() } finally { CFRelease(data) }
                }
                errSecItemNotFound -> null
                else -> error("Keychain read failed ($status)")
            }
        }
    }

    fun save(reference: String, key: ByteArray) {
        withQuery(reference, false, null) { query ->
            val data = key.toCFData()
            try {
                val attributes = CFDictionaryCreate(null,
                    listOf<COpaquePointer?>(requireNotNull(kSecValueData).reinterpret<CPointed>()).toOpaqueCValues(),
                    listOf<COpaquePointer?>(data.reinterpret<CPointed>()).toOpaqueCValues(), 1, null, null)
                    ?: error("Keychain update could not be created")
                val status = try { SecItemUpdate(query, attributes) } finally { CFRelease(attributes) }
                if (status == errSecItemNotFound) {
                    withQuery(reference, false, key) { check(SecItemAdd(it, null) == errSecSuccess) { "Keychain write failed" } }
                } else check(status == errSecSuccess) { "Keychain update failed ($status)" }
            } finally { CFRelease(data) }
        }
    }

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
        try { block(dictionary) } finally {
            CFRelease(dictionary)
            keyData?.let { CFRelease(it) }
            CFRelease(account)
            CFRelease(service)
        }
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
            is MobileStateDecodeResult.Failure -> AppState().also { empty ->
                // The obsolete v1 file contained relay secrets. Replace it on migration.
                if (result.reason == MobileStateDecodeReason.UnsupportedVersion) {
                    check(MobileStateCodec.encode(empty).toNSData().writeToFile(path, atomically = true))
                }
            }
        }
    } catch (_: Throwable) {
        AppState()
    }
}

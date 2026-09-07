package dev.remoteagent.mobile

import android.content.Context
import java.util.concurrent.locks.ReentrantReadWriteLock
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put

/** Android adapter over the native raw Codex JSON-RPC client. */
class AndroidHostGateway(private val context: Context) : HostGateway {
    private val json = Json { ignoreUnknownKeys = false; isLenient = false }
    /** Guards ownership of the handle map and subscription state. */
    private val stateLock = Any()
    private val handles = mutableMapOf<String, NativeHandle>()
    private val subscriptions = mutableMapOf<String, MutableSet<(RawCodexMessage) -> Unit>>()
    private val pollers = mutableMapOf<String, Thread>()
    private val commonCodexClient = CommonCodexClient(this)

    override suspend fun pair(payload: PairingQrPayload): GatewayResult<HostProfile> = invoke {
        val reference = payload.hostIdentity
        val key = AndroidCredentialStore(context, "key:$reference").loadOrCreate {
            java.util.Base64.getUrlDecoder().decode(NativeHostTransport.generateDeviceKey())
        }
        AndroidCredentialStore(context, "relay:$reference").save(payload.relayToken.encodeToByteArray())
        val profile = HostProfile(payload.runnerId, payload.hostName, payload.relayUrl, payload.hostIdentity, reference)
        try {
            closeNativeHandle(NativeHandle(open(profile, payload.relayToken, key, payload.ticket)))
        } finally { key.fill(0) }
        profile
    }

    override suspend fun discover(profile: HostProfile): GatewayResult<List<String>> =
        GatewayResult.Success(listOf(profile.relayUrl))

    override suspend fun connect(profile: HostProfile): GatewayResult<Unit> = invoke {
        val old = synchronized(stateLock) {
            val oldHandle = handles.remove(profile.id)
            subscriptions.remove(profile.id)
            pollers.remove(profile.id) to oldHandle
        }
        old.first?.interrupt()
        old.second?.let(::closeNativeHandle)
        val key = AndroidCredentialStore(context, "key:${profile.deviceIdentityReference}").load() ?: error("Device key is unavailable; pair again")
        val token = AndroidCredentialStore(context, "relay:${profile.deviceIdentityReference}").load() ?: error("Relay credential is unavailable; pair again")
        val handle = try { NativeHandle(open(profile, token.decodeToString(), key, null)) } finally { key.fill(0); token.fill(0) }
        synchronized(stateLock) { handles[profile.id] = handle }
    }

    override suspend fun disconnect(profile: HostProfile): GatewayResult<Unit> = invoke {
        val (poller, handle) = synchronized(stateLock) {
            val currentHandle = handles.remove(profile.id)
            subscriptions.remove(profile.id)
            pollers.remove(profile.id) to currentHandle
        }
        poller?.interrupt()
        handle?.let(::closeNativeHandle)
    }

    override suspend fun transfer(profile: HostProfile, params: JsonElement): GatewayResult<JsonElement> = invoke {
        json.parseToJsonElement(withNativeHandle(profile) { NativeHostTransport.transfer(it, params.toString()) })
    }

    override suspend fun rawRequest(
        profile: HostProfile,
        method: String,
        params: JsonElement,
    ): GatewayResult<JsonElement> = invoke {
        val raw = withNativeHandle(profile) { handle ->
            NativeHostTransport.request(handle, method, params.toString())
        }
        json.parseToJsonElement(raw)
    }

    override suspend fun listThreads(profile: HostProfile, query: ThreadListQuery): GatewayResult<ThreadListPage> =
        commonCodexClient.listThreads(profile, query)

    override suspend fun readThread(profile: HostProfile, threadId: String): GatewayResult<ThreadReadResult> =
        commonCodexClient.readThread(profile, threadId)

    override suspend fun startThread(profile: HostProfile, cwd: String): GatewayResult<ThreadSnapshot> =
        commonCodexClient.startThread(profile, cwd)

    override suspend fun startTurn(profile: HostProfile, threadId: String, cwd: String, text: String, attachments: List<CodexAttachment>, resume: Boolean, clientUserMessageId: String): GatewayResult<String> =
        commonCodexClient.startTurn(profile, threadId, cwd, text, attachments, resume, clientUserMessageId)

    override suspend fun steerTurn(profile: HostProfile, threadId: String, turnId: String, text: String, attachments: List<CodexAttachment>, clientUserMessageId: String): GatewayResult<Unit> =
        commonCodexClient.steerTurn(profile, threadId, turnId, text, attachments, clientUserMessageId)

    override suspend fun queueTurn(profile: HostProfile, threadId: String, text: String, attachments: List<CodexAttachment>, clientUserMessageId: String): GatewayResult<String> =
        commonCodexClient.queueTurn(profile, threadId, text, attachments, clientUserMessageId)

    override suspend fun interrupt(profile: HostProfile, threadId: String, turnId: String): GatewayResult<Unit> =
        commonCodexClient.interrupt(profile, threadId, turnId)

    override suspend fun respondResult(
        profile: HostProfile,
        requestId: JsonElement,
        result: JsonElement,
    ): GatewayResult<Unit> = invoke {
        withNativeHandle(profile) { handle ->
            check(NativeHostTransport.respondResult(handle, requestId.toString(), result.toString()))
        }
    }

    override suspend fun respondError(
        profile: HostProfile,
        requestId: JsonElement,
        error: JsonElement,
    ): GatewayResult<Unit> = invoke {
        withNativeHandle(profile) { handle ->
            check(NativeHostTransport.respondError(handle, requestId.toString(), error.toString()))
        }
    }

    /**
     * One poller drains both native queues and fans every raw message out to
     * subscribers. No notification is decoded into a lossy allow-list here.
     */
    override fun subscribeRaw(profile: HostProfile, onMessage: (RawCodexMessage) -> Unit): HostEventSubscription =
        subscribeRaw(profile, onMessage) {}

    override fun subscribeRaw(
        profile: HostProfile,
        onMessage: (RawCodexMessage) -> Unit,
        onClosed: (String) -> Unit,
    ): HostEventSubscription {
        val subscribed = synchronized(stateLock) {
            val handle = handles[profile.id] ?: return@synchronized false
            subscriptions.getOrPut(profile.id) { linkedSetOf() }.add(onMessage)
            if (pollers[profile.id] == null) {
                pollers[profile.id] = Thread {
                    pollLoop(profile.id, handle, onClosed)
                }.apply {
                    isDaemon = true
                    start()
                }
            }
            true
        }
        if (!subscribed) return HostEventSubscription {}
        return HostEventSubscription {
            synchronized(stateLock) {
                subscriptions[profile.id]?.remove(onMessage)
                if (subscriptions[profile.id].isNullOrEmpty()) {
                    subscriptions.remove(profile.id)
                    pollers.remove(profile.id)?.interrupt()
                }
            }
        }
    }

    private fun pollLoop(hostIdentity: String, handle: NativeHandle, onClosed: (String) -> Unit) {
        while (!Thread.currentThread().isInterrupted) {
            val currentHandle = synchronized(stateLock) { handles[hostIdentity] }
            if (currentHandle !== handle) return
            val readLock = handle.lifetime.readLock()
            readLock.lock()
            val current = try {
                if (synchronized(stateLock) { handles[hostIdentity] } !== handle || handle.closed) return
                NativeHostTransport.nextNotification(handle.pointer)
                    ?: NativeHostTransport.nextServerRequest(handle.pointer)
            } catch (failure: Throwable) {
                runCatching { onClosed(failure.message ?: "PC Host connection closed") }
                return
            } finally {
                readLock.unlock()
            }
            if (current != null) {
                parseRawCodexMessage(current)?.let { message ->
                    val listeners = synchronized(stateLock) {
                        if (handles[hostIdentity] === handle) {
                            subscriptions[hostIdentity]?.toList().orEmpty()
                        } else {
                            emptyList()
                        }
                    }
                    listeners.forEach { listener -> runCatching { listener(message) } }
                }
            } else {
                try {
                    Thread.sleep(PollIntervalMs)
                } catch (_: InterruptedException) {
                    return
                }
            }
        }
    }

    /** Opens the single configured Phoenix relay path. */
    private fun open(profile: HostProfile, relayToken: String, key: ByteArray, ticket: String?): Long {
        val handle = NativeHostTransport.connect(config(profile, relayToken, ticket), key)
        check(handle != 0L) { "Native connect returned no handle" }
        return handle
    }

    private fun closeNativeHandle(handle: NativeHandle) {
        val writeLock = handle.lifetime.writeLock()
        writeLock.lock()
        try {
            if (!handle.closed) {
                handle.closed = true
                if (handle.pointer != 0L) runCatching { NativeHostTransport.close(handle.pointer) }
            }
        } finally {
            writeLock.unlock()
        }
    }

    private fun <T> withNativeHandle(profile: HostProfile, block: (Long) -> T): T {
        val handle = synchronized(stateLock) { handles[profile.id] }
            ?: error("Host is not connected")
        val readLock = handle.lifetime.readLock()
        readLock.lock()
        return try {
            check(synchronized(stateLock) { handles[profile.id] } === handle && !handle.closed) {
                "Host is not connected"
            }
            block(handle.pointer)
        } finally {
            readLock.unlock()
        }
    }

    private class NativeHandle(
        val pointer: Long,
        val lifetime: ReentrantReadWriteLock = ReentrantReadWriteLock(true),
        var closed: Boolean = false,
    )

    private fun config(profile: HostProfile, relayToken: String, ticket: String?): String = buildJsonObject {
        put("relayUrl", profile.relayUrl)
        put("runnerId", profile.runnerId)
        put("hostIdentity", profile.hostIdentity)
        put("deviceName", android.os.Build.MODEL)
        put("relayToken", relayToken)
        ticket?.let { put("pairingTicket", it) }
        put("requestTimeoutMs", 30_000)
    }.toString()

    private suspend fun <T> invoke(block: () -> T): GatewayResult<T> = withContext(Dispatchers.IO) {
        try {
            GatewayResult.Success(block())
        } catch (failure: Throwable) {
            val raw = runCatching { json.parseToJsonElement(failure.message.orEmpty()) }.getOrNull()
            GatewayResult.Failure(failure.message ?: "PC Host connection failed.", raw)
        }
    }

    private companion object {
        const val PollIntervalMs = 50L
    }
}

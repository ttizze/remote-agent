package dev.remoteagent.mobile

import android.content.Context
import android.os.Build
import android.os.Handler
import android.os.Looper
import java.util.Base64
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.locks.ReentrantReadWriteLock
import kotlin.coroutines.resume
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.suspendCancellableCoroutine
import kotlinx.coroutines.withContext
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put

private const val DefaultMaxFrameBytes = 256 * 1024 * 1024

/** Android adapter over the native raw Codex JSON-RPC client. */
class AndroidHostGateway(private val context: Context) : HostGateway {
    private val json = Json { ignoreUnknownKeys = false; isLenient = false }
    /** Guards ownership of the handle map and subscription state. */
    private val stateLock = Any()
    private val handles = mutableMapOf<String, NativeHandle>()
    private val subscriptions = mutableMapOf<String, MutableSet<(RawCodexMessage) -> Unit>>()
    private val pollers = mutableMapOf<String, Thread>()
    private val discovery = AndroidMdnsDiscovery(context)
    private val commonCodexClient = CommonCodexClient(this)

    override suspend fun pair(payload: PairingQrPayload): GatewayResult<HostProfile> = invoke {
        val keyStore = AndroidDeviceIdentityStore(context, payload.hostIdentity)
        val key = keyStore.loadOrCreate {
            decodeKey(NativeHostTransport.generateDeviceKey())
        }
        try {
            closeNativeHandle(NativeHandle(openFirst(payload.addresses, payload.hostIdentity, payload.ticket, key)))
        } finally {
            key.fill(0)
        }
        HostProfile(payload.hostIdentity, payload.hostIdentity.take(12), payload.addresses, keyStore.deviceIdentityReference())
    }

    override suspend fun discover(profile: HostProfile): GatewayResult<List<String>> = suspendCancellableCoroutine { continuation ->
        val finished = AtomicBoolean(false)
        val timeout = Runnable { finishDiscovery(profile, emptyList(), finished, continuation) }
        val handler = Handler(Looper.getMainLooper())
        discovery.start(
            onEndpoint = { endpoint ->
                finishDiscovery(profile, listOf(endpoint), finished, continuation)
                handler.removeCallbacks(timeout)
            },
            onError = {
                finishDiscovery(profile, emptyList(), finished, continuation)
                handler.removeCallbacks(timeout)
            },
        )
        handler.postDelayed(timeout, DiscoveryTimeoutMs)
        continuation.invokeOnCancellation { handler.removeCallbacks(timeout); discovery.stop() }
    }

    override suspend fun connect(profile: HostProfile): GatewayResult<Unit> = invoke {
        val key = AndroidDeviceIdentityStore(context, profile.hostIdentity).load()
            ?: error("Device identity is unavailable")
        try {
            val old = synchronized(stateLock) {
                val oldHandle = handles.remove(profile.hostIdentity)
                subscriptions.remove(profile.hostIdentity)
                pollers.remove(profile.hostIdentity) to oldHandle
            }
            old.first?.interrupt()
            old.second?.let(::closeNativeHandle)
            val handle = NativeHandle(openFirst(profile.addresses, profile.hostIdentity, null, key))
            synchronized(stateLock) { handles[profile.hostIdentity] = handle }
        } finally {
            key.fill(0)
        }
    }

    override suspend fun disconnect(profile: HostProfile): GatewayResult<Unit> = invoke {
        val (poller, handle) = synchronized(stateLock) {
            val currentHandle = handles.remove(profile.hostIdentity)
            subscriptions.remove(profile.hostIdentity)
            pollers.remove(profile.hostIdentity) to currentHandle
        }
        poller?.interrupt()
        handle?.let(::closeNativeHandle)
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

    override suspend fun listProjects(profile: HostProfile): GatewayResult<List<CodexProject>> =
        commonCodexClient.listProjects(profile)

    override suspend fun listThreads(profile: HostProfile, cwd: String): GatewayResult<List<ThreadSummary>> =
        commonCodexClient.listThreads(profile, cwd)

    override suspend fun readThread(profile: HostProfile, threadId: String): GatewayResult<ThreadReadResult> =
        commonCodexClient.readThread(profile, threadId)

    override suspend fun startThread(profile: HostProfile, cwd: String): GatewayResult<ThreadSnapshot> =
        commonCodexClient.startThread(profile, cwd)

    override suspend fun startThread(
        profile: HostProfile,
        cwd: String,
        projectId: String?,
        firstPrompt: String,
    ): GatewayResult<ThreadStartResult> = commonCodexClient.startThread(profile, cwd, projectId, firstPrompt)

    override suspend fun startTurn(profile: HostProfile, threadId: String, cwd: String, text: String): GatewayResult<String> =
        commonCodexClient.startTurn(profile, threadId, cwd, text)

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
            val handle = handles[profile.hostIdentity] ?: return@synchronized false
            subscriptions.getOrPut(profile.hostIdentity) { linkedSetOf() }.add(onMessage)
            if (pollers[profile.hostIdentity] == null) {
                pollers[profile.hostIdentity] = Thread {
                    pollLoop(profile.hostIdentity, handle, onClosed)
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
                subscriptions[profile.hostIdentity]?.remove(onMessage)
                if (subscriptions[profile.hostIdentity].isNullOrEmpty()) {
                    subscriptions.remove(profile.hostIdentity)
                    pollers.remove(profile.hostIdentity)?.interrupt()
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

    /** Rust validates the pinned Host identity during every connect attempt. */
    private fun openFirst(addresses: List<String>, identity: String, ticket: String?, key: ByteArray): Long {
        var lastFailure: Throwable? = null
        val keyBase64 = encodeKey(key)
        for (address in addresses.asSequence().map(String::trim).filter(String::isNotEmpty).distinct()) {
            try {
                val handle = NativeHostTransport.connect(config(address, identity, ticket), keyBase64)
                if (handle != 0L) return handle
                lastFailure = IllegalStateException("Native connect returned no handle")
            } catch (failure: Throwable) {
                lastFailure = failure
            }
        }
        throw (lastFailure ?: IllegalStateException("No discovered host address"))
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
        val handle = synchronized(stateLock) { handles[profile.hostIdentity] }
            ?: error("Host is not connected")
        val readLock = handle.lifetime.readLock()
        readLock.lock()
        return try {
            check(synchronized(stateLock) { handles[profile.hostIdentity] } === handle && !handle.closed) {
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

    private fun config(address: String, identity: String, ticket: String?): String = buildJsonObject {
        put("address", address)
        put("serverName", "remote-agent")
        put("hostIdentity", identity)
        put("deviceName", Build.MODEL)
        put("maxFrameBytes", DefaultMaxFrameBytes)
        put("requestTimeoutMs", 30_000)
        ticket?.let { put("pairingTicket", it) }
    }.toString()

    private suspend fun <T> invoke(block: () -> T): GatewayResult<T> = withContext(Dispatchers.IO) {
        try {
            GatewayResult.Success(block())
        } catch (failure: Throwable) {
            val raw = runCatching { json.parseToJsonElement(failure.message.orEmpty()) }.getOrNull()
            GatewayResult.Failure(failure.message ?: "PC Host connection failed.", raw)
        }
    }

    private fun finishDiscovery(
        profile: HostProfile,
        discovered: List<String>,
        finished: AtomicBoolean,
        continuation: kotlinx.coroutines.CancellableContinuation<GatewayResult<List<String>>>,
    ) {
        if (finished.compareAndSet(false, true)) {
            discovery.stop()
            if (continuation.isActive) continuation.resume(GatewayResult.Success((profile.addresses + discovered).distinct()))
        }
    }

    private fun decodeKey(value: String) = Base64.getUrlDecoder().decode(value)
    private fun encodeKey(value: ByteArray) = Base64.getUrlEncoder().withoutPadding().encodeToString(value)

    private companion object {
        const val DiscoveryTimeoutMs = 1_500L
        const val PollIntervalMs = 50L
    }
}

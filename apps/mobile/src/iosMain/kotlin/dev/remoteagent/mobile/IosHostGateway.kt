package dev.remoteagent.mobile

import kotlinx.cinterop.ByteVar
import kotlinx.cinterop.CPointerVar
import kotlinx.cinterop.ExperimentalForeignApi
import kotlinx.cinterop.alloc
import kotlinx.cinterop.memScoped
import kotlinx.cinterop.ptr
import kotlinx.cinterop.value
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.delay
import kotlinx.coroutines.ensureActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put
import mobile_client.mobile_client_agent_command
import mobile_client.mobile_client_transfer

/** Native request execution uses leased handles; replacement never closes a borrowed handle. */
@OptIn(ExperimentalForeignApi::class)
internal class IosHostGateway : HostGateway {
    override val codex = CommonCodexClient(this, deferItemDetails = true)
    private val handles = IosClientHandles()
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)

    override suspend fun pair(payload: PairingQrPayload): GatewayResult<HostProfile> =
        withContext(Dispatchers.Default) {
            try {
                val reference = payload.hostIdentity
                val key = IosCredentialStore.loadOrGenerate("key:$reference") { generatedPkcs8() }
                IosCredentialStore.save("relay:$reference", payload.relayToken.encodeToByteArray())
                val profile =
                    HostProfile(payload.runnerId, payload.hostName, payload.relayUrl, payload.hostIdentity, reference)
                when (val result = connectIosHost(handles, profile, payload.relayToken, key, payload.ticket)) {
                    is GatewayResult.Success -> GatewayResult.Success(profile)
                    is GatewayResult.Failure -> result
                }
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (failure: IllegalStateException) {
                GatewayResult.Failure(failure.message ?: "Keychainへの保存に失敗しました")
            } catch (failure: IllegalArgumentException) {
                GatewayResult.Failure(failure.message ?: "Keychainへの保存に失敗しました")
            }
        }

    override suspend fun discover(profile: HostProfile): GatewayResult<List<String>> =
        GatewayResult.Success(listOf(profile.relayUrl))

    override suspend fun connect(profile: HostProfile): GatewayResult<Unit> =
        withContext(Dispatchers.Default) {
            try {
                val key =
                    IosCredentialStore.load("key:${profile.deviceIdentityReference}") ?: error("秘密鍵がありません。再ペアリングしてください")
                val token =
                    IosCredentialStore.load("relay:${profile.deviceIdentityReference}")
                        ?: error("接続資格情報がありません。再ペアリングしてください")
                connectIosHost(handles, profile, token.decodeToString(), key, null)
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (failure: IllegalStateException) {
                GatewayResult.Failure(failure.message ?: "Keychainを読み出せません")
            } catch (failure: IllegalArgumentException) {
                GatewayResult.Failure(failure.message ?: "Keychainを読み出せません")
            }
        }

    override suspend fun transfer(profile: HostProfile, params: JsonElement): GatewayResult<JsonElement> =
        withContext(Dispatchers.Default) {
            handles.withHandle(profile.id) { current ->
                memScoped {
                    val error = alloc<CPointerVar<ByteVar>>()
                    error.value = null
                    takeResult(mobile_client_transfer(current, params.toString(), error.ptr), error.value).mapGateway {
                        iosJson.parseToJsonElement(it)
                    }
                }
            } ?: GatewayResult.Failure("PC Hostへ接続されていません")
        }

    override suspend fun agentCommand(profile: HostProfile, command: AgentCommand): GatewayResult<String> =
        withContext(Dispatchers.Default) {
                handles.withHandle(profile.id) { current ->
                    memScoped {
                        val error = alloc<CPointerVar<ByteVar>>()
                        error.value = null
                        takeResult(mobile_client_agent_command(current, command.encode(), error.ptr), error.value)
                    }
                } ?: GatewayResult.Failure("PC Hostへ接続されていません")
            }
            .agentResult()

    override suspend fun rawRequest(
        profile: HostProfile,
        method: String,
        params: JsonElement,
    ): GatewayResult<JsonElement> =
        withContext(Dispatchers.Default) {
            handles
                .withHandle(profile.id) { nativeRequest(it, method, params) }
                .let { it ?: GatewayResult.Failure("PC Hostへ接続されていません") }
                .mapGateway { response -> iosJson.parseToJsonElement(response) }
        }

    override suspend fun disconnect(profile: HostProfile): GatewayResult<Unit> =
        withContext(Dispatchers.Default) {
            handles.retire(profile.id)
            GatewayResult.Success(Unit)
        }

    override suspend fun respondResult(
        profile: HostProfile,
        requestId: JsonElement,
        result: JsonElement,
    ): GatewayResult<Unit> =
        handles.withHandle(profile.id) { nativeResponse(it, requestId, result, isError = false) }
            ?: GatewayResult.Failure("PC Hostへ接続されていません")

    override suspend fun respondError(
        profile: HostProfile,
        requestId: JsonElement,
        error: JsonElement,
    ): GatewayResult<Unit> =
        handles.withHandle(profile.id) { nativeResponse(it, requestId, error, isError = true) }
            ?: GatewayResult.Failure("PC Hostへ接続されていません")

    override fun subscribeRaw(
        profile: HostProfile,
        onMessage: (RawCodexMessage) -> Unit,
        onClosed: (String) -> Unit,
    ): HostEventSubscription {
        val hostIdentity = profile.id
        val job =
            handles.register(hostIdentity) { subscribedHandle ->
                scope.launch(start = CoroutineStart.LAZY) {
                    drainRawMessages(handles, hostIdentity, subscribedHandle, onMessage, onClosed)
                }
            } ?: return HostEventSubscription {}
        job.start()
        return HostEventSubscription {
            handles.removeSubscription(hostIdentity, job)
            job.cancel()
        }
    }
}

private const val POLL_INTERVAL_MS = 50L

@OptIn(ExperimentalForeignApi::class)
private suspend fun drainRawMessages(
    handles: IosClientHandles,
    hostIdentity: String,
    subscribedHandle: IosClientHandles.NativeHandle,
    onMessage: (RawCodexMessage) -> Unit,
    onClosed: (String) -> Unit,
) {
    try {
        while (true) {
            currentCoroutineContext().ensureActive()
            if (!handles.isCurrentHandle(hostIdentity, subscribedHandle)) return
            val message = handles.withHandle(hostIdentity, subscribedHandle, ::nextRawMessage)
            if (message == null) delay(POLL_INTERVAL_MS)
            else if (handles.isCurrentHandle(hostIdentity, subscribedHandle)) onMessage(message)
        }
    } catch (cancelled: CancellationException) {
        throw cancelled
    } catch (failure: IllegalStateException) {
        notifyClosed(handles, hostIdentity, subscribedHandle, failure, onClosed)
    } catch (failure: IllegalArgumentException) {
        notifyClosed(handles, hostIdentity, subscribedHandle, failure, onClosed)
    } finally {
        currentCoroutineContext()[Job]?.let { handles.removeSubscription(hostIdentity, it) }
    }
}

@OptIn(ExperimentalForeignApi::class)
private fun notifyClosed(
    handles: IosClientHandles,
    hostIdentity: String,
    subscribedHandle: IosClientHandles.NativeHandle,
    failure: Exception,
    onClosed: (String) -> Unit,
) {
    if (handles.isCurrentHandle(hostIdentity, subscribedHandle)) {
        runCatching { onClosed(failure.message ?: "PC Hostとの接続が切れました") }
    }
}

@OptIn(ExperimentalForeignApi::class)
private suspend fun connectIosHost(
    handles: IosClientHandles,
    profile: HostProfile,
    relayToken: String,
    key: ByteArray,
    ticket: String?,
): GatewayResult<Unit> {
    val runnerId = profile.id
    val config =
        buildJsonObject {
                put("relayUrl", profile.relayUrl)
                put("runnerId", profile.runnerId)
                put("hostIdentity", profile.hostIdentity)
                put("deviceName", platform.UIKit.UIDevice.currentDevice.name)
                put("relayToken", relayToken)
                ticket?.let { put("pairingTicket", it) }
                put("requestTimeoutMs", DEFAULT_REQUEST_TIMEOUT_MS)
            }
            .toString()
    return when (val result = callConnect(config, key)) {
        is GatewayResult.Success -> {
            handles.replace(runnerId, result.value)
            GatewayResult.Success(Unit)
        }
        is GatewayResult.Failure -> result
    }
}

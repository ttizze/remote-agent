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
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.withContext
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put
import mobile_client.mobile_client_agent_command
import mobile_client.mobile_client_close
import mobile_client.mobile_client_transfer

/** Native request execution uses leased handles; replacement never closes a borrowed handle. */
@OptIn(ExperimentalForeignApi::class)
internal class IosHostGateway : HostGateway {
    private val handles = MutableStateFlow<Map<String, NativeHostConnection>>(emptyMap())
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
            handles.value[profile.id]?.handle?.let { current ->
                memScoped {
                    val error = alloc<CPointerVar<ByteVar>>()
                    error.value = null
                    takeResult(mobile_client_transfer(current.toULong(), params.toString(), error.ptr), error.value)
                        .mapGateway { iosJson.parseToJsonElement(it) }
                }
            } ?: GatewayResult.Failure("PC Hostへ接続されていません")
        }

    override suspend fun agentCommand(profile: HostProfile, command: AgentCommand): GatewayResult<String> =
        withContext(Dispatchers.Default) {
                handles.value[profile.id]?.handle?.let { current ->
                    memScoped {
                        val error = alloc<CPointerVar<ByteVar>>()
                        error.value = null
                        takeResult(
                            mobile_client_agent_command(current.toULong(), command.encode(), error.ptr),
                            error.value,
                        )
                    }
                } ?: GatewayResult.Failure("PC Hostへ接続されていません")
            }
            .agentResult()

    override suspend fun disconnect(profile: HostProfile): GatewayResult<Unit> =
        withContext(Dispatchers.Default) {
            handles.replaceNativeHost(profile.id, null) { mobile_client_close(it.toULong()) }
            GatewayResult.Success(Unit)
        }

    override fun subscribeRaw(
        profile: HostProfile,
        onMessage: (RawCodexMessage) -> Unit,
        onClosed: (String) -> Unit,
    ): HostEventSubscription = handles.subscribeNativeHost(profile.id, scope, ::nextEvent, onMessage, onClosed)
}

@OptIn(ExperimentalForeignApi::class)
private suspend fun connectIosHost(
    handles: MutableStateFlow<Map<String, NativeHostConnection>>,
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
        put("requestTimeoutMs", DEFAULT_REQUEST_TIMEOUT_MS)
    }
        .toString()
    return when (val result = callConnect(config, key)) {
        is GatewayResult.Success -> {
            handles.replaceNativeHost(runnerId, result.value) { mobile_client_close(it.toULong()) }
            GatewayResult.Success(Unit)
        }
        is GatewayResult.Failure -> result
    }
}

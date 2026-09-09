package dev.remoteagent.mobile

import android.content.Context
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.withContext
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonElement

/** Android adapter over the native raw Codex JSON-RPC client. */
class AndroidHostGateway(private val context: Context) : HostGateway {
    private val connections = MutableStateFlow<Map<String, NativeHostConnection>>(emptyMap())
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)

    override suspend fun pair(payload: PairingQrPayload): GatewayResult<HostProfile> = invokeAndroidHost {
        val reference = payload.hostIdentity
        val key =
            AndroidCredentialStore(context, "key:$reference").loadOrCreate {
                java.util.Base64.getUrlDecoder().decode(NativeHostTransport.generateDeviceKey())
            }
        AndroidCredentialStore(context, "relay:$reference").save(payload.relayToken.encodeToByteArray())
        val profile = HostProfile(payload.runnerId, payload.hostName, payload.relayUrl, payload.hostIdentity, reference)
        try {
            NativeHostTransport.close(openAndroidHost(profile, payload.relayToken, key, payload.ticket))
        } finally {
            key.fill(0)
        }
        profile
    }

    override suspend fun discover(profile: HostProfile): GatewayResult<List<String>> =
        GatewayResult.Success(listOf(profile.relayUrl))

    override suspend fun connect(profile: HostProfile): GatewayResult<Unit> = invokeAndroidHost {
        connections.replaceNativeHost(profile.id, null, NativeHostTransport::close)
        val key =
            AndroidCredentialStore(context, "key:${profile.deviceIdentityReference}").load()
                ?: error("Device key is unavailable; pair again")
        val token =
            AndroidCredentialStore(context, "relay:${profile.deviceIdentityReference}").load()
                ?: error("Relay credential is unavailable; pair again")
        val handle =
            try {
                openAndroidHost(profile, token.decodeToString(), key, null)
            } finally {
                key.fill(0)
                token.fill(0)
            }
        connections.replaceNativeHost(profile.id, handle, NativeHostTransport::close)
    }

    override suspend fun disconnect(profile: HostProfile): GatewayResult<Unit> = invokeAndroidHost {
        connections.replaceNativeHost(profile.id, null, NativeHostTransport::close)
    }

    override suspend fun transfer(profile: HostProfile, params: JsonElement): GatewayResult<JsonElement> =
        invokeAndroidHost {
            Json.parseToJsonElement(
                NativeHostTransport.transfer(connections.nativeHandle(profile.id), params.toString())
            )
        }

    override suspend fun agentCommand(profile: HostProfile, command: AgentCommand): GatewayResult<String> =
        invokeAndroidHost {
            NativeHostTransport.agentCommand(connections.nativeHandle(profile.id), command.encode())
        }
        .agentResult()

    override fun subscribeRaw(
        profile: HostProfile,
        onMessage: (RawCodexMessage) -> Unit,
        onClosed: (String) -> Unit,
    ): HostEventSubscription =
        connections.subscribeNativeHost(profile.id, scope, NativeHostTransport::nextEvent, onMessage, onClosed)
}

private suspend fun <T> invokeAndroidHost(block: () -> T): GatewayResult<T> =
    withContext(Dispatchers.IO) {
        try {
            GatewayResult.Success(block())
        } catch (cancelled: kotlinx.coroutines.CancellationException) {
            throw cancelled
        } catch (failure: java.io.IOException) {
            val raw = runCatching { Json.parseToJsonElement(failure.message.orEmpty()) }.getOrNull()
            GatewayResult.Failure(failure.message ?: "PC Host connection failed.", raw)
        } catch (failure: java.security.GeneralSecurityException) {
            val raw = runCatching { Json.parseToJsonElement(failure.message.orEmpty()) }.getOrNull()
            GatewayResult.Failure(failure.message ?: "PC Host connection failed.", raw)
        } catch (failure: IllegalArgumentException) {
            val raw = runCatching { Json.parseToJsonElement(failure.message.orEmpty()) }.getOrNull()
            GatewayResult.Failure(failure.message ?: "PC Host connection failed.", raw)
        } catch (failure: IllegalStateException) {
            val raw = runCatching { Json.parseToJsonElement(failure.message.orEmpty()) }.getOrNull()
            GatewayResult.Failure(failure.message ?: "PC Host connection failed.", raw)
        } catch (failure: SecurityException) {
            val raw = runCatching { Json.parseToJsonElement(failure.message.orEmpty()) }.getOrNull()
            GatewayResult.Failure(failure.message ?: "PC Host connection failed.", raw)
        }
    }

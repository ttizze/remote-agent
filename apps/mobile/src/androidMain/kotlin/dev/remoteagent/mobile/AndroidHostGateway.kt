package dev.remoteagent.mobile

import android.content.Context
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonElement

/** Android adapter over the native raw Codex JSON-RPC client. */
class AndroidHostGateway(private val context: Context) : HostGateway {
    private val json = Json {
        ignoreUnknownKeys = false
        isLenient = false
    }
    private val connections = AndroidHostConnections()
    override val codex = CommonCodexClient(this)

    override suspend fun pair(payload: PairingQrPayload): GatewayResult<HostProfile> = invoke {
        val reference = payload.hostIdentity
        val key =
            AndroidCredentialStore(context, "key:$reference").loadOrCreate {
                java.util.Base64.getUrlDecoder().decode(NativeHostTransport.generateDeviceKey())
            }
        AndroidCredentialStore(context, "relay:$reference").save(payload.relayToken.encodeToByteArray())
        val profile = HostProfile(payload.runnerId, payload.hostName, payload.relayUrl, payload.hostIdentity, reference)
        try {
            openAndroidHost(profile, payload.relayToken, key, payload.ticket).close()
        } finally {
            key.fill(0)
        }
        profile
    }

    override suspend fun discover(profile: HostProfile): GatewayResult<List<String>> =
        GatewayResult.Success(listOf(profile.relayUrl))

    override suspend fun connect(profile: HostProfile): GatewayResult<Unit> = invoke {
        connections.retire(profile.id)
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
        connections.replace(profile.id, handle)
    }

    override suspend fun disconnect(profile: HostProfile): GatewayResult<Unit> = invoke {
        connections.retire(profile.id)
    }

    override suspend fun transfer(profile: HostProfile, params: JsonElement): GatewayResult<JsonElement> = invoke {
        json.parseToJsonElement(connections.withHandle(profile) { NativeHostTransport.transfer(it, params.toString()) })
    }

    override suspend fun rawRequest(
        profile: HostProfile,
        method: String,
        params: JsonElement,
    ): GatewayResult<JsonElement> = invoke {
        val raw =
            connections.withHandle(profile) { handle -> NativeHostTransport.request(handle, method, params.toString()) }
        json.parseToJsonElement(raw)
    }

    override suspend fun respondResult(
        profile: HostProfile,
        requestId: JsonElement,
        result: JsonElement,
    ): GatewayResult<Unit> = invoke {
        connections.withHandle(profile) { handle ->
            check(NativeHostTransport.respondResult(handle, requestId.toString(), result.toString()))
        }
    }

    override suspend fun respondError(
        profile: HostProfile,
        requestId: JsonElement,
        error: JsonElement,
    ): GatewayResult<Unit> = invoke {
        connections.withHandle(profile) { handle ->
            check(NativeHostTransport.respondError(handle, requestId.toString(), error.toString()))
        }
    }

    override fun subscribeRaw(
        profile: HostProfile,
        onMessage: (RawCodexMessage) -> Unit,
        onClosed: (String) -> Unit,
    ): HostEventSubscription = connections.subscribe(profile, onMessage, onClosed)

    private suspend fun <T> invoke(block: () -> T): GatewayResult<T> =
        withContext(Dispatchers.IO) {
            try {
                GatewayResult.Success(block())
            } catch (cancelled: kotlinx.coroutines.CancellationException) {
                throw cancelled
            } catch (failure: java.io.IOException) {
                val raw = runCatching { json.parseToJsonElement(failure.message.orEmpty()) }.getOrNull()
                GatewayResult.Failure(failure.message ?: "PC Host connection failed.", raw)
            } catch (failure: java.security.GeneralSecurityException) {
                val raw = runCatching { json.parseToJsonElement(failure.message.orEmpty()) }.getOrNull()
                GatewayResult.Failure(failure.message ?: "PC Host connection failed.", raw)
            } catch (failure: IllegalArgumentException) {
                val raw = runCatching { json.parseToJsonElement(failure.message.orEmpty()) }.getOrNull()
                GatewayResult.Failure(failure.message ?: "PC Host connection failed.", raw)
            } catch (failure: IllegalStateException) {
                val raw = runCatching { json.parseToJsonElement(failure.message.orEmpty()) }.getOrNull()
                GatewayResult.Failure(failure.message ?: "PC Host connection failed.", raw)
            } catch (failure: SecurityException) {
                val raw = runCatching { json.parseToJsonElement(failure.message.orEmpty()) }.getOrNull()
                GatewayResult.Failure(failure.message ?: "PC Host connection failed.", raw)
            }
        }
}

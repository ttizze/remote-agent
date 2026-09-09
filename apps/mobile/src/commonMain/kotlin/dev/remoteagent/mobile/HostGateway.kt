package dev.remoteagent.mobile

import kotlinx.serialization.Serializable
import kotlinx.serialization.json.JsonElement

@Serializable data class CodexAttachment(val path: String, val name: String, val isImage: Boolean)

/** One client-assigned input identity is shared by start, steer, and queue. */
@Serializable
data class CodexTurnInput(
    val text: String,
    val attachments: List<CodexAttachment> = emptyList(),
    val clientUserMessageId: String,
)

@Serializable data class CodexTurnOptions(val model: String? = null, val effort: String? = null)

/** The only effect boundary used by common mobile presentation code. */
interface HostGateway {
    suspend fun agentCommand(profile: HostProfile, command: AgentCommand): GatewayResult<String>

    suspend fun pair(payload: PairingQrPayload): GatewayResult<HostProfile>

    suspend fun discover(profile: HostProfile): GatewayResult<List<String>>

    suspend fun connect(profile: HostProfile): GatewayResult<Unit>

    suspend fun disconnect(profile: HostProfile): GatewayResult<Unit>

    suspend fun transfer(profile: HostProfile, params: JsonElement): GatewayResult<JsonElement>

    /** Reports terminal transport failure without weakening the lossless message callback. */
    fun subscribeRaw(
        profile: HostProfile,
        onMessage: (RawCodexMessage) -> Unit,
        onClosed: (String) -> Unit,
    ): HostEventSubscription
}

fun interface HostEventSubscription {
    fun cancel()
}

sealed interface GatewayResult<out T> {
    data class Success<T>(val value: T) : GatewayResult<T>

    data class Failure(val message: String, val rawError: JsonElement? = null) : GatewayResult<Nothing>
}

internal fun <T, R> GatewayResult<T>.mapGateway(transform: (T) -> R): GatewayResult<R> =
    when (this) {
        is GatewayResult.Success -> GatewayResult.Success(transform(value))
        is GatewayResult.Failure -> this
    }

/** Platform storage owns durable I/O; common code never assumes its cache is authoritative. */
interface MobileRepository {
    fun load(): AppState

    fun save(state: AppState)
}

/** Platform I/O failure; only its sanitized cause type is exposed to diagnostics. */
internal class MobilePersistenceException(cause: Throwable) : Exception(cause)

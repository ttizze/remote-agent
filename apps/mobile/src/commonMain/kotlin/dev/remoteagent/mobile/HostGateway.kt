package dev.remoteagent.mobile

import kotlinx.serialization.json.JsonElement

data class CodexAttachment(val path: String, val name: String, val isImage: Boolean)

/** One client-assigned input identity is shared by start, steer, and queue. */
data class CodexTurnInput(
    val text: String,
    val attachments: List<CodexAttachment> = emptyList(),
    val clientUserMessageId: String,
)

/** The only effect boundary used by common mobile presentation code. */
interface HostGateway : RawCodexGateway {
    val codex: CodexGateway

    suspend fun pair(payload: PairingQrPayload): GatewayResult<HostProfile>

    suspend fun discover(profile: HostProfile): GatewayResult<List<String>>

    suspend fun connect(profile: HostProfile): GatewayResult<Unit>

    suspend fun disconnect(profile: HostProfile): GatewayResult<Unit>

    suspend fun transfer(profile: HostProfile, params: JsonElement): GatewayResult<JsonElement>
}

/** Typed conversation operations; platform transports expose one common client. */
interface CodexGateway {
    suspend fun listThreads(
        profile: HostProfile,
        query: ThreadListQuery = ThreadListQuery(),
    ): GatewayResult<ThreadListPage>

    suspend fun readThread(profile: HostProfile, threadId: String): GatewayResult<ThreadReadResult>

    suspend fun startThread(profile: HostProfile, cwd: String): GatewayResult<ThreadSnapshot>

    suspend fun startTurn(
        profile: HostProfile,
        threadId: String,
        cwd: String,
        input: CodexTurnInput,
        resume: Boolean,
    ): GatewayResult<String>

    suspend fun steerTurn(
        profile: HostProfile,
        threadId: String,
        turnId: String,
        input: CodexTurnInput,
    ): GatewayResult<Unit>

    suspend fun queueTurn(profile: HostProfile, threadId: String, input: CodexTurnInput): GatewayResult<String>

    suspend fun interrupt(profile: HostProfile, threadId: String, turnId: String): GatewayResult<Unit>
}

/**
 * Lossless Codex boundary. New Codex methods can be used without adding a HostGateway method, and server-originated
 * requests can be answered by the feature that owns them.
 */
interface RawCodexGateway {
    suspend fun rawRequest(
        profile: HostProfile,
        method: String,
        params: JsonElement = emptyJsonObject(),
    ): GatewayResult<JsonElement>

    /** Reports terminal transport failure without weakening the lossless message callback. */
    fun subscribeRaw(
        profile: HostProfile,
        onMessage: (RawCodexMessage) -> Unit,
        onClosed: (String) -> Unit,
    ): HostEventSubscription

    suspend fun respondResult(profile: HostProfile, requestId: JsonElement, result: JsonElement): GatewayResult<Unit>

    suspend fun respondError(profile: HostProfile, requestId: JsonElement, error: JsonElement): GatewayResult<Unit>
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

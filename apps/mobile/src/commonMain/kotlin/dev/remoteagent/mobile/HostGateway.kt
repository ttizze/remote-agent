package dev.remoteagent.mobile

import kotlinx.serialization.json.JsonElement

/** The only effect boundary used by common mobile presentation code. */
interface HostGateway : RawCodexGateway {
    suspend fun pair(payload: PairingQrPayload): GatewayResult<HostProfile>
    suspend fun discover(profile: HostProfile): GatewayResult<List<String>>
    suspend fun connect(profile: HostProfile): GatewayResult<Unit>
    suspend fun listThreads(profile: HostProfile, cwd: String): GatewayResult<List<ThreadSummary>>
    suspend fun readThread(profile: HostProfile, threadId: String): GatewayResult<ThreadReadResult>
    suspend fun startThread(profile: HostProfile, cwd: String): GatewayResult<ThreadSnapshot>
    suspend fun startTurn(profile: HostProfile, threadId: String, text: String): GatewayResult<String>
    suspend fun interrupt(profile: HostProfile, threadId: String, turnId: String): GatewayResult<Unit>

    /**
     * Convenience adapter for the current UI.  The transport-level API is
     * [subscribeRaw], which also exposes unknown notifications and requests.
     */
    fun subscribe(profile: HostProfile, onEvent: (ThreadEvent) -> Unit): HostEventSubscription =
        subscribeRaw(profile) { message ->
            message.toThreadEventOrNull()?.let(onEvent)
        }
}

/**
 * Lossless Codex boundary.  New Codex methods can be used without adding a
 * HostGateway method, and server-originated requests can be answered by the
 * feature that owns them.
 */
interface RawCodexGateway {
    suspend fun rawRequest(
        profile: HostProfile,
        method: String,
        params: JsonElement = emptyJsonObject(),
    ): GatewayResult<JsonElement>

    fun subscribeRaw(profile: HostProfile, onMessage: (RawCodexMessage) -> Unit): HostEventSubscription

    suspend fun respondResult(
        profile: HostProfile,
        requestId: JsonElement,
        result: JsonElement,
    ): GatewayResult<Unit>

    suspend fun respondError(
        profile: HostProfile,
        requestId: JsonElement,
        error: JsonElement,
    ): GatewayResult<Unit>
}

private fun RawCodexMessage.toThreadEventOrNull(): ThreadEvent? = when (this) {
    is RawCodexMessage.Notification -> codexThreadEvent(method, params)
    is RawCodexMessage.ServerRequest -> null
}

fun interface HostEventSubscription {
    fun cancel()
}

sealed interface GatewayResult<out T> {
    data class Success<T>(val value: T) : GatewayResult<T>
    data class Failure(val message: String, val rawError: JsonElement? = null) : GatewayResult<Nothing>
}

internal fun <T, R> GatewayResult<T>.mapGateway(transform: (T) -> R): GatewayResult<R> = when (this) {
    is GatewayResult.Success -> GatewayResult.Success(transform(value))
    is GatewayResult.Failure -> this
}

object UnavailableHostGateway : HostGateway {
    private fun <T> unavailable(): GatewayResult<T> = GatewayResult.Failure("PC Host connection is not configured on this device.")

    override suspend fun pair(payload: PairingQrPayload): GatewayResult<HostProfile> = unavailable()
    override suspend fun discover(profile: HostProfile): GatewayResult<List<String>> = unavailable()
    override suspend fun connect(profile: HostProfile): GatewayResult<Unit> = unavailable()
    override suspend fun listThreads(profile: HostProfile, cwd: String): GatewayResult<List<ThreadSummary>> = unavailable()
    override suspend fun readThread(profile: HostProfile, threadId: String): GatewayResult<ThreadReadResult> = unavailable()
    override suspend fun startThread(profile: HostProfile, cwd: String): GatewayResult<ThreadSnapshot> = unavailable()
    override suspend fun startTurn(profile: HostProfile, threadId: String, text: String): GatewayResult<String> = unavailable()
    override suspend fun interrupt(profile: HostProfile, threadId: String, turnId: String): GatewayResult<Unit> = unavailable()
    override suspend fun rawRequest(profile: HostProfile, method: String, params: JsonElement): GatewayResult<JsonElement> = unavailable()
    override fun subscribeRaw(profile: HostProfile, onMessage: (RawCodexMessage) -> Unit): HostEventSubscription = HostEventSubscription {}
    override suspend fun respondResult(profile: HostProfile, requestId: JsonElement, result: JsonElement): GatewayResult<Unit> = unavailable()
    override suspend fun respondError(profile: HostProfile, requestId: JsonElement, error: JsonElement): GatewayResult<Unit> = unavailable()
}

/** Platform storage owns durable I/O; common code never assumes its cache is authoritative. */
interface MobileRepository {
    fun load(): AppState
    fun save(state: AppState)
}

class InMemoryMobileRepository(initial: AppState = AppState()) : MobileRepository {
    private var state = initial
    override fun load(): AppState = state
    override fun save(state: AppState) {
        this.state = state
    }
}

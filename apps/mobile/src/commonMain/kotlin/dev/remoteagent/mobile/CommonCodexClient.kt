package dev.remoteagent.mobile

import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive

data class CodexModel(
    val id: String,
    val model: String,
    val displayName: String,
    val defaultReasoningEffort: String,
    val reasoningEfforts: List<String>,
)

data class CodexTurnOptions(val model: String? = null, val effort: String? = null)

/** Mobile DTO adapter. Rust owns request construction, validation, and agent operations. */
class CommonCodexClient(internal val gateway: HostGateway, private val deferItemDetails: Boolean = false) :
    CodexGateway {
    internal suspend fun command(profile: HostProfile, command: AgentCommand): GatewayResult<JsonElement> =
        gateway.agentCommand(profile, command).mapGateway(Json::parseToJsonElement)

    override suspend fun listThreads(profile: HostProfile, query: ThreadListQuery): GatewayResult<ThreadListPage> =
        command(profile, AgentCommand.ListThreads(query)).mapGateway(::parseThreadListPage)

    override suspend fun readThread(profile: HostProfile, threadId: String): GatewayResult<ThreadReadResult> =
        command(profile, AgentCommand.ReadThread(threadId, deferItemDetails)).mapGateway {
            ThreadReadResult(codexThreadFromResponse(it as JsonObject), emptyList())
        }

    suspend fun readOlderHistory(
        profile: HostProfile,
        threadId: String,
        cursor: String?,
        turnId: String? = null,
    ): GatewayResult<ThreadSnapshot> =
        command(profile, AgentCommand.ReadOlder(threadId, cursor, turnId, deferItemDetails)).mapGateway {
            codexThreadFromResponse(it as JsonObject)
        }

    suspend fun readItemDetails(
        profile: HostProfile,
        threadId: String,
        turnId: String,
        itemId: String,
    ): GatewayResult<String> =
        command(profile, AgentCommand.ReadItem(threadId, turnId, itemId)).mapGateway {
            codexItem((it as JsonObject).getValue("item") as JsonObject).expandedThreadItemBody()
        }

    override suspend fun startThread(
        profile: HostProfile,
        cwd: String,
        options: CodexTurnOptions,
    ): GatewayResult<ThreadSnapshot> =
        command(profile, AgentCommand.StartThread(cwd, options.model)).mapGateway {
            codexThreadFromResponse(it as JsonObject)
        }

    override suspend fun startTurn(
        profile: HostProfile,
        threadId: String,
        cwd: String,
        input: CodexTurnInput,
        resume: Boolean,
        options: CodexTurnOptions,
    ): GatewayResult<String> =
        command(profile, AgentCommand.StartTurn(threadId, cwd, input, resume, options.model, options.effort))
            .mapGateway { (it as JsonPrimitive).content }

    override suspend fun steerTurn(
        profile: HostProfile,
        threadId: String,
        turnId: String,
        input: CodexTurnInput,
    ): GatewayResult<Unit> = command(profile, AgentCommand.SteerTurn(threadId, turnId, input)).mapGateway { Unit }

    override suspend fun queueTurn(
        profile: HostProfile,
        threadId: String,
        input: CodexTurnInput,
    ): GatewayResult<String> =
        command(profile, AgentCommand.QueueTurn(threadId, input)).mapGateway { (it as JsonPrimitive).content }

    override suspend fun interrupt(profile: HostProfile, threadId: String, turnId: String): GatewayResult<Unit> =
        command(profile, AgentCommand.InterruptTurn(threadId, turnId)).mapGateway { Unit }
}

@Serializable
data class ThreadListQuery(
    val projectLimit: Int = 5,
    val chatLimit: Int = 5,
    val projectThreadLimits: Map<String, Int> = emptyMap(),
    val searchTerm: String = "",
)

data class ThreadListPage(
    val threads: List<ThreadSummary>,
    val projects: List<CodexProject> = emptyList(),
    val moreProjectIds: Set<String> = emptySet(),
    val hasMoreChats: Boolean = false,
    val hasMoreProjects: Boolean = false,
)

private fun parseThreadListPage(value: JsonElement): ThreadListPage {
    val root = value as JsonObject
    return ThreadListPage(
        (root.getValue("data") as JsonArray).map { codexThreadSummary(it as JsonObject) },
        (root.getValue("projects") as JsonArray).map(::codexProject),
        (root.getValue("moreProjectIds") as JsonArray).mapTo(mutableSetOf()) { (it as JsonPrimitive).content },
        root.boolean("hasMoreChats")!!,
        root.boolean("hasMoreProjects")!!,
    )
}

internal fun invalid(message: String): Nothing = throw IllegalArgumentException(message)

internal fun <T> GatewayResult<JsonElement>.decode(method: String, transform: (JsonElement) -> T): GatewayResult<T> =
    when (this) {
        is GatewayResult.Failure -> this
        is GatewayResult.Success ->
            try {
                GatewayResult.Success(transform(value))
            } catch (failure: IllegalArgumentException) {
                GatewayResult.Failure(
                    message = "Invalid $method response: ${failure.message ?: "malformed payload"}",
                    rawError = value,
                )
            }
    }

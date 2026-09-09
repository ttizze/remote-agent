package dev.remoteagent.mobile

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.encodeToString
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.decodeFromJsonElement

/** Native intent binding. Rust owns each operation's RPC and state semantics. */
@Serializable
sealed interface AgentCommand {
    @Serializable @SerialName("transcribe") data class Transcribe(val audio: String) : AgentCommand

    @Serializable
    @SerialName("respond")
    data class Respond(val request: JsonElement, val answer: RequestAnswer) : AgentCommand

    @Serializable @SerialName("sessionImages") data class SessionImages(val threadId: String) : AgentCommand

    @Serializable
    @SerialName("watchThread")
    data class WatchThread(val threadId: String, val watchKey: Long, val watchId: Long, val path: String) : AgentCommand

    @Serializable
    @SerialName("unwatchThread")
    data class UnwatchThread(val watchKey: Long, val watchId: Long) : AgentCommand

    @Serializable @SerialName("models") data object Models : AgentCommand

    @Serializable @SerialName("listThreads") data class ListThreads(val query: ThreadListQuery) : AgentCommand

    @Serializable
    @SerialName("readThread")
    data class ReadThread(val threadId: String, val deferItemDetails: Boolean) : AgentCommand

    @Serializable
    @SerialName("readOlder")
    data class ReadOlder(
        val threadId: String,
        val cursor: String?,
        val turnId: String?,
        val deferItemDetails: Boolean,
    ) : AgentCommand

    @Serializable
    @SerialName("readItem")
    data class ReadItem(val threadId: String, val turnId: String, val itemId: String) : AgentCommand

    @Serializable @SerialName("startThread") data class StartThread(val cwd: String, val model: String?) : AgentCommand

    @Serializable
    @SerialName("sendTurn")
    data class SendTurn(
        val threadId: String,
        val snapshot: JsonElement,
        val listed: JsonElement,
        val input: CodexTurnInput,
        val model: String?,
        val effort: String?,
    ) : AgentCommand

    @Serializable
    @SerialName("interruptTurn")
    data class InterruptTurn(val threadId: String, val turnId: String) : AgentCommand

    @Serializable @SerialName("listFiles") data class ListFiles(val path: String) : AgentCommand

    @Serializable @SerialName("readFile") data class ReadFile(val path: String) : AgentCommand

    @Serializable
    @SerialName("writeFile")
    data class WriteFile(val path: String, val revision: String, val text: String) : AgentCommand

    @Serializable @SerialName("reviewWorkspace") data class ReviewWorkspace(val cwd: String) : AgentCommand

    @Serializable @SerialName("worktreeSettings") data object WorktreeSettings : AgentCommand

    @Serializable
    @SerialName("updateWorktreeSettings")
    data class UpdateWorktreeSettings(val settings: HostWorktreeSettings) : AgentCommand

    @Serializable @SerialName("accounts") data object Accounts : AgentCommand

    @Serializable @SerialName("selectAccount") data class SelectAccount(val accountId: String) : AgentCommand

    @Serializable @SerialName("startAccountLogin") data object StartAccountLogin : AgentCommand

    @Serializable @SerialName("accountLoginStatus") data class AccountLoginStatus(val loginId: String) : AgentCommand

    @Serializable @SerialName("cancelAccountLogin") data class CancelAccountLogin(val loginId: String) : AgentCommand

    @Serializable
    @SerialName("forkThread")
    data class ForkThread(val threadId: String, val lastTurnId: String) : AgentCommand
}

@Serializable
sealed interface RequestAnswer {
    @Serializable @SerialName("decision") data class Decision(val index: Int) : RequestAnswer

    @Serializable @SerialName("permissions") data class Permissions(val allow: Boolean) : RequestAnswer

    @Serializable @SerialName("answers") data class Answers(val answers: Map<String, String>) : RequestAnswer

    @Serializable @SerialName("raw") data class Raw(val json: String) : RequestAnswer
}

private val agentCommandJson = Json { encodeDefaults = true }

internal fun AgentCommand.encode(): String = agentCommandJson.encodeToString(this)

@Serializable private data class AgentFailure(val message: String, val rawError: JsonElement?)

/** Decode the Rust intent boundary; transport failures before Rust retain their native error. */
internal fun GatewayResult<String>.agentResult(): GatewayResult<String> {
    if (this !is GatewayResult.Failure || rawError !is JsonObject || "rawError" !in rawError) return this
    val error = Json.decodeFromJsonElement<AgentFailure>(rawError)
    return GatewayResult.Failure(error.message, error.rawError)
}

internal suspend fun HostGateway.command(profile: HostProfile, command: AgentCommand): GatewayResult<JsonElement> =
    agentCommand(profile, command).mapGateway(Json::parseToJsonElement)

package dev.remoteagent.mobile

import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json

@Serializable private data class SessionImage(val source: String, val encoded: Boolean)

internal suspend fun HostGateway.sessionImages(profile: HostProfile, threadId: String): List<String> =
    when (val result = agentCommand(profile, AgentCommand.SessionImages(threadId))) {
        is GatewayResult.Failure -> error(result.message)
        is GatewayResult.Success ->
            Json.decodeFromString<List<SessionImage>>(result.value).map {
                if (it.encoded && !it.source.startsWith("data:")) "data:image/png;base64,${it.source}" else it.source
            }
    }

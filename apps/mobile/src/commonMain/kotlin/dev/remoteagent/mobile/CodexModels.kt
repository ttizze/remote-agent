package dev.remoteagent.mobile

import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonObject

suspend fun CommonCodexClient.listModels(profile: HostProfile): GatewayResult<List<CodexModel>> =
    command(profile, AgentCommand.Models).mapGateway { value ->
        (value as JsonArray).map { entry ->
            val model = entry as JsonObject
            CodexModel(
                model.string("id")!!,
                model.string("model")!!,
                model.string("displayName")!!,
                model.string("defaultReasoningEffort")!!,
                (model.getValue("supportedReasoningEfforts") as JsonArray).map {
                    (it as JsonObject).string("reasoningEffort")!!
                },
            )
        }
    }

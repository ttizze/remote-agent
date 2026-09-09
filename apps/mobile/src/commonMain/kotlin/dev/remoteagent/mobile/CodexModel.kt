package dev.remoteagent.mobile

import kotlinx.atomicfu.AtomicRef
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put

data class CodexModel(
    val id: String,
    val model: String,
    val displayName: String,
    val defaultReasoningEffort: String,
    val reasoningEfforts: List<String>,
    internal val metadata: JsonObject,
)

suspend fun HostGateway.listModels(profile: HostProfile): GatewayResult<List<CodexModel>> =
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
                model,
            )
        }
    }

internal fun CodexTurnOptions.supported(model: CodexModel?): CodexTurnOptions {
    val request = buildJsonObject {
        put("operation", "modelSettings")
        put("model", model?.metadata ?: JsonNull)
        put("effort", effort.orEmpty())
        put("tier", "")
    }
    val selection = Json.parseToJsonElement(nativeConversationPresentation(request.toString())) as JsonObject
    return copy(effort = selection.string("effort")?.takeIf(String::isNotEmpty))
}

internal suspend fun AtomicRef<MobileApp>.loadModels() {
    val profile = state.selectedProfile
    val context = value.settingsContext
    if (profile == null || context?.connected != true || settingsState.loadingModels) return
    val revision = settingsState.modelRevision + 1
    publishSettings(settingsState.copy(loadingModels = true, modelError = null, modelRevision = revision))
    val result = value.effects.gateway.listModels(profile)
    if (value.settingsContext == context && settingsState.modelRevision == revision)
        publishSettings(
            when (result) {
                is GatewayResult.Success ->
                    settingsState.copy(
                        loadingModels = false,
                        models = result.value,
                        options =
                            settingsState.options.supported(
                                result.value.firstOrNull { it.model == settingsState.options.model }
                            ),
                    )
                is GatewayResult.Failure ->
                    settingsState.copy(loadingModels = false, models = emptyList(), modelError = result.message)
            }
        )
}

internal fun AtomicRef<MobileApp>.chooseModel(value: String) {
    chooseTurnOptions(CodexTurnOptions(model = value.takeIf(String::isNotEmpty)))
}

internal fun AtomicRef<MobileApp>.chooseEffort(value: String) {
    chooseTurnOptions(settingsState.options.copy(effort = value.takeIf(String::isNotEmpty)))
}

private fun AtomicRef<MobileApp>.chooseTurnOptions(value: CodexTurnOptions) {
    val host = state.selectedProfileId ?: return
    val options = value.supported(settingsState.models.firstOrNull { it.model == value.model })
    dispatch(AppAction.TurnOptionsChanged(host, options))
    publishSettings(settingsState.copy(options = options))
}

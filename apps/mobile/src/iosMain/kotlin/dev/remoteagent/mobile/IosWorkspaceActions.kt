package dev.remoteagent.mobile

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.launch
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonElement

class IosWorkspaceActions
internal constructor(
    private val controller: MobileController,
    private val scope: CoroutineScope,
    private val dependencies: IosMobileDependencies,
) {
    /** Files and reviews use the same authenticated RPC connection as the task. */
    fun request(method: String, paramsJson: String, completion: (String?, String?) -> Unit) {
        val profile =
            controller.state.selectedProfile
                ?: run {
                    completion(null, "接続先が選択されていません")
                    return
                }
        if (method !in setOf("host/file/list", "host/file/read", "host/file/write", "host/workspace/review")) {
            completion(null, "未対応のファイル操作です")
            return
        }
        scope.launch {
            try {
                completeJson(
                    dependencies.gateway.rawRequest(profile, method, Json.parseToJsonElement(paramsJson)),
                    completion,
                )
            } catch (failure: IllegalArgumentException) {
                completion(null, failure.message ?: "JSONが無効です")
            }
        }
    }

    fun transfer(paramsJson: String, completion: (String?, String?) -> Unit) {
        val profile =
            controller.state.selectedProfile
                ?: run {
                    completion(null, "接続先が選択されていません")
                    return
                }
        scope.launch {
            try {
                completeJson(dependencies.gateway.transfer(profile, Json.parseToJsonElement(paramsJson)), completion)
            } catch (failure: IllegalArgumentException) {
                completion(null, failure.message ?: "転送に失敗しました")
            }
        }
    }

    private fun completeJson(result: GatewayResult<JsonElement>, completion: (String?, String?) -> Unit) =
        when (result) {
            is GatewayResult.Success -> completion(result.value.toString(), null)
            is GatewayResult.Failure -> completion(null, result.message)
        }
}

package dev.remoteagent.mobile

import kotlinx.atomicfu.AtomicRef
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.launch
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonElement

class IosWorkspaceActions
internal constructor(private val controller: AtomicRef<MobileApp>, private val scope: CoroutineScope) {
    fun listFiles(path: String, completion: (HostFileList?, String?) -> Unit) {
        scope.launch { complete(controller.listFiles(path), completion) }
    }

    fun readFile(path: String, completion: (HostFileDocument?, String?) -> Unit) {
        scope.launch { complete(controller.readFile(path), completion) }
    }

    fun writeFile(path: String, revision: String, text: String, completion: (HostFileDocument?, String?) -> Unit) {
        scope.launch { complete(controller.writeFile(path, revision, text), completion) }
    }

    fun reviewWorkspace(cwd: String, completion: (HostWorkspaceReview?, String?) -> Unit) {
        scope.launch { complete(controller.reviewWorkspace(cwd), completion) }
    }

    private fun <T> complete(result: GatewayResult<T>, completion: (T?, String?) -> Unit) {
        when (result) {
            is GatewayResult.Success -> completion(result.value, null)
            is GatewayResult.Failure -> completion(null, result.message)
        }
    }

    fun worktreeSettings(
        hostIdentity: String,
        update: HostWorktreeSettings?,
        completion: (HostWorktreeSettings?, String?) -> Unit,
    ) {
        scope.launch {
            when (val result = controller.worktreeSettings(hostIdentity, update)) {
                is GatewayResult.Success -> completion(result.value, null)
                is GatewayResult.Failure -> completion(null, result.message)
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
                completeJson(
                    controller.value.effects.gateway.transfer(profile, Json.parseToJsonElement(paramsJson)),
                    completion,
                )
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

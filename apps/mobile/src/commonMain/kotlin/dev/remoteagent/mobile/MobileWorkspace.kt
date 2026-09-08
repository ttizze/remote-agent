package dev.remoteagent.mobile

import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json

@Serializable
data class HostWorktreeSettings(
    val createOnNewSession: Boolean = false,
    val copyOnCreate: Boolean = false,
    val copyPaths: List<String> = emptyList(),
    val worktreeDirectory: String = "",
)

@Serializable data class HostFileEntry(val name: String, val path: String, val directory: Boolean, val size: Long)

@Serializable data class HostFileList(val path: String, val entries: List<HostFileEntry>, val truncated: Boolean)

@Serializable
data class HostFileDocument(
    val path: String,
    val revision: String,
    val text: String,
    val bom: Boolean,
    val lineEnding: String,
    val size: Long,
)

@Serializable
data class HostWorkspaceFileChange(val path: String, val status: String, val additions: Long?, val deletions: Long?)

@Serializable
data class HostWorkspaceReview(
    val branch: String,
    val additions: Long,
    val deletions: Long,
    val files: List<HostWorkspaceFileChange>,
    val diff: String,
)

internal suspend fun MobileController.listFiles(path: String): GatewayResult<HostFileList> =
    workspaceProjection(AgentCommand.ListFiles(path))

internal suspend fun MobileController.readFile(path: String): GatewayResult<HostFileDocument> =
    workspaceProjection(AgentCommand.ReadFile(path))

internal suspend fun MobileController.writeFile(
    path: String,
    revision: String,
    text: String,
): GatewayResult<HostFileDocument> = workspaceProjection(AgentCommand.WriteFile(path, revision, text))

internal suspend fun MobileController.reviewWorkspace(cwd: String): GatewayResult<HostWorkspaceReview> =
    workspaceProjection(AgentCommand.ReviewWorkspace(cwd))

private suspend inline fun <reified T> MobileController.workspaceProjection(command: AgentCommand): GatewayResult<T> {
    val profile = state.selectedProfile ?: return GatewayResult.Failure("接続先が選択されていません")
    return when (val result = requestAgent(profile.id, command)) {
        is GatewayResult.Failure -> result
        is GatewayResult.Success -> GatewayResult.Success(Json.decodeFromString<T>(result.value))
    }
}

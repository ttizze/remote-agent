package dev.remoteagent.mobile

import kotlinx.atomicfu.AtomicRef
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonElement

internal suspend fun AtomicRef<MobileApp>.requestAgent(
    hostIdentity: String,
    command: AgentCommand,
): GatewayResult<String> {
    val profile = state.selectedProfile?.takeIf { it.id == hostIdentity } ?: return GatewayResult.Failure("接続先が変更されました")
    val generation = value.effects.sessions.currentGeneration(profile.id)
    return if (generation == null) GatewayResult.Failure("接続されていません")
    else {
        val result = value.effects.gateway.agentCommand(profile, command)
        if (state.selectedProfile?.id == profile.id && isConnected(profile.id, generation)) result
        else GatewayResult.Failure("接続先が変更されました")
    }
}

internal suspend fun AtomicRef<MobileApp>.forkThread(threadId: String, lastTurnId: String): GatewayResult<JsonElement> {
    val profile = state.selectedProfile ?: return GatewayResult.Failure("接続先が選択されていません")
    return requestAgent(profile.id, AgentCommand.ForkThread(threadId, lastTurnId)).mapGateway(Json::parseToJsonElement)
}

internal suspend fun AtomicRef<MobileApp>.worktreeSettings(
    hostIdentity: String,
    update: HostWorktreeSettings?,
): GatewayResult<HostWorktreeSettings> =
    requestAgent(hostIdentity, update?.let(AgentCommand::UpdateWorktreeSettings) ?: AgentCommand.WorktreeSettings)
        .mapGateway { Json.decodeFromString<HostWorktreeSettings>(it) }

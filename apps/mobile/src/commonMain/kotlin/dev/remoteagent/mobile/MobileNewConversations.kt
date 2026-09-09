package dev.remoteagent.mobile

import kotlinx.atomicfu.AtomicRef
import kotlinx.atomicfu.getAndUpdate
import kotlinx.atomicfu.update

internal suspend fun AtomicRef<MobileApp>.sendMessage(
    profile: HostProfile,
    text: String,
    attachments: List<CodexAttachment> = emptyList(),
    options: CodexTurnOptions = CodexTurnOptions(),
): MessageSendResult {
    val view = state.profileViews[profile.id]
    return when {
        (text.isBlank() && attachments.isEmpty()) || view == null -> MessageSendResult(false, null)
        view.selectedThreadId != null ->
            MessageSendResult(
                startTurn(profile, view.selectedThreadId, text, attachments, options),
                view.selectedThreadId,
            )
        view.newThreadCwd == null -> MessageSendResult(false, null)
        else -> startConversation(profile, view.newThreadCwd, text, attachments, options)
    }
}

private suspend fun AtomicRef<MobileApp>.startConversation(
    profile: HostProfile,
    cwd: String,
    text: String,
    attachments: List<CodexAttachment>,
    options: CodexTurnOptions,
): MessageSendResult {
    val generation = connectedGeneration(profile.id)
    if (
        generation == null ||
            profile.id in
                getAndUpdate {
                    if (profile.id in it.startingConversations) it
                    else it.copy(startingConversations = it.startingConversations + profile.id)
                }
                    .startingConversations
    )
        return MessageSendResult(false, null)
    return try {
        when (
            val result =
                value.effects.gateway
                    .command(profile, AgentCommand.StartThread(cwd, options.model))
                    .mapGateway(::codexThreadFromResponse)
        ) {
            is GatewayResult.Failure -> {
                dispatchIfCurrent(profile.id, generation) { AppAction.ThreadStartFailed(profile.id, result.message) }
                MessageSendResult(false, null)
            }
            is GatewayResult.Success -> {
                val snapshot = result.value
                if (!isConnected(profile.id, generation)) MessageSendResult(false, snapshot.summary.id)
                else {
                    val current = state.profileViews[profile.id]
                    dispatchIfCurrent(profile.id, generation) {
                        AppAction.SnapshotReceived(
                            profile.id,
                            ThreadReadResult(snapshot, emptyList()),
                            select = current?.selectedThreadId == null && current?.newThreadCwd == cwd,
                        )
                    }
                    // Retain the created conversation even when its first turn fails; retries must reuse it.
                    MessageSendResult(
                        startTurn(profile, snapshot.summary.id, text, attachments, options),
                        snapshot.summary.id,
                    )
                }
            }
        }
    } finally {
        update { it.copy(startingConversations = it.startingConversations - profile.id) }
    }
}

package dev.remoteagent.mobile

internal suspend fun MobileController.sendMessage(
    profile: HostProfile,
    text: String,
    attachments: List<CodexAttachment> = emptyList(),
): MessageSendResult {
    val generation = connectedGeneration(profile.id) ?: return MessageSendResult(false, null)
    val view = state.profileViews[profile.id]
    return when {
        (text.isBlank() && attachments.isEmpty()) || view == null -> MessageSendResult(false, null)
        view.selectedThreadId != null ->
            MessageSendResult(startTurn(profile, view.selectedThreadId, text, attachments), view.selectedThreadId)
        view.newThreadCwd == null -> MessageSendResult(false, null)
        else -> startConversation(profile, view.newThreadCwd, text, attachments, generation)
    }
}

private suspend fun MobileController.startConversation(
    profile: HostProfile,
    cwd: String,
    text: String,
    attachments: List<CodexAttachment>,
    generation: Long,
): MessageSendResult {
    if (!startingConversations.add(profile.id)) return MessageSendResult(false, null)
    return try {
        when (val result = gateway.codex.startThread(profile, cwd)) {
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
                    MessageSendResult(startTurn(profile, snapshot.summary.id, text, attachments), snapshot.summary.id)
                }
            }
        }
    } finally {
        startingConversations.remove(profile.id)
    }
}

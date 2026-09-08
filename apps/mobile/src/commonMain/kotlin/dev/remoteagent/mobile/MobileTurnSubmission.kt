package dev.remoteagent.mobile

internal suspend fun MobileController.startTurn(
    profile: HostProfile,
    threadId: String,
    text: String,
    attachments: List<CodexAttachment> = emptyList(),
): Boolean {
    val generation = connectedGeneration(profile.id) ?: return false
    val cached = state.cache.snapshot(profile.id, threadId)
    val input = CodexTurnInput(text, attachments, clientUserMessageIdGenerator())
    val displayText = input.displayText()
    val activeTurnId = cached?.turns?.lastOrNull { it.status == TurnStatus.InProgress && it.id.isNotBlank() }?.id
    var acceptedTurnId = activeTurnId
    val result =
        when {
            activeTurnId != null -> gateway.codex.steerTurn(profile, threadId, activeTurnId, input)
            threadIsActive(profile.id, threadId, cached) -> gateway.codex.queueTurn(profile, threadId, input)
            else -> {
                val cwd = cachedWorkingDirectory(profile.id, threadId)
                if (cwd == null) GatewayResult.Failure(MISSING_THREAD_WORKING_DIRECTORY_MESSAGE)
                else
                    gateway.codex
                        .startTurn(
                            profile,
                            threadId,
                            cwd,
                            input,
                            resume = cached == null || cached.summary.status == ThreadStatus.NotLoaded,
                        )
                        .also { if (it is GatewayResult.Success) acceptedTurnId = it.value }
            }
        }
    return when (result) {
        is GatewayResult.Failure -> {
            dispatchIfCurrent(profile.id, generation) { AppAction.TurnFailed(profile.id, result.message) }
            false
        }
        is GatewayResult.Success -> {
            dispatchIfCurrent(profile.id, generation) {
                AppAction.MessageAccepted(profile.id, threadId, input.submission(cached, displayText, acceptedTurnId))
            }
            // Acceptance precedes persistence; subscribed items carry the subsequent body updates.
            true
        }
    }
}

private fun MobileController.threadIsActive(hostIdentity: String, threadId: String, cached: ThreadSnapshot?): Boolean =
    cached?.summary?.status is ThreadStatus.Active ||
        state.cache.profile(hostIdentity).threadList.lastOrNull { it.id == threadId }?.status is ThreadStatus.Active

private fun CodexTurnInput.displayText(): String = buildString {
    append(text)
    for (attachment in attachments) {
        if (!attachment.isImage) {
            if (isNotEmpty()) append('\n')
            append(attachmentMessageLabel(attachment.isImage, attachment.path, attachment.name))
        }
    }
}

private fun CodexTurnInput.submission(cached: ThreadSnapshot?, displayText: String, turnId: String?) =
    SubmittedMessage(
        clientUserMessageId,
        displayText,
        turnId,
        cached?.turns?.firstOrNull { it.id == turnId }?.items?.lastOrNull()?.id,
        attachments.mapNotNull { if (it.isImage) it.path else null },
    )

internal suspend fun MobileController.respond(
    profile: HostProfile,
    requestId: kotlinx.serialization.json.JsonElement,
    response: kotlinx.serialization.json.JsonElement,
): GatewayResult<Unit> {
    val generation = connectedGeneration(profile.id) ?: return GatewayResult.Failure("接続が切れています")
    val result = gateway.respondResult(profile, requestId, response)
    if (result is GatewayResult.Failure)
        dispatchIfCurrent(profile.id, generation) { AppAction.TurnFailed(profile.id, result.message) }
    return result
}

internal suspend fun MobileController.interrupt(profile: HostProfile, threadId: String, turnId: String) {
    val generation = sessions.currentGeneration(profile.id) ?: return
    if (!isConnected(profile.id, generation)) return
    dispatchIfCurrent(profile.id, generation) { AppAction.InterruptStarted(profile.id, turnId) }
    gateway.codex
        .interrupt(profile, threadId, turnId)
        .fold(
            success = { dispatchIfCurrent(profile.id, generation) { AppAction.InterruptFinished(profile.id) } },
            failure = { message ->
                dispatchIfCurrent(profile.id, generation) { AppAction.InterruptFinished(profile.id) }
                dispatchIfCurrent(profile.id, generation) { AppAction.TurnFailed(profile.id, message) }
            },
        )
}

internal fun MobileController.cachedWorkingDirectory(hostIdentity: String, threadId: String): String? {
    val cache = state.cache.profile(hostIdentity)
    val snapshotCwd = cache.snapshots[threadId]?.summary?.workingDirectory?.path
    if (!snapshotCwd.isNullOrBlank()) return snapshotCwd
    return cache.threadList.firstOrNull { it.id == threadId }?.workingDirectory?.path?.takeIf(String::isNotBlank)
}

internal suspend fun <T> GatewayResult<T>.fold(success: suspend (T) -> Unit, failure: suspend (String) -> Unit) =
    when (this) {
        is GatewayResult.Success -> success(value)
        is GatewayResult.Failure -> failure(message)
    }

private const val MISSING_THREAD_WORKING_DIRECTORY_MESSAGE = "タスクの作業ディレクトリが不明です。タスク一覧を更新してください"

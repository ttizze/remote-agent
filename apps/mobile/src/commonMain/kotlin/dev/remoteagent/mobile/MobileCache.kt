package dev.remoteagent.mobile

private const val DEFAULT_CACHE_BYTES = 512 * 1024

/** Limits are local-device bounds, never a statement about Codex history retention. */
data class MobileCacheLimits(
    /** Number of conversation bodies retained; summaries remain available for list expansion. */
    val maxThreads: Int = 20,
    val maxTurnsPerThread: Int = 10,
    val maxApproximateBytes: Int = DEFAULT_CACHE_BYTES,
) {
    init {
        require(maxThreads > 0)
        require(maxTurnsPerThread > 0)
        require(maxApproximateBytes > 0)
    }
}

data class ProfileMobileCache(
    val projects: List<CodexProject> = emptyList(),
    val threadList: List<ThreadSummary> = emptyList(),
    val snapshots: Map<String, ThreadSnapshot> = emptyMap(),
)

data class MobileCache(val profiles: Map<String, ProfileMobileCache> = emptyMap()) {
    fun profile(hostIdentity: String): ProfileMobileCache = profiles[hostIdentity] ?: ProfileMobileCache()

    fun snapshot(hostIdentity: String, threadId: String): ThreadSnapshot? = profile(hostIdentity).snapshots[threadId]
}

fun reconcileThreadList(
    cache: MobileCache,
    hostIdentity: String,
    threads: List<ThreadSummary>,
    limits: MobileCacheLimits,
): MobileCache =
    cache.replaceProfile(hostIdentity, cache.profile(hostIdentity).copy(threadList = threads).bounded(limits))

fun reconcileProjectList(
    cache: MobileCache,
    hostIdentity: String,
    projects: List<CodexProject>,
    limits: MobileCacheLimits,
): MobileCache =
    cache.replaceProfile(hostIdentity, cache.profile(hostIdentity).copy(projects = projects).bounded(limits))

/** Retain accepted input until Codex echoes its client ID, including across reads. */
fun acknowledgeMessage(
    cache: MobileCache,
    hostIdentity: String,
    threadId: String,
    submission: SubmittedMessage,
    limits: MobileCacheLimits,
): MobileCache {
    val profile = cache.profile(hostIdentity)
    val snapshot = profile.snapshots[threadId] ?: return cache
    val turnId = submission.turnId
    val updated =
        if (turnId == null) snapshot
        else
            snapshot.upsertTurn(
                snapshot.turns.firstOrNull { it.id == turnId } ?: CodexTurn(turnId, TurnStatus.InProgress)
            )
    val retained =
        updated.copy(
            submittedMessages =
                retainPendingSubmissions(
                    updated.submittedMessages + submission,
                    updated.turns
                        .asSequence()
                        .flatMap { it.items.asSequence() }
                        .filterIsInstance<CodexItem.UserMessage>()
                        .mapNotNull { it.clientId },
                )
        )
    return cache.replaceProfile(
        hostIdentity,
        profile
            .copy(
                threadList = profile.threadList.replaceById(threadId, retained.summary, append = false),
                snapshots = profile.snapshots + (threadId to retained),
            )
            .boundedEvent(limits),
    )
}

/**
 * A ThreadReadResult is one atomic UI reconciliation transition: replace the native Codex Thread projection first, then
 * apply any locally buffered events.
 */
fun reconcileThreadRead(
    cache: MobileCache,
    hostIdentity: String,
    result: ThreadReadResult,
    limits: MobileCacheLimits,
): MobileCache {
    val existing = cache.snapshot(hostIdentity, result.thread.summary.id)
    val refreshed = mergeHistoryRefresh(existing, result.thread)
    val pending = existing?.submittedMessages.orEmpty()
    val thread =
        if (pending.isEmpty()) refreshed
        else {
            val echoed =
                result.thread.turns
                    .asSequence()
                    .flatMap { it.items.asSequence() }
                    .filterIsInstance<CodexItem.UserMessage>()
                    .mapNotNull { it.clientId }
            refreshed.copy(submittedMessages = retainPendingSubmissions(pending, echoed))
        }
    var profile =
        cache
            .profile(hostIdentity)
            .copy(
                threadList =
                    cache.profile(hostIdentity).threadList.replaceById(result.thread.summary.id, result.thread.summary),
                snapshots = cache.profile(hostIdentity).snapshots + (thread.summary.id to thread),
            )
    // A snapshot response is authoritative for exactly one thread. Do not
    // let a malformed or mixed buffered stream mutate another cached thread.
    result.bufferedEvents
        .asSequence()
        .filter { it.threadId == result.thread.summary.id }
        .forEach { event -> profile = profile.apply(event) }
    return cache.replaceProfile(hostIdentity, profile.bounded(limits))
}

/** Applies events only through the declared Thread/Turn/Item identifiers. */
fun applyLiveEvent(
    cache: MobileCache,
    hostIdentity: String,
    event: ThreadEvent,
    limits: MobileCacheLimits,
): MobileCache {
    val profile = cache.profile(hostIdentity)
    val updated = profile.apply(event)
    return if (updated === profile) cache else cache.replaceProfile(hostIdentity, updated.boundedEvent(limits))
}

private fun MobileCache.replaceProfile(hostIdentity: String, profile: ProfileMobileCache): MobileCache =
    if (profiles[hostIdentity] === profile) this else copy(profiles = profiles + (hostIdentity to profile))

private fun ProfileMobileCache.apply(event: ThreadEvent): ProfileMobileCache =
    when (event) {
        is ThreadEvent.Unknown -> this
        is ThreadEvent.ThreadStatusChanged -> applyThreadStatus(event)
        else -> applyBodyEvent(event)
    }

private fun ProfileMobileCache.applyThreadStatus(event: ThreadEvent.ThreadStatusChanged): ProfileMobileCache {
    val updatedList = threadList.map { summary ->
        if (summary.id == event.threadId) summary.copy(status = event.status) else summary
    }
    val snapshot = snapshots[event.threadId]
    val updatedSnapshots =
        if (snapshot == null) snapshots
        else snapshots + (event.threadId to snapshot.copy(summary = snapshot.summary.copy(status = event.status)))
    return copy(threadList = updatedList, snapshots = updatedSnapshots)
}

private fun ProfileMobileCache.applyBodyEvent(event: ThreadEvent): ProfileMobileCache {
    val existing = snapshots[event.threadId] ?: return this
    val updated = existing.applyConversationEvent(event)
    return if (updated === existing) this
    else {
        val echoed =
            when (event) {
                is ThreadEvent.ItemStarted -> event.item as? CodexItem.UserMessage
                is ThreadEvent.ItemCompleted -> event.item as? CodexItem.UserMessage
                else -> null
            }?.clientId
        val reconciled =
            if (echoed == null || updated.submittedMessages.isEmpty()) updated
            else
                updated.copy(
                    submittedMessages = retainPendingSubmissions(updated.submittedMessages, sequenceOf(echoed))
                )
        val updatedList =
            if (updated.summary == existing.summary) threadList
            else threadList.replaceById(updated.summary.id, updated.summary, append = false)
        copy(threadList = updatedList, snapshots = snapshots + (event.threadId to reconciled))
    }
}

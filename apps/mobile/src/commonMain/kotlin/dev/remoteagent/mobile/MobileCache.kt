package dev.remoteagent.mobile

/** Limits are local-device bounds, never a statement about Codex history retention. */
data class MobileCacheLimits(
    val maxThreads: Int = 20,
    val maxTurnsPerThread: Int = 10,
    val maxApproximateBytes: Int = 512 * 1024,
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
    val unknownEvents: List<ThreadEvent.Unknown> = emptyList(),
    /** Bounded raw messages; durable codec excludes actionable server requests. */
    val rawMessages: List<RawCodexMessage> = emptyList(),
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
): MobileCache = cache.replaceProfile(hostIdentity, cache.profile(hostIdentity).copy(threadList = threads)).bounded(limits)

fun reconcileProjectList(
    cache: MobileCache,
    hostIdentity: String,
    projects: List<CodexProject>,
    limits: MobileCacheLimits,
): MobileCache = cache.replaceProfile(hostIdentity, cache.profile(hostIdentity).copy(projects = projects)).bounded(limits)

/**
 * Records the turn id returned by a successful turn/start or turn/steer when
 * the corresponding turn/started notification was missed. An already-applied
 * terminal event remains authoritative when notifications are reordered.
 */
fun acknowledgeTurnStart(
    cache: MobileCache,
    hostIdentity: String,
    threadId: String,
    turnId: String,
    limits: MobileCacheLimits,
): MobileCache {
    val profile = cache.profile(hostIdentity)
    val snapshot = profile.snapshots[threadId] ?: return cache
    val acknowledgedTurn = snapshot.turns.firstOrNull { it.id == turnId }
        ?: CodexTurn(turnId, TurnStatus.InProgress)
    val updated = snapshot.upsertTurn(acknowledgedTurn)
    return cache.replaceProfile(
        hostIdentity,
        profile.copy(
            threadList = profile.threadList.replaceById(updated.summary.id, updated.summary),
            snapshots = profile.snapshots + (threadId to updated),
        ),
    ).bounded(limits)
}

/**
 * A ThreadReadResult is one atomic UI reconciliation transition: replace the
 * native Codex Thread projection first, then apply any locally buffered events.
 */
fun reconcileThreadRead(
    cache: MobileCache,
    hostIdentity: String,
    result: ThreadReadResult,
    limits: MobileCacheLimits,
): MobileCache {
    var profile = cache.profile(hostIdentity).copy(
        threadList = cache.profile(hostIdentity).threadList.replaceById(result.thread.summary.id, result.thread.summary),
        snapshots = cache.profile(hostIdentity).snapshots + (result.thread.summary.id to result.thread),
    )
    // A snapshot response is authoritative for exactly one thread. Do not
    // let a malformed or mixed buffered stream mutate another cached thread.
    result.bufferedEvents
        .asSequence()
        .filter { it.threadId == result.thread.summary.id }
        .forEach { event -> profile = profile.apply(event) }
    return cache.replaceProfile(hostIdentity, profile).bounded(limits)
}

/** Applies events only through the declared Thread/Turn/Item identifiers. */
fun applyLiveEvent(
    cache: MobileCache,
    hostIdentity: String,
    event: ThreadEvent,
    limits: MobileCacheLimits,
): MobileCache = cache.replaceProfile(hostIdentity, cache.profile(hostIdentity).apply(event)).bounded(limits)

fun retainRawMessage(
    cache: MobileCache,
    hostIdentity: String,
    message: RawCodexMessage,
    limits: MobileCacheLimits,
): MobileCache = cache.replaceProfile(
    hostIdentity,
    cache.profile(hostIdentity).copy(rawMessages = (cache.profile(hostIdentity).rawMessages + message).takeLast(128)),
).bounded(limits)

private fun MobileCache.replaceProfile(hostIdentity: String, profile: ProfileMobileCache): MobileCache =
    copy(profiles = profiles + (hostIdentity to profile))

private fun ProfileMobileCache.apply(event: ThreadEvent): ProfileMobileCache {
    if (event is ThreadEvent.Unknown) {
        return copy(unknownEvents = (unknownEvents + event).takeLast(128))
    }
    if (event is ThreadEvent.ThreadStatusChanged) {
        val updatedList = threadList.map { summary ->
            if (summary.id == event.threadId) summary.copy(status = event.status) else summary
        }
        val snapshot = snapshots[event.threadId]
        val updatedSnapshots = if (snapshot == null) snapshots else snapshots + (
            event.threadId to snapshot.copy(summary = snapshot.summary.copy(status = event.status))
        )
        return copy(threadList = updatedList, snapshots = updatedSnapshots)
    }
    val existing = snapshots[event.threadId] ?: return this
    val updated = when (event) {
        is ThreadEvent.TurnStarted -> existing.mergeTurnLifecycle(
            CodexTurn(event.turnId, event.status, startedAtMs = event.startedAtMs),
        )
        is ThreadEvent.TurnCompleted -> existing.mergeTurnLifecycle(
            CodexTurn(
                id = event.turnId,
                status = event.status,
                startedAtMs = event.startedAtMs,
                completedAtMs = event.completedAtMs,
                durationMs = event.durationMs,
                error = event.error,
            ),
        )
        is ThreadEvent.ItemStarted -> existing.upsertItem(event.turnId, event.item)
        is ThreadEvent.AgentMessageDelta -> existing.appendDelta(event.turnId, event.itemId, event.delta, DeltaKind.AgentMessage)
        is ThreadEvent.ReasoningDelta -> existing.appendDelta(event.turnId, event.itemId, event.delta, DeltaKind.Reasoning)
        is ThreadEvent.ReasoningSummaryDelta -> existing.appendDelta(event.turnId, event.itemId, event.delta, DeltaKind.Reasoning)
        is ThreadEvent.CommandOutputDelta -> existing.appendDelta(event.turnId, event.itemId, event.delta, DeltaKind.CommandOutput)
        is ThreadEvent.FileChangeOutputDelta -> existing.appendDelta(event.turnId, event.itemId, event.delta, DeltaKind.FileChangeOutput)
        is ThreadEvent.Error -> existing.updateTurnError(event.turnId, event.error)
        is ThreadEvent.RequestStarted -> existing.addPendingRequest(event.turnId, event.request)
        is ThreadEvent.RequestResolved -> existing.removePendingRequest(event.requestId)
        is ThreadEvent.GuardianReviewChanged -> if (event.status == "approved") {
            existing.removeItem(event.turnId, event.reviewId)
        } else {
            existing.upsertItem(
                event.turnId,
                CodexItem.Unknown(event.reviewId, "automaticApprovalReview", event.raw),
            )
        }
        is ThreadEvent.ItemCompleted -> existing.upsertItem(event.turnId, event.item)
        is ThreadEvent.ThreadStatusChanged -> existing
        is ThreadEvent.Unknown -> existing
    }
    return copy(
        threadList = threadList.replaceById(updated.summary.id, updated.summary),
        snapshots = snapshots + (event.threadId to updated),
    )
}

private fun MobileCache.bounded(limits: MobileCacheLimits): MobileCache {
    return MobileCache(
        profiles = profiles.mapValues { (_, profile) -> profile.bounded(limits) },
    )
}

private fun ProfileMobileCache.bounded(limits: MobileCacheLimits): ProfileMobileCache {
    val projects = projects.distinctBy { it.id }.sortedBy { it.position }
    val list = threadList
        .groupBy { it.id }
        .values
        .map { summaries -> summaries.maxBy { it.updatedAtMs } }
        .sortedByDescending { it.updatedAtMs }
        .take(limits.maxThreads)
    val allowed = list.mapTo(mutableSetOf()) { it.id }
    val snapshots = snapshots
        .filterKeys { it in allowed }
        .values
        .associate { it.summary.id to it.bounded(limits) }
    return ProfileMobileCache(
        projects = projects,
        threadList = list,
        snapshots = snapshots,
        unknownEvents = unknownEvents.takeLast(128),
        rawMessages = rawMessages.takeLast(128),
    )
}

private fun ThreadSnapshot.bounded(limits: MobileCacheLimits): ThreadSnapshot {
    return copy(turns = turns.takeLast(limits.maxTurnsPerThread))
}

private fun <T> List<T>.replaceById(id: String, value: T, idOf: (T) -> String): List<T> {
    val index = indexOfFirst { idOf(it) == id }
    return if (index < 0) this + value else toMutableList().also { it[index] = value }
}

private fun List<ThreadSummary>.replaceById(id: String, value: ThreadSummary): List<ThreadSummary> =
    replaceById(id, value) { it.id }

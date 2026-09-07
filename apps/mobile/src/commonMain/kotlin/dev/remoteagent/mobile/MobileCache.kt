package dev.remoteagent.mobile

import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonNull

/** Limits are local-device bounds, never a statement about Codex history retention. */
data class MobileCacheLimits(
    /** Number of conversation bodies retained; summaries remain available for list expansion. */
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
): MobileCache = cache.replaceProfile(hostIdentity, cache.profile(hostIdentity).copy(threadList = threads).bounded(limits))

fun reconcileProjectList(
    cache: MobileCache,
    hostIdentity: String,
    projects: List<CodexProject>,
    limits: MobileCacheLimits,
): MobileCache = cache.replaceProfile(hostIdentity, cache.profile(hostIdentity).copy(projects = projects).bounded(limits))

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
    val updated = if (turnId == null) snapshot else snapshot.upsertTurn(
        snapshot.turns.firstOrNull { it.id == turnId } ?: CodexTurn(turnId, TurnStatus.InProgress),
    )
    val alreadyEchoed = updated.turns.any { turn ->
        turn.items.any { it is CodexItem.UserMessage && it.clientId == submission.clientId }
    }
    val retained = if (alreadyEchoed || updated.submittedMessages.any { it.clientId == submission.clientId }) updated else
        updated.copy(submittedMessages = updated.submittedMessages + submission)
    return cache.replaceProfile(hostIdentity, profile.copy(
        threadList = profile.threadList.replaceById(threadId, retained.summary, append = false),
        snapshots = profile.snapshots + (threadId to retained),
    ).boundedEvent(limits))
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
    val existing = cache.snapshot(hostIdentity, result.thread.summary.id)
    val refreshed = mergeHistoryRefresh(existing, result.thread)
    val pending = existing?.submittedMessages.orEmpty()
    val thread = if (pending.isEmpty()) refreshed else {
        val echoed = result.thread.turns.asSequence().flatMap { it.items.asSequence() }
            .filterIsInstance<CodexItem.UserMessage>().mapNotNull { it.clientId }.toSet()
        refreshed.copy(submittedMessages = pending.filterNot { it.clientId in echoed })
    }
    var profile = cache.profile(hostIdentity).copy(
        threadList = cache.profile(hostIdentity).threadList.replaceById(result.thread.summary.id, result.thread.summary),
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

private fun ProfileMobileCache.apply(event: ThreadEvent): ProfileMobileCache {
    if (event is ThreadEvent.Unknown) return this
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
    val echoed = when (event) {
        is ThreadEvent.ItemStarted -> event.item as? CodexItem.UserMessage
        is ThreadEvent.ItemCompleted -> event.item as? CodexItem.UserMessage
        else -> null
    }?.clientId
    val reconciled = if (echoed == null || updated.submittedMessages.isEmpty()) updated else
        updated.copy(submittedMessages = updated.submittedMessages.filterNot { it.clientId == echoed })
    val updatedList = if (updated.summary == existing.summary) threadList else
        threadList.replaceById(updated.summary.id, updated.summary, append = false)
    return copy(
        threadList = updatedList,
        snapshots = snapshots + (event.threadId to reconciled),
    )
}

private fun ProfileMobileCache.bounded(limits: MobileCacheLimits): ProfileMobileCache {
    val list = threadList
        .groupBy { it.id }
        .values
        .map { summaries -> summaries.maxBy { it.updatedAtMs } }
        .sortedByDescending { it.updatedAtMs }
    val latestProjectActivity = mutableMapOf<String, Long>()
    list.forEach { summary ->
        summary.projectId?.let { latestProjectActivity.getOrPut(it) { summary.updatedAtMs } }
    }
    val projects = projects.distinctBy { it.id }.sortedWith(
        compareByDescending<CodexProject> { latestProjectActivity[it.id] ?: Long.MIN_VALUE }
            .thenBy { it.position },
    )
    return ProfileMobileCache(
        projects = projects,
        threadList = list,
        snapshots = snapshots.values.toList().takeLast(limits.maxThreads).associate { it.summary.id to it.bounded(limits) },
    )
}

private fun ProfileMobileCache.boundedEvent(limits: MobileCacheLimits): ProfileMobileCache {
    val snapshots = if (snapshots.size <= limits.maxThreads && snapshots.values.all { it.turns.size <= limits.maxTurnsPerThread }) snapshots
    else snapshots.values.toList().takeLast(limits.maxThreads).associate { it.summary.id to it.bounded(limits) }
    return if (snapshots === this.snapshots) this else copy(snapshots = snapshots)
}

private fun ThreadSnapshot.bounded(limits: MobileCacheLimits): ThreadSnapshot {
    return if (turns.size <= limits.maxTurnsPerThread) this else copy(turns = turns.takeLast(limits.maxTurnsPerThread), raw = raw?.let { JsonObject(it - "historyCursor") })
}

private fun List<ThreadSummary>.replaceById(id: String, value: ThreadSummary, append: Boolean = true): List<ThreadSummary> {
    val index = indexOfFirst { it.id == id }
    return if (index < 0) {
        if (append) this + value else this
    } else if (this[index] == value) this else toMutableList().also { it[index] = value }
}

internal val ThreadSnapshot.olderTurnsCursor: String? get() = raw?.string("historyCursor")
internal val CodexTurn.olderItemsCursor: String? get() = raw?.string("itemsNextCursor")
internal val CodexTurn.hasOlderItems: Boolean get() = raw?.boolean("itemsHasMore") ?: (olderItemsCursor != null)

/** Older pages prepend; already observed live items win overlapping IDs. */
internal fun mergeOlderHistory(current: ThreadSnapshot, page: ThreadSnapshot, turnId: String?): ThreadSnapshot {
    if (turnId != null) {
        val older = page.turns.single { it.id == turnId }
        return current.copy(turns = current.turns.map { turn ->
            if (turn.id != turnId) turn else turn.copy(
                items = prependDistinctItems(older.items, turn.items),
                raw = JsonObject(turn.raw.orEmpty() + older.raw.orEmpty().filterKeys { it == "openingUserMessage" } + mapOf(
                    "itemsNextCursor" to (older.raw?.get("itemsNextCursor") ?: JsonNull),
                    "itemsHasMore" to kotlinx.serialization.json.JsonPrimitive(older.hasOlderItems),
                    "deferredItemIds" to mergeDeferredIds(older, turn),
                )),
            )
        })
    }
    val known = current.turns.mapTo(mutableSetOf()) { it.id }
    return current.copy(
        turns = page.turns.filterNot { it.id in known } + current.turns,
        raw = JsonObject(current.raw.orEmpty() + ("historyCursor" to (page.raw?.get("historyCursor") ?: JsonNull))),
    )
}

private fun prependDistinctItems(older: List<CodexItem>, current: List<CodexItem>): List<CodexItem> {
    val known = current.mapTo(mutableSetOf()) { it.id }
    return older.filter { known.add(it.id) } + current
}

private fun mergeDeferredIds(a: CodexTurn, b: CodexTurn) = kotlinx.serialization.json.JsonArray(
    ((a.raw?.get("deferredItemIds") as? kotlinx.serialization.json.JsonArray).orEmpty() +
        (b.raw?.get("deferredItemIds") as? kotlinx.serialization.json.JsonArray).orEmpty()).distinct(),
)

/** Keep fetched prefixes only across an overlapping, authoritative tail read. */
private fun mergeHistoryRefresh(previous: ThreadSnapshot?, fresh: ThreadSnapshot): ThreadSnapshot {
    if (previous?.raw?.containsKey("historyCursor") != true || fresh.raw?.containsKey("historyCursor") != true || fresh.turns.isEmpty()) return fresh
    val boundary = previous.turns.indexOfFirst { it.id == fresh.turns.first().id }
    if (boundary < 0) return fresh
    val oldTurns = previous.turns.associateBy { it.id }
    val turns = fresh.turns.map { turn ->
        val old = oldTurns[turn.id] ?: return@map turn
        val first = turn.items.firstOrNull()?.id ?: return@map if (turn.hasOlderItems) turn.copy(items = old.items, raw = old.raw) else turn
        val itemBoundary = old.items.indexOfFirst { it.id == first }
        if (itemBoundary < 0) return@map turn
        turn.copy(
            items = old.items.take(itemBoundary) + turn.items,
            raw = JsonObject(turn.raw.orEmpty() + mapOf(
                "itemsNextCursor" to (old.raw?.get("itemsNextCursor") ?: JsonNull),
                "itemsHasMore" to kotlinx.serialization.json.JsonPrimitive(old.hasOlderItems),
                "deferredItemIds" to mergeDeferredIds(old, turn),
            )),
        )
    }
    return fresh.copy(turns = previous.turns.take(boundary) + turns,
        raw = JsonObject(fresh.raw.orEmpty() + ("historyCursor" to (previous.raw?.get("historyCursor") ?: JsonNull))))
}

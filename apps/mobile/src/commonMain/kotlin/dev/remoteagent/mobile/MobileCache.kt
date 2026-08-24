package dev.remoteagent.mobile

import kotlinx.serialization.json.JsonObject

/** Limits are local-device bounds, never a statement about Codex history retention. */
data class MobileCacheLimits(
    val maxThreads: Int = 64,
    val maxItemsPerThread: Int = 256,
    val maxTextCharacters: Int = 4 * 1024,
    val maxApproximateBytes: Int = 512 * 1024,
) {
    init {
        require(maxThreads > 0)
        require(maxItemsPerThread > 0)
        require(maxTextCharacters > 0)
        require(maxApproximateBytes > 0)
    }
}

data class ProfileMobileCache(
    val threadList: List<ThreadSummary> = emptyList(),
    val snapshots: Map<String, ThreadSnapshot> = emptyMap(),
    val unknownEvents: List<ThreadEvent.Unknown> = emptyList(),
    /** Ephemeral raw notifications/requests retained for feature handlers. */
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

fun approximateCacheBytes(cache: MobileCache): Int = cache.profiles.entries.sumOf { (hostIdentity, profile) ->
    hostIdentity.approximateBytes() + profile.threadList.sumOf { it.approximateBytes() } +
        profile.snapshots.values.sumOf { it.approximateBytes() } +
        profile.unknownEvents.sumOf {
            it.threadId.approximateBytes() + it.turnId.approximateBytes() +
                it.method.approximateBytes() + it.raw.approximateBytes() + it.extensions.approximateBytes()
        } +
        profile.rawMessages.sumOf { it.approximateBytes() }
}

private fun MobileCache.replaceProfile(hostIdentity: String, profile: ProfileMobileCache): MobileCache =
    copy(profiles = profiles + (hostIdentity to profile))

private fun ProfileMobileCache.apply(event: ThreadEvent): ProfileMobileCache {
    if (event is ThreadEvent.Unknown) {
        return copy(unknownEvents = (unknownEvents + event).takeLast(128))
    }
    val existing = snapshots[event.threadId] ?: return this
    val updated = when (event) {
        is ThreadEvent.TurnStarted -> existing.upsertTurn(CodexTurn(event.turnId, event.status))
        is ThreadEvent.TurnCompleted -> existing.changeTurnStatus(event.turnId, event.status)
        is ThreadEvent.ItemStarted -> existing.upsertItem(event.turnId, event.item)
        is ThreadEvent.AgentMessageDelta -> existing.appendDelta(event.turnId, event.itemId, event.delta, DeltaKind.AgentMessage)
        is ThreadEvent.ReasoningDelta -> existing.appendDelta(event.turnId, event.itemId, event.delta, DeltaKind.Reasoning)
        is ThreadEvent.ReasoningSummaryDelta -> existing.appendDelta(event.turnId, event.itemId, event.delta, DeltaKind.Reasoning)
        is ThreadEvent.CommandOutputDelta -> existing.appendDelta(event.turnId, event.itemId, event.delta, DeltaKind.CommandOutput)
        is ThreadEvent.FileChangeOutputDelta -> existing.appendDelta(event.turnId, event.itemId, event.delta, DeltaKind.FileChangeOutput)
        is ThreadEvent.ItemCompleted -> existing.upsertItem(event.turnId, event.item)
            is ThreadEvent.Unknown -> existing
    }
    return copy(
        threadList = threadList.replaceById(updated.summary.id, updated.summary),
        snapshots = snapshots + (event.threadId to updated),
    )
}

private fun MobileCache.bounded(limits: MobileCacheLimits): MobileCache {
    var profiles = profiles.mapValues { (_, profile) -> profile.bounded(limits) }
    var result = MobileCache(profiles)
    while (approximateCacheBytes(result) > limits.maxApproximateBytes && profiles.isNotEmpty()) {
        val snapshotToDrop = profiles.entries
            .flatMap { (host, profile) -> profile.snapshots.values.map { host to it } }
            .minByOrNull { (_, snapshot) -> snapshot.summary.updatedAtMs }
        if (snapshotToDrop != null) {
            val (host, snapshot) = snapshotToDrop
            val profile = profiles.getValue(host)
            profiles = profiles + (host to profile.copy(snapshots = profile.snapshots - snapshot.summary.id))
        } else {
            val host = profiles.keys.first()
            val profile = profiles.getValue(host)
            profiles = if (profile.threadList.isEmpty()) {
                profiles - host
            } else {
                profiles + (host to profile.copy(threadList = profile.threadList.drop(1)))
            }
        }
        result = MobileCache(profiles)
    }
    return result
}

private fun ProfileMobileCache.bounded(limits: MobileCacheLimits): ProfileMobileCache {
    val list = threadList.asReversed().distinctBy { it.id }.take(limits.maxThreads).asReversed().map {
        it.bounded(limits.maxTextCharacters)
    }
    val allowed = list.mapTo(mutableSetOf()) { it.id }
    val snapshots = snapshots.filterKeys { it in allowed }.values
        .sortedByDescending { it.summary.updatedAtMs }
        .take(limits.maxThreads)
        .associate { it.summary.id to it.bounded(limits) }
    return ProfileMobileCache(list, snapshots, unknownEvents.takeLast(128), rawMessages.takeLast(128))
}

private fun ThreadSnapshot.bounded(limits: MobileCacheLimits): ThreadSnapshot {
    val retained = turns.flatMap { turn -> turn.items.map { turn.id to it.id } }.takeLast(limits.maxItemsPerThread).toSet()
    return copy(
        summary = summary.bounded(limits.maxTextCharacters),
        turns = turns.map { turn ->
            turn.copy(items = turn.items.filter { turn.id to it.id in retained }.map { it.bounded(limits.maxTextCharacters) })
        }.filter { it.items.isNotEmpty() || it.status == TurnStatus.InProgress },
    )
}

private fun ThreadSummary.bounded(max: Int): ThreadSummary = copy(
    name = name?.truncated(max),
    preview = preview.truncated(max),
    workingDirectory = workingDirectory.copy(path = workingDirectory.path.truncated(max)),
)

private fun CodexItem.bounded(max: Int): CodexItem = when (this) {
    is CodexItem.UserMessage -> copy(text = text.truncated(max))
    is CodexItem.AgentMessage -> copy(text = text.truncated(max))
    is CodexItem.Reasoning -> copy(summary = summary.truncated(max))
    is CodexItem.CommandExecution -> copy(
        command = command.truncated(max), cwd = cwd?.truncated(max), output = output.truncated(max),
    )
    is CodexItem.FileChange -> copy(changes = changes.map { change ->
        change.copy(path = change.path.truncated(max), diff = change.diff.truncated(max))
    })
    is CodexItem.Unknown -> copy(raw = raw)
}

private fun <T> List<T>.replaceById(id: String, value: T, idOf: (T) -> String): List<T> {
    val index = indexOfFirst { idOf(it) == id }
    return if (index < 0) this + value else toMutableList().also { it[index] = value }
}

private fun List<ThreadSummary>.replaceById(id: String, value: ThreadSummary): List<ThreadSummary> =
    replaceById(id, value) { it.id }

private fun String.truncated(max: Int): String = if (length <= max) this else take(max - 1) + "…"
private fun String.approximateBytes(): Int = length * 2 + 8
private fun JsonObject.approximateBytes(): Int = toString().approximateBytes()

private fun RawCodexMessage.approximateBytes(): Int = when (this) {
    is RawCodexMessage.Notification -> method.approximateBytes() + params.toString().approximateBytes() + extensions.approximateBytes()
    is RawCodexMessage.ServerRequest -> id.toString().approximateBytes() + method.approximateBytes() +
        params.toString().approximateBytes() + extensions.approximateBytes()
}

private fun ThreadSummary.approximateBytes(): Int = id.approximateBytes() + (name?.approximateBytes() ?: 0) +
    preview.approximateBytes() + workingDirectory.path.approximateBytes() + 24 + status.approximateBytes() +
    (raw?.approximateBytes() ?: 0)
private fun ThreadSnapshot.approximateBytes(): Int = summary.approximateBytes() + turns.sumOf { it.approximateBytes() } +
    (raw?.approximateBytes() ?: 0)
private fun CodexTurn.approximateBytes(): Int = id.approximateBytes() + 8 + items.sumOf { it.approximateBytes() } +
    (raw?.approximateBytes() ?: 0)
private fun ThreadStatus.approximateBytes(): Int = when (this) {
    is ThreadStatus.Active -> activeFlags.sumOf { it.approximateBytes() } + 4
    ThreadStatus.NotLoaded, ThreadStatus.Idle, ThreadStatus.SystemError -> 4
}
private fun CodexItem.approximateBytes(): Int = when (this) {
    is CodexItem.UserMessage -> id.approximateBytes() + text.approximateBytes()
    is CodexItem.AgentMessage -> id.approximateBytes() + text.approximateBytes()
    is CodexItem.Reasoning -> id.approximateBytes() + summary.approximateBytes()
    is CodexItem.CommandExecution -> id.approximateBytes() + command.approximateBytes() +
        (cwd?.approximateBytes() ?: 0) + output.approximateBytes() + 8
    is CodexItem.FileChange -> id.approximateBytes() + changes.sumOf { it.path.approximateBytes() + it.diff.approximateBytes() + 4 }
    is CodexItem.Unknown -> id.approximateBytes() + codexType.approximateBytes() + raw.toString().approximateBytes()
}

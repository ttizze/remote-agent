package dev.remoteagent.mobile

import kotlinx.serialization.json.JsonObject

internal fun ProfileMobileCache.bounded(limits: MobileCacheLimits): ProfileMobileCache {
    val list =
        threadList
            .groupBy { it.id }
            .values
            .map { summaries -> summaries.maxBy { it.updatedAtMs } }
            .sortedByDescending { it.updatedAtMs }
    val latestProjectActivity = mutableMapOf<String, Long>()
    list.forEach { summary -> summary.projectId?.let { latestProjectActivity.getOrPut(it) { summary.updatedAtMs } } }
    val projects =
        projects
            .distinctBy { it.id }
            .sortedWith(
                compareByDescending<CodexProject> { latestProjectActivity[it.id] ?: Long.MIN_VALUE }
                    .thenBy { it.position }
            )
    return ProfileMobileCache(
        projects = projects,
        threadList = list,
        snapshots =
            snapshots.values.toList().takeLast(limits.maxThreads).associate { it.summary.id to it.bounded(limits) },
    )
}

internal fun ProfileMobileCache.boundedEvent(limits: MobileCacheLimits): ProfileMobileCache {
    val snapshots =
        if (snapshots.size <= limits.maxThreads && snapshots.values.all { it.turns.size <= limits.maxTurnsPerThread })
            snapshots
        else snapshots.values.toList().takeLast(limits.maxThreads).associate { it.summary.id to it.bounded(limits) }
    return if (snapshots === this.snapshots) this else copy(snapshots = snapshots)
}

private fun ThreadSnapshot.bounded(limits: MobileCacheLimits): ThreadSnapshot {
    return if (turns.size <= limits.maxTurnsPerThread) this
    else copy(turns = turns.takeLast(limits.maxTurnsPerThread), raw = raw?.let { JsonObject(it - "historyCursor") })
}

internal fun List<ThreadSummary>.replaceById(
    id: String,
    value: ThreadSummary,
    append: Boolean = true,
): List<ThreadSummary> {
    val index = indexOfFirst { it.id == id }
    return if (index < 0) {
        if (append) this + value else this
    } else if (this[index] == value) this else toMutableList().also { it[index] = value }
}

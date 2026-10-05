package dev.remoteagent.mobile

/** Tracks native edits until the receipt for the newest edit arrives. */
internal class DraftRevision {
    var pending: Long? = null
        private set

    var base = ""
    private var revision = 0L

    fun edit(text: String): Pair<Long, String> {
        val previous = base
        base = text
        pending = ++revision
        return revision to previous
    }

    fun acknowledge(completed: Long): Boolean {
        if (pending != completed) return false
        pending = null
        return true
    }

    fun reset() {
        revision++
        pending = null
        base = ""
    }
}

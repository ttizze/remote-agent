package dev.remoteagent.mobile

import kotlinx.serialization.Serializable
import kotlinx.serialization.SerializationException
import kotlinx.serialization.encodeToString
import kotlinx.serialization.json.Json

private val codecJson = Json {
    encodeDefaults = true
    explicitNulls = true
    ignoreUnknownKeys = true
    isLenient = false
}

/** Malformed state cannot prevent app startup. */
sealed interface MobileStateDecodeResult<out T> {
    data class Success<T>(val value: T) : MobileStateDecodeResult<T>

    data class Failure(val reason: MobileStateDecodeReason, val version: Int? = null) : MobileStateDecodeResult<Nothing>
}

enum class MobileStateDecodeReason {
    Oversize,
    Corrupt,
    UnsupportedVersion,
}

/** Durable fields are declared on the state types; credentials remain in secure storage. */
object MobileStateCodec {
    const val CurrentVersion: Int = 3
    const val MaxInputBytes: Int = 1024 * 1024

    /** Keep navigation if the disposable display cache exceeds storage. */
    fun encode(state: AppState): ByteArray {
        val bytes = encodeEnvelope(state.copy(cache = state.cache.forPersistence()), enforceLimit = false)
        if (bytes.size <= MaxInputBytes) return bytes
        return encodeEnvelope(state.copy(cache = MobileCache()))
    }

    fun decode(
        bytes: ByteArray,
        cacheLimits: MobileCacheLimits = MobileCacheLimits(),
    ): MobileStateDecodeResult<AppState> {
        if (bytes.size > MaxInputBytes) return MobileStateDecodeResult.Failure(MobileStateDecodeReason.Oversize)
        return try {
            val envelope = codecJson.decodeFromString<PersistedEnvelope>(bytes.decodeToString())
            when {
                envelope.version != CurrentVersion ->
                    MobileStateDecodeResult.Failure(MobileStateDecodeReason.UnsupportedVersion, envelope.version)
                envelope.format != Format -> MobileStateDecodeResult.Failure(MobileStateDecodeReason.Corrupt)
                else -> MobileStateDecodeResult.Success(envelope.payload.restore(cacheLimits))
            }
        } catch (_: SerializationException) {
            MobileStateDecodeResult.Failure(MobileStateDecodeReason.Corrupt)
        } catch (_: IllegalArgumentException) {
            MobileStateDecodeResult.Failure(MobileStateDecodeReason.Corrupt)
        } catch (_: IllegalStateException) {
            MobileStateDecodeResult.Failure(MobileStateDecodeReason.Corrupt)
        }
    }

    private fun encodeEnvelope(payload: AppState, enforceLimit: Boolean = true): ByteArray {
        val bytes =
            codecJson
                .encodeToString(PersistedEnvelope(format = Format, version = CurrentVersion, payload = payload))
                .encodeToByteArray()
        require(!enforceLimit || bytes.size <= MaxInputBytes) {
            "Mobile state exceeds the $MaxInputBytes-byte storage limit"
        }
        return bytes
    }

    private const val Format = "remote-agent-mobile-state"
}

@Serializable private data class PersistedEnvelope(val format: String, val version: Int, val payload: AppState)

private fun AppState.restore(cacheLimits: MobileCacheLimits): AppState {
    val restoredProfiles =
        profiles
            .asSequence()
            .filter {
                it.hostIdentity.isNotBlank() &&
                    it.runnerId.isNotBlank() &&
                    it.relayUrl.isNotBlank() &&
                    it.deviceIdentityReference.isNotBlank()
            }
            .map { if (it.name.isBlank()) it.copy(name = it.hostIdentity) else it }
            .distinctBy { it.id }
            .toList()
    val restoredProfileIds = restoredProfiles.mapTo(mutableSetOf()) { it.id }
    return copy(
        profiles = restoredProfiles,
        selectedProfileId = selectedProfileId?.takeIf { it in restoredProfileIds },
        profileViews = restoredProfiles.associate { it.id to (profileViews[it.id] ?: ProfileViewState()) },
        cache = MobileCache(cache.profiles.filterKeys { it in restoredProfileIds }).applyCacheLimits(cacheLimits),
    )
}

private fun MobileCache.forPersistence(): MobileCache =
    copy(
        profiles =
            profiles.mapValues { (_, profile) ->
                profile.copy(
                    threadList = profile.threadList.map { it.withoutRawBody() },
                    snapshots = profile.snapshots.mapValues { (_, snapshot) -> snapshot.forPersistence() },
                )
            }
    )

/** Apply the same bounds as live reads; keep only snapshots with retained list entries. */
private fun MobileCache.applyCacheLimits(limits: MobileCacheLimits): MobileCache {
    var result = MobileCache()
    profiles.forEach { (hostIdentity, profile) ->
        result = reconcileThreadList(result, hostIdentity, profile.threadList, limits)
        profile.snapshots.values
            .sortedWith(compareBy<ThreadSnapshot> { it.summary.updatedAtMs }.thenBy { it.summary.id })
            .forEach { snapshot ->
                if (snapshot.summary.id in result.profile(hostIdentity).threadList.map { it.id }) {
                    result =
                        reconcileThreadRead(
                            result,
                            hostIdentity,
                            ThreadReadResult(thread = snapshot, bufferedEvents = emptyList()),
                            limits,
                        )
                }
            }
    }
    return result
}

private fun ThreadSummary.withoutRawBody(): ThreadSummary {
    val metadata = raw?.without("turns")
    return if (metadata === raw) this else copy(raw = metadata)
}

private fun ThreadSnapshot.forPersistence(): ThreadSnapshot {
    val summary = summary.withoutRawBody()
    val metadata = raw?.without("turns")
    var updated: MutableList<CodexTurn>? = null
    turns.forEachIndexed { index, turn ->
        val turnMetadata = turn.raw?.without("items")
        val error = turn.error?.takeUnless(CodexTurnError::willRetry)
        if (turnMetadata !== turn.raw || error !== turn.error || turn.pendingRequests.isNotEmpty()) {
            val destination = updated ?: turns.toMutableList().also { updated = it }
            destination[index] = turn.copy(raw = turnMetadata, error = error, pendingRequests = emptyList())
        }
    }
    return if (summary !== this.summary || metadata !== raw || updated != null)
        copy(summary = summary, raw = metadata, turns = updated ?: turns)
    else this
}

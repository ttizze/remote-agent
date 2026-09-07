package dev.remoteagent.mobile

import kotlinx.serialization.Serializable
import kotlinx.serialization.SerializationException
import kotlinx.serialization.encodeToString
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.decodeFromJsonElement

private val codecJson = Json {
    encodeDefaults = true
    explicitNulls = true
    ignoreUnknownKeys = true
    isLenient = false
}

/**
 * The result returned by the state decoder.  Decode failures are data rather
 * than thrown exceptions so a malformed file cannot take down app startup.
 */
sealed interface MobileStateDecodeResult<out T> {
    data class Success<T>(val value: T) : MobileStateDecodeResult<T>

    data class Failure(
        val reason: MobileStateDecodeReason,
        val version: Int? = null,
    ) : MobileStateDecodeResult<Nothing>
}

enum class MobileStateDecodeReason {
    Oversize,
    Corrupt,
    UnsupportedVersion,
    WrongPayloadKind,
}

/**
 * Versioned, platform-neutral persistence for the mobile profile and cache
 * state. Only secure-store references are retained; platform secure storage
 * owns device keys and relay credentials.
 *
 * The codec intentionally accepts and returns ByteArray values.  Android and
 * iOS repositories therefore only need to own atomic file I/O; they do not
 * need to duplicate the model projection or JSON schema.
 */
object MobileStateCodec {
    const val CurrentVersion: Int = 2
    const val MaxInputBytes: Int = 1024 * 1024

    /** Navigation is durable even when the disposable display cache exceeds storage. */
    fun encode(state: AppState): ByteArray {
        val bytes = encodeEnvelope(AppStateKind, state.toPersisted(), enforceLimit = false)
        if (bytes.size <= MaxInputBytes) return bytes
        return encodeEnvelope(AppStateKind, state.copy(cache = MobileCache()).toPersisted())
    }

    /** Encode only the display cache into the current envelope. */
    fun encode(cache: MobileCache): ByteArray = encodeCache(cache)

    fun encodeCache(cache: MobileCache): ByteArray = encodeEnvelope(
        kind = CacheKind,
        payload = cache.toPersisted(),
    )

    /** Decode an application state and apply local cache limits after decode. */
    fun decode(
        bytes: ByteArray,
        cacheLimits: MobileCacheLimits = MobileCacheLimits(),
    ): MobileStateDecodeResult<AppState> = decodeAppState(bytes, cacheLimits)

    fun decodeAppState(
        bytes: ByteArray,
        cacheLimits: MobileCacheLimits = MobileCacheLimits(),
    ): MobileStateDecodeResult<AppState> = when (val envelope = readEnvelope(bytes, AppStateKind)) {
        is MobileStateDecodeResult.Failure -> envelope
        is MobileStateDecodeResult.Success -> decodePayload<PersistedAppState, AppState>(envelope.value.payload) {
            it.toAppState(cacheLimits)
        }
    }

    /** Decode only a display cache and apply local cache limits after decode. */
    fun decodeCache(
        bytes: ByteArray,
        cacheLimits: MobileCacheLimits = MobileCacheLimits(),
    ): MobileStateDecodeResult<MobileCache> = when (val envelope = readEnvelope(bytes, CacheKind)) {
        is MobileStateDecodeResult.Failure -> envelope
        is MobileStateDecodeResult.Success -> decodePayload<PersistedMobileCache, MobileCache>(envelope.value.payload) {
            it.toMobileCache(cacheLimits)
        }
    }

    private inline fun <reified T, R> decodePayload(
        payload: JsonObject,
        transform: (T) -> R,
    ): MobileStateDecodeResult<R> = try {
        MobileStateDecodeResult.Success(transform(codecJson.decodeFromJsonElement<T>(payload)))
    } catch (_: SerializationException) {
        MobileStateDecodeResult.Failure(MobileStateDecodeReason.Corrupt)
    } catch (_: IllegalArgumentException) {
        MobileStateDecodeResult.Failure(MobileStateDecodeReason.Corrupt)
    } catch (_: IllegalStateException) {
        MobileStateDecodeResult.Failure(MobileStateDecodeReason.Corrupt)
    }

    private fun readEnvelope(
        bytes: ByteArray,
        expectedKind: String,
    ): MobileStateDecodeResult<PersistedEnvelope<JsonObject>> {
        if (bytes.size > MaxInputBytes) {
            return MobileStateDecodeResult.Failure(MobileStateDecodeReason.Oversize)
        }

        val root = try {
            codecJson.parseToJsonElement(bytes.decodeToString()) as? JsonObject
        } catch (_: SerializationException) {
            null
        } catch (_: IllegalArgumentException) {
            null
        }
            ?: return MobileStateDecodeResult.Failure(MobileStateDecodeReason.Corrupt)

        // Inspect the version before decoding the payload.  This lets a
        // future format fail closed without attempting to interpret fields it
        // does not understand.
        val version = (root[VersionField] as? JsonPrimitive)?.content?.toIntOrNull()
            ?: return MobileStateDecodeResult.Failure(MobileStateDecodeReason.Corrupt)
        if (version != CurrentVersion) {
            return MobileStateDecodeResult.Failure(MobileStateDecodeReason.UnsupportedVersion, version)
        }

        val envelope = try {
            codecJson.decodeFromJsonElement<PersistedEnvelope<JsonObject>>(root)
        } catch (_: SerializationException) {
            return MobileStateDecodeResult.Failure(MobileStateDecodeReason.Corrupt)
        } catch (_: IllegalArgumentException) {
            return MobileStateDecodeResult.Failure(MobileStateDecodeReason.Corrupt)
        }
        if (envelope.format != Format) {
            return MobileStateDecodeResult.Failure(MobileStateDecodeReason.Corrupt)
        }
        if (envelope.kind != expectedKind) {
            return MobileStateDecodeResult.Failure(MobileStateDecodeReason.WrongPayloadKind)
        }
        return MobileStateDecodeResult.Success(envelope)
    }

    private inline fun <reified T> encodeEnvelope(kind: String, payload: T, enforceLimit: Boolean = true): ByteArray {
        val bytes = codecJson.encodeToString(
            PersistedEnvelope(
                format = Format,
                version = CurrentVersion,
                kind = kind,
                payload = payload,
            ),
        ).encodeToByteArray()
        require(!enforceLimit || bytes.size <= MaxInputBytes) { "Mobile state exceeds the $MaxInputBytes-byte storage limit" }
        return bytes
    }

    private const val Format = "remote-agent-mobile-state"
    private const val VersionField = "version"
    private const val AppStateKind = "appState"
    private const val CacheKind = "mobileCache"
}

/** A small explicit envelope keeps the persisted schema independent of model DTOs. */
@Serializable
private data class PersistedEnvelope<T>(
    val format: String,
    val version: Int,
    val kind: String,
    val payload: T,
)

@Serializable
private data class PersistedAppState(
    val profiles: List<PersistedHostProfile> = emptyList(),
    val selectedProfileId: String? = null,
    val profileViews: Map<String, PersistedProfileViewState> = emptyMap(),
    val cache: PersistedMobileCache = PersistedMobileCache(),
)

@Serializable
private data class PersistedHostProfile(
    val runnerId: String,
    val name: String,
    val relayUrl: String,
    val hostIdentity: String,
    val deviceIdentityReference: String,
)

/** Only durable view choices are persisted; connection/load state is runtime-only. */
@Serializable
private data class PersistedProfileViewState(
    val workingDirectoryPath: String = "",
    val selectedThreadId: String? = null,
    val unreadCompletedThreadIds: Set<String> = emptySet(),
)

@Serializable
private data class PersistedMobileCache(
    val profiles: Map<String, PersistedProfileMobileCache> = emptyMap(),
)

@Serializable
private data class PersistedProfileMobileCache(
    val threadList: List<ThreadSummary> = emptyList(),
    val snapshots: Map<String, ThreadSnapshot> = emptyMap(),
    val unknownEvents: List<ThreadEvent.Unknown> = emptyList(),
    val rawNotifications: List<PersistedRawNotification> = emptyList(),
)

@Serializable
private data class PersistedRawNotification(
    val method: String,
    val params: JsonElement,
    val extensions: JsonObject = JsonObject(emptyMap()),
)

private fun AppState.toPersisted(): PersistedAppState = PersistedAppState(
    profiles = profiles.map { profile ->
        PersistedHostProfile(
            runnerId = profile.runnerId,
            name = profile.name,
            relayUrl = profile.relayUrl,
            hostIdentity = profile.hostIdentity,
            deviceIdentityReference = profile.deviceIdentityReference,
        )
    },
    selectedProfileId = selectedProfileId,
    profileViews = profileViews.mapValues { (_, view) ->
        PersistedProfileViewState(
            workingDirectoryPath = view.workingDirectoryPath,
            selectedThreadId = view.selectedThreadId,
            unreadCompletedThreadIds = view.unreadCompletedThreadIds,
        )
    },
    cache = cache.toPersisted(),
)

private fun PersistedAppState.toAppState(cacheLimits: MobileCacheLimits): AppState {
    val restoredProfiles = profiles.asSequence()
        .filter { it.hostIdentity.isNotBlank() && it.runnerId.isNotBlank() && it.relayUrl.isNotBlank() && it.deviceIdentityReference.isNotBlank() }
        .map { profile ->
            HostProfile(
                runnerId = profile.runnerId,
                name = profile.name.ifBlank { profile.hostIdentity },
                relayUrl = profile.relayUrl,
                hostIdentity = profile.hostIdentity,
            deviceIdentityReference = profile.deviceIdentityReference,
            )
        }
        .distinctBy { it.id }
        .toList()
    val restoredProfileIds = restoredProfiles.mapTo(mutableSetOf()) { it.id }
    return AppState(
        profiles = restoredProfiles,
        selectedProfileId = selectedProfileId?.takeIf { it in restoredProfileIds },
        profileViews = restoredProfiles.associate { profile ->
            val view = profileViews[profile.id] ?: PersistedProfileViewState()
            profile.id to ProfileViewState(
                connection = ConnectionPhase.Disconnected,
                workingDirectoryPath = view.workingDirectoryPath,
                threadList = LoadPhase.Idle,
                selectedThreadId = view.selectedThreadId,
                unreadCompletedThreadIds = view.unreadCompletedThreadIds,
                threadDetail = LoadPhase.Idle,
                interruptingTurnId = null,
                notice = null,
            )
        },
        cache = cache.toMobileCache(cacheLimits).let { decoded ->
            MobileCache(decoded.profiles.filterKeys { it in restoredProfileIds })
        },
        showingPairing = false,
        pairingError = null,
    )
}

private fun MobileCache.toPersisted(): PersistedMobileCache = PersistedMobileCache(
    profiles = profiles.mapValues { (_, profile) ->
        PersistedProfileMobileCache(
            threadList = profile.threadList,
            snapshots = profile.snapshots.mapValues { (_, snapshot) ->
                snapshot.copy(
                    turns = snapshot.turns.map { turn ->
                        turn.copy(
                            error = turn.error?.takeUnless(CodexTurnError::willRetry),
                            pendingRequests = emptyList(),
                        )
                    },
                )
            },
            unknownEvents = profile.unknownEvents,
            rawNotifications = profile.rawMessages
                .filterIsInstance<RawCodexMessage.Notification>()
                .map { PersistedRawNotification(it.method, it.params, it.extensions) },
        )
    },
)

private fun PersistedMobileCache.toMobileCache(cacheLimits: MobileCacheLimits): MobileCache {
    val decoded = MobileCache(
        profiles = profiles.mapValues { (_, profile) ->
            ProfileMobileCache(
                threadList = profile.threadList,
                snapshots = profile.snapshots,
                unknownEvents = profile.unknownEvents,
                rawMessages = profile.rawNotifications.map {
                    RawCodexMessage.Notification(it.method, it.params, it.extensions)
                },
            )
        },
    )
    return decoded.applyCacheLimits(cacheLimits)
}

/**
 * Reuse the cache's public reconciliation entry points so decode applies the
 * same thread/item/byte limits as live updates.  In particular, snapshots
 * without a retained list entry are dropped just as [reconcileThreadRead]
 * would drop them through the normal bounded cache path.
 */
private fun MobileCache.applyCacheLimits(limits: MobileCacheLimits): MobileCache {
    var result = MobileCache()
    profiles.forEach { (hostIdentity, profile) ->
        result = reconcileThreadList(result, hostIdentity, profile.threadList, limits)
        profile.snapshots.values
            .sortedWith(compareBy<ThreadSnapshot> { it.summary.updatedAtMs }.thenBy { it.summary.id })
            .forEach { snapshot ->
                if (snapshot.summary.id in result.profile(hostIdentity).threadList.map { it.id }) {
                    result = reconcileThreadRead(
                        result,
                        hostIdentity,
                        ThreadReadResult(thread = snapshot, bufferedEvents = emptyList()),
                        limits,
                    )
                }
            }
        profile.unknownEvents.forEach { event ->
            result = applyLiveEvent(result, hostIdentity, event, limits)
        }
        profile.rawMessages.forEach { message ->
            result = retainRawMessage(result, hostIdentity, message, limits)
        }
    }
    return result
}

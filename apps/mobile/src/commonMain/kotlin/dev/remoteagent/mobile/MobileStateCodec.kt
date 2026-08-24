package dev.remoteagent.mobile

import kotlinx.serialization.Serializable
import kotlinx.serialization.SerializationException
import kotlinx.serialization.encodeToString
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.decodeFromJsonElement
import kotlinx.serialization.json.encodeToJsonElement

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
 * Versioned, platform-neutral persistence for the non-secret mobile state.
 *
 * The codec intentionally accepts and returns ByteArray values.  Android and
 * iOS repositories therefore only need to own atomic file I/O; they do not
 * need to duplicate the model projection or JSON schema.
 */
object MobileStateCodec {
    const val CurrentVersion: Int = 1
    const val MaxInputBytes: Int = 1024 * 1024

    /** Encode an entire application state into the current envelope. */
    fun encode(state: AppState): ByteArray = encodeEnvelope(
        kind = AppStateKind,
        payload = codecJson.encodeToJsonElement(PersistedAppState.serializer(), state.toPersisted()),
    )

    /** Encode only the display cache into the current envelope. */
    fun encode(cache: MobileCache): ByteArray = encodeCache(cache)

    fun encodeCache(cache: MobileCache): ByteArray = encodeEnvelope(
        kind = CacheKind,
        payload = codecJson.encodeToJsonElement(PersistedMobileCache.serializer(), cache.toPersisted()),
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
        is MobileStateDecodeResult.Failure -> if (envelope.reason == MobileStateDecodeReason.Corrupt) {
            decodeLegacyAppState(bytes, cacheLimits)
        } else {
            envelope
        }
        is MobileStateDecodeResult.Success -> decodePayload<PersistedAppState, AppState>(envelope.value.payload) {
            it.toAppState(cacheLimits)
        }
    }

    /** Decode only a display cache and apply local cache limits after decode. */
    fun decodeCache(
        bytes: ByteArray,
        cacheLimits: MobileCacheLimits = MobileCacheLimits(),
    ): MobileStateDecodeResult<MobileCache> = when (val envelope = readEnvelope(bytes, CacheKind)) {
        is MobileStateDecodeResult.Failure -> if (envelope.reason == MobileStateDecodeReason.Corrupt) {
            decodeLegacyCache(bytes, cacheLimits)
        } else {
            envelope
        }
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
    ): MobileStateDecodeResult<PersistedEnvelope> {
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
            codecJson.decodeFromJsonElement<PersistedEnvelope>(root)
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

    private fun encodeEnvelope(kind: String, payload: JsonElement): ByteArray {
        val bytes = codecJson.encodeToString(
            PersistedEnvelope(
                format = Format,
                version = CurrentVersion,
                kind = kind,
                payload = payload as? JsonObject ?: error("State payload must be an object"),
            ),
        ).encodeToByteArray()
        require(bytes.size <= MaxInputBytes) { "Mobile state exceeds the $MaxInputBytes-byte storage limit" }
        return bytes
    }

    private const val Format = "remote-agent-mobile-state"
    private const val VersionField = "version"
    private const val AppStateKind = "appState"
    private const val CacheKind = "mobileCache"
}

/*
 * Legacy migration
 * ----------------
 *
 * Before the common codec existed Android wrote a hand-shaped JSON object
 * (`version = 1`) while iOS wrote the Kotlin serialization shape (no version
 * field).  The two forms deliberately share the same top-level concepts, so
 * migration stays here instead of leaking platform-specific parsers back into
 * the repositories.
 */

private fun decodeLegacyAppState(
    bytes: ByteArray,
    cacheLimits: MobileCacheLimits,
): MobileStateDecodeResult<AppState> {
    val root = parseRootForLegacy(bytes)
        ?: return MobileStateDecodeResult.Failure(MobileStateDecodeReason.Corrupt)
    if (root["profiles"] !is JsonArray) {
        return MobileStateDecodeResult.Failure(MobileStateDecodeReason.Corrupt)
    }

    return try {
        val profiles = (root["profiles"] as JsonArray)
            .mapNotNull(::parseLegacyHostProfile)
            .distinctBy { it.hostIdentity }
        val profileIds = profiles.mapTo(mutableSetOf()) { it.hostIdentity }
        val views = profiles.associate { profile ->
            val view = root.childObject("views")?.childObject(profile.hostIdentity)
            profile.hostIdentity to ProfileViewState(
                connection = ConnectionPhase.Disconnected,
                workingDirectoryPath = view?.string("workingDirectoryPath").orEmpty(),
                threadList = LoadPhase.Idle,
                selectedThreadId = view?.string("selectedThreadId")?.takeIf { it.isNotBlank() },
                threadDetail = LoadPhase.Idle,
            )
        }
        AppState(
            profiles = profiles,
            selectedProfileId = root.string("selectedProfileId")?.takeIf { it in profileIds },
            profileViews = views,
            cache = parseLegacyCache(root.childObject("cache"), cacheLimits),
            showingPairing = false,
            pairingError = null,
        ).let { MobileStateDecodeResult.Success(it) }
    } catch (_: IllegalArgumentException) {
        MobileStateDecodeResult.Failure(MobileStateDecodeReason.Corrupt)
    } catch (_: IllegalStateException) {
        MobileStateDecodeResult.Failure(MobileStateDecodeReason.Corrupt)
    }
}

private fun decodeLegacyCache(
    bytes: ByteArray,
    cacheLimits: MobileCacheLimits,
): MobileStateDecodeResult<MobileCache> {
    val root = parseRootForLegacy(bytes)
        ?: return MobileStateDecodeResult.Failure(MobileStateDecodeReason.Corrupt)
    val cache = root.childObject("cache")
        ?: return MobileStateDecodeResult.Failure(MobileStateDecodeReason.Corrupt)
    return try {
        MobileStateDecodeResult.Success(parseLegacyCache(cache, cacheLimits))
    } catch (_: IllegalArgumentException) {
        MobileStateDecodeResult.Failure(MobileStateDecodeReason.Corrupt)
    } catch (_: IllegalStateException) {
        MobileStateDecodeResult.Failure(MobileStateDecodeReason.Corrupt)
    }
}

private fun parseRootForLegacy(bytes: ByteArray): JsonObject? {
    if (bytes.size > MobileStateCodec.MaxInputBytes) return null
    return try {
        codecJson.parseToJsonElement(bytes.decodeToString()) as? JsonObject
    } catch (_: SerializationException) {
        null
    } catch (_: IllegalArgumentException) {
        null
    }
}

private fun parseLegacyHostProfile(value: JsonElement): HostProfile? {
    val raw = value.asObjectOrNull() ?: return null
    val hostIdentity = raw.string("hostIdentity")?.takeIf { it.isNotBlank() } ?: return null
    val deviceIdentityReference = raw.string("deviceIdentityReference")?.takeIf { it.isNotBlank() } ?: return null
    return HostProfile(
        hostIdentity = hostIdentity,
        name = raw.string("name") ?: hostIdentity,
        addresses = raw.array("addresses").orEmpty().mapNotNull(JsonElement::stringOrNull).filter(String::isNotBlank).distinct(),
        deviceIdentityReference = deviceIdentityReference,
    )
}

private fun parseLegacyCache(
    cache: JsonObject?,
    limits: MobileCacheLimits,
): MobileCache {
    if (cache == null) return MobileCache()
    val decoded = MobileCache(
        profiles = cache.mapValues { (_, value) -> parseLegacyProfileCache(value) },
    )
    return decoded.applyCacheLimits(limits)
}

private fun parseLegacyProfileCache(value: JsonElement): ProfileMobileCache {
    val raw = value.asObjectOrNull() ?: return ProfileMobileCache()
    val threadList = raw.array("threadList").orEmpty().mapNotNull(::parseLegacyThreadSummary)
    val snapshots = raw["snapshots"].legacyValues()
        .mapNotNull(::parseLegacyThreadSnapshot)
        .associateBy { it.summary.id }
    val unknownEvents = raw.array("unknownEvents").orEmpty().mapNotNull(::parseLegacyUnknownEvent)
    return ProfileMobileCache(
        threadList = threadList,
        snapshots = snapshots,
        unknownEvents = unknownEvents,
        // Raw messages are transport-lifetime data and were never part of the
        // durable Android/iOS contracts.
        rawMessages = emptyList(),
    )
}

private fun JsonElement?.legacyValues(): List<JsonElement> = when (this) {
    is JsonArray -> this
    is JsonObject -> values.toList()
    else -> emptyList()
}

private fun parseLegacyThreadSummary(value: JsonElement): ThreadSummary? {
    val raw = value.asObjectOrNull() ?: return null
    val id = raw.string("id")?.takeIf { it.isNotBlank() } ?: return null
    val workingDirectory = when (val value = raw["workingDirectory"]) {
        is JsonObject -> value.string("path").orEmpty()
        null -> ""
        else -> value.stringOrNull().orEmpty()
    }
    return ThreadSummary(
        id = id,
        name = raw.string("name"),
        preview = raw.string("preview").orEmpty(),
        workingDirectory = WorkingDirectory(workingDirectory),
        createdAtMs = raw.long("createdAtMs") ?: raw.long("createdAt") ?: 0L,
        updatedAtMs = raw.long("updatedAtMs") ?: raw.long("updatedAt") ?: 0L,
        status = parseLegacyThreadStatus(raw["status"]),
        raw = raw.childObject("raw"),
    )
}

private fun parseLegacyThreadSnapshot(value: JsonElement): ThreadSnapshot? {
    val raw = value.asObjectOrNull() ?: return null
    val summary = raw.childObject("summary")?.let(::parseLegacyThreadSummary) ?: return null
    return ThreadSnapshot(
        summary = summary,
        turns = raw.array("turns").orEmpty().mapNotNull(::parseLegacyTurn),
        raw = raw.childObject("raw"),
    )
}

private fun parseLegacyTurn(value: JsonElement): CodexTurn? {
    val raw = value.asObjectOrNull() ?: return null
    val id = raw.string("id")?.takeIf { it.isNotBlank() } ?: return null
    return CodexTurn(
        id = id,
        status = parseLegacyTurnStatus(raw["status"]),
        items = raw.array("items").orEmpty().mapNotNull(::parseLegacyItem),
        raw = raw.childObject("raw"),
    )
}

private fun parseLegacyItem(value: JsonElement): CodexItem? {
    val raw = value.asObjectOrNull() ?: return null
    val id = raw.string("id").orEmpty()
    return when (raw.string("type").legacyType()) {
        "usermessage" -> CodexItem.UserMessage(id, raw.textLike())
        "agentmessage" -> CodexItem.AgentMessage(id, raw.textLike())
        "reasoning" -> CodexItem.Reasoning(id, raw.textLike())
        "commandexecution" -> CodexItem.CommandExecution(
            id = id,
            command = raw.string("command").orEmpty(),
            cwd = raw.string("cwd"),
            output = raw.string("output").orEmpty(),
            status = parseLegacyCommandStatus(raw["status"]),
            exitCode = raw.long("exitCode")?.toInt(),
        )
        "filechange" -> CodexItem.FileChange(
            id = id,
            changes = raw.array("changes").orEmpty().mapNotNull(::parseLegacyFileChange),
            status = parseLegacyFileChangeStatus(raw["status"]),
        )
        "unknown" -> CodexItem.Unknown(
            id = id,
            codexType = raw.string("codexType") ?: raw.string("type") ?: "unknown",
            raw = raw.childObject("raw") ?: raw,
        )
        else -> CodexItem.Unknown(
            id = id,
            codexType = raw.string("type") ?: "unknown",
            raw = raw,
        )
    }
}

private fun parseLegacyFileChange(value: JsonElement): FileUpdateChange? {
    val raw = value.asObjectOrNull() ?: return null
    return FileUpdateChange(
        path = raw.string("path").orEmpty(),
        kind = parseLegacyFileUpdateKind(raw["kind"] ?: raw["type"]),
        diff = raw.string("diff") ?: raw.string("patch").orEmpty(),
    )
}

private fun parseLegacyUnknownEvent(value: JsonElement): ThreadEvent.Unknown? {
    val raw = value.asObjectOrNull() ?: return null
    return ThreadEvent.Unknown(
        threadId = raw.string("threadId").orEmpty(),
        turnId = raw.string("turnId").orEmpty(),
        method = raw.string("method") ?: "unknown",
        raw = raw.childObject("raw") ?: raw,
        extensions = raw.childObject("extensions") ?: JsonObject(emptyMap()),
    )
}

private fun parseLegacyThreadStatus(value: JsonElement?): ThreadStatus {
    val raw = value?.asObjectOrNull()
    return when ((raw?.string("type") ?: value?.stringOrNull()).legacyType()) {
        "active", "inprogress", "running" -> ThreadStatus.Active(
            raw?.array("activeFlags").orEmpty().mapNotNull(JsonElement::stringOrNull),
        )
        "notloaded" -> ThreadStatus.NotLoaded
        "systemerror", "error" -> ThreadStatus.SystemError
        else -> ThreadStatus.Idle
    }
}

private fun parseLegacyTurnStatus(value: JsonElement?): TurnStatus = when (value.legacyType()) {
    "inprogress", "started", "active", "running" -> TurnStatus.InProgress
    "interrupted", "cancelled", "canceled" -> TurnStatus.Interrupted
    "failed", "error" -> TurnStatus.Failed
    else -> TurnStatus.Completed
}

private fun parseLegacyCommandStatus(value: JsonElement?): CommandExecutionStatus = when (value.legacyType()) {
    "inprogress", "started", "active", "running" -> CommandExecutionStatus.InProgress
    "failed", "error" -> CommandExecutionStatus.Failed
    "declined", "rejected" -> CommandExecutionStatus.Declined
    else -> CommandExecutionStatus.Completed
}

private fun parseLegacyFileChangeStatus(value: JsonElement?): FileChangeStatus = when (value.legacyType()) {
    "inprogress", "started", "active", "running" -> FileChangeStatus.InProgress
    "failed", "error" -> FileChangeStatus.Failed
    "declined", "rejected" -> FileChangeStatus.Declined
    else -> FileChangeStatus.Completed
}

private fun parseLegacyFileUpdateKind(value: JsonElement?): FileUpdateKind = when (value.legacyType()) {
    "add", "create" -> FileUpdateKind.Add
    "delete", "remove" -> FileUpdateKind.Delete
    else -> FileUpdateKind.Update
}

private fun String?.legacyType(): String? = this?.lowercase()

private fun JsonElement?.legacyType(): String? = this?.stringOrNull().legacyType()

/** A small explicit envelope keeps storage migration independent of model DTOs. */
@Serializable
private data class PersistedEnvelope(
    val format: String,
    val version: Int,
    val kind: String,
    val payload: JsonObject,
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
    val hostIdentity: String,
    val name: String,
    val addresses: List<String>,
    val deviceIdentityReference: String,
)

/** Only durable view choices are persisted; connection/load state is runtime-only. */
@Serializable
private data class PersistedProfileViewState(
    val workingDirectoryPath: String = "",
    val selectedThreadId: String? = null,
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
            hostIdentity = profile.hostIdentity,
            name = profile.name,
            addresses = profile.addresses,
            deviceIdentityReference = profile.deviceIdentityReference,
        )
    },
    selectedProfileId = selectedProfileId,
    profileViews = profileViews.mapValues { (_, view) ->
        PersistedProfileViewState(
            workingDirectoryPath = view.workingDirectoryPath,
            selectedThreadId = view.selectedThreadId,
        )
    },
    cache = cache.toPersisted(),
)

private fun PersistedAppState.toAppState(cacheLimits: MobileCacheLimits): AppState {
    val restoredProfiles = profiles.asSequence()
        .filter { it.hostIdentity.isNotBlank() && it.deviceIdentityReference.isNotBlank() }
        .map { profile ->
            HostProfile(
                hostIdentity = profile.hostIdentity,
                name = profile.name.ifBlank { profile.hostIdentity },
                addresses = profile.addresses.filter(String::isNotBlank).distinct(),
                deviceIdentityReference = profile.deviceIdentityReference,
            )
        }
        .distinctBy { it.hostIdentity }
        .toList()
    val restoredProfileIds = restoredProfiles.mapTo(mutableSetOf()) { it.hostIdentity }
    return AppState(
        profiles = restoredProfiles,
        selectedProfileId = selectedProfileId?.takeIf { it in restoredProfileIds },
        profileViews = restoredProfiles.associate { profile ->
            val view = profileViews[profile.hostIdentity] ?: PersistedProfileViewState()
            profile.hostIdentity to ProfileViewState(
                connection = ConnectionPhase.Disconnected,
                workingDirectoryPath = view.workingDirectoryPath,
                threadList = LoadPhase.Idle,
                selectedThreadId = view.selectedThreadId,
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
            snapshots = profile.snapshots,
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

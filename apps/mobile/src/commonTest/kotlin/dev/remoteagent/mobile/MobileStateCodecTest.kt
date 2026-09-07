package dev.remoteagent.mobile

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertIs
import kotlin.test.assertNull
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.jsonObject

class MobileStateCodecTest {
    @Test
    fun encoded_profile_excludes_secrets_and_retains_secure_store_reference() {
        val state = AppState(
            profiles = listOf(
                HostProfile(
                    runnerId = "runner-1",
                    name = "Host",
                    relayUrl = "wss://relay.example.test/socket/websocket",
                    hostIdentity = "pinned-host-key",
                    deviceIdentityReference = "device-key-ref",
                ),
            ),
        )

        val encoded = MobileStateCodec.encode(state).decodeToString()

        assertEquals(true, "\"runnerId\"" in encoded)
        assertEquals(true, "\"relayUrl\"" in encoded)
        assertEquals(false, "\"relayToken\"" in encoded)
        assertEquals(true, "\"hostIdentity\"" in encoded)
        assertEquals(false, "\"addresses\"" in encoded)
        assertEquals(true, "\"deviceIdentityReference\"" in encoded)
    }

    @Test
    fun app_state_round_trip_keeps_profiles_views_cache_and_unknown_extensions() {
        val profile = HostProfile(
            runnerId = "runner-1",
            name = "Development Mac",
            relayUrl = "wss://relay.example.test/socket/websocket",
            hostIdentity = "pinned-host-key",
                    deviceIdentityReference = "device-key-ref",
        )
        val summary = summary("thread-1")
        val snapshot = ThreadSnapshot(
            summary = summary,
            turns = listOf(
                CodexTurn(
                    id = "turn-1",
                    status = TurnStatus.InProgress,
                    items = listOf(CodexItem.AgentMessage("item-1", "hello")),
                    raw = buildJsonObject { put("turnExtension", JsonPrimitive("kept")) },
                    error = CodexTurnError("reconnecting", willRetry = true),
                    pendingRequests = listOf(
                        CodexServerRequest(
                            id = "request-1",
                            method = "item/tool/requestUserInput",
                            params = buildJsonObject { put("question", JsonPrimitive("Continue?")) },
                        ),
                    ),
                ),
            ),
            raw = buildJsonObject { put("snapshotExtension", JsonPrimitive(true)) },
        )
        val unknownEvent = ThreadEvent.Unknown(
            threadId = "thread-1",
            turnId = "turn-1",
            method = "future/event",
            raw = buildJsonObject {
                put("futureValue", JsonPrimitive(42))
                put("nested", buildJsonObject { put("preserve", JsonPrimitive(true)) })
            },
            extensions = buildJsonObject { put("vendorExtension", JsonPrimitive("yes")) },
        )
        val rawMessage = RawCodexMessage.ServerRequest(
            id = JsonPrimitive(7),
            method = "approval/request",
            params = buildJsonObject { put("prompt", JsonPrimitive("Allow?")) },
            extensions = buildJsonObject { put("vendorField", JsonPrimitive("kept")) },
        )
        val rawNotification = RawCodexMessage.Notification(
            method = "future/notification",
            params = buildJsonObject { put("futurePayload", JsonPrimitive(true)) },
            extensions = buildJsonObject { put("notificationExtension", JsonPrimitive("kept")) },
        )
        val cache = MobileCache(
            profiles = mapOf(
                profile.id to ProfileMobileCache(
                    threadList = listOf(summary),
                    snapshots = mapOf(summary.id to snapshot),
                    unknownEvents = listOf(unknownEvent),
                    rawMessages = listOf(rawMessage, rawNotification),
                ),
            ),
        )
        val state = AppState(
            profiles = listOf(profile),
            selectedProfileId = profile.id,
            profileViews = mapOf(
                profile.id to ProfileViewState(
                    connection = ConnectionPhase.Connected,
                    workingDirectoryPath = "/workspace",
                    threadList = LoadPhase.Ready,
                    selectedThreadId = summary.id,
                    threadDetail = LoadPhase.Loading,
                    interruptingTurnId = "turn-1",
                    notice = "transient notice",
                ),
            ),
            cache = cache,
            showingPairing = true,
            pairingError = "transient pairing error",
        )

        val restored = success(MobileStateCodec.decode(MobileStateCodec.encode(state)))

        assertEquals(state.profiles, restored.profiles)
        assertEquals(state.selectedProfileId, restored.selectedProfileId)
        assertEquals(
            ProfileViewState(
                connection = ConnectionPhase.Disconnected,
                workingDirectoryPath = "/workspace",
                threadList = LoadPhase.Idle,
                selectedThreadId = summary.id,
                threadDetail = LoadPhase.Idle,
            ),
            restored.profileViews.getValue(profile.id),
        )
        assertEquals(false, restored.showingPairing)
        assertNull(restored.pairingError)
        assertEquals(unknownEvent, restored.cache.profile(profile.id).unknownEvents.single())
        // Notifications remain durable, while server requests must never be
        // actionable after process restart.
        assertEquals(listOf(rawNotification), restored.cache.profile(profile.id).rawMessages)
        assertEquals(
            snapshot.copy(
                turns = snapshot.turns.map { it.copy(error = null, pendingRequests = emptyList()) },
            ),
            restored.cache.snapshot(profile.id, summary.id),
        )
    }

    @Test
    fun cache_codec_round_trip_applies_limits_and_keeps_raw_notifications() {
        val summaries = (1..3).map { summary("thread-$it", updatedAtMs = it.toLong()) }
        val notification = RawCodexMessage.Notification(
            method = "future/notification",
            params = JsonPrimitive("payload"),
        )
        val cache = MobileCache(
            profiles = mapOf(
                "host-1" to ProfileMobileCache(
                    threadList = summaries,
                    snapshots = summaries.associate { it.id to ThreadSnapshot(it) },
                    rawMessages = listOf(notification),
                ),
            ),
        )

        val restored = success(
            MobileStateCodec.decodeCache(
                MobileStateCodec.encodeCache(cache),
                MobileCacheLimits(maxThreads = 1, maxTurnsPerThread = 10, maxApproximateBytes = 16 * 1024),
            ),
        )

        assertEquals(listOf("thread-3", "thread-2", "thread-1"), restored.profile("host-1").threadList.map { it.id })
        assertEquals(setOf("thread-3"), restored.profile("host-1").snapshots.keys)
        assertEquals(listOf(notification), restored.profile("host-1").rawMessages)
    }

    @Test
    fun unknown_version_is_a_safe_structured_failure() {
        val root = Json.parseToJsonElement(MobileStateCodec.encode(AppState()).decodeToString()).jsonObject
        val future = buildJsonObject {
            root.forEach { (name, value) ->
                put(name, if (name == "version") JsonPrimitive(999) else value)
            }
        }

        val failure = assertIs<MobileStateDecodeResult.Failure>(MobileStateCodec.decode(future.toString().encodeToByteArray()))

        assertEquals(MobileStateDecodeReason.UnsupportedVersion, failure.reason)
        assertEquals(999, failure.version)
    }

    @Test
    fun malformed_json_is_a_safe_structured_failure() {
        val failure = assertIs<MobileStateDecodeResult.Failure>(MobileStateCodec.decode("{not-json".encodeToByteArray()))

        assertEquals(MobileStateDecodeReason.Corrupt, failure.reason)
    }

    @Test
    fun input_larger_than_one_megabyte_is_rejected_before_json_parsing() {
        val oversized = ByteArray(MobileStateCodec.MaxInputBytes + 1) { ' '.code.toByte() }

        val failure = assertIs<MobileStateDecodeResult.Failure>(MobileStateCodec.decode(oversized))

        assertEquals(MobileStateDecodeReason.Oversize, failure.reason)
    }

    @Test
    fun oversized_cache_does_not_prevent_navigation_from_being_saved() {
        val profile = HostProfile("runner-1", "Host", "wss://relay.example.test/socket/websocket", "runner-1", "device-key-ref")
        val summary = summary("thread-1")
        val state = AppState(
            profiles = listOf(profile), selectedProfileId = profile.id,
            profileViews = mapOf(profile.id to ProfileViewState(selectedThreadId = summary.id, workingDirectoryPath = "/workspace")),
            cache = MobileCache(profiles = mapOf(profile.id to ProfileMobileCache(
                threadList = listOf(summary), snapshots = mapOf(summary.id to ThreadSnapshot(summary,
                    raw = buildJsonObject { put("large", JsonPrimitive("x".repeat(MobileStateCodec.MaxInputBytes))) },
                )),
            ))),
        )
        val encoded = MobileStateCodec.encode(state)
        val restored = assertIs<MobileStateDecodeResult.Success<AppState>>(MobileStateCodec.decode(encoded)).value
        assertEquals(profile, restored.selectedProfile)
        assertEquals("thread-1", restored.selectedView.selectedThreadId)
        assertEquals("/workspace", restored.selectedView.workingDirectoryPath)
        assertEquals(MobileCache(), restored.cache)
        assertEquals(true, state.cache.snapshot(profile.id, "thread-1") != null)
    }

    @Test
    fun encoded_state_cannot_exceed_the_decode_limit() {
        val profile = HostProfile("runner-1", "Host", "wss://relay.example.test/socket/websocket", "runner-1", "device-key-ref")
        val oversized = AppState(
            profiles = listOf(profile),
            selectedProfileId = profile.id,
            profileViews = mapOf(profile.id to ProfileViewState(workingDirectoryPath = "x".repeat(MobileStateCodec.MaxInputBytes))),
        )

        assertFailsWith<IllegalArgumentException> { MobileStateCodec.encode(oversized) }
    }

    private fun summary(id: String, updatedAtMs: Long = 1L) = ThreadSummary(
        id = id,
        name = "Name $id",
        preview = "Preview $id",
        workingDirectory = WorkingDirectory("/workspace/$id"),
        createdAtMs = updatedAtMs,
        updatedAtMs = updatedAtMs,
        status = ThreadStatus.Idle,
    )

    private fun <T> success(result: MobileStateDecodeResult<T>): T = when (result) {
        is MobileStateDecodeResult.Success -> result.value
        is MobileStateDecodeResult.Failure -> error("Expected decode success, got ${result.reason}")
    }
}

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
    fun app_state_round_trip_keeps_profiles_views_cache_and_unknown_extensions() {
        val profile = HostProfile(
            hostIdentity = "host-1",
            name = "Development Mac",
            addresses = listOf("192.0.2.1:49152", "[2001:db8::1]:49152"),
            deviceIdentityReference = "device-ref-1",
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
        val cache = MobileCache(
            profiles = mapOf(
                profile.hostIdentity to ProfileMobileCache(
                    threadList = listOf(summary),
                    snapshots = mapOf(summary.id to snapshot),
                    unknownEvents = listOf(unknownEvent),
                    rawMessages = listOf(rawMessage),
                ),
            ),
        )
        val state = AppState(
            profiles = listOf(profile),
            selectedProfileId = profile.hostIdentity,
            profileViews = mapOf(
                profile.hostIdentity to ProfileViewState(
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
            restored.profileViews.getValue(profile.hostIdentity),
        )
        assertEquals(false, restored.showingPairing)
        assertNull(restored.pairingError)
        assertEquals(unknownEvent, restored.cache.profile(profile.hostIdentity).unknownEvents.single())
        // Raw notifications/requests belong to the transport lifetime and
        // are intentionally not written to durable state.
        assertEquals(emptyList(), restored.cache.profile(profile.hostIdentity).rawMessages)
        assertEquals(snapshot, restored.cache.snapshot(profile.hostIdentity, summary.id))
    }

    @Test
    fun cache_codec_round_trip_applies_limits_and_drops_ephemeral_raw_messages() {
        val summaries = (1..3).map { summary("thread-$it", updatedAtMs = it.toLong()) }
        val cache = MobileCache(
            profiles = mapOf(
                "host-1" to ProfileMobileCache(
                    threadList = summaries,
                    snapshots = summaries.associate { it.id to ThreadSnapshot(it) },
                    rawMessages = listOf(
                        RawCodexMessage.Notification(
                            method = "future/notification",
                            params = JsonPrimitive("payload"),
                        ),
                    ),
                ),
            ),
        )

        val restored = success(
            MobileStateCodec.decodeCache(
                MobileStateCodec.encodeCache(cache),
                MobileCacheLimits(maxThreads = 1, maxItemsPerThread = 2, maxTextCharacters = 100, maxApproximateBytes = 16 * 1024),
            ),
        )

        assertEquals(listOf("thread-3"), restored.profile("host-1").threadList.map { it.id })
        assertEquals(setOf("thread-3"), restored.profile("host-1").snapshots.keys)
        assertEquals(emptyList(), restored.profile("host-1").rawMessages)
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
    fun encoded_state_cannot_exceed_the_decode_limit() {
        val profile = HostProfile("host-1", "Host", emptyList(), "device-ref")
        val oversized = AppState(
            profiles = listOf(profile),
            selectedProfileId = profile.hostIdentity,
            profileViews = mapOf(profile.hostIdentity to ProfileViewState(workingDirectoryPath = "x".repeat(MobileStateCodec.MaxInputBytes))),
        )

        assertFailsWith<IllegalArgumentException> { MobileStateCodec.encode(oversized) }
    }

    @Test
    fun legacy_android_state_is_migrated_with_array_snapshots_and_kotlin_enum_names() {
        val restored = success(MobileStateCodec.decode(legacyAndroidState.encodeToByteArray()))
        val profile = restored.profiles.single()
        val cached = restored.cache.profile(profile.hostIdentity)

        assertEquals("Android host", profile.name)
        assertEquals("device-ref-android", profile.deviceIdentityReference)
        assertEquals("/android/worktree", restored.selectedView.workingDirectoryPath)
        assertEquals(listOf("thread-android"), cached.threadList.map { it.id })
        assertEquals(TurnStatus.Completed, restored.cache.snapshot(profile.hostIdentity, "thread-android")!!.turns.single().status)
        assertEquals(
            "future/item",
            (restored.cache.snapshot(profile.hostIdentity, "thread-android")!!.turns.single().items.single() as CodexItem.Unknown).codexType,
        )
        assertEquals("vendor", cached.unknownEvents.single().extensions.keys.singleOrNull())
        assertEquals(emptyList(), cached.rawMessages)
    }

    @Test
    fun legacy_ios_state_is_migrated_with_map_snapshots_and_serial_names() {
        val restored = success(MobileStateCodec.decode(legacyIosState.encodeToByteArray()))
        val profile = restored.profiles.single()
        val cached = restored.cache.profile(profile.hostIdentity)
        val unknown = cached.unknownEvents.single()

        assertEquals("iOS host", profile.name)
        assertEquals("device-ref-ios", profile.deviceIdentityReference)
        assertEquals("/ios/worktree", restored.selectedView.workingDirectoryPath)
        assertEquals(listOf("thread-ios"), cached.threadList.map { it.id })
        assertEquals("future/event", unknown.method)
        assertEquals("kept", unknown.extensions["vendor"]?.toString()?.trim('"'))
        assertEquals("future/item", (restored.cache.snapshot(profile.hostIdentity, "thread-ios")!!.turns.single().items.single() as CodexItem.Unknown).codexType)
    }

    private val legacyAndroidState = """
        {
          "version": 1,
          "selectedProfileId": "host-android",
          "profiles": [
            {"hostIdentity":"host-android","name":"Android host","addresses":["192.0.2.10:1"],"deviceIdentityReference":"device-ref-android"},
            {"hostIdentity":"host-android","name":"duplicate","addresses":[],"deviceIdentityReference":"other-ref"}
          ],
          "views": {"host-android":{"workingDirectoryPath":"/android/worktree","selectedThreadId":"thread-android"}},
          "cache": {
            "host-android": {
              "threadList": [{"id":"thread-android","name":"Android","preview":"preview","workingDirectory":"/android/worktree","createdAtMs":1,"updatedAtMs":2,"status":{"type":"Idle"}}],
              "snapshots": [{"summary":{"id":"thread-android","name":"Android","preview":"preview","workingDirectory":"/android/worktree","createdAtMs":1,"updatedAtMs":2,"status":{"type":"Idle"}},"turns":[{"id":"turn-android","status":"Completed","items":[{"type":"future/item","id":"item-android","vendorField":true}]}]}],
              "unknownEvents": [{"threadId":"thread-android","turnId":"turn-android","method":"future/event","raw":{"payload":true},"extensions":{"vendor":"legacy"}}]
            }
          }
        }
    """.trimIndent()

    private val legacyIosState = """
        {
          "profiles": [{"hostIdentity":"host-ios","name":"iOS host","addresses":["192.0.2.11:1"],"deviceIdentityReference":"device-ref-ios"}],
          "selectedProfileId":"host-ios",
          "views":{"host-ios":{"workingDirectoryPath":"/ios/worktree","selectedThreadId":"thread-ios"}},
          "cache":{"host-ios":{"threadList":[{"id":"thread-ios","name":"iOS","preview":"preview","workingDirectory":{"path":"/ios/worktree"},"createdAtMs":1,"updatedAtMs":2,"status":{"type":"idle"}}],"snapshots":{"thread-ios":{"summary":{"id":"thread-ios","name":"iOS","preview":"preview","workingDirectory":{"path":"/ios/worktree"},"createdAtMs":1,"updatedAtMs":2,"status":{"type":"idle"}},"turns":[{"id":"turn-ios","status":"completed","items":[{"type":"unknown","id":"item-ios","codexType":"future/item","raw":{"kind":"future/item","newField":true}}]}]}},"unknownEvents":[{"type":"unknown","threadId":"thread-ios","turnId":"turn-ios","method":"future/event","raw":{"payload":true},"extensions":{"vendor":"kept"}}]}}
        }
    """.trimIndent()

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

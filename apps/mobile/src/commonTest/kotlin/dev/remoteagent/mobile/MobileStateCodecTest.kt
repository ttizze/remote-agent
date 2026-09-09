package dev.remoteagent.mobile

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertIs
import kotlin.test.assertNull
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.jsonObject

class MobileStateCodecTest {
    @Test
    fun a_large_reply_is_saved_once_without_losing_paging_metadata() {
        val body = "x".repeat(256 * 1024)
        val raw = largeReplyRaw(body)
        val snapshot = codexThreadSnapshot(raw)
        val profile = HostProfile("runner", "Fixture", "wss://fixture.invalid", "host", "key-reference")
        val state =
            AppState(
                profiles = listOf(profile),
                selectedProfileId = profile.id,
                cache =
                    MobileCache(
                        mapOf(
                            profile.id to
                                ProfileMobileCache(
                                    threadList = listOf(snapshot.summary),
                                    snapshots = mapOf(snapshot.summary.id to snapshot),
                                )
                        )
                    ),
            )
        val bytes = MobileStateCodec.encode(state)
        val restored =
            success(MobileStateCodec.decode(bytes)).cache.snapshot(profile.id, "thread-1")
                ?: error("A 256 KiB reply must fit without discarding its conversation")
        assertEquals(body, (restored.turns.single().items.single() as CodexItem.AgentMessage).text)
        assertEquals(JsonPrimitive("/fixture/rollout.jsonl"), restored.raw?.get("path"))
        assertEquals("older-turns", restored.olderTurnsCursor)
        assertEquals("older-items", restored.turns.single().olderItemsCursor)
        assertEquals(JsonArray(listOf(JsonPrimitive("tool-1"))), restored.turns.single().raw?.get("deferredItemIds"))
        assertEquals(true, bytes.size < body.length + 8_192)
    }

    @Test
    fun encoded_profile_excludes_secrets_and_retains_secure_store_reference() {
        val state =
            AppState(
                profiles =
                    listOf(
                        HostProfile(
                            runnerId = "runner-1",
                            name = "Host",
                            relayUrl = "wss://relay.example.test/socket/websocket",
                            hostIdentity = "pinned-host-key",
                            deviceIdentityReference = "device-key-ref",
                        )
                    )
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
        val profile = persistedProfile()
        val summary = summary("thread-1")
        val snapshot = snapshotWithTransientTurn(summary)
        val cache =
            MobileCache(
                profiles =
                    mapOf(
                        profile.id to
                            ProfileMobileCache(threadList = listOf(summary), snapshots = mapOf(summary.id to snapshot))
                    )
            )
        val state =
            AppState(
                profiles = listOf(profile),
                selectedProfileId = profile.id,
                profileViews =
                    mapOf(
                        profile.id to
                            ProfileViewState(
                                connection = ConnectionPhase.Connected,
                                workingDirectoryPath = "/workspace",
                                threadList = LoadPhase.Ready,
                                selectedThreadId = summary.id,
                                threadDetail = LoadPhase.Loading,
                                interruptingTurnId = "turn-1",
                                notice = "transient notice",
                                unreadCompletedThreadIds = setOf("unread-thread"),
                            )
                    ),
                cache = cache,
                showingPairing = true,
                pairingError = "transient pairing error",
                turnChoices = mapOf(profile.id to CodexTurnOptions("chosen-model", "high")),
            )

        val restored = success(MobileStateCodec.decode(MobileStateCodec.encode(state)))

        assertEquals(state.profiles, restored.profiles)
        assertEquals(state.selectedProfileId, restored.selectedProfileId)
        assertEquals(state.turnChoices, restored.turnChoices)
        assertEquals(
            ProfileViewState(
                connection = ConnectionPhase.Disconnected,
                workingDirectoryPath = "/workspace",
                threadList = LoadPhase.Idle,
                selectedThreadId = summary.id,
                threadDetail = LoadPhase.Idle,
                unreadCompletedThreadIds = setOf("unread-thread"),
            ),
            restored.profileViews.getValue(profile.id),
        )
        assertEquals(false, restored.showingPairing)
        assertNull(restored.pairingError)
        assertEquals(
            snapshot.copy(turns = snapshot.turns.map { it.copy(error = null, pendingRequests = emptyList()) }),
            restored.cache.snapshot(profile.id, summary.id),
        )
    }

    @Test
    fun restored_app_state_applies_cache_limits() {
        val summaries = (1..3).map { summary("thread-$it", updatedAtMs = it.toLong()) }
        val cache =
            MobileCache(
                profiles =
                    mapOf(
                        "host-1" to
                            ProfileMobileCache(
                                threadList = summaries,
                                snapshots = summaries.associate { it.id to ThreadSnapshot(it) },
                            )
                    )
            )

        val restored =
            success(
                MobileStateCodec.decode(
                    MobileStateCodec.encode(
                        AppState(
                            profiles =
                                listOf(HostProfile("runner", "Fixture", "wss://fixture.invalid", "host-1", "key")),
                            cache = cache,
                        )
                    ),
                    MobileCacheLimits(maxThreads = 1, maxTurnsPerThread = 10, maxApproximateBytes = 16 * 1024),
                )
            )

        assertEquals(
            listOf("thread-3", "thread-2", "thread-1"),
            restored.cache.profile("host-1").threadList.map { it.id },
        )
        assertEquals(setOf("thread-3"), restored.cache.profile("host-1").snapshots.keys)
    }

    @Test
    fun old_and_unknown_versions_are_rejected_without_migration() {
        val root = Json.parseToJsonElement(MobileStateCodec.encode(AppState()).decodeToString()).jsonObject
        for (version in listOf(1, 2, 999)) {
            val incompatible = buildJsonObject {
                root.forEach { (name, value) -> put(name, if (name == "version") JsonPrimitive(version) else value) }
            }
            val failure =
                assertIs<MobileStateDecodeResult.Failure>(
                    MobileStateCodec.decode(incompatible.toString().encodeToByteArray())
                )
            assertEquals(MobileStateDecodeReason.UnsupportedVersion, failure.reason)
            assertEquals(version, failure.version)
        }
    }

    @Test
    fun malformed_json_is_a_safe_structured_failure() {
        val failure =
            assertIs<MobileStateDecodeResult.Failure>(MobileStateCodec.decode("{not-json".encodeToByteArray()))

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
        val profile =
            HostProfile("runner-1", "Host", "wss://relay.example.test/socket/websocket", "runner-1", "device-key-ref")
        val summary = summary("thread-1")
        val state =
            AppState(
                profiles = listOf(profile),
                selectedProfileId = profile.id,
                profileViews =
                    mapOf(
                        profile.id to
                            ProfileViewState(selectedThreadId = summary.id, workingDirectoryPath = "/workspace")
                    ),
                cache =
                    MobileCache(
                        profiles =
                            mapOf(
                                profile.id to
                                    ProfileMobileCache(
                                        threadList = listOf(summary),
                                        snapshots =
                                            mapOf(
                                                summary.id to
                                                    ThreadSnapshot(
                                                        summary,
                                                        raw =
                                                            buildJsonObject {
                                                                put(
                                                                    "large",
                                                                    JsonPrimitive(
                                                                        "x".repeat(MobileStateCodec.MaxInputBytes)
                                                                    ),
                                                                )
                                                            },
                                                    )
                                            ),
                                    )
                            )
                    ),
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
        val profile =
            HostProfile("runner-1", "Host", "wss://relay.example.test/socket/websocket", "runner-1", "device-key-ref")
        val oversized =
            AppState(
                profiles = listOf(profile),
                selectedProfileId = profile.id,
                profileViews =
                    mapOf(
                        profile.id to
                            ProfileViewState(workingDirectoryPath = "x".repeat(MobileStateCodec.MaxInputBytes))
                    ),
            )

        assertFailsWith<IllegalArgumentException> { MobileStateCodec.encode(oversized) }
    }

    private fun largeReplyRaw(body: String): kotlinx.serialization.json.JsonObject {
        return buildJsonObject {
            put("id", JsonPrimitive("thread-1"))
            put("cwd", JsonPrimitive("/fixture"))
            put("path", JsonPrimitive("/fixture/rollout.jsonl"))
            put("historyCursor", JsonPrimitive("older-turns"))
            put(
                "turns",
                JsonArray(
                    listOf(
                        buildJsonObject {
                            put("id", JsonPrimitive("turn-1"))
                            put("status", JsonPrimitive("completed"))
                            put("itemsNextCursor", JsonPrimitive("older-items"))
                            put("deferredItemIds", JsonArray(listOf(JsonPrimitive("tool-1"))))
                            put(
                                "items",
                                JsonArray(
                                    listOf(
                                        buildJsonObject {
                                            put("id", JsonPrimitive("answer"))
                                            put("type", JsonPrimitive("agentMessage"))
                                            put("text", JsonPrimitive(body))
                                        }
                                    )
                                ),
                            )
                        }
                    )
                ),
            )
        }
    }

    private fun snapshotWithTransientTurn(summary: ThreadSummary): ThreadSnapshot {
        return ThreadSnapshot(
            summary = summary,
            turns =
                listOf(
                    CodexTurn(
                        id = "turn-1",
                        status = TurnStatus.InProgress,
                        items = listOf(CodexItem.AgentMessage("item-1", "hello")),
                        raw = buildJsonObject { put("turnExtension", JsonPrimitive("kept")) },
                        error = CodexTurnError("reconnecting", willRetry = true),
                        pendingRequests =
                            listOf(
                                CodexServerRequest(
                                    id = "request-1",
                                    method = "item/tool/requestUserInput",
                                    params = buildJsonObject { put("question", JsonPrimitive("Continue?")) },
                                )
                            ),
                    )
                ),
            raw = buildJsonObject { put("snapshotExtension", JsonPrimitive(true)) },
        )
    }

    private fun summary(id: String, updatedAtMs: Long = 1L) =
        ThreadSummary(
            id = id,
            name = "Name $id",
            preview = "Preview $id",
            workingDirectory = WorkingDirectory("/workspace/$id"),
            createdAtMs = updatedAtMs,
            updatedAtMs = updatedAtMs,
            status = ThreadStatus.Idle,
        )

    private fun <T> success(result: MobileStateDecodeResult<T>): T =
        when (result) {
            is MobileStateDecodeResult.Success -> result.value
            is MobileStateDecodeResult.Failure -> error("Expected decode success, got ${result.reason}")
        }

    private fun persistedProfile(): HostProfile =
        HostProfile(
            runnerId = "runner-1",
            name = "Development Mac",
            relayUrl = "wss://relay.example.test/socket/websocket",
            hostIdentity = "pinned-host-key",
            deviceIdentityReference = "device-key-ref",
        )
}

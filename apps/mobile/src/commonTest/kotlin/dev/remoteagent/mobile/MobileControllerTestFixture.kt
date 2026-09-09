package dev.remoteagent.mobile

import kotlin.coroutines.Continuation
import kotlin.coroutines.CoroutineContext
import kotlin.coroutines.EmptyCoroutineContext
import kotlin.coroutines.startCoroutine
import kotlin.test.AfterTest
import kotlinx.atomicfu.AtomicRef
import kotlinx.coroutines.CoroutineDispatcher
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Runnable
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.encodeToJsonElement
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.put

internal abstract class MobileControllerTestFixture {
    protected val persistenceScopes = mutableListOf<CoroutineScope>()

    protected fun persistenceScope(): CoroutineScope =
        CoroutineScope(SupervisorJob() + Dispatchers.Unconfined).also(persistenceScopes::add)

    @AfterTest
    fun cancelPersistenceWorkers() {
        persistenceScopes.forEach(CoroutineScope::cancel)
    }

    protected val profile =
        HostProfile("runner-1", "Host", "wss://relay.example.test/socket/websocket", "runner-1", "device-key-ref")
    protected val thread = snapshot("thread-1")

    protected fun emitExternalConversation(gateway: FakeHostGateway) {
        gateway.emit(
            notification(
                "turn/started",
                """
            {"threadId":"thread-1","turn":{"id":"turn-external","status":"inProgress"}}
        """,
            )
        )
        gateway.emit(
            notification(
                "item/started",
                """
            {"threadId":"thread-1","turnId":"turn-external",
             "item":{"id":"user-external","type":"userMessage","content":[{"type":"text","text":"from another phone"}]}}
        """,
            )
        )
        gateway.emit(
            notification(
                "item/started",
                """
            {"threadId":"thread-1","turnId":"turn-external",
             "item":{"id":"agent-external","type":"agentMessage","text":""}}
        """,
            )
        )
        gateway.emit(
            notification(
                "item/agentMessage/delta",
                """
            {"threadId":"thread-1","turnId":"turn-external",
             "itemId":"agent-external","delta":"live reply"}
        """,
            )
        )
        gateway.emit(
            notification(
                "turn/completed",
                """
            {"threadId":"thread-1","turn":{"id":"turn-external","status":"completed"}}
        """,
            )
        )
    }

    protected fun replaceWatchedAnswer(gateway: FakeHostGateway, initial: ThreadSnapshot, revision: Long) {
        val refreshed =
            initial.copy(
                turns =
                    listOf(
                        initial.turns
                            .single()
                            .copy(items = listOf(CodexItem.AgentMessage("item-1", "Persisted external answer")))
                    )
            )
        gateway.readResult = GatewayResult.Success(ThreadReadResult(refreshed, emptyList()))
        repeat(3) {
            gateway.emit(
                notification(
                    "host/thread/changed",
                    """
                {"watchKey":1,"watchId":$revision,"threadId":"thread-1"}
            """,
                )
            )
        }
    }

    protected fun controller(
        gateway: FakeHostGateway,
        selectedThreadId: String? = null,
        workingDirectory: String = "/workspace",
        cachedThread: ThreadSnapshot? = thread,
        cacheLimits: MobileCacheLimits = MobileCacheLimits(),
    ): AtomicRef<MobileApp> {
        val initialCache =
            cachedThread?.let {
                reconcileThreadRead(MobileCache(), profile.id, ThreadReadResult(it, emptyList()), cacheLimits)
            } ?: MobileCache()
        return mobileApp(
            gateway = gateway,
            repository =
                InMemoryMobileRepository(
                    AppState(
                        profiles = listOf(profile),
                        selectedProfileId = profile.id,
                        profileViews =
                            mapOf(
                                profile.id to
                                    ProfileViewState(
                                        workingDirectoryPath = workingDirectory,
                                        selectedThreadId = selectedThreadId,
                                    )
                            ),
                        cache = initialCache,
                    )
                ),
            persistenceScope = persistenceScope(),
            cacheLimits = cacheLimits,
        )
    }

    protected fun snapshot(id: String): ThreadSnapshot =
        ThreadSnapshot(
            summary =
                ThreadSummary(
                    id = id,
                    preview = "preview",
                    workingDirectory = WorkingDirectory("/workspace"),
                    createdAtMs = 1,
                    updatedAtMs = 1,
                    status = ThreadStatus.Idle,
                ),
            turns =
                listOf(
                    CodexTurn(
                        id = "turn-1",
                        status = TurnStatus.InProgress,
                        items = listOf(CodexItem.AgentMessage("item-1", "old")),
                    )
                ),
        )

    protected class FakeHostGateway : HostGateway {

        var discoveryResult: GatewayResult<List<String>> = GatewayResult.Success(emptyList())
        var connectResult: GatewayResult<Unit> = GatewayResult.Success(Unit)
        var connectBlock: (suspend () -> GatewayResult<Unit>)? = null
        var listResult: GatewayResult<List<ThreadSummary>> = GatewayResult.Success(emptyList())
        var projectResult: GatewayResult<List<CodexProject>> = GatewayResult.Success(emptyList())
        var readResult: GatewayResult<ThreadReadResult> = GatewayResult.Failure("not configured")
        var startResult: GatewayResult<ThreadSnapshot> = GatewayResult.Failure("not configured")
        var turnResult: GatewayResult<String> = GatewayResult.Success("turn-1")
        var steerResult: GatewayResult<Unit> = GatewayResult.Success(Unit)
        var queueResult: GatewayResult<String> = GatewayResult.Success("queue-1")
        var interruptResult: GatewayResult<Unit> = GatewayResult.Success(Unit)
        var disconnectResult: GatewayResult<Unit> = GatewayResult.Success(Unit)
        var disconnectBlock: (suspend () -> GatewayResult<Unit>)? = null
        var agentBlock: (suspend (AgentCommand) -> GatewayResult<String>?)? = null
        var readHook: (() -> Unit)? = null
        var callback: ((RawCodexMessage) -> Unit)? = null
        var closeCallback: ((String) -> Unit)? = null
        var listQueries = mutableListOf<ThreadListQuery>()
        var readIds = mutableListOf<String>()
        var startCalls = 0
        var startCwds = mutableListOf<String>()
        var turnResumes = mutableListOf<Boolean>()
        var turnCwds = mutableListOf<String>()
        var turnTexts = mutableListOf<String>()
        val submittedClientIds = mutableListOf<String>()
        var steerHook: (() -> Unit)? = null
        var steerTurnIds = mutableListOf<String>()
        var steerTexts = mutableListOf<String>()
        var queueThreadIds = mutableListOf<String>()
        var queueTexts = mutableListOf<String>()
        var connectHook: (() -> Unit)? = null
        var listHook: (() -> Unit)? = null
        var turnHook: (() -> Unit)? = null
        var interruptHook: (() -> Unit)? = null
        var subscriptionCancelCount = 0
        var disconnectCalls = 0
        var subscriptionWasRetiredAtDisconnect = false
        var connectInFlight = false
        var disconnectObservedConnectInFlight = false
        val connectedProfiles = mutableListOf<HostProfile>()

        override suspend fun transfer(profile: HostProfile, params: JsonElement): GatewayResult<JsonElement> =
            GatewayResult.Failure("Transfer is not exercised by this fixture")

        override suspend fun pair(payload: PairingQrPayload): GatewayResult<HostProfile> =
            GatewayResult.Failure("unused")

        override suspend fun discover(profile: HostProfile): GatewayResult<List<String>> =
            when (val result = discoveryResult) {
                is GatewayResult.Success -> GatewayResult.Success(result.value.ifEmpty { listOf(profile.relayUrl) })
                is GatewayResult.Failure -> result
            }

        override suspend fun connect(profile: HostProfile): GatewayResult<Unit> {
            connectedProfiles += profile
            connectHook?.invoke()
            return connectBlock?.invoke() ?: connectResult
        }

        override suspend fun disconnect(profile: HostProfile): GatewayResult<Unit> {
            disconnectCalls += 1
            disconnectObservedConnectInFlight = disconnectObservedConnectInFlight || connectInFlight
            subscriptionWasRetiredAtDisconnect = callback == null
            return disconnectBlock?.invoke() ?: disconnectResult
        }

        var listBlock: (suspend (ThreadListQuery) -> GatewayResult<ThreadListPage>)? = null

        override suspend fun agentCommand(profile: HostProfile, command: AgentCommand): GatewayResult<String> {
            agentBlock?.invoke(command)?.let {
                return it
            }
            val result: GatewayResult<JsonElement> =
                when (command) {
                    AgentCommand.Models -> GatewayResult.Success(JsonArray(emptyList()))
                    is AgentCommand.ListThreads -> {
                        listQueries += command.query
                        listHook?.invoke()
                        val page =
                            listBlock?.invoke(command.query)
                                ?: when (val projects = projectResult) {
                                    is GatewayResult.Success ->
                                        listResult.mapGateway { ThreadListPage(it, projects.value) }
                                    is GatewayResult.Failure -> projects
                                }
                        page.mapGateway { it.fixtureJson() }
                    }
                    is AgentCommand.ReadThread -> {
                        readIds += command.threadId
                        readHook?.invoke()
                        readResult.mapGateway { it.thread.fixtureResponse() }
                    }
                    is AgentCommand.StartThread -> {
                        startCalls += 1
                        startCwds += command.cwd
                        startResult.mapGateway { it.fixtureResponse() }
                    }
                    is AgentCommand.SendTurn -> send(command)
                    is AgentCommand.InterruptTurn -> {
                        interruptHook?.invoke()
                        interruptResult.mapGateway { JsonNull }
                    }
                    else -> error("No response configured for native agent intent: $command")
                }
            return result.mapGateway(JsonElement::toString)
        }

        private fun send(command: AgentCommand.SendTurn): GatewayResult<JsonElement> {
            // Only transport effects are controlled here; use the real shared routing policy.
            val plan =
                Json.parseToJsonElement(
                        nativeConversationPresentation(
                            buildJsonObject {
                                put("operation", "sendPlan")
                                put("snapshot", command.snapshot)
                                put("listed", command.listed)
                            }
                                .toString()
                        )
                    )
                    .jsonObject
            submittedClientIds += command.input.clientUserMessageId
            return when (plan.string("action")) {
                "steer" -> {
                    val turnId = plan.string("turnId")!!
                    steerHook?.invoke()
                    steerTurnIds += turnId
                    steerTexts += command.input.text
                    steerResult.mapGateway { JsonPrimitive(turnId) }
                }
                "queue" -> {
                    queueThreadIds += command.threadId
                    queueTexts += command.input.text
                    queueResult.mapGateway { JsonNull }
                }
                "start" -> {
                    turnResumes += plan.boolean("resume")!!
                    turnCwds += plan.string("cwd")!!
                    turnTexts += command.input.text
                    turnHook?.invoke()
                    turnResult.mapGateway(::JsonPrimitive)
                }
                else -> GatewayResult.Failure(plan.string("message")!!)
            }
        }

        override fun subscribeRaw(
            profile: HostProfile,
            onMessage: (RawCodexMessage) -> Unit,
            onClosed: (String) -> Unit,
        ): HostEventSubscription {
            closeCallback = onClosed
            callback = onMessage
            return HostEventSubscription {
                subscriptionCancelCount += 1
                if (callback === onMessage) callback = null
            }
        }

        fun emit(message: RawCodexMessage) {
            callback?.invoke(message)
        }

        fun closeStream(message: String) {
            closeCallback?.invoke(message)
        }
    }

    protected class FailingOnceMobileRepository(private val initialState: AppState) : MobileRepository {
        var saveCalls = 0
        var savedState: AppState? = null

        override fun load(): AppState = initialState

        override fun save(state: AppState) {
            saveCalls += 1
            if (saveCalls == 1) error("sanitized test failure")
            savedState = state
        }
    }

    protected class QueuedDispatcher : CoroutineDispatcher() {
        private val queued = ArrayDeque<Runnable>()

        override fun dispatch(context: CoroutineContext, block: Runnable) {
            queued.addLast(block)
        }

        fun runAll() {
            while (queued.isNotEmpty()) queued.removeFirst().run()
        }
    }

    protected fun summary(id: String, cwd: String = "/workspace") =
        ThreadSummary(
            id = id,
            preview = "preview",
            workingDirectory = WorkingDirectory(cwd),
            createdAtMs = 1,
            updatedAtMs = 1,
            status = ThreadStatus.Idle,
        )

    protected fun runSuspend(block: suspend () -> Unit) {
        var completion: Result<Unit>? = null
        block.startCoroutine(
            object : Continuation<Unit> {
                override val context = EmptyCoroutineContext

                override fun resumeWith(result: Result<Unit>) {
                    completion = result
                }
            }
        )
        (completion ?: error("test coroutine suspended unexpectedly")).getOrThrow()
    }
}

internal fun notification(method: String, params: String): RawCodexMessage.Notification =
    RawCodexMessage.Notification(method, Json.parseToJsonElement(params))

internal class InMemoryMobileRepository(initial: AppState = AppState()) : MobileRepository {
    private var state = initial

    override fun load(): AppState = state

    override fun save(state: AppState) {
        this.state = state
    }
}

private fun ThreadSummary.fixtureJson(): JsonObject = buildJsonObject {
    raw?.forEach { (key, value) -> put(key, value) }
    put("id", id)
    put("name", name)
    put("preview", preview)
    put("cwd", workingDirectory.path)
    put("projectId", projectId)
    put("createdAt", createdAtMs)
    put("updatedAt", updatedAtMs)
    put("status", Json.encodeToJsonElement(ThreadStatus.serializer(), status))
}

private fun ThreadSnapshot.fixtureResponse(): JsonObject = buildJsonObject {
    put(
        "thread",
        buildJsonObject {
            summary.fixtureJson().forEach { (key, value) -> put(key, value) }
            raw?.forEach { (key, value) -> if (key !in setOf("id", "cwd", "status")) put(key, value) }
            put("turns", JsonArray(turns.map(CodexTurn::fixtureJson)))
        },
    )
}

internal fun CodexTurn.fixtureJson(): JsonObject = buildJsonObject {
    raw?.forEach { (key, value) -> put(key, value) }
    put("id", id)
    put("status", Json.encodeToJsonElement(TurnStatus.serializer(), status))
    startedAtMs?.let { put("startedAt", it / 1000) }
    completedAtMs?.let { put("completedAt", it / 1000) }
    durationMs?.let { put("durationMs", it) }
    error?.let { put("error", Json.encodeToJsonElement(CodexTurnError.serializer(), it)) }
    put("items", JsonArray(items.map(CodexItem::fixtureJson)))
}

internal fun CodexItem.fixtureJson(): JsonObject =
    when (val item = this) {
        is CodexItem.Unknown -> item.raw
        is CodexItem.CommandExecution ->
            JsonObject(
                Json.encodeToJsonElement<CodexItem>(item).jsonObject +
                    ("aggregatedOutput" to JsonPrimitive(item.output))
            )
        is CodexItem.UserMessage ->
            buildJsonObject {
                put("id", item.id)
                put("type", "userMessage")
                put("clientId", item.clientId)
                put(
                    "content",
                    JsonArray(
                        listOf(
                            buildJsonObject {
                                put("type", "text")
                                put("text", item.text)
                            }
                        ) +
                            item.imageSources.map {
                                buildJsonObject {
                                    put("type", "localImage")
                                    put("path", it)
                                }
                            }
                    ),
                )
            }
        is CodexItem.FileChange ->
            buildJsonObject {
                put("id", item.id)
                put("type", "fileChange")
                put("status", Json.encodeToJsonElement(FileChangeStatus.serializer(), item.status))
                put(
                    "changes",
                    JsonArray(
                        item.changes.map { change ->
                            buildJsonObject {
                                put("path", change.path)
                                put("diff", change.diff)
                                put(
                                    "kind",
                                    buildJsonObject {
                                        put("type", Json.encodeToJsonElement(FileUpdateKind.serializer(), change.kind))
                                    },
                                )
                            }
                        }
                    ),
                )
            }
        else -> Json.encodeToJsonElement<CodexItem>(item).jsonObject
    }

private fun ThreadListPage.fixtureJson(): JsonObject = buildJsonObject {
    put("data", JsonArray(threads.map { it.fixtureJson() }))
    put(
        "projects",
        JsonArray(
            projects.map { project ->
                buildJsonObject {
                    project.raw?.forEach { (key, value) -> put(key, value) }
                    put("id", project.id)
                    put("name", project.name)
                    put("roots", JsonArray(project.roots.map { buildJsonObject { put("path", it.path) } }))
                    put("position", project.position)
                    put("createdAt", project.createdAtMs)
                    put("updatedAt", project.updatedAtMs)
                }
            }
        ),
    )
    put("moreProjectIds", JsonArray(moreProjectIds.map(::JsonPrimitive)))
    put("hasMoreChats", hasMoreChats)
    put("hasMoreProjects", hasMoreProjects)
}

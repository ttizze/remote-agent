package dev.remoteagent.mobile

import kotlin.coroutines.Continuation
import kotlin.coroutines.CoroutineContext
import kotlin.coroutines.EmptyCoroutineContext
import kotlin.coroutines.startCoroutine
import kotlin.test.AfterTest
import kotlinx.coroutines.CoroutineDispatcher
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Runnable
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject

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
                {"watchId":$revision,"threadId":"thread-1"}
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
    ): MobileController {
        val initialCache =
            cachedThread?.let {
                reconcileThreadRead(MobileCache(), profile.id, ThreadReadResult(it, emptyList()), cacheLimits)
            } ?: MobileCache()
        return MobileController(
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

    protected fun notification(method: String, params: String): RawCodexMessage.Notification =
        RawCodexMessage.Notification(method, Json.parseToJsonElement(params))

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

    protected class FakeHostGateway : HostGateway, CodexGateway {
        override val codex: CodexGateway
            get() = this

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
        var agentBlock: (suspend (AgentCommand) -> GatewayResult<String>)? = null
        var rawBlock: (suspend (String, JsonElement) -> GatewayResult<JsonElement>)? = null
        var rawHook: ((String, JsonElement) -> Unit)? = null
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

        override suspend fun listThreads(profile: HostProfile, query: ThreadListQuery): GatewayResult<ThreadListPage> {
            listQueries += query
            listHook?.invoke()
            listBlock?.let {
                return it(query)
            }
            return when (val result = projectResult) {
                is GatewayResult.Success -> listResult.mapGateway { ThreadListPage(it, result.value) }
                is GatewayResult.Failure -> result
            }
        }

        override suspend fun readThread(profile: HostProfile, threadId: String): GatewayResult<ThreadReadResult> {
            readIds += threadId
            readHook?.invoke()
            return readResult
        }

        override suspend fun startThread(
            profile: HostProfile,
            cwd: String,
            options: CodexTurnOptions,
        ): GatewayResult<ThreadSnapshot> {
            startCalls += 1
            startCwds += cwd
            return startResult
        }

        override suspend fun startTurn(
            profile: HostProfile,
            threadId: String,
            cwd: String,
            input: CodexTurnInput,
            resume: Boolean,
            options: CodexTurnOptions,
        ): GatewayResult<String> {
            submittedClientIds += input.clientUserMessageId
            turnResumes += resume
            turnCwds += cwd
            turnTexts += input.text
            turnHook?.invoke()
            return turnResult
        }

        override suspend fun steerTurn(
            profile: HostProfile,
            threadId: String,
            turnId: String,
            input: CodexTurnInput,
        ): GatewayResult<Unit> {
            submittedClientIds += input.clientUserMessageId
            steerHook?.invoke()
            steerTurnIds += turnId
            steerTexts += input.text
            return steerResult
        }

        override suspend fun queueTurn(
            profile: HostProfile,
            threadId: String,
            input: CodexTurnInput,
        ): GatewayResult<String> {
            submittedClientIds += input.clientUserMessageId
            queueThreadIds += threadId
            queueTexts += input.text
            return queueResult
        }

        override suspend fun interrupt(profile: HostProfile, threadId: String, turnId: String): GatewayResult<Unit> {
            interruptHook?.invoke()
            return interruptResult
        }

        override suspend fun agentCommand(profile: HostProfile, command: AgentCommand): GatewayResult<String> =
            requireNotNull(agentBlock) { "No response configured for native agent intent" }(command)

        override suspend fun rawRequest(
            profile: HostProfile,
            method: String,
            params: JsonElement,
        ): GatewayResult<JsonElement> {
            rawBlock?.let {
                return it(method, params)
            }
            rawHook?.invoke(method, params)
            return GatewayResult.Success(JsonObject(emptyMap()))
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

        override suspend fun respondResult(
            profile: HostProfile,
            requestId: JsonElement,
            result: JsonElement,
        ): GatewayResult<Unit> = GatewayResult.Success(Unit)

        override suspend fun respondError(
            profile: HostProfile,
            requestId: JsonElement,
            error: JsonElement,
        ): GatewayResult<Unit> = GatewayResult.Success(Unit)

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

internal class InMemoryMobileRepository(initial: AppState = AppState()) : MobileRepository {
    private var state = initial

    override fun load(): AppState = state

    override fun save(state: AppState) {
        this.state = state
    }
}

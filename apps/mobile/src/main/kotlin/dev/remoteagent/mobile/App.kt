package dev.remoteagent.mobile

import android.content.Context
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.lifecycle.ViewModel
import dev.remoteagent.core.AgentStore
import dev.remoteagent.core.BrowserFrame
import dev.remoteagent.core.BrowserRequest
import dev.remoteagent.core.Connection
import dev.remoteagent.core.DictationPreparation
import dev.remoteagent.core.Intent
import dev.remoteagent.core.Invitation
import dev.remoteagent.core.Outcome
import dev.remoteagent.core.Snapshot
import dev.remoteagent.core.generateIdentity
import dev.remoteagent.core.parseInvitation
import dev.remoteagent.core.validateInvitation
import java.io.File
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.joinAll
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

internal data class EnvironmentActivityRow(
    val threadId: String,
    val title: String,
    val headline: String,
    val detail: String?,
    val phase: String,
)

internal data class EnvironmentRow(
    val profileId: String,
    val label: String,
    val state: String,
    val platform: String?,
    val machine: String?,
    val capabilities: List<String>,
    val reconnectReason: String?,
    val activities: List<EnvironmentActivityRow>,
)

private const val PERSISTENCE_QUEUE_CAPACITY = 8
private const val MILLIS_PER_SECOND = 1000L
private const val PERSISTENCE_DEBOUNCE_MILLIS = 250L

/** Screens of the native stack. Terminals exist only under a thread. */
internal sealed interface Route {
    data object Hosts : Route

    data object Pairing : Route

    data object Home : Route

    data class Thread(val id: String) : Route

    /** "Choose project" before a new task's draft, or to change the draft's project. */
    data object ChooseProject : Route

    data object AddProject : Route

    data object AddProjectLocal : Route

    data object NewTask : Route

    data class Terminal(
        val threadId: String,
        val terminalId: String,
        val project: String? = null,
        val cwd: String? = null,
    ) : Route

    /** `file` opens that file of the Files tab. */
    data class Workspace(val tab: WorkspaceTab, val file: String? = null, val line: ULong? = null) : Route

    data class Settings(val projectId: String? = null) : Route

    data object Archived : Route
}

internal enum class WorkspaceTab {
    Files,
    Diff,
    Browser,
}

// One owner coordinates native lifecycle, receipts, navigation and persistence.
@Suppress("TooManyFunctions", "TooGenericExceptionCaught", "ReturnCount", "LongMethod")
internal class AndroidAppModel(private val context: Context) : ViewModel() {
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)
    private val repository = AndroidMobileRepository(context)
    var snapshot by mutableStateOf(Snapshot.empty())
        private set

    private val draftEdits = DraftRevision()
    private var composerKey = ""

    var profiles by mutableStateOf(emptyList<HostProfile>())
        private set

    var environments by mutableStateOf(emptyList<EnvironmentRow>())
        private set

    var profileId by mutableStateOf<String?>(null)
        private set

    var stack by mutableStateOf(listOf<Route>(Route.Hosts))
        private set

    val route: Route
        get() = stack.last()

    var busy by mutableStateOf(false)
        private set

    var notice by mutableStateOf<String?>(null)
    var invitation by mutableStateOf<Invitation?>(null)
        private set

    var composerText by mutableStateOf("")
        private set

    private var followingFrom: String? = null
    private var owner: AgentStore? = null
    private val backgroundOwners = mutableMapOf<String, AgentStore>()
    private val backgroundJobs = mutableMapOf<String, Job>()
    private var initialization: Job? = null
    private var connection: Job? = null
    private var observation: Job? = null
    private var persistence: Job? = null
    private val pending = ArrayDeque<Pair<Intent, (Result<Outcome>) -> Unit>>()
    private val operations = mutableSetOf<Job>()
    private val writes = Channel<Snapshot>(PERSISTENCE_QUEUE_CAPACITY)
    private val writer =
        scope.launch(Dispatchers.IO) {
            for (current in writes) {
                runCatching { repository.saveModelPreferences(current.serializeModelPreferences()) }
                    .onFailure { error -> withContext(Dispatchers.Main) { notice = error.message } }
            }
        }

    init {
        runCatching { profiles = repository.profiles() }.onFailure { notice = it.message }
        if (profiles.isEmpty()) stack = listOf(Route.Pairing)
        repository.selected?.takeIf { id -> profiles.any { it.id == id } }?.let(::selectProfile)
            ?: startBackgroundProfiles(null)
    }

    fun perform(intent: Intent, complete: (Result<Outcome>) -> Unit = {}) {
        val store = owner
        if (store == null) {
            if (initialization != null) pending.addLast(intent to complete)
            else complete(Result.failure(IllegalStateException("Host not connected")))
            return
        }
        val receipt =
            runCatching { store.dispatch(intent) }
                .getOrElse {
                    complete(Result.failure(it))
                    return
                }
        val host = profileId
        lateinit var operation: Job
        operation =
            scope.launch(start = CoroutineStart.LAZY) {
                try {
                    val result = runCatching { receipt.wait() }
                    if (host == profileId && owner === store) {
                        publish(store.snapshot())
                        result.exceptionOrNull()?.let { notice = it.message }
                        result.getOrNull()?.let(::follow)
                        complete(result)
                    } else complete(Result.failure(CancellationException("Host changed")))
                } finally {
                    operations.remove(operation)
                }
            }
        operations.add(operation)
        operation.start()
    }

    private fun follow(outcome: Outcome) {
        if (outcome is Outcome.StartedThread && route == Route.NewTask) {
            stack = stack.dropLast(1) + Route.Thread(outcome.id)
            perform(Intent.OpenThread(outcome.id))
        }
    }

    /** Cancels the setup and restarts the first message locally; the screen follows the new thread. */
    fun workLocally() {
        val leaving = (route as? Route.Thread)?.id ?: return
        followingFrom = leaving
        perform(Intent.WorkLocally) { if (it.isFailure) followingFrom = null }
    }

    /** "New thread on <branch>": core opens the draft on that branch, then the new-task screen shows it. */
    fun newThreadOnBranch(projectId: String, branch: String, worktreePath: String?) {
        draftEdits.reset()
        perform(Intent.NewThreadOnBranch(projectId, branch, worktreePath))
        stack = stack + Route.NewTask
    }

    fun editDraft(text: String) {
        composerText = text
        val (revision, base) = draftEdits.edit(text)
        perform(Intent.EditDraft(text, base)) {
            if (draftEdits.acknowledge(revision)) {
                composerText = snapshot.draft().text
                draftEdits.base = composerText
            }
        }
    }

    fun navigate(next: Route, selection: Intent? = null) {
        when (next) {
            is Route.Thread -> {
                draftEdits.reset()
                perform(selection ?: Intent.OpenThread(next.id))
            }
            Route.NewTask -> {
                draftEdits.reset()
                perform(selection ?: Intent.NewThread(snapshot.selectedProjectId()))
            }
            Route.Archived -> perform(Intent.ShowArchived(true))
            else -> Unit
        }
        stack = stack + next
    }

    /**
     * A project chosen on "Choose project" (`null` is "No project"): the draft beneath takes it, otherwise the new
     * task's draft opens on it.
     */
    fun chooseProject(projectId: String?) {
        draftEdits.reset()
        perform(Intent.NewThread(projectId))
        val below = stack.getOrNull(stack.size - 2)
        stack = if (below == Route.NewTask) stack.dropLast(1) else stack + Route.NewTask
    }

    /** A project added (or found) from "Add project": the new task's draft opens on it in place of the flow. */
    fun projectAdded(projectId: String) {
        val flow = setOf(Route.ChooseProject, Route.AddProject, Route.AddProjectLocal, Route.NewTask)
        draftEdits.reset()
        perform(Intent.NewThread(projectId))
        stack = stack.takeWhile { it !in flow } + Route.NewTask
    }

    /** Replaces the open thread, as a thread link inside a thread does. */
    fun openThread(id: String) {
        if (route is Route.Thread) stack = stack.dropLast(1)
        navigate(Route.Thread(id))
    }

    fun back() {
        val leaving = stack.lastOrNull() ?: return
        if (stack.size <= 1) return
        stack = stack.dropLast(1)
        when (leaving) {
            is Route.Thread,
            Route.NewTask -> {
                draftEdits.reset()
                when (val current = route) {
                    is Route.Thread -> perform(Intent.OpenThread(current.id))
                    else -> perform(Intent.LeaveThread)
                }
            }
            Route.Archived -> perform(Intent.ShowArchived(false))
            Route.Hosts -> Unit
            else -> Unit
        }
    }

    fun showHosts() {
        persist()
        stack = listOf(Route.Hosts)
    }

    fun selectProfile(id: String) {
        if (profiles.none { it.id == id }) return
        stack = listOf(Route.Home)
        if (profileId == id && owner != null) {
            startBackgroundProfiles(id)
            connect()
            return
        }
        val old = detach()
        val background = backgroundOwners.remove(id)
        backgroundJobs.remove(id)?.cancel()
        profileId = id
        repository.selected = id
        publish(Snapshot.empty())
        scope.launch { runCatching { old?.shutdown(); background?.shutdown() } }
        initialization = scope.launch {
            try {
                val store =
                    AgentStore.offline(
                        repository.stateFile(id),
                        repository.modelPreferences(),
                        repository.cacheDirectory(id),
                        repository.diagnosticsDirectory(id),
                    )
                if (profileId != id || !isActive) {
                    store.shutdown()
                    return@launch
                }
                owner = store
                initialization = null
                publish(store.snapshot())
                while (pending.isNotEmpty()) {
                    val (intent, complete) = pending.removeFirst()
                    perform(intent, complete)
                }
                observe(store, id)
                connect()
                startBackgroundProfiles(id)
            } catch (error: CancellationException) {
                throw error
            } catch (error: Exception) {
                if (profileId == id) {
                    initialization = null
                    notice = error.message
                    while (pending.isNotEmpty()) pending.removeFirst().second(Result.failure(error))
                }
            }
        }
    }

    private fun detach(): AgentStore? {
        persist()
        draftEdits.reset()
        initialization?.cancel()
        observation?.cancel()
        connection?.cancel()
        initialization = null
        busy = false
        while (pending.isNotEmpty()) pending.removeFirst().second(Result.failure(CancellationException()))
        val old = owner
        owner = null
        return old
    }

    /** Starts one independent cached Store and retry loop for every saved Host. */
    private fun startBackgroundProfiles(selected: String?) {
        profiles
            .filterNot { it.id == selected }
            .filterNot { backgroundJobs.containsKey(it.id) }
            .forEach { profile -> superviseBackground(profile) }
    }

    private fun superviseBackground(profile: HostProfile) {
        lateinit var job: Job
        job = scope.launch {
            var delayMillis = 250L
            try {
                while (isActive && profiles.any { it.id == profile.id } && profile.id != profileId) {
                    try {
                        val store =
                            backgroundOwners.getOrPut(profile.id) {
                                AgentStore.offline(
                                    repository.stateFile(profile.id),
                                    repository.modelPreferences(),
                                    repository.cacheDirectory(profile.id),
                                    repository.diagnosticsDirectory(profile.id),
                                )
                            }
                        val identity =
                            withContext(Dispatchers.IO) {
                                AndroidCredentialStore(context, profile.id).loadOrCreate(::generateIdentity)
                            }
                        publishEnvironment(profile, store.snapshot())
                        try {
                            store.resume(Connection(profile.ticket, identity, null, true))
                        } finally {
                            identity.fill(0)
                        }
                        publishEnvironment(profile, store.snapshot())
                        var previous = store.snapshot()
                        while (isActive && profile.id != profileId) {
                            store.nextSnapshot(previous)
                            val latest = store.snapshot()
                            publishEnvironment(profile, latest)
                            if (!latest.connected()) break
                            previous = latest
                        }
                        delayMillis = 250L
                    } catch (error: CancellationException) {
                        throw error
                    } catch (error: Exception) {
                        notice = notice ?: "${profile.name}: ${error.message}"
                    }
                    if (isActive && profile.id != profileId) {
                        delay(delayMillis)
                        delayMillis = (delayMillis * 2).coerceAtMost(300_000L)
                    }
                }
            } finally {
                backgroundJobs.remove(profile.id)
            }
        }
        backgroundJobs[profile.id] = job
    }

    private fun publishEnvironment(profile: HostProfile, next: Snapshot) {
        val row = EnvironmentRow(
            profileId = profile.id,
            label = next.environmentLabel() ?: profile.name,
            state = next.environmentConnectionState() ?: "connecting",
            platform = next.environmentPlatform(),
            machine = next.environmentMachine(),
            capabilities = next.environmentCapabilities(),
            reconnectReason = next.environmentReconnectReason(),
            activities = next.awarenessActivities().map { activity ->
                EnvironmentActivityRow(
                    threadId = next.scopedThreadId(activity.threadId) ?: activity.threadId,
                    title = activity.threadTitle,
                    headline = activity.headline,
                    detail = activity.detail,
                    phase = activity.phase,
                )
            },
        )
        environments = (environments.filterNot { it.profileId == profile.id } + row)
            .sortedBy { it.label.lowercase() }
    }

    fun removeProfile(id: String) {
        runCatching {
                val background = backgroundOwners.remove(id)
                backgroundJobs.remove(id)?.cancel()
                scope.launch { runCatching { background?.shutdown() } }
                AndroidCredentialStore(context, id).remove()
                if (profileId == id) {
                    val old = detach()
                    profileId = null
                    repository.selected = null
                    publish(Snapshot.empty())
                    scope.launch { old?.shutdown() }
                }
                profiles = profiles.filterNot { it.id == id }
                environments = environments.filterNot { it.profileId == id }
                repository.saveProfiles(profiles)
                File(repository.cacheDirectory(id)).deleteRecursively()
                if (profiles.isEmpty()) stack = listOf(Route.Pairing)
            }
            .onFailure { notice = it.message }
    }

    fun preparePairing(contents: String) {
        invitation = null
        notice = null
        runCatching { parseInvitation(contents, (System.currentTimeMillis() / MILLIS_PER_SECOND).toULong()) }
            .onSuccess { invitation = it }
            .onFailure { notice = it.message }
    }

    fun openPairing() {
        if (route != Route.Pairing) stack = stack + Route.Pairing
        invitation = null
        notice = null
    }

    fun pair() {
        val target = invitation ?: return
        if (busy) return
        busy = true
        notice = null
        connection = scope.launch {
            var paired: AgentStore? = null
            try {
                val id = validateInvitation(target, (System.currentTimeMillis() / MILLIS_PER_SECOND).toULong())
                val identity =
                    withContext(Dispatchers.IO) { AndroidCredentialStore(context, id).loadOrCreate(::generateIdentity) }
                // The new store reads the state the current one keeps for this Host.
                if (profileId == id) runCatching { owner?.flush() }
                val store =
                    try {
                        AgentStore.connect(
                            Connection(target.endpoint, identity, target.invitation, true),
                            repository.stateFile(id),
                            repository.modelPreferences(),
                            repository.cacheDirectory(id),
                            repository.diagnosticsDirectory(id),
                        )
                    } finally {
                        identity.fill(0)
                    }
                paired = store
                if (!isActive) {
                    store.shutdown()
                    return@launch
                }
                persist()
                draftEdits.reset()
                initialization?.cancel()
                observation?.cancel()
                val old = owner
                owner = null
                publish(Snapshot.empty())
                profiles = profiles.filterNot { it.id == id } + HostProfile(id, target.hostName, target.endpoint)
                repository.saveProfiles(profiles)
                repository.selected = id
                profileId = id
                owner = store
                paired = null
                publish(store.snapshot())
                observe(store, id)
                invitation = null
                stack = listOf(Route.Home)
                busy = false
                scope.launch { old?.shutdown() }
            } catch (error: CancellationException) {
                runCatching { paired?.shutdown() }
                throw error
            } catch (error: Exception) {
                runCatching { paired?.shutdown() }
                busy = false
                notice = error.message
            }
        }
    }

    /** Foreground: subscriptions resume from their cursors and the connection is checked. */
    fun foreground() {
        owner?.appBecameActive()
        connect()
        startBackgroundProfiles(profileId)
    }

    fun connect() {
        val store = owner ?: return
        val profile = profiles.firstOrNull { it.id == profileId } ?: return
        if (busy || route == Route.Pairing) return
        busy = true
        notice = null
        connection = scope.launch {
            try {
                val identity =
                    withContext(Dispatchers.IO) {
                        AndroidCredentialStore(context, profile.id).loadOrCreate(::generateIdentity)
                    }
                try {
                    store.resume(Connection(profile.ticket, identity, null, true))
                } finally {
                    identity.fill(0)
                }
                if (profileId == profile.id && owner === store) {
                    publish(store.snapshot())
                    busy = false
                }
            } catch (error: CancellationException) {
                throw error
            } catch (error: Exception) {
                if (profileId == profile.id && owner === store) {
                    busy = false
                    notice = error.message
                }
            }
        }
    }

    private fun observe(store: AgentStore, id: String) {
        observation = scope.launch {
            var previous = store.snapshot()
            try {
                while (isActive) {
                    store.nextSnapshot(previous)
                    if (profileId != id || owner !== store) return@launch
                    val latest = store.snapshot()
                    publish(latest)
                    if (previous.connected() && !latest.connected() && !busy) connect()
                    previous = latest
                }
            } catch (error: CancellationException) {
                throw error
            } catch (error: Exception) {
                if (profileId == id && owner === store) notice = error.message
            }
        }
    }

    private fun publish(next: Snapshot) {
        if (!next.supersedes(snapshot)) return
        if (next === snapshot) return
        val name = next.hostName()
        if (name != null && profiles.any { it.id == profileId && it.name != name }) {
            profiles = profiles.map { if (it.id == profileId) it.copy(name = name) else it }
            repository.saveProfiles(profiles)
        }
        snapshot = next
        profileId?.let { id ->
            profiles.firstOrNull { it.id == id }?.let { profile -> publishEnvironment(profile, next) }
        }
        val selected = next.selectedThreadId()
        val from = followingFrom
        if (from != null && selected != null && selected != from) {
            if (route == Route.Thread(from)) stack = stack.dropLast(1) + Route.Thread(selected)
            followingFrom = null
        }
        val key = next.currentDraftKey()
        if (key != composerKey) {
            draftEdits.reset()
            composerKey = key
        }
        if (draftEdits.pending == null) {
            composerText = next.draft().text
            draftEdits.base = composerText
        }
        persistence?.cancel()
        persistence = scope.launch {
            delay(PERSISTENCE_DEBOUNCE_MILLIS)
            persist()
        }
    }

    /** Saves the model preferences every Host shares; the store writes its own state. */
    fun persist() {
        val current = owner?.snapshot() ?: return
        scope.launch { writes.send(current) }
    }

    /** Counts the app's moves to the background, which end a dictation. */
    var backgrounds by mutableIntStateOf(0)
        private set

    /** The app left the foreground: everything the store holds reaches storage. */
    fun background() {
        backgrounds += 1
        persist()
        val store = owner ?: return
        scope.launch { runCatching { store.flush() } }
    }

    private suspend fun <T> withStore(block: suspend (AgentStore) -> T): T {
        val store = owner ?: error("Host not connected")
        val host = profileId
        val result = block(store)
        if (host != profileId) throw CancellationException("Host changed")
        return result
    }

    /** Warms the Host's transcription while a recording runs; dropping it cancels. */
    fun prepareDictation(): DictationPreparation? = owner?.prepareDictation()

    suspend fun downloadBytes(path: String): ByteArray {
        val temporary = File.createTempFile("markdown-", ".image", context.cacheDir)
        try {
            download(path, temporary.path)
            return withContext(Dispatchers.IO) { temporary.readBytes() }
        } finally {
            temporary.delete()
        }
    }

    suspend fun download(path: String, destination: String) = withStore { it.downloadFile(path, destination) }

    suspend fun downloadAttachment(id: String, destination: String) = withStore {
        it.downloadAttachment(id, destination)
    }

    suspend fun upload(source: String, directory: String, name: String): String = withStore {
        it.uploadFile(source, directory, name)
    }

    suspend fun browser(request: BrowserRequest): BrowserFrame = withStore { it.browser(request) }

    override fun onCleared() {
        observation?.cancel()
        connection?.cancel()
        initialization?.cancel()
        persistence?.cancel()
        backgroundJobs.values.forEach { it.cancel() }
        val backgroundStores = backgroundOwners.values.toList()
        backgroundOwners.clear()
        scope.launch {
            operations.toList().joinAll()
            backgroundStores.forEach { store -> runCatching { store.shutdown() } }
            owner?.let { store ->
                runCatching { store.shutdown() }
                writes.send(store.snapshot())
            }
            writes.close()
            writer.join()
            scope.cancel()
        }
    }
}

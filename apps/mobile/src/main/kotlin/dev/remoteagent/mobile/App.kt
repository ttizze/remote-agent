package dev.remoteagent.mobile

import android.content.Context
import android.content.IntentFilter
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.lifecycle.ViewModel
import com.google.firebase.messaging.FirebaseMessaging
import dev.remoteagent.core.AgentStore
import dev.remoteagent.core.ArtifactTemplate
import dev.remoteagent.core.BrowserFrame
import dev.remoteagent.core.BrowserRequest
import dev.remoteagent.core.Connection
import dev.remoteagent.core.DictationPreparation
import dev.remoteagent.core.EnvironmentLoadBalancingPreferenceView
import dev.remoteagent.core.EnvironmentProjectRow
import dev.remoteagent.core.EnvironmentSettingsEntryView
import dev.remoteagent.core.EnvironmentThreadListView
import dev.remoteagent.core.Intent
import dev.remoteagent.core.Invitation
import dev.remoteagent.core.Outcome
import dev.remoteagent.core.PushDeviceRegistration
import dev.remoteagent.core.ShareContent
import dev.remoteagent.core.Snapshot
import dev.remoteagent.core.ThreadListOptions
import dev.remoteagent.core.agentActivityOverviewDeepLink
import dev.remoteagent.core.appendArtifactTemplateUsePrompt
import dev.remoteagent.core.environmentProjectRows as buildEnvironmentProjectRows
import dev.remoteagent.core.environmentSettings as buildEnvironmentSettings
import dev.remoteagent.core.environmentThreadList as buildEnvironmentThreadList
import dev.remoteagent.core.generateIdentity
import dev.remoteagent.core.notificationEvents as buildNotificationEvents
import dev.remoteagent.core.parseInvitation
import dev.remoteagent.core.subscriptionUsageWidgetsJson
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
    val environmentId: String,
    val label: String,
    val state: String,
    val platform: String?,
    val machine: String?,
    val capabilities: List<String>,
    val reconnectReason: String?,
    val activities: List<EnvironmentActivityRow>,
)

private data class PendingLoadBalancedNewThread(
    val projectId: String,
    val sourceEnvironmentId: String,
    val startedAtMillis: Long,
    val generation: Long,
)

private const val PERSISTENCE_QUEUE_CAPACITY = 8
private const val MILLIS_PER_SECOND = 1000L
private const val PERSISTENCE_DEBOUNCE_MILLIS = 250L

internal fun pushDeviceIdForUnregister(context: Context, hostId: String, registeredDeviceId: String?): String =
    registeredDeviceId ?: PushRegistrationStore.deviceId(context, hostId)

/** Screens of the native stack. Terminals exist only under a thread. */
internal sealed interface Route {
    data object Hosts : Route

    data object Pairing : Route

    data object Home : Route

    data class Thread(val id: String) : Route

    data class Device(val threadId: String) : Route

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

    /** A resource-backed PDF preview; PDFs never enter the text file reader. */
    data class Pdf(val file: String) : Route

    data class Settings(val projectId: String? = null) : Route

    data object Appearance : Route

    data object ScheduledTasks : Route

    data object Usage : Route

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
    private var clientPreferences = repository.modelPreferences()
    private var clientPreferencesGeneration = 0L
    var snapshot by mutableStateOf(Snapshot.empty())
        private set

    private val draftEdits = DraftRevision()
    private val usageWidget = UsageWidgetPublisher(context)
    private var composerKey = ""

    var profiles by mutableStateOf(emptyList<HostProfile>())
        private set

    var environments by mutableStateOf(emptyList<EnvironmentRow>())
        private set

    /** Latest immutable core snapshot for every saved environment. */
    var environmentSnapshots by mutableStateOf(emptyMap<String, Snapshot>())
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
    var notificationThreadRoute by mutableStateOf<String?>(null)
    var invitation by mutableStateOf<Invitation?>(null)
        private set

    var composerText by mutableStateOf("")
        private set

    /** Number of native Widget/notification requests waiting for Usage navigation. */
    var usageDeepLinkRequests by mutableIntStateOf(0)
        private set

    var pushCapability by mutableStateOf(PushCapability.UnsupportedUnconfigured)
        private set

    /** Counts requests to focus the composer with the cursor at the end of the draft. */
    var composerFocusRequests by mutableIntStateOf(0)
        private set

    private var followingFrom: String? = null
    private var pendingPushThread: Pair<String, String>? = null
    private var pushPermissionPrompted = false
    private val pushRegistrations = mutableMapOf<String, PushDeviceRegistration>()
    private val pushGenerations = mutableMapOf<String, Long>()
    private val pendingPushActive = mutableMapOf<String, Pair<String, Boolean>>()
    /**
     * Store instances are replaced on profile switches; replay the retained registration to the new owner even when the
     * FCM token is unchanged.
     */
    private val registeredPushOwners = mutableMapOf<String, AgentStore>()
    private val registeredPushConfigurations = mutableMapOf<String, PushDeviceRegistration>()
    private var owner: AgentStore? = null
    private val backgroundOwners = mutableMapOf<String, AgentStore>()
    private val backgroundJobs = mutableMapOf<String, Job>()
    private val backgroundJobGenerations = mutableMapOf<String, Long>()
    private val clientPreferenceSyncFailures = mutableMapOf<String, String>()
    private var pendingSelectedClientPreferences: Pair<AgentStore, ByteArray>? = null
    private var initialization: Job? = null
    private var connection: Job? = null
    private var observation: Job? = null
    private var persistence: Job? = null
    private var pendingLoadBalancedNewThread: PendingLoadBalancedNewThread? = null
    private var loadBalancingAttemptGeneration = 0L
    private var automaticRouteProfileId: String? = null
    private var browserProfileRemovalGeneration = 0UL
    private val pending = ArrayDeque<Pair<Intent, (Result<Outcome>) -> Unit>>()
    private val operations = mutableSetOf<Job>()
    private val writes = Channel<ByteArray>(PERSISTENCE_QUEUE_CAPACITY)
    private val writer =
        scope.launch(Dispatchers.IO) {
            for (current in writes) {
                runCatching { repository.saveModelPreferences(current) }
                    .onFailure { error -> withContext(Dispatchers.Main) { notice = error.message } }
            }
        }
    private val pushTokenReceiver =
        object : android.content.BroadcastReceiver() {
            override fun onReceive(context: Context, intent: android.content.Intent) {
                if (intent.action == ACTION_PUSH_TOKEN_UPDATED) refreshPushRegistration()
            }
        }

    init {
        context.registerReceiver(
            pushTokenReceiver,
            IntentFilter(ACTION_PUSH_TOKEN_UPDATED),
            Context.RECEIVER_NOT_EXPORTED,
        )
        runCatching { profiles = repository.profiles() }.onFailure { notice = it.message }
        if (profiles.isEmpty()) stack = listOf(Route.Pairing)
        repository.selected?.takeIf { id -> profiles.any { it.id == id } }?.let(::selectProfile)
            ?: startBackgroundProfiles(null)
    }

    fun perform(intent: Intent, complete: (Result<Outcome>) -> Unit = {}) {
        val (routed, profile) = routeIntent(intent)
        if (profile != null && profile != profileId) {
            selectProfile(profile)
            pending.addLast(routed to complete)
            return
        }
        performOnCurrent(routed, complete)
    }

    /**
     * Clears a browser profile in every connected Host before removing its device-owned row. Core validates the target
     * set and folds receipts; this owner only maps each environment to its Store.
     */
    fun removeBrowserProfile(profileId: String) {
        browserProfileRemovalGeneration += 1UL
        val generation = browserProfileRemovalGeneration
        val snapshots =
            (environmentSnapshots.values + snapshot).filter { it.connected() }.distinctBy { it.environmentId() }
        val environmentIds = snapshots.mapNotNull { it.environmentId() }.distinct().sorted()
        val defaults = snapshot.browserDefaults()
        val plan =
            runCatching {
                    dev.remoteagent.core.beginBrowserProfileRemoval(
                        defaults.profiles,
                        profileId,
                        environmentIds,
                        generation,
                    )
                }
                .getOrElse {
                    notice = it.message
                    return
                }
        val stores = mutableMapOf<String, AgentStore>()
        if (snapshot.connected()) {
            snapshot.environmentId()?.let { id -> owner?.let { stores[id] = it } }
        }
        backgroundOwners.forEach { (profile, store) ->
            val current = store.snapshot()
            if (current.connected()) {
                current.environmentId()?.let { id -> stores[id] = store }
            }
        }
        val missing = plan.environmentIds.any { it !in stores }
        scope.launch {
            val cleared = mutableListOf<String>()
            var failed = missing
            for (environmentId in plan.environmentIds) {
                val store = stores[environmentId] ?: continue
                try {
                    store.dispatch(Intent.PreviewClearProfileData(plan.profileId)).wait()
                    cleared += environmentId
                } catch (error: CancellationException) {
                    throw error
                } catch (_: Exception) {
                    failed = true
                }
            }
            if (generation != browserProfileRemovalGeneration) return@launch
            when (dev.remoteagent.core.browserProfileRemovalDecision(plan, generation, cleared, failed)) {
                dev.remoteagent.core.BrowserProfileRemovalDecision.READY -> {
                    var removalFailed = false
                    for (environmentId in plan.environmentIds) {
                        if (generation != browserProfileRemovalGeneration) return@launch
                        val store = stores[environmentId] ?: continue
                        try {
                            store.dispatch(Intent.RemoveBrowserProfile(plan.profileId)).wait()
                        } catch (error: CancellationException) {
                            throw error
                        } catch (_: Exception) {
                            removalFailed = true
                        }
                    }
                    if (!removalFailed) {
                        val canonicalStore =
                            owner?.takeIf { selectedOwner -> stores.values.any { it === selectedOwner } }
                            ?: stores.toSortedMap().values.firstOrNull()
                        canonicalStore?.let { synchronizeClientPreferences(it.snapshot(), includeSelected = true) }
                    }
                    if (removalFailed) {
                        notice = "Browser profile could not be removed from every connected Host; try again."
                    }
                }
                dev.remoteagent.core.BrowserProfileRemovalDecision.FAILED ->
                    notice = "Browser profile data could not be cleared on every connected Host; the profile was kept."
                dev.remoteagent.core.BrowserProfileRemovalDecision.PENDING,
                dev.remoteagent.core.BrowserProfileRemovalDecision.STALE -> Unit
            }
        }
    }

    private fun performOnCurrent(intent: Intent, complete: (Result<Outcome>) -> Unit = {}) {
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

    private fun routeIntent(intent: Intent): Pair<Intent, String?> {
        fun route(value: String?): Pair<String?, String?> = scopedValue(value)
        return when (intent) {
            is Intent.OpenThread -> {
                val (id, profile) = route(intent.threadId)
                Intent.OpenThread(id ?: intent.threadId) to profile
            }
            is Intent.NewThread -> {
                val (id, profile) = route(intent.projectId)
                Intent.NewThread(id) to profile
            }
            is Intent.Thread -> {
                val (id, profile) = route(intent.threadId)
                Intent.Thread(id ?: intent.threadId, intent.action) to profile
            }
            is Intent.MoveThread -> {
                val (id, profile) = route(intent.threadId)
                Intent.MoveThread(id ?: intent.threadId, intent.section, intent.destination) to profile
            }
            is Intent.FilterProject -> {
                val (id, profile) = route(intent.projectId)
                Intent.FilterProject(id) to profile
            }
            is Intent.NewThreadOnBranch -> {
                val (id, profile) = route(intent.projectId)
                Intent.NewThreadOnBranch(id ?: intent.projectId, intent.branch, intent.worktreePath) to profile
            }
            is Intent.ResetProjectSettings -> {
                val (id, profile) = route(intent.projectId)
                Intent.ResetProjectSettings(id ?: intent.projectId) to profile
            }
            else -> intent to null
        }
    }

    private fun scopedValue(value: String?): Pair<String?, String?> {
        val raw = value ?: return null to null
        val separator = raw.indexOf(':')
        if (separator <= 0 || separator == raw.lastIndex) return value to null
        val environmentId = raw.substring(0, separator)
        val profile = environments.firstOrNull { it.environmentId == environmentId }?.profileId ?: return value to null
        return raw.substring(separator + 1) to profile
    }

    fun environmentSnapshotsForCore(): List<Snapshot> = environmentSnapshots.values.toList()

    fun environmentProjects(query: String): List<EnvironmentProjectRow> =
        buildEnvironmentProjectRows(environmentSnapshotsForCore(), query)

    fun environmentSettings(): List<EnvironmentSettingsEntryView> =
        buildEnvironmentSettings(environmentSnapshotsForCore())

    fun loadBalancingPreferences(): List<EnvironmentLoadBalancingPreferenceView> =
        dev.remoteagent.core.environmentLoadBalancingPreferences(environmentSnapshotsForCore())

    fun setLoadBalancingEnabled(enabled: Boolean) {
        perform(Intent.SetLoadBalancingEnabled(enabled))
        backgroundOwners.forEach { (profile, store) ->
            scope.launch {
                runCatching {
                    store.dispatch(Intent.SetLoadBalancingEnabled(enabled)).wait()
                    synchronizeClientPreferences(store.snapshot(), includeSelected = true)
                    profiles.firstOrNull { it.id == profile }?.let { publishEnvironment(it, store.snapshot()) }
                }
            }
        }
    }

    fun setLoadBalancingWeight(environmentId: String, weight: UByte) {
        val intent = Intent.SetLoadBalancingWeight(environmentId, weight)
        val profile = environments.firstOrNull { it.environmentId == environmentId }?.profileId
        if (profile == profileId) perform(intent)
        else {
            val store = profile?.let { backgroundOwners[it] }
            if (store != null) {
                scope.launch {
                    runCatching {
                        store.dispatch(intent).wait()
                        synchronizeClientPreferences(store.snapshot(), includeSelected = true)
                        profiles.firstOrNull { it.id == profile }?.let { publishEnvironment(it, store.snapshot()) }
                    }
                }
            } else perform(intent)
        }
    }

    fun environmentThreadList(
        nowMs: Long,
        options: ThreadListOptions,
        query: String,
        selectedProject: String?,
        selectedThread: String?,
    ): EnvironmentThreadListView =
        buildEnvironmentThreadList(
            environmentSnapshotsForCore(),
            nowMs,
            options,
            query,
            selectedProject,
            selectedThread,
        )

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

    fun newThreadFromShortcut() {
        draftEdits.reset()
        openNewThread(snapshot.selectedProjectId())
        stack = stack + Route.NewTask
    }

    /** Opens a fresh draft on the best matching environment selected by core. */
    fun openNewThread(projectId: String?) {
        val sourceEnvironmentId = snapshot.environmentId()
        if (projectId == null || sourceEnvironmentId == null) {
            invalidatePendingLoadBalancedNewThread()
            perform(Intent.NewThread(projectId))
            return
        }
        // A project chosen from the aggregate picker already names its Host;
        // automatic balancing is only for the selected Host's local project.
        if (scopedValue(projectId).second != null) {
            invalidatePendingLoadBalancedNewThread()
            perform(Intent.NewThread(projectId))
            return
        }
        val evaluation =
            dev.remoteagent.core.environmentLoadBalancingRoute(
                environmentSnapshotsForCore(),
                sourceEnvironmentId,
                projectId,
                System.currentTimeMillis(),
            )
        if (evaluation.pendingResources) {
            loadBalancingAttemptGeneration += 1
            val generation = loadBalancingAttemptGeneration
            pendingLoadBalancedNewThread =
                PendingLoadBalancedNewThread(projectId, sourceEnvironmentId, System.currentTimeMillis(), generation)
            requestLoadBalancingResources()
            scope.launch {
                delay(3_000L)
                retryPendingLoadBalancedNewThread(generation)
            }
            return
        }
        invalidatePendingLoadBalancedNewThread()
        val route = evaluation.route
        if (route == null) {
            perform(Intent.NewThread(projectId))
            return
        }
        startRoutedNewThread(route, projectId, sourceEnvironmentId, loadBalancingAttemptGeneration)
    }

    private fun startRoutedNewThread(
        route: dev.remoteagent.core.EnvironmentLoadBalancedRouteView,
        fallbackProjectId: String,
        sourceEnvironmentId: String,
        generation: Long,
    ) {
        automaticRouteProfileId = environments.firstOrNull { it.environmentId == route.environmentId }?.profileId
        perform(Intent.NewThread("${route.environmentId}:${route.projectId}")) { result ->
            if (generation != loadBalancingAttemptGeneration) return@perform
            if (result.isFailure) {
                if (snapshot.environmentId() == route.environmentId) {
                    val fallback = "$sourceEnvironmentId:$fallbackProjectId"
                    if (scopedValue(fallback).second != null) perform(Intent.NewThread(fallback))
                }
            } else {
                perform(Intent.SetModel(route.providerInstance, route.driver, route.model, route.options))
                perform(Intent.SetRuntimeMode(route.runtimeMode))
                perform(Intent.SetInteractionMode(route.interactionMode))
            }
        }
        automaticRouteProfileId = null
    }

    private fun retryPendingLoadBalancedNewThread(generation: Long? = null) {
        val pending = pendingLoadBalancedNewThread ?: return
        if (generation != null && generation != pending.generation) return
        val nowMillis = System.currentTimeMillis()
        when (
            dev.remoteagent.core.environmentLoadBalancingPendingAction(
                pending.generation,
                loadBalancingAttemptGeneration,
                pending.sourceEnvironmentId,
                snapshot.environmentId(),
                pending.startedAtMillis,
                nowMillis,
                3_000L,
            )
        ) {
            dev.remoteagent.core.PendingRouteAction.CANCEL -> {
                invalidatePendingLoadBalancedNewThread()
                return
            }
            dev.remoteagent.core.PendingRouteAction.FALLBACK -> {
                invalidatePendingLoadBalancedNewThread()
                perform(Intent.NewThread("${pending.sourceEnvironmentId}:${pending.projectId}"))
                return
            }
            dev.remoteagent.core.PendingRouteAction.RETRY -> Unit
        }
        val evaluation =
            dev.remoteagent.core.environmentLoadBalancingRoute(
                environmentSnapshotsForCore(),
                pending.sourceEnvironmentId,
                pending.projectId,
                nowMillis,
            )
        if (evaluation.pendingResources) {
            if (generation != null) {
                scope.launch {
                    delay(50L)
                    retryPendingLoadBalancedNewThread(generation)
                }
            }
            return
        }
        invalidatePendingLoadBalancedNewThread()
        val route = evaluation.route
        if (route == null) {
            perform(Intent.NewThread("${pending.sourceEnvironmentId}:${pending.projectId}"))
            return
        }
        startRoutedNewThread(route, pending.projectId, pending.sourceEnvironmentId, loadBalancingAttemptGeneration)
    }

    private fun invalidatePendingLoadBalancedNewThread() {
        loadBalancingAttemptGeneration += 1
        pendingLoadBalancedNewThread = null
    }

    private fun requestLoadBalancingResources() {
        perform(Intent.RefreshLoadBalancingResources)
        backgroundOwners.values.forEach { store ->
            runCatching { store.dispatch(Intent.RefreshLoadBalancingResources) }
        }
    }

    fun importShare(text: String, urls: List<String> = emptyList()) {
        perform(Intent.ImportShare(ShareContent(text = text, urls = urls)))
        if (route != Route.NewTask && route !is Route.Thread) {
            stack = stack + Route.NewTask
        }
    }

    fun notificationsEnabled(): Boolean =
        when (snapshot.preferences().notificationMode) {
            dev.remoteagent.core.NotificationMode.NOTIFICATIONS,
            dev.remoteagent.core.NotificationMode.NOTIFICATIONS_AND_SOUND -> true
            else -> false
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

    /** Adds the template's prompt to the draft and brings the composer up with the cursor after it. */
    fun useArtifactTemplate(template: ArtifactTemplate) {
        val next = appendArtifactTemplateUsePrompt(composerText, template)
        if (next != composerText) editDraft(next)
        composerFocusRequests += 1
    }

    fun navigate(next: Route, selection: Intent? = null) {
        when (next) {
            is Route.Thread -> {
                draftEdits.reset()
                perform(selection ?: Intent.OpenThread(next.id))
            }
            Route.NewTask -> {
                draftEdits.reset()
                if (selection == null) openNewThread(snapshot.selectedProjectId()) else perform(selection)
            }
            is Route.Settings -> {
                val (projectId, profile) = scopedValue(next.projectId)
                if (profile != null && profile != profileId) selectProfile(profile)
                stack = stack + Route.Settings(projectId)
                return
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
        val below = stack.getOrNull(stack.size - 2)
        if (below == Route.NewTask) perform(Intent.SetNewThreadProject(projectId)) else openNewThread(projectId)
        stack = if (below == Route.NewTask) stack.dropLast(1) else stack + Route.NewTask
    }

    /** A project added (or found) from "Add project": the new task's draft opens on it in place of the flow. */
    fun projectAdded(projectId: String) {
        val flow = setOf(Route.ChooseProject, Route.AddProject, Route.AddProjectLocal, Route.NewTask)
        draftEdits.reset()
        if (Route.NewTask in stack) perform(Intent.SetNewThreadProject(projectId)) else openNewThread(projectId)
        stack = stack.takeWhile { it !in flow } + Route.NewTask
    }

    /** Replaces the open thread, as a thread link inside a thread does. */
    fun openThread(id: String) {
        if (route is Route.Thread) stack = stack.dropLast(1)
        navigate(Route.Thread(id))
    }

    fun visibleThreadDeepLink(): String? {
        val host = profileId ?: return null
        val thread = route as? Route.Thread ?: return null
        return "remoteagent://threads/${android.net.Uri.encode(host)}/${android.net.Uri.encode(thread.id)}"
    }

    fun openNotificationThread() {
        val route = notificationThreadRoute ?: return
        notificationThreadRoute = null
        notice = null
        openPushDeepLink(
            android.content
                .Intent(android.content.Intent.ACTION_VIEW, android.net.Uri.parse(route))
                .putExtra(EXTRA_PUSH_DEEP_LINK, route)
        )
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
        if (automaticRouteProfileId != null && automaticRouteProfileId != id) {
            invalidatePendingLoadBalancedNewThread()
        }
        stack = listOf(Route.Home)
        if (profileId == id && owner != null) {
            if (automaticRouteProfileId == null) invalidatePendingLoadBalancedNewThread()
            startBackgroundProfiles(id)
            connect()
            return
        }
        val old = detach()
        val background = backgroundOwners.remove(id)
        backgroundJobGenerations[id] = (backgroundJobGenerations[id] ?: 0L) + 1L
        backgroundJobs.remove(id)?.cancel()
        profileId = id
        repository.selected = id
        publish(Snapshot.empty())
        scope.launch {
            runCatching {
                old?.shutdown()
                background?.shutdown()
            }
        }

        initialization = scope.launch {
            try {
                val store =
                    AgentStore.offline(
                        repository.stateFile(id),
                        clientPreferences,
                        repository.cacheDirectory(id),
                        repository.diagnosticsDirectory(id),
                    )
               try {
                   applyCurrentClientPreferences(store)
               } catch (error: Exception) {
                   store.shutdown()
                   throw error
               }
                if (profileId != id || !isActive) {
                    store.shutdown()
                    return@launch
                }
                owner = store
                initialization = null
                publish(store.snapshot())
                refreshPushRegistration()
                openPendingPushThreadIfReady()
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
        if (automaticRouteProfileId == null) invalidatePendingLoadBalancedNewThread()
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
        pendingSelectedClientPreferences = null
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
        val generation = (backgroundJobGenerations[profile.id] ?: 0L) + 1L
        backgroundJobGenerations[profile.id] = generation
        lateinit var job: Job
        job = scope.launch {
            var delayMillis = 250L
            try {
                while (isActive && profiles.any { it.id == profile.id } && profile.id != profileId) {
                    var createdStore: AgentStore? = null
                    try {
                        val store =
                            backgroundOwners.getOrPut(profile.id) {
                                val created = AgentStore.offline(
                                    repository.stateFile(profile.id),
                                    clientPreferences,
                                    repository.cacheDirectory(profile.id),
                                    repository.diagnosticsDirectory(profile.id),
                                )
                                createdStore = created
                                created
                            }
                       applyCurrentClientPreferences(store)
                       if (!ownsBackground(profile, store, generation)) {
                           throw CancellationException("background owner changed")
                       }
                        registerPushForHost(profile.id)
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
                        if (!ownsBackground(profile, store, generation)) {
                            throw CancellationException("background owner changed")
                        }
                        publishEnvironment(profile, store.snapshot())
                        var previous = store.snapshot()
                        while (isActive && ownsBackground(profile, store, generation)) {
                            store.nextSnapshot(previous)
                            if (!ownsBackground(profile, store, generation)) {
                                throw CancellationException("background owner changed")
                            }
                            val latest = store.snapshot()
                            publishEnvironment(profile, latest)
                            if (!latest.connected()) break
                            previous = latest
                        }
                        delayMillis = 250L
                    } catch (error: CancellationException) {
                        if (createdStore != null && backgroundOwners[profile.id] === createdStore) {
                            backgroundOwners.remove(profile.id)
                            createdStore?.shutdown()
                        }
                        throw error
                    } catch (error: Exception) {
                        if (clientPreferenceSyncFailures[profile.id] == null) {
                            notice = notice ?: "${profile.name}: ${error.message}"
                        }
                    }
                    if (isActive && profile.id != profileId) {
                        delay(delayMillis)
                        delayMillis = (delayMillis * 2).coerceAtMost(300_000L)
                    }
                }
            } finally {
                if (backgroundJobGenerations[profile.id] == generation) {
                    backgroundJobs.remove(profile.id)
                    backgroundJobGenerations.remove(profile.id)
                }
            }
        }
        backgroundJobs[profile.id] = job
    }

    private fun ownsBackground(profile: HostProfile, store: AgentStore, generation: Long): Boolean =
        profile.id != profileId &&
            profiles.any { it.id == profile.id } &&
            backgroundJobGenerations[profile.id] == generation &&
            backgroundOwners[profile.id] === store

    private fun publishEnvironment(profile: HostProfile, next: Snapshot) {
        val previous = environmentSnapshots[profile.id]
        if (previous?.connected() == true && !next.connected()) {
            LocalNotifications.removeEnvironment(context, previous.environmentId() ?: profile.id)
        }
        val pushPreferencesChanged =
            previous?.preferences()?.liveActivitiesEnabled != next.preferences().liveActivitiesEnabled
        environmentSnapshots = environmentSnapshots + (profile.id to next)
        previous?.let { deliverAttentionEvents(it, next) }
        val row =
            EnvironmentRow(
                profileId = profile.id,
                environmentId = next.environmentId() ?: profile.id,
                label = next.environmentLabel() ?: profile.name,
                state = next.environmentConnectionState() ?: "connecting",
                platform = next.environmentPlatform(),
                machine = next.environmentMachine(),
                capabilities = next.environmentCapabilities(),
                reconnectReason = next.environmentReconnectReason(),
                activities =
                    next.awarenessActivities().map { activity ->
                        EnvironmentActivityRow(
                            threadId = next.scopedThreadId(activity.threadId) ?: activity.threadId,
                            title = activity.threadTitle,
                            headline = activity.headline,
                            detail = activity.detail,
                            phase = activity.phase,
                        )
                    },
            )
        environments = (environments.filterNot { it.profileId == profile.id } + row).sortedBy { it.label.lowercase() }
        if (pushPreferencesChanged) {
            renderActivityAggregate(
                context,
                setActivityDeliveryEnabled(context, profile.id, next.preferences().liveActivitiesEnabled),
            )
        }
        if (pushPreferencesChanged || pushRegistrations[profile.id] == null) {
            registerPushForHost(profile.id, force = pushPreferencesChanged)
        }
        publishUsageWidget()
        retryPendingLoadBalancedNewThread()
    }

    private fun publishUsageWidget() {
        usageWidget.publish(subscriptionUsageWidgetsJson(environmentSnapshotsForCore(), 6u))
    }

    fun removeProfile(id: String) {
        runCatching {
            val environmentId = environmentSnapshots[id]?.environmentId() ?: id
            LocalNotifications.removeEnvironment(context, environmentId)
            val unregistration = unregisterPush(id)
            val background = backgroundOwners.remove(id)
            backgroundJobGenerations[id] = (backgroundJobGenerations[id] ?: 0L) + 1L
            backgroundJobs.remove(id)?.cancel()
            clientPreferenceSyncFailures.remove(id)
            AndroidCredentialStore(context, id).remove()
            var old: AgentStore? = null
            if (profileId == id) {
                old = detach()
                profileId = null
                repository.selected = null
                publish(Snapshot.empty(), syncClientPreferences = false)
            }
            profiles = profiles.filterNot { it.id == id }
            environments = environments.filterNot { it.profileId == id }
            environmentSnapshots = environmentSnapshots - id
            renderActivityAggregate(removeActivityState(context, id))
            publishUsageWidget()
            repository.saveProfiles(profiles)
            File(repository.cacheDirectory(id)).deleteRecursively()
            scope.launch {
                unregistration?.join()
                runCatching { background?.shutdown() }
                old?.shutdown()
            }
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
        val expectedProfile = profileId
        val expectedOwner = owner
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
                            clientPreferences,
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
                applyCurrentClientPreferences(store)
                if (!isActive || profileId != expectedProfile || owner !== expectedOwner) {
                    store.shutdown()
                    return@launch
                }
                persist()
                draftEdits.reset()
                initialization?.cancel()
                observation?.cancel()
                val old = owner
                owner = null
                pendingSelectedClientPreferences = null
                publish(Snapshot.empty(), syncClientPreferences = false)
                profiles = profiles.filterNot { it.id == id } + HostProfile(id, target.hostName, target.endpoint)
                repository.saveProfiles(profiles)
                repository.selected = id
                profileId = id
                owner = store
                paired = null
                publish(store.snapshot())
                refreshPushRegistration()
                openPendingPushThreadIfReady()
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
        appInBackground = false
        LocalNotifications.clearDelivered(context)
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
                    refreshPushRegistration()
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

    private fun publish(next: Snapshot, syncClientPreferences: Boolean = true) {
        if (!next.supersedes(snapshot)) return
        if (next === snapshot) return
        val name = next.hostName()
        if (name != null && profiles.any { it.id == profileId && it.name != name }) {
            profiles = profiles.map { if (it.id == profileId) it.copy(name = name) else it }
            repository.saveProfiles(profiles)
        }
        snapshot = next
        if (syncClientPreferences) synchronizeClientPreferences(next)
        if (syncClientPreferences) {
            profileId?.let { id ->
                profiles.firstOrNull { it.id == id }?.let { profile -> publishEnvironment(profile, next) }
            }
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

    private fun deliverAttentionEvents(previous: Snapshot, current: Snapshot) {
        val appActive = !appInBackground
        val modeChanged = previous.preferences().notificationMode != current.preferences().notificationMode
        if (appActive || modeChanged) LocalNotifications.clearDelivered(context)
        val attentionEvents = buildNotificationEvents(previous, current, appActive, appActive)
        attentionEvents.forEach { event ->
            if (event.inApp) {
                notice = "${event.kind}: ${event.body}"
                notificationThreadRoute = event.deepLink
            }
            if (event.operatingSystem) {
                LocalNotifications.deliver(
                    context = context,
                    title = event.title,
                    body = event.body,
                    sound = event.sound,
                    threadId = event.threadId,
                    deepLink = event.deepLink,
                    kind = event.kind.toString(),
                    soundKind = event.soundKind.toString(),
                )
            } else if (event.sound) {
                LocalNotifications.playSound(context, event.soundKind.toString())
            }
        }
        if (appActive || !nativeNotificationsEnabled(current)) {
            LocalNotifications.clearDelivered(context)
        } else {
            LocalNotifications.updateBadge(context)
        }
    }

    private fun nativeNotificationsEnabled(snapshot: Snapshot): Boolean =
        when (snapshot.preferences().notificationMode) {
            dev.remoteagent.core.NotificationMode.NOTIFICATIONS,
            dev.remoteagent.core.NotificationMode.NOTIFICATIONS_AND_SOUND -> true
            else -> false
        }

    /** Saves and broadcasts the client preferences every Host shares. */
    fun persist() {
        val current = owner?.snapshot() ?: return
        synchronizeClientPreferences(current)
    }

    /** Seeds a Store with the current client-global payload before it becomes visible. */
    private suspend fun applyCurrentClientPreferences(store: AgentStore) {
        while (true) {
            if (!kotlinx.coroutines.currentCoroutineContext().isActive) {
                throw CancellationException()
            }
            val bytes = clientPreferences.copyOf()
            val generation = clientPreferencesGeneration
            if (bytes.isEmpty()) return
            try {
                store.applyClientPreferences(bytes).wait()
            } catch (error: Exception) {
                val key = if (owner === store) profileId ?: "selected" else {
                    backgroundOwners.entries.firstOrNull { it.value === store }?.key ?: "unknown"
                }
                clientPreferenceSyncFailures[key] = error.message ?: "client preferences sync failed"
                throw error
            }
            backgroundOwners.entries.firstOrNull { it.value === store }?.key?.let {
                clientPreferenceSyncFailures.remove(it)
            }
            if (owner === store) clientPreferenceSyncFailures.remove(profileId ?: "selected")
            if (generation == clientPreferencesGeneration) return
        }
    }

    private fun synchronizeClientPreferences(source: Snapshot, includeSelected: Boolean = false) {
        val bytes = runCatching { source.serializeModelPreferences() }
            .getOrElse {
                notice = it.message
                return
            }
        if (!includeSelected) {
            val pending = pendingSelectedClientPreferences
            if (pending != null && owner === pending.first && !bytes.contentEquals(pending.second)) {
                return
            }
        }
        val selectedNeedsSync = includeSelected && owner?.let { selected ->
            pendingSelectedClientPreferences?.let { pending ->
                pending.first !== selected || !pending.second.contentEquals(bytes)
            } ?: true
        } == true
        val changed = !bytes.contentEquals(clientPreferences)
        if (!changed && !selectedNeedsSync) return
        if (changed) {
            clientPreferences = bytes.copyOf()
            clientPreferencesGeneration += 1
            scope.launch { writes.send(bytes) }
        }
        val owners = mutableMapOf<String, AgentStore>()
        if (includeSelected) owner?.let {
            pendingSelectedClientPreferences = it to bytes.copyOf()
            owners[profileId ?: "selected"] = it
        }
        if (changed) backgroundOwners.forEach { (profile, store) -> owners[profile] = store }
        owners.forEach { (profile, store) ->
            val receipt = runCatching { store.applyClientPreferences(bytes) }.getOrNull() ?: return@forEach
            val isSelected = includeSelected && store === owner
            scope.launch {
                val result = runCatching { receipt.wait() }
                result
                    .onSuccess { clientPreferenceSyncFailures.remove(profile) }
                    .onFailure { error ->
                        clientPreferenceSyncFailures[profile] = error.message ?: "client preferences sync failed"
                    }
                if (result.isSuccess
                    && isSelected
                    && pendingSelectedClientPreferences?.first === store
                    && pendingSelectedClientPreferences?.second?.contentEquals(bytes) == true
                    && clientPreferences.contentEquals(bytes)
                ) {
                    pendingSelectedClientPreferences = null
                }
            }
        }
    }

    /** Counts the app's moves to the background, which end a dictation. */
    var backgrounds by mutableIntStateOf(0)
        private set

    private var appInBackground = false

    /** The app left the foreground: everything the store holds reaches storage. */
    fun background() {
        appInBackground = true
        backgrounds += 1
        persist()
        val store = owner ?: return
        scope.launch { runCatching { store.flush() } }
    }

    fun openPushDeepLink(intent: android.content.Intent) {
        if (intent.getBooleanExtra(EXTRA_OPEN_USAGE, false)) {
            openUsageDeepLink()
            return
        }
        val value =
            intent.getStringExtra(EXTRA_PUSH_DEEP_LINK)
                ?: intent.data?.takeIf { it.scheme == "remoteagent" }?.toString()
                ?: return
        LocalNotifications.acknowledge(context, value)
        val uri = runCatching { android.net.Uri.parse(value) }.getOrNull() ?: return
        if (isUsageDeepLink(uri)) {
            openUsageDeepLink()
            return
        }
        if (isActivityOverviewDeepLink(uri)) {
            openActivityOverviewDeepLink()
            return
        }
        if (uri.scheme != "remoteagent" || uri.host != "threads") return
        if (uri.userInfo != null || uri.port != -1 || uri.fragment != null || uri.query != null) return
        val parts =
            uri.encodedPath
                ?.split('/')
                ?.takeIf { values ->
                    values.size == 3 && values[0].isEmpty() && values[1].isNotEmpty() && values[2].isNotEmpty()
                }
                ?.drop(1)
                ?.map { android.net.Uri.decode(it) }
                ?.takeIf { values -> values.all(::validPushRouteSegment) } ?: return
        openPushThread(parts[0], parts[1])
    }

    private fun isUsageDeepLink(uri: android.net.Uri): Boolean =
        uri.scheme == "remoteagent" &&
            uri.host == "settings" &&
            uri.encodedPath == "/usage" &&
            uri.userInfo == null &&
            uri.port == -1 &&
            uri.fragment == null &&
            uri.queryParameterNames == setOf("tab") &&
            uri.getQueryParameter("tab") == "limits"

    private fun isActivityOverviewDeepLink(uri: android.net.Uri): Boolean =
        uri.toString() == agentActivityOverviewDeepLink()

    private fun validPushRouteSegment(value: String): Boolean =
        value.isNotEmpty() && value != "." && value != ".." && value.none { it == '\\' || it.isISOControl() }

    private fun openUsageDeepLink() {
        usageDeepLinkRequests += 1
    }

    /** Opens the host-aware activity overview used by grouped alerts. */
    internal fun openActivityOverviewDeepLink() {
        pendingPushThread = null
        stack = if (profiles.isEmpty()) listOf(Route.Pairing) else listOf(Route.Hosts)
    }

    /** Widget/notification launches enter the real Usage screen before consumption. */
    fun openUsageRouteFromDeepLink() {
        if (route != Route.Usage) stack = stack + Route.Usage
    }

    /** Usage navigation consumes one launch request after selecting the limits tab. */
    fun consumeUsageDeepLinkRequest() {
        if (usageDeepLinkRequests > 0) usageDeepLinkRequests -= 1
    }

    private fun openPushThread(hostId: String, threadId: String) {
        if (profiles.none { it.id == hostId }) return
        if (profileId != hostId || owner == null) {
            pendingPushThread = hostId to threadId
            if (profileId != hostId) selectProfile(hostId)
            return
        }
        openThread(threadId)
    }

    private fun openPendingPushThreadIfReady() {
        val pending = pendingPushThread ?: return
        if (profileId != pending.first || owner == null) return
        pendingPushThread = null
        openThread(pending.second)
    }

    fun refreshPushRegistration() {
        reconcileActivityNotificationPreferences()
        if (!FirebasePushBootstrap.ensure(context)) {
            pushCapability = PushCapability.UnsupportedUnconfigured
            deactivateRegisteredPush()
            return
        }
        pushCapability =
            if (PushNotificationCenter.notificationsEnabled(context)) PushCapability.Ready
            else PushCapability.DisabledByPreference
        val token = PushRegistrationStore.token(context)
        if (token == null) {
            requestPushToken()
            return
        }
        profiles.forEach { registerPushForHost(it.id) }
    }

    private fun reconcileActivityNotificationPreferences() {
        var changed = false
        var aggregate: String? = null
        profiles.forEach { profile ->
            if (!liveActivitiesEnabled(profile.id)) {
                changed = true
                aggregate = setActivityDeliveryEnabled(context, profile.id, enabled = false)
            }
        }
        if (!changed) {
            renderActivitySnapshot(context, currentActivitySnapshot(context))
            return
        }
        renderActivityAggregate(context, aggregate)
    }

    private fun pushOwner(hostId: String): AgentStore? = if (hostId == profileId) owner else backgroundOwners[hostId]

    private fun registerPushForHost(hostId: String, force: Boolean = false) {
        if (profiles.none { it.id == hostId }) return
        val token = PushRegistrationStore.token(context) ?: return
        val pushAvailable = FirebasePushBootstrap.ensure(context)
        if (!pushAvailable) return
        val notificationsAuthorized = PushNotificationCenter.notificationsEnabled(context)
        val registration =
            PushDeviceRegistration(
                PushRegistrationStore.deviceId(context, hostId),
                "android",
                token,
                null,
                null,
                null,
                null,
                pushAvailable,
                notificationsAuthorized,
                notificationsAuthorized,
            )
        val changed = pushRegistrations[hostId] != registration
        if (changed) {
            pushRegistrations[hostId] = registration
            pushGenerations[hostId] = (pushGenerations[hostId] ?: 0L) + 1L
            registeredPushOwners.remove(hostId)
            registeredPushConfigurations.remove(hostId)
        }
        val store = pushOwner(hostId) ?: return
        if (force) {
            registeredPushOwners.remove(hostId)
            registeredPushConfigurations.remove(hostId)
        }
        if (!force && registeredPushOwners[hostId] === store && registeredPushConfigurations[hostId] == registration) {
            pendingPushActive[hostId]?.let { setPushActive(hostId, it.first, it.second) }
            return
        }
        pushGenerations[hostId] = (pushGenerations[hostId] ?: 0L) + 1L
        val generation = pushGenerations[hostId] ?: 0L
        dispatchPush(hostId, store, generation, Intent.RegisterPushDevice(registration)) { result ->
            if (
                result.isSuccess &&
                    pushRegistrations[hostId] == registration &&
                    pushGenerations[hostId] == generation &&
                    pushOwner(hostId) === store
            ) {
                registeredPushOwners[hostId] = store
                registeredPushConfigurations[hostId] = registration
            }
        }
    }

    private fun dispatchPush(
        hostId: String,
        store: AgentStore,
        generation: Long,
        intent: Intent,
        complete: (Result<Outcome>) -> Unit = {},
    ) {
        val receipt =
            runCatching { store.dispatch(intent) }
                .getOrElse {
                    complete(Result.failure(it))
                    return
                }
        scope.launch {
            val result = runCatching { receipt.wait() }
            if (pushOwner(hostId) === store && pushGenerations[hostId] == generation) complete(result)
        }
    }

    private fun setPushActive(hostId: String, deviceId: String, active: Boolean) {
        pendingPushActive[hostId] = deviceId to active
        val registration = pushRegistrations[hostId] ?: return
        if (registration.deviceId != deviceId) return
        val store = pushOwner(hostId) ?: return
        pendingPushActive.remove(hostId)
        val generation = pushGenerations[hostId] ?: return
        dispatchPush(hostId, store, generation, Intent.SetPushDeviceActive(deviceId, active))
    }

    private fun deactivateRegisteredPush() {
        pushRegistrations.keys.toList().forEach { hostId ->
            val registration = pushRegistrations[hostId] ?: return@forEach
            registeredPushOwners.remove(hostId)
            registeredPushConfigurations.remove(hostId)
            setPushActive(hostId, registration.deviceId, false)
        }
    }

    private fun requestPushToken() {
        if (!FirebasePushBootstrap.ensure(context)) {
            pushCapability = PushCapability.UnsupportedUnconfigured
            return
        }
        runCatching {
                FirebaseMessaging.getInstance()
                    .token
                    .addOnSuccessListener { token ->
                        if (token.isBlank()) {
                            pushCapability = PushCapability.ProviderUnavailable
                            return@addOnSuccessListener
                        }
                        PushRegistrationStore.saveToken(context, token)
                        refreshPushRegistration()
                    }
                    .addOnFailureListener { pushCapability = PushCapability.ProviderUnavailable }
            }
            .onFailure { pushCapability = PushCapability.ProviderUnavailable }
    }

    /** A configured Host gates the OS prompt and provider registration. */
    fun requestPushPermissionIfNeeded(request: () -> Unit) {
        val wantsNotifications = profiles.isNotEmpty()
        if (!wantsNotifications) {
            if (pushRegistrations.isNotEmpty()) refreshPushRegistration()
            return
        }
        if (!FirebasePushBootstrap.ensure(context)) {
            pushCapability = PushCapability.UnsupportedUnconfigured
            return
        }
        if (!PushNotificationCenter.canRequestPermission(context)) {
            refreshPushRegistration()
            return
        }
        if (pushPermissionPrompted) return
        pushPermissionPrompted = true
        request()
    }

    private fun liveActivitiesEnabled(hostId: String): Boolean {
        val source = if (hostId == profileId) snapshot else environmentSnapshots[hostId] ?: Snapshot.empty()
        // Android has no ActivityKit token; core owns this persistent ongoing
        // activity preference for the FCM presentation.
        return source.preferences().liveActivitiesEnabled
    }

    private fun unregisterPush(hostId: String): Job? {
        val deviceId = pushDeviceIdForUnregister(context, hostId, pushRegistrations.remove(hostId)?.deviceId)
        pushGenerations[hostId] = (pushGenerations[hostId] ?: 0L) + 1L
        pendingPushActive.remove(hostId)
        registeredPushOwners.remove(hostId)
        registeredPushConfigurations.remove(hostId)
        val store = pushOwner(hostId) ?: return null
        val receipt = runCatching { store.dispatch(Intent.UnregisterPushDevice(deviceId)) }.getOrNull() ?: return null
        return scope.launch { runCatching { receipt.wait() } }
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
        context.unregisterReceiver(pushTokenReceiver)
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
                runCatching { store.snapshot().serializeModelPreferences() }.getOrNull()?.let { writes.send(it) }
            }
            writes.close()
            writer.join()
            scope.cancel()
        }
    }
}

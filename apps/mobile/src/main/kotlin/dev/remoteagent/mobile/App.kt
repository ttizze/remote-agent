package dev.remoteagent.mobile

import android.content.Context
import android.net.Uri
import androidx.activity.ComponentActivity
import androidx.activity.compose.BackHandler
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.consumeWindowInsets
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Scaffold
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateMapOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.lifecycle.ViewModel
import dev.remoteagent.core.AgentException
import dev.remoteagent.core.AgentStore
import dev.remoteagent.core.ClientBuild
import dev.remoteagent.core.ClientSession
import dev.remoteagent.core.ClientSessions
import dev.remoteagent.core.ConversationRowPolicy
import dev.remoteagent.core.ConversationRows
import dev.remoteagent.core.DevicePreferences
import dev.remoteagent.core.ErrorScope
import dev.remoteagent.core.Intent
import dev.remoteagent.core.ListSessions
import dev.remoteagent.core.OperationKey
import dev.remoteagent.core.Outcome
import dev.remoteagent.core.PanelScope
import dev.remoteagent.core.Presentation
import dev.remoteagent.core.ProgressRows
import dev.remoteagent.core.RenderedConversation
import dev.remoteagent.core.Snapshot
import dev.remoteagent.core.ThreadList
import dev.remoteagent.core.UploadAttachment
import dev.remoteagent.core.parseInvitation
import dev.remoteagent.core.ticketIdentity
import java.io.File
import java.io.IOException
import kotlin.time.Duration.Companion.milliseconds
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import kotlinx.serialization.SerializationException

internal enum class Screen {
    Hosts,
    Environment,
    Pairing,
    Threads,
    Conversation,
}

/** Core presentations feed each surface; business changes are queued typed intents. */
// The selected Store owns browser requests as well as conversation operations.
@Suppress("TooManyFunctions")
internal class AndroidAppModel(private val context: Context, private val preferences: DevicePreferences) : ViewModel() {
    var language by mutableStateOf(preferences.language())
        private set

    fun selectLanguage(value: dev.remoteagent.core.UiLanguage) {
        preferences.selectLanguage(value)
        updateLanguage(preferences.language())
    }

    private fun updateLanguage(value: dev.remoteagent.core.UiLanguage) {
        if (language == value) return
        language = value
        if (owner == null) publish(session?.presentation() ?: Presentation.empty(value))
    }

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)
    private val repository = AndroidMobileRepository(context)
    @Volatile private var publication = Presentation.empty(language)
    val snapshot: Snapshot
        get() = publication.snapshot()

    var settings by mutableStateOf(publication.settings())
        private set

    var conversationContext by mutableStateOf(publication.conversationContext())
        private set

    var composer by mutableStateOf(publication.composer())
        private set

    var files by mutableStateOf(requireNotNull(snapshot.files("")))
        private set

    var terminals by mutableStateOf(dev.remoteagent.core.Terminals.empty())
        private set

    var fileLocation by mutableStateOf(FileLocation("", null))
        private set

    var isConnected by mutableStateOf(false)
        private set

    var loadingThreads by mutableStateOf(false)
        private set

    var listQuery by mutableStateOf(snapshot.listQuery())
        private set

    var applicationNotices by mutableStateOf(emptyList<dev.remoteagent.core.ErrorNotice>())
        private set

    var projectNotices by mutableStateOf(emptyList<dev.remoteagent.core.ErrorNotice>())
        private set

    var conversation by mutableStateOf<ConversationRows?>(null)
        private set

    var profiles by mutableStateOf(emptyList<HostProfile>())
        private set

    val connectionStates = mutableStateMapOf<String, HostConnectionState>()

    var connectionSelection by mutableStateOf(dev.remoteagent.core.ConnectionSelection(null, false))
        private set

    var screen by mutableStateOf(Screen.Threads)

    val busy: Boolean
        get() =
            if (screen == Screen.Pairing) pairingConnection != null
            else connectionSelection.hostId?.let(connections::containsKey) == true

    var notice by mutableStateOf<String?>(null)

    val shouldReconnect: Boolean
        get() =
            !isConnected &&
                connectionSelection.hostId != null &&
                connectionStates[connectionSelection.hostId]?.enabled != false &&
                screen != Screen.Pairing

    var list by mutableStateOf<ThreadList?>(null)
        private set

    private val sessions =
        ClientSessions(true, ClientBuild(0uL, BuildConfig.BUILD_REVISION), repository.deviceName, preferences)
    private var session: ClientSession? = null
    // Observed so a browser pane follows the selected Host's Store.
    private var owner by mutableStateOf<AgentStore?>(null)

    private val connections = mutableStateMapOf<String, Job>()
    private var pairingConnection by mutableStateOf<Job?>(null)
    private var activityExpansion = emptyMap<String, String>()
    private var attachmentTransfers = 0
    private val observations = mutableMapOf<String, Job>()
    val draftKey
        get() = conversationContext.navigation().draftKey

    val selectionKey
        get() = connectionSelection.hostId to draftKey

    init {
        scope.launch {
            while (isActive) {
                preferences.nextLanguage(language)
                updateLanguage(preferences.language())
            }
        }
        try {
            profiles = repository.profiles()
            val selection =
                dev.remoteagent.core.resolveConnectionSelection(
                    profiles.map { it.id },
                    repository.selected,
                    repository.allHosts,
                )
            connectionSelection = selection
            selection.hostId?.let(::activateProfile)
            profiles.forEach { connect(it) }
        } catch (error: SerializationException) {
            notice = error.displayMessage()
        }
    }

    /** The selected Host's Store, which owns its browser. */
    val selectedStore: AgentStore?
        get() = owner

    fun perform(intent: Intent, complete: (Result<Outcome>) -> Unit = {}) {
        val store = session
        if (store == null) {
            complete(Result.failure(IllegalStateException("ホストに接続していません")))
            return
        }
        val receipt =
            try {
                store.dispatch(intent)
            } catch (error: AgentException) {
                complete(Result.failure(error))
                return
            }
        publish(store.presentation())
        val host = connectionSelection.hostId
        scope.launch {
            val result = runCatching { receipt.wait() }
            if (host == connectionSelection.hostId && session === store) publish(store.presentation())
            complete(result)
        }
    }

    fun selectConnection(id: String?) {
        connectionSelection = connectionSelection.copy(all = id == null)
        repository.allHosts = id == null
        id?.let(::activateProfile)
        showThreads()
        updateTaskList()
    }

    private fun activateProfile(id: String) {
        if (session != null && connectionSelection.hostId == id) return
        screen = Screen.Threads
        val storage = repository.sessionStorage(id)
        val selected = sessions.open(id, storage, storage)
        session = selected
        owner = selected.store()
        connectionSelection = connectionSelection.copy(hostId = id)
        repository.selected = id
        notice = null
        publish(Presentation.empty(language))
        selected.dispatch(Intent.ShowThreadList)
        publish(selected.presentation())
        observe(id, selected)
    }

    private fun updateTaskList() {
        list = sessions.threadList(if (connectionSelection.all) null else connectionSelection.hostId)
    }

    fun performList(intent: Intent, host: String? = null) {
        if (host != null) {
            performOnHost(intent, host)
            return
        }
        if (intent is Intent.NewChat) {
            screen = Screen.Conversation
            perform(intent)
        } else {
            profiles
                .filter { connectionSelection.all || it.id == connectionSelection.hostId }
                .forEach { performOnHost(intent, it.id) }
        }
    }

    fun performOnHost(intent: Intent, host: String) {
        if (intent is Intent.NewChat || intent is Intent.ReadThread) {
            activateProfile(host)
            screen = Screen.Conversation
            perform(intent)
            return
        }
        val selected = sessions.session(host) ?: return
        try {
            val receipt = selected.dispatch(intent)
            updateTaskList()
            scope.launch {
                try {
                    receipt.wait()
                } catch (error: AgentException) {
                    notice = error.displayMessage()
                }
                updateTaskList()
            }
        } catch (error: AgentException) {
            notice = error.displayMessage()
        }
    }

    fun pair(contents: String) {
        pairingConnection?.cancel()
        notice = null
        pairingConnection =
            scope.launch(start = CoroutineStart.LAZY) {
                try {
                    val invitation =
                        parseInvitation(contents, System.currentTimeMillis().milliseconds.inWholeSeconds.toULong())
                    val id = ticketIdentity(invitation.endpoint)
                    val storage = repository.sessionStorage(id)
                    val store = sessions.pair(storage, storage, invitation)
                    if (!isActive) {
                        store.shutdown()
                        return@launch
                    }
                    val paired =
                        profiles.filterNot { it.id == id } + HostProfile(id, invitation.hostName, invitation.endpoint)
                    repository.saveProfiles(paired)
                    observations.remove(id)?.cancel()
                    connections.remove(id)?.cancel()
                    sessions.adopt(id, store)
                    if (connectionSelection.hostId == id) session = null
                    if (!isActive) return@launch
                    profiles = paired
                    pairingConnection = null
                    selectConnection(id)
                } catch (error: AgentException) {
                    notice = error.displayMessage()
                } finally {
                    if (isActive) pairingConnection = null
                }
            }
        pairingConnection?.start()
    }

    fun connect() {
        val profile = profiles.firstOrNull { it.id == connectionSelection.hostId } ?: return
        if (!connections.containsKey(profile.id)) connect(profile, reconnect = true)
    }

    fun openEnvironment(host: String) {
        activateProfile(host)
        screen = Screen.Environment
    }

    fun setConnectionEnabled(host: String, enabled: Boolean) {
        val selected = sessions.session(host) ?: return
        if (!enabled) connections.remove(host)?.cancel()
        try {
            val receipt = selected.dispatch(Intent.SetConnectionEnabled(enabled))
            val current = selected.snapshot()
            connectionStates[host] = HostConnectionState(current.connectionEnabled(), current.connected())
            scope.launch {
                try {
                    receipt.wait()
                    selected.flush()
                    if (profiles.none { it.id == host }) return@launch
                    val updated = selected.snapshot()
                    connectionStates[host] = HostConnectionState(updated.connectionEnabled(), updated.connected())
                    if (enabled) profiles.firstOrNull { it.id == host }?.let { connect(it) }
                } catch (error: AgentException) {
                    notice = error.displayMessage()
                }
            }
        } catch (error: AgentException) {
            notice = error.displayMessage()
        }
    }

    fun removeProfile(host: String) {
        if (profiles.none { it.id == host }) return
        profiles = profiles.filterNot { it.id == host }
        repository.saveProfiles(profiles)
        connections.remove(host)?.cancel()
        observations.remove(host)?.cancel()
        connectionStates.remove(host)
        val previous = sessions.remove(host)
        if (connectionSelection.hostId == host) {
            session = null
            owner = null
            publish(Presentation.empty(language))
        }
        connectionSelection =
            dev.remoteagent.core.resolveConnectionSelection(
                profiles.map { it.id },
                connectionSelection.hostId,
                connectionSelection.all,
            )
        repository.selected = connectionSelection.hostId
        repository.allHosts = connectionSelection.all
        connectionSelection.hostId?.let(::activateProfile)
        updateTaskList()
        screen = Screen.Hosts
        scope.launch {
            try {
                previous?.shutdown()
            } catch (error: AgentException) {
                notice = error.displayMessage()
            }
            try {
                repository.removeHost(host)
            } catch (error: java.security.GeneralSecurityException) {
                notice = error.displayMessage()
            } catch (error: IOException) {
                notice = error.displayMessage()
            }
        }
    }

    private fun connect(profile: HostProfile, reconnect: Boolean = false) {
        val storage = repository.sessionStorage(profile.id)
        val store = sessions.open(profile.id, storage, storage)
        observe(profile.id, store)
        if (!reconnect && (store.snapshot().connected() || connections.containsKey(profile.id))) return
        connections.remove(profile.id)?.cancel()
        if (connectionSelection.hostId == profile.id) notice = null
        connections[profile.id] =
            scope.launch(start = CoroutineStart.LAZY) {
                try {
                    store.resume(profile.ticket)
                    if (connectionSelection.hostId == profile.id) {
                        publish(store.presentation())
                        notice = null
                    }
                } catch (error: AgentException) {
                    if (connectionSelection.hostId == profile.id) notice = error.displayMessage()
                } finally {
                    if (isActive) connections.remove(profile.id)
                }
            }
        connections[profile.id]?.start()
    }

    fun lifecycle(active: Boolean) {
        sessions.recordScene(if (active) 1uL else 3uL)
        if (active) {
            // Returning from a picker keeps the transport underneath its upload.
            if (attachmentTransfers == 0) profiles.forEach { connect(it, reconnect = true) }
        } else persist()
    }

    private fun observe(host: String, selected: ClientSession) {
        if (observations.containsKey(host)) return
        observations[host] = scope.launch {
            try {
                val store = selected.ready()
                if (!isActive) return@launch
                var previous = store.presentation()
                receiveHost(previous, host, true)
                while (isActive) {
                    store.nextPresentation(previous)
                    // UI edits may acknowledge newer input before this notification resumes.
                    val latest = store.presentation()
                    receiveHost(latest, host, latest.changes(previous).list)
                    previous = latest
                }
            } catch (error: AgentException) {
                if (session === selected && isActive) notice = error.displayMessage()
            }
        }
    }

    private fun receiveHost(next: Presentation, host: String, listChanged: Boolean) {
        val current = next.snapshot()
        connectionStates[host] = HostConnectionState(current.connectionEnabled(), current.connected())
        updateProfile(current, host)
        if (connectionSelection.hostId == host) {
            // Each read returns a new binding wrapper; take the Store once it is ready.
            if (owner == null) owner = session?.store()
            publish(next)
        } else if (connectionSelection.all && listChanged) updateTaskList()
    }

    private fun publish(value: Presentation) {
        val changes = value.changes(publication)
        val next = value.snapshot()
        if (!changes.changed) return
        if (changes.reconnected) notice = null
        if (changes.list) updateTaskList()
        val context = if (changes.context) value.conversationContext() else conversationContext
        val navigated = context.navigation().draftKey != draftKey
        if (navigated) activityExpansion = emptyMap()
        if (changes.context) conversationContext = context
        if (changes.composer) composer = value.composer()
        projectConversation(value.conversation(), next.panelScope(connectionSelection.hostId ?: "local", draftKey))
        publication = value
        if (changes.settings) settings = value.settings()
        isConnected = next.connected()
        loadingThreads = next.operationRunning(OperationKey.SessionList)
        listQuery = next.listQuery()
        applicationNotices = next.errorNotices("", ErrorScope.Application)
        projectNotices = next.errorNotices("", ErrorScope.Projects)
        if (changes.groups.files || changes.groups.panes) files = requireNotNull(next.files(value.paneId()))
        if (changes.groups.terminals) terminals = next.terminals()
        fileLocation = FileLocation(context.navigation().cwd, context.home())
        session?.persistenceError()?.let { notice = it }
    }

    private fun projectConversation(rendered: RenderedConversation?, scope: PanelScope) {
        val next = rendered?.let {
            ConversationRows.project(
                it,
                scope,
                activityExpansion,
                ConversationRowPolicy(requests = true, progress = ProgressRows.ALWAYS),
                conversation,
            )
        }
        if (next == null || conversation?.let(next::unchanged) != true) conversation = next
    }

    fun toggleActivity(id: String) {
        val current = conversation ?: return
        val change = current.toggleActivity(id, activityExpansion) ?: return
        activityExpansion = change
        projectConversation(current.source(), current.scope())
    }

    private fun updateProfile(next: Snapshot, host: String) {
        // The Host's latest addresses let the next cold connection dial it directly.
        val current = profiles.firstOrNull { it.id == host }
        val refreshed =
            current?.copy(name = next.hostName() ?: current.name, ticket = next.hostTicket() ?: current.ticket)
        if (refreshed != null && refreshed != current) {
            profiles = profiles.map { if (it.id == host) refreshed else it }
            repository.saveProfiles(profiles)
        }
    }

    fun attach(selection: Pair<String?, dev.remoteagent.core.DraftKey>, uri: Uri, complete: () -> Unit) {
        if (selection != selectionKey) {
            complete()
            return
        }
        val key = draftKey
        if (!snapshot.connected()) connect()
        attachmentTransfers++
        var local: File? = null
        fun finish() {
            local?.parentFile?.deleteRecursively()
            attachmentTransfers--
            complete()
        }
        scope.launch {
            var uploading = false
            try {
                connections[selection.first]?.join()
                if (selection != selectionKey) return@launch
                if (!snapshot.connected()) throw IOException("PCに接続できません。接続を確認してもう一度添付してください。")
                notice = null
                val attachment =
                    withContext(Dispatchers.IO) { importAttachment(context, uri).also { local = File(it.path) } }
                if (selection != selectionKey) return@launch
                uploading = true
                perform(Intent.UploadAttachment(UploadAttachment(key, attachment))) { finish() }
            } catch (error: SecurityException) {
                if (selection == selectionKey) notice = error.displayMessage()
            } catch (error: IOException) {
                if (selection == selectionKey) notice = error.displayMessage()
            } finally {
                if (!uploading) finish()
            }
        }
    }

    fun persist() {
        scope.launch {
            try {
                sessions.flush()
            } catch (error: AgentException) {
                notice = error.displayMessage()
            }
        }
    }

    override fun onCleared() {
        observations.values.forEach { it.cancel() }
        pairingConnection?.cancel()
        connections.values.forEach { it.cancel() }
        connections.clear()
        session = null
        owner = null
        sessions.retire()
        scope.launch {
            try {
                sessions.shutdown()
            } finally {
                scope.cancel()
            }
        }
    }
}

internal fun AndroidAppModel.showThreads() {
    screen = Screen.Threads
    perform(Intent.ShowThreadList)
    performList(Intent.ListSessions(ListSessions(query = snapshot.listQuery())))
}

internal fun AndroidAppModel.showHosts() {
    screen = Screen.Hosts
    perform(Intent.ShowThreadList)
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun RemoteAgentApp(
    activity: ComponentActivity,
    model: AndroidAppModel,
    requestQrScan: (onContents: (String) -> Unit) -> Unit,
) {
    var settings by remember { mutableStateOf(false) }
    ObserveConnectionLifecycle(activity, model::lifecycle)
    BackHandler(model.screen != Screen.Threads) {
        if (model.screen == Screen.Environment) model.showHosts() else model.showThreads()
    }
    CompositionLocalProvider(
        LocalUiLanguage provides model.language,
        LocalFiles provides model.files,
        LocalFileLocation provides model.fileLocation,
    ) {
        AppTheme {
            Scaffold(
                topBar = { if (model.screen in listOf(Screen.Pairing, Screen.Hosts)) HostAppBar { settings = true } }
            ) { padding ->
                Column(Modifier.fillMaxSize().appContentInsets(padding)) {
                    ConnectionStatus(model.notice, model.busy, model.shouldReconnect, model::connect)
                    ErrorNotices(
                        model.applicationNotices,
                        model::perform,
                        if (model.isConnected) null else model::connect,
                    )
                    AppScreenContent(model, requestQrScan) { settings = true }
                }
            }
        }
    }
    if (settings)
        LanguageSettingsDialog(
            model.language,
            model::selectLanguage,
            openConnections = {
                settings = false
                model.showHosts()
            },
        ) {
            settings = false
        }
}

@Composable
private fun androidx.compose.foundation.layout.ColumnScope.AppScreenContent(
    model: AndroidAppModel,
    requestQrScan: (onContents: (String) -> Unit) -> Unit,
    openSettings: () -> Unit,
) {
    when (model.screen) {
        Screen.Pairing -> PairingScreen(model.busy, model::pair, model::showThreads, requestQrScan)
        Screen.Hosts ->
            ProfilesScreen(
                model.profiles.map { it to model.connectionStates[it.id] },
                model::setConnectionEnabled,
                model::removeProfile,
                model::openEnvironment,
            ) {
                model.screen = Screen.Pairing
            }
        Screen.Environment -> {
            model.profiles
                .firstOrNull { it.id == model.connectionSelection.hostId }
                ?.let { profile ->
                    EnvironmentSettingsScreen(
                        profile,
                        model.settings,
                        model::setConnectionEnabled,
                        model::perform,
                        model::showHosts,
                    )
                }
        }
        Screen.Threads -> {
            ErrorNotices(model.projectNotices, model::perform)
            ThreadListScreen(
                model.list,
                model.listQuery,
                model.loadingThreads,
                model::performList,
                title = {
                    ConnectionListTitle(
                        model.profiles,
                        model.connectionSelection,
                        model.isConnected,
                        select = model::selectConnection,
                        openSettings = openSettings,
                    )
                },
                scopeKey = if (model.connectionSelection.all) "all" else model.connectionSelection.hostId.orEmpty(),
                emptyContent =
                    if (model.profiles.isEmpty()) {
                        { EmptyConnections(connect = { model.screen = Screen.Pairing }) }
                    } else null,
            )
        }
        Screen.Conversation -> ConversationPane(model)
    }
}

@Composable
private fun androidx.compose.foundation.layout.ColumnScope.ConversationPane(model: AndroidAppModel) {
    androidx.compose.runtime.key(model.selectionKey) {
        var scrollToTopRequest by remember { mutableStateOf(0) }
        ConversationHeader(
            model.conversationContext.title(),
            model::showThreads,
        ) {
            scrollToTopRequest += 1
        }
        androidx.compose.foundation.layout.Row {
            FilesLauncher(model.files, model.fileLocation.cwd, model::perform)
            TerminalLauncher(model.terminals, model.conversationContext.navigation().cwd, model::perform)
            BrowserLauncher(model.conversationContext.navigation().threadId, model.selectedStore)
        }
        ThreadDetailScreen(
            model.conversationContext,
            model.conversation,
            ConversationActions(model::perform, model::toggleActivity, model.terminals),
            Modifier.weight(1f),
            scrollToTopRequest = scrollToTopRequest,
        ) { onSend ->
            ThreadComposer(
                model.composer,
                model::perform,
                onSend,
                settings = { selected, dismiss ->
                    ModelPickerDialog(model.settings, selected, model::perform, dismiss)
                },
            ) { transferState ->
                AttachmentButton(model.selectionKey, model::attach, model.composer.canAttach(), transferState)
            }
        }
    }
}

// Consume Scaffold/system-bar padding before applying the remaining keyboard inset.
// Keep IME handling at the screen boundary so it cannot create a gap below the composer.
internal fun Modifier.appContentInsets(padding: PaddingValues): Modifier =
    padding(padding).consumeWindowInsets(padding).imePadding()

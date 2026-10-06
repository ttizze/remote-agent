package dev.remoteagent.mobile

import android.content.Context
import androidx.activity.ComponentActivity
import androidx.activity.compose.BackHandler
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.key
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import androidx.lifecycle.ViewModel
import dev.remoteagent.core.AgentStore
import dev.remoteagent.core.BrowserFrame
import dev.remoteagent.core.BrowserRequest
import dev.remoteagent.core.Connection
import dev.remoteagent.core.Intent
import dev.remoteagent.core.Invitation
import dev.remoteagent.core.Outcome
import dev.remoteagent.core.Snapshot
import dev.remoteagent.core.ThreadAction
import dev.remoteagent.core.applyModelPreferences
import dev.remoteagent.core.generateIdentity
import dev.remoteagent.core.parseInvitation
import dev.remoteagent.core.validateInvitation
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

private const val PERSISTENCE_QUEUE_CAPACITY = 8
private const val MILLIS_PER_SECOND = 1000L
private const val PRESENTATION_COALESCE_MILLIS = 16L
private const val PERSISTENCE_DEBOUNCE_MILLIS = 250L

internal enum class Screen {
    Hosts,
    Pairing,
    Threads,
    Conversation,
}

// One owner coordinates native lifecycle, receipts and persistence.
@Suppress("TooManyFunctions", "TooGenericExceptionCaught", "ReturnCount")
internal class AndroidAppModel(private val context: Context) : ViewModel() {
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)
    private val repository = AndroidMobileRepository(context)
    var snapshot by mutableStateOf(Snapshot.empty())
        private set

    var conversation by mutableStateOf(snapshot.conversation())
        private set

    private var presentation: Job? = null
    private val draftEdits = DraftRevision()
    private var composerKey = ""

    var profiles by mutableStateOf(emptyList<HostProfile>())
        private set

    var profileId by mutableStateOf<String?>(null)
        private set

    var screen by mutableStateOf(Screen.Hosts)
    var busy by mutableStateOf(false)
        private set

    var deleteThreadId by mutableStateOf<String?>(null)
    var notice by mutableStateOf<String?>(null)
    var invitation by mutableStateOf<Invitation?>(null)
        private set

    var composerText by mutableStateOf("")
        private set

    private var owner: AgentStore? = null
    private var initialization: Job? = null
    private var connection: Job? = null
    private var observation: Job? = null
    private var persistence: Job? = null
    private val pending = ArrayDeque<Pair<Intent, (Result<Outcome>) -> Unit>>()
    private val operations = mutableSetOf<Job>()
    private val writes = Channel<Pair<String, Snapshot>>(PERSISTENCE_QUEUE_CAPACITY)
    private val writer =
        scope.launch(Dispatchers.IO) {
            for ((id, current) in writes) {
                runCatching {
                        repository.save(id, current.serializeLocalState())
                        repository.saveModelPreferences(current.serializeModelPreferences())
                    }
                    .onFailure { error -> withContext(Dispatchers.Main) { notice = error.message } }
            }
        }

    init {
        runCatching { profiles = repository.profiles() }.onFailure { notice = it.message }
        repository.selected?.takeIf { id -> profiles.any { it.id == id } }?.let(::selectProfile)
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
                        result.exceptionOrNull()?.let { notice = snapshot.error() ?: it.message }
                        complete(result)
                    } else complete(Result.failure(CancellationException("Host changed")))
                } finally {
                    operations.remove(operation)
                }
            }
        operations.add(operation)
        operation.start()
    }

    private fun resetEditor() {
        draftEdits.reset()
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

    fun openThread(id: String) {
        resetEditor()
        screen = Screen.Conversation
        perform(Intent.OpenThread(id))
    }

    fun newThread(project: String? = snapshot.selectedProjectId()) {
        resetEditor()
        screen = Screen.Conversation
        perform(Intent.NewThread(project))
    }

    fun showThreads() {
        resetEditor()
        screen = Screen.Threads
        perform(Intent.LeaveThread)
    }

    fun showHosts() {
        persist()
        screen = Screen.Hosts
    }

    fun selectProfile(id: String) {
        if (profiles.none { it.id == id }) return
        screen = Screen.Threads
        if (profileId == id && owner != null) {
            connect()
            return
        }
        val old = detach()
        profileId = id
        repository.selected = id
        publish(Snapshot.empty())
        scope.launch { runCatching { old?.shutdown() } }
        initialization = scope.launch {
            try {
                val bytes =
                    withContext(Dispatchers.IO) {
                        applyModelPreferences(repository.load(id), repository.modelPreferences())
                    }
                val store = AgentStore.offline(bytes, repository.diagnosticsDirectory(id))
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
        resetEditor()
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

    fun removeProfile(id: String) {
        runCatching {
                AndroidCredentialStore(context, id).remove()
                if (profileId == id) {
                    val old = detach()
                    profileId = null
                    repository.selected = null
                    publish(Snapshot.empty())
                    scope.launch { old?.shutdown() }
                }
                profiles = profiles.filterNot { it.id == id }
                repository.saveProfiles(profiles)
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
        screen = Screen.Pairing
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
                val bytes =
                    withContext(Dispatchers.IO) {
                        applyModelPreferences(repository.load(id), repository.modelPreferences())
                    }
                val store =
                    try {
                        AgentStore.connect(
                            Connection(target.endpoint, identity, target.invitation, true),
                            bytes,
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
                resetEditor()
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
                screen = Screen.Threads
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

    fun connect() {
        val store = owner ?: return
        val profile = profiles.firstOrNull { it.id == profileId } ?: return
        if (busy || screen == Screen.Pairing) return
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
        val key = next.currentDraftKey()
        if (key != composerKey) {
            draftEdits.reset()
            composerKey = key
        }
        if (draftEdits.pending == null) {
            composerText = next.draft().text
            draftEdits.base = composerText
        }
        schedulePresentation()
        persistence?.cancel()
        persistence = scope.launch {
            delay(PERSISTENCE_DEBOUNCE_MILLIS)
            persist()
        }
    }

    private fun schedulePresentation() {
        if (owner == null) {
            presentation?.cancel()
            presentation = null
            conversation = snapshot.conversation()
        } else if (presentation?.isActive != true) {
            val expectedOwner = owner
            val expectedHost = profileId
            presentation = scope.launch {
                while (isActive) {
                    delay(PRESENTATION_COALESCE_MILLIS)
                    val latest = snapshot
                    val view = withContext(Dispatchers.Default) { latest.conversation() }
                    if (owner !== expectedOwner || profileId != expectedHost) return@launch
                    if (snapshot.selectedThreadId() == latest.selectedThreadId()) conversation = view
                    if (snapshot === latest) return@launch
                }
            }
        }
    }

    fun persist() {
        val id = profileId ?: return
        val current = owner?.snapshot() ?: return
        scope.launch { writes.send(id to current) }
    }

    suspend fun download(path: String, destination: String) {
        val store = owner ?: error("Host not connected")
        val host = profileId
        store.downloadFile(path, destination)
        if (host != profileId) throw CancellationException("Host changed")
    }

    suspend fun downloadAttachment(id: String, destination: String) {
        val store = owner ?: error("Host not connected")
        val host = profileId
        store.downloadAttachment(id, destination)
        if (host != profileId) throw CancellationException("Host changed")
    }

    suspend fun upload(source: String, directory: String, name: String): String {
        val store = owner ?: error("Host not connected")
        val host = profileId
        val result = store.uploadFile(source, directory, name)
        if (host != profileId) throw CancellationException("Host changed")
        return result
    }

    suspend fun browser(request: BrowserRequest): BrowserFrame {
        val store = owner ?: error("Host not connected")
        val host = profileId
        val result = store.browser(request)
        if (host != profileId) throw CancellationException("Host changed")
        return result
    }

    override fun onCleared() {
        observation?.cancel()
        connection?.cancel()
        initialization?.cancel()
        persistence?.cancel()
        scope.launch {
            operations.toList().joinAll()
            owner?.let { store ->
                runCatching { store.shutdown() }
                profileId?.let { writes.send(it to store.snapshot()) }
            }
            writes.close()
            writer.join()
            scope.cancel()
        }
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
// Declarative native layout; the conversation decisions are supplied by core.
@Suppress("LongMethod", "CyclomaticComplexMethod")
internal fun RemoteAgentApp(
    activity: ComponentActivity,
    model: AndroidAppModel,
    requestQrScan: (onContents: (String) -> Unit) -> Unit,
) {
    DisposableEffect(model, activity) {
        val observer = AndroidConnectionLifecycle(model::connect, model::persist)
        activity.lifecycle.addObserver(observer)
        onDispose { activity.lifecycle.removeObserver(observer) }
    }
    BackHandler(model.screen != Screen.Hosts) {
        if (model.screen == Screen.Conversation) model.showThreads() else model.showHosts()
    }
    AppMaterialTheme {
        model.deleteThreadId?.let { id ->
            AlertDialog(
                onDismissRequest = { model.deleteThreadId = null },
                title = { Text("Delete thread?") },
                text = { Text("This permanently deletes the conversation.") },
                confirmButton = {
                    TextButton(
                        onClick = {
                            val wasOpen = model.snapshot.selectedThreadId() == id
                            model.perform(Intent.Thread(id, ThreadAction.Delete)) { result ->
                                val selected = model.snapshot.selectedThreadId()
                                val stillOnDeletedThread = selected == id || selected == null
                                if (result.isSuccess && wasOpen) {
                                    if (model.screen == Screen.Conversation && stillOnDeletedThread) model.showThreads()
                                }
                            }
                            model.deleteThreadId = null
                        }
                    ) {
                        Text("Delete")
                    }
                },
                dismissButton = { TextButton(onClick = { model.deleteThreadId = null }) { Text("Cancel") } },
            )
        }
        Scaffold(
            topBar = {
                TopAppBar(
                    expandedHeight = 48.dp,
                    title = {
                        Text(
                            if (model.screen == Screen.Conversation) model.conversation.title
                            else model.profiles.firstOrNull { it.id == model.profileId }?.name ?: "Bex"
                        )
                    },
                    navigationIcon = {
                        if (model.screen != Screen.Hosts)
                            TextButton(
                                onClick = {
                                    if (model.screen == Screen.Conversation) model.showThreads() else model.showHosts()
                                }
                            ) {
                                Text("‹")
                            }
                    },
                )
            }
        ) { padding ->
            Column(Modifier.padding(padding).fillMaxSize()) {
                (model.notice ?: model.snapshot.error())?.let {
                    Text(it, color = AppTheme.color("errorForeground"), modifier = Modifier.padding(12.dp))
                }
                if (model.busy) LinearProgressIndicator(Modifier.fillMaxWidth())
                if (!model.snapshot.connected() && model.profileId != null && model.screen != Screen.Pairing)
                    TextButton(onClick = model::connect, enabled = !model.busy) { Text("Reconnect") }
                when {
                    model.screen == Screen.Pairing || model.profiles.isEmpty() -> PairingScreen(model, requestQrScan)
                    model.screen == Screen.Hosts -> ProfilesScreen(model)
                    model.screen == Screen.Threads -> ThreadListScreen(model, Modifier.weight(1f))
                    else -> ThreadDetailScreen(model, Modifier.weight(1f))
                }
            }
        }
    }
}

@Composable
private fun PairingScreen(model: AndroidAppModel, scan: (onContents: (String) -> Unit) -> Unit) {
    var contents by remember { mutableStateOf("") }
    Column(Modifier.fillMaxSize().padding(24.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
        Text("PCとペアリング", style = MaterialTheme.typography.headlineMedium)
        val invitation = model.invitation
        if (invitation == null) {
            Text("PC Host Manager の QR コードを読み取ります。")
            Button(onClick = { scan { model.preparePairing(it) } }) { Text("QRコードを読み取る") }
            OutlinedTextField(
                contents,
                { contents = it },
                Modifier.fillMaxWidth(),
                label = { Text("接続情報") },
                minLines = 3,
            )
            Button(
                onClick = {
                    model.preparePairing(contents)
                    contents = ""
                },
                enabled = contents.isNotBlank() && !model.busy,
            ) {
                Text("接続先を確認")
            }
        } else {
            Text(invitation.hostName, style = MaterialTheme.typography.titleLarge)
            Text("メッセージと作業に必要な内容をこの PC と AI サービスに送信します。")
            Text("AI処理: ${invitation.aiRecipients.joinToString("、")}")
            invitation.transcriptionRecipient?.let { Text("音声入力: $it") }
            Button(onClick = model::pair, enabled = !model.busy) { Text("同意して接続") }
            TextButton(onClick = model::openPairing) { Text("変更") }
        }
        TextButton(onClick = model::showHosts) { Text("戻る") }
    }
}

@Composable
private fun ProfilesScreen(model: AndroidAppModel) {
    LazyColumn(Modifier.fillMaxSize(), contentPadding = PaddingValues(20.dp)) {
        item { Button(onClick = model::openPairing) { Text("PCを追加") } }
        items(model.profiles, key = { it.id }) { profile ->
            Card(
                onClick = { model.selectProfile(profile.id) },
                modifier = Modifier.fillMaxWidth().padding(vertical = 4.dp),
            ) {
                Row(Modifier.padding(16.dp), horizontalArrangement = Arrangement.SpaceBetween) {
                    Text(profile.name, Modifier.weight(1f))
                    TextButton(onClick = { model.removeProfile(profile.id) }) { Text("Remove") }
                }
            }
        }
    }
}

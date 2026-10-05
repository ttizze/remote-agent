package dev.remoteagent.mobile

import android.content.Context
import androidx.activity.ComponentActivity
import androidx.activity.compose.BackHandler
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import androidx.lifecycle.ViewModel
import dev.remoteagent.core.*
import kotlinx.coroutines.*
import kotlinx.coroutines.channels.Channel

internal enum class Screen {
    Hosts,
    Pairing,
    Threads,
    Conversation,
}

internal class AndroidAppModel(private val context: Context) : ViewModel() {
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)
    private val repository = AndroidMobileRepository(context)
    var snapshot by mutableStateOf(Snapshot.empty())
        private set

    val conversation
        get() = snapshot.conversation()

    var profiles by mutableStateOf(emptyList<HostProfile>())
        private set

    var profileId by mutableStateOf<String?>(null)
        private set

    var screen by mutableStateOf(Screen.Hosts)
    var busy by mutableStateOf(false)
        private set

    var notice by mutableStateOf<String?>(null)
    var invitation by mutableStateOf<Invitation?>(null)
        private set

    var composerText by mutableStateOf("")
        private set

    private var draftRevision = 0L
    private var pendingDraft: Long? = null
    private var owner: AgentStore? = null
    private var initialization: Job? = null
    private var connection: Job? = null
    private var observation: Job? = null
    private var persistence: Job? = null
    private val pending = ArrayDeque<Pair<Intent, (Result<Outcome>) -> Unit>>()
    private val operations = mutableSetOf<Job>()
    private val writes = Channel<Pair<String, Snapshot>>(8)
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
            if (initialization != null && pending.size < 64) pending.addLast(intent to complete)
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
                    if (host == profileId) {
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
        draftRevision += 1
        pendingDraft = null
    }

    fun editDraft(text: String) {
        composerText = text
        val revision = ++draftRevision
        pendingDraft = revision
        perform(Intent.EditDraft(snapshot.draft().copy(text = text))) {
            if (pendingDraft == revision) {
                pendingDraft = null
                composerText = snapshot.draft().text
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
        runCatching { parseInvitation(contents, (System.currentTimeMillis() / 1000).toULong()) }
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
                val id = validateInvitation(target, (System.currentTimeMillis() / 1000).toULong())
                val identity =
                    withContext(Dispatchers.IO) { AndroidCredentialStore(context, id).loadOrCreate(::generateIdentity) }
                val bytes =
                    withContext(Dispatchers.IO) { applyModelPreferences(byteArrayOf(), repository.modelPreferences()) }
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
                if (profileId == profile.id) {
                    publish(store.snapshot())
                    busy = false
                }
            } catch (error: CancellationException) {
                throw error
            } catch (error: Exception) {
                if (profileId == profile.id) {
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
                    if (profileId != id) return@launch
                    val latest = store.snapshot()
                    publish(latest)
                    if (previous.connected() && !latest.connected() && !busy) connect()
                    previous = latest
                }
            } catch (error: CancellationException) {
                throw error
            } catch (error: Exception) {
                if (profileId == id) notice = error.message
            }
        }
    }

    private fun publish(next: Snapshot) {
        val name = next.hostName()
        if (name != null && profiles.any { it.id == profileId && it.name != name }) {
            profiles = profiles.map { if (it.id == profileId) it.copy(name = name) else it }
            repository.saveProfiles(profiles)
        }
        snapshot = next
        if (pendingDraft == null) composerText = next.draft().text
        persistence?.cancel()
        persistence = scope.launch {
            delay(250)
            persist()
        }
    }

    fun persist() {
        val id = profileId ?: return
        val current = owner?.snapshot() ?: snapshot
        scope.launch { writes.send(id to current) }
    }

    suspend fun download(path: String, destination: String) {
        val store = owner ?: error("Host not connected")
        val host = profileId
        store.downloadFile(path, destination)
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
    T3Theme {
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
                    Text(it, color = T3.color("errorForeground"), modifier = Modifier.padding(12.dp))
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

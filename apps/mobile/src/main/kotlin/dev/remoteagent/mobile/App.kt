package dev.remoteagent.mobile

import android.content.Context
import android.net.Uri
import androidx.activity.ComponentActivity
import androidx.activity.compose.BackHandler
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import androidx.lifecycle.ViewModel
import dev.remoteagent.core.AgentException
import dev.remoteagent.core.AgentStore
import dev.remoteagent.core.Connection
import dev.remoteagent.core.Intent
import dev.remoteagent.core.ListThreads
import dev.remoteagent.core.Outcome
import dev.remoteagent.core.Snapshot
import dev.remoteagent.core.ThreadList
import dev.remoteagent.core.UploadAttachment
import dev.remoteagent.core.generateIdentity
import dev.remoteagent.core.parseInvitation
import dev.remoteagent.core.ticketIdentity
import java.io.File
import java.io.IOException
import java.security.GeneralSecurityException
import kotlin.time.Duration.Companion.milliseconds
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.Deferred
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.async
import kotlinx.coroutines.cancel
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.joinAll
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import kotlinx.serialization.SerializationException

internal enum class Screen {
    Hosts,
    Pairing,
    Threads,
    Conversation,
}

/** One core Snapshot feeds Compose; all business changes are queued typed intents. */
internal class AndroidAppModel(private val context: Context) : ViewModel() {
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)
    private val repository = AndroidMobileRepository(context)
    var snapshot by mutableStateOf(Snapshot.empty())
        private set

    var conversation by mutableStateOf<ConversationProjection?>(null)
        private set

    var profiles by mutableStateOf(emptyList<HostProfile>())
        private set

    var profileId by mutableStateOf<String?>(null)
        private set

    var screen by mutableStateOf(Screen.Hosts)
    var busy by mutableStateOf(false)
    var notice by mutableStateOf<String?>(null)
    var loadingHistory by mutableStateOf(false)
    var list by mutableStateOf<ThreadList?>(null)
        private set

    private var owner: AgentStore? = null
    private var initialization: Job? = null
    private var connection: Job? = null
    private var observation: Job? = null
    private var persistence: Job? = null
    private val writes = Channel<Pair<String, Snapshot>>(Channel.UNLIMITED)
    private val writer =
        scope.launch(Dispatchers.IO) {
            for ((id, current) in writes) {
                try {
                    repository.save(id, current.serialize())
                } catch (error: IOException) {
                    withContext(Dispatchers.Main) { notice = error.message }
                }
            }
        }
    private val operations = mutableSetOf<Deferred<Outcome>>()
    private val flushes = mutableSetOf<Job>()
    private val pending = ArrayDeque<Pair<Intent, (Result<Outcome>) -> Unit>>()
    val draftKey
        get() = snapshot.navigation().draftKey

    val selectionKey
        get() = "$profileId:$draftKey"

    init {
        try {
            profiles = repository.profiles()
            repository.selected?.takeIf { id -> profiles.any { it.id == id } }?.let(::selectProfile)
        } catch (error: SerializationException) {
            notice = error.message
        }
    }

    fun perform(intent: Intent, complete: (Result<Outcome>) -> Unit = {}) {
        val store = owner
        if (store == null) {
            if (initialization != null) pending.addLast(intent to complete)
            else complete(Result.failure(IllegalStateException("Host not connected")))
            return
        }
        val receipt =
            try {
                store.dispatch(intent)
            } catch (error: AgentException) {
                complete(Result.failure(error))
                return
            }
        publish(store.snapshot())
        val host = profileId
        val operation = scope.async(start = CoroutineStart.LAZY) { receipt.wait() }
        operations.add(operation)
        scope.launch {
            val result = runCatching { operation.await() }
            operations.remove(operation)
            if (host == profileId) publish(store.snapshot())
            complete(result)
        }
    }

    fun selectProfile(id: String) {
        screen = Screen.Threads
        if (profileId == id && owner != null) {
            connect()
            return
        }
        persist()
        initialization?.cancel()
        observation?.cancel()
        connection?.cancel()
        val old = owner
        owner = null
        while (pending.isNotEmpty()) pending.removeFirst().second(Result.failure(CancellationException()))
        profileId = id
        repository.selected = id
        busy = false
        initialization = scope.launch {
            old?.shutdown()
            try {
                val bytes = withContext(Dispatchers.IO) { repository.load(id) }
                val store = AgentStore.offline(bytes)
                if (profileId != id || !isActive) {
                    store.shutdown()
                    return@launch
                }
                owner = store
                initialization = null
                publish(store.snapshot())
                perform(Intent.ShowThreadList)
                while (pending.isNotEmpty()) {
                    val (intent, complete) = pending.removeFirst()
                    perform(intent, complete)
                }
                observe(store, id)
                connect()
            } catch (error: IOException) {
                failInitialization(id, error)
            } catch (error: AgentException) {
                failInitialization(id, error)
            }
        }
    }

    private fun failInitialization(id: String, error: Exception) {
        if (profileId == id) {
            initialization = null
            notice = error.message
            while (pending.isNotEmpty()) pending.removeFirst().second(Result.failure(error))
        }
    }

    fun pair(contents: String) {
        connection?.cancel()
        busy = true
        notice = null
        connection = scope.launch {
            try {
                val invitation =
                    parseInvitation(contents, System.currentTimeMillis().milliseconds.inWholeSeconds.toULong())
                val id = ticketIdentity(invitation.endpoint)
                val identity =
                    withContext(Dispatchers.IO) { AndroidCredentialStore(context, id).loadOrCreate(::generateIdentity) }
                val store =
                    try {
                        AgentStore.connect(
                            Connection(invitation.endpoint, identity, invitation.invitation, true),
                            byteArrayOf(),
                        )
                    } finally {
                        identity.fill(0)
                    }
                persist()
                initialization?.cancel()
                observation?.cancel()
                owner?.shutdown()
                profiles = profiles.filterNot { it.id == id } + HostProfile(id, "PC Host", invitation.endpoint)
                repository.saveProfiles(profiles)
                repository.selected = id
                profileId = id
                owner = store
                publish(store.snapshot())
                observe(store, id)
                screen = Screen.Threads
                busy = false
            } catch (error: AgentException) {
                busy = false
                notice = error.message
            } catch (error: IOException) {
                busy = false
                notice = error.message
            } catch (error: GeneralSecurityException) {
                busy = false
                notice = error.message
            } catch (error: IllegalArgumentException) {
                busy = false
                notice = error.message
            }
        }
    }

    fun connect() {
        val store = owner
        val profile = profiles.firstOrNull { it.id == profileId }
        if (store == null || profile == null || busy) return
        busy = true
        connection = scope.launch {
            try {
                val identity =
                    withContext(Dispatchers.IO) {
                        AndroidCredentialStore(context, profile.id).loadOrCreate(::generateIdentity)
                    }
                try {
                    store.reconnect(Connection(profile.ticket, identity, null, true))
                } finally {
                    identity.fill(0)
                }
                if (profileId != profile.id) return@launch
                publish(store.snapshot())
                notice = null
                busy = false
            } catch (error: AgentException) {
                connectionFailed(profile.id, error)
            } catch (error: IOException) {
                connectionFailed(profile.id, error)
            } catch (error: GeneralSecurityException) {
                connectionFailed(profile.id, error)
            } catch (error: IllegalArgumentException) {
                connectionFailed(profile.id, error)
            }
        }
    }

    private fun observe(store: AgentStore, id: String) {
        val initial = snapshot
        observation = scope.launch {
            var previous = initial
            while (isActive) {
                store.nextSnapshot(previous)
                if (profileId != id) return@launch
                val latest = store.snapshot()
                publish(latest)
                if (previous.connected() && !latest.connected() && !busy) connect()
                previous = latest
            }
        }
    }

    private fun publish(next: Snapshot) {
        if (!next.listUnchanged(snapshot)) list = next.threadList()
        conversation = projectConversationRows(next, next.navigation().threadId?.let(next::conversation), conversation)
        snapshot = next
        persistence?.cancel()
        persistence = scope.launch {
            delay(250.milliseconds)
            persist()
        }
    }

    fun attach(selection: String, uri: Uri, complete: () -> Unit) {
        if (selection != selectionKey) {
            complete()
            return
        }
        val key = draftKey
        val directory = snapshot.navigation().cwd
        scope.launch {
            var local: File? = null
            var uploading = false
            try {
                val attachment =
                    withContext(Dispatchers.IO) { importAttachment(context, uri).also { local = File(it.path) } }
                if (selection != selectionKey) return@launch
                uploading = true
                perform(Intent.UploadAttachment(UploadAttachment(key, attachment, directory))) {
                    local?.parentFile?.deleteRecursively()
                    complete()
                }
            } catch (error: SecurityException) {
                if (selection == selectionKey) notice = error.message
            } catch (error: IOException) {
                if (selection == selectionKey) notice = error.message
            } finally {
                if (!uploading) {
                    local?.parentFile?.deleteRecursively()
                    complete()
                }
            }
        }
    }

    fun persist() {
        val id = profileId ?: return
        val store = owner
        val current = snapshot
        val receipts = operations.toList()
        lateinit var flush: Job
        flush =
            scope.launch(start = CoroutineStart.LAZY) {
                try {
                    for (receipt in receipts) runCatching { receipt.await() }
                    writes.send(id to (store?.snapshot() ?: current))
                } finally {
                    flushes.remove(flush)
                }
            }
        flushes.add(flush)
        flush.start()
    }

    override fun onCleared() {
        observation?.cancel()
        connection?.cancel()
        initialization?.cancel()
        persistence?.cancel()
        scope.launch {
            for (operation in operations.toList()) runCatching { operation.await() }
            persistence?.cancel()
            flushes.toList().joinAll()
            val store = owner
            if (store != null) {
                runCatching { store.shutdown() }
                profileId?.let { writes.send(it to store.snapshot()) }
            }
            writes.close()
            writer.join()
            scope.cancel()
        }
    }
}

private fun AndroidAppModel.connectionFailed(id: String, error: Exception) {
    if (profileId == id) {
        busy = false
        notice = error.message
    }
}

internal fun AndroidAppModel.showThreads() {
    screen = Screen.Threads
    perform(Intent.ShowThreadList)
    perform(Intent.ListThreads(ListThreads(query = snapshot.listQuery())))
}

internal fun AndroidAppModel.showHosts() {
    screen = Screen.Hosts
    perform(Intent.ShowThreadList)
}

internal fun AndroidAppModel.older() {
    val id = snapshot.navigation().threadId ?: return
    loadingHistory = true
    perform(Intent.ReadOlder(id)) { loadingHistory = false }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun RemoteAgentApp(
    activity: ComponentActivity,
    model: AndroidAppModel,
    requestQrScan: ((onContents: (String) -> Unit) -> Unit)?,
) {
    DisposableEffect(model, activity) {
        val observer = AndroidConnectionLifecycle(model::connect, model::persist)
        activity.lifecycle.addObserver(observer)
        onDispose { activity.lifecycle.removeObserver(observer) }
    }
    BackHandler(model.screen != Screen.Hosts) {
        if (model.screen == Screen.Conversation) model.showThreads() else model.showHosts()
    }
    MaterialTheme {
        Scaffold(topBar = { TopAppBar(title = { Text("Remote Agent") }) }) { padding ->
            Column(Modifier.padding(padding)) {
                ConnectionStatus(
                    model.notice ?: model.snapshot.error(),
                    model.busy,
                    !model.snapshot.connected() && model.profileId != null && model.screen != Screen.Pairing,
                    model::connect,
                )
                when {
                    model.screen == Screen.Pairing || model.profiles.isEmpty() ->
                        PairingScreen(model.busy, model::pair, model::showHosts, requestQrScan)
                    model.screen == Screen.Hosts ->
                        ProfilesScreen(model.profiles, model::selectProfile) { model.screen = Screen.Pairing }
                    model.screen == Screen.Threads ->
                        ThreadListScreen(
                            model.list,
                            model.snapshot.listQuery(),
                            { model.perform(it) },
                            model::showHosts,
                            { intent ->
                                model.screen = Screen.Conversation
                                model.perform(intent)
                            },
                            Modifier.weight(1f),
                        )
                    else -> androidx.compose.runtime.key(model.selectionKey) {
                        var scrollToTopRequest by remember { mutableStateOf(0) }
                        ConversationHeader(
                            model.snapshot.navigation().threadId?.let(model.snapshot::conversation)?.title(),
                            model::showThreads,
                        ) { scrollToTopRequest += 1 }
                        ThreadDetailScreen(
                            model.snapshot,
                            model.conversation,
                            model::perform,
                            if (model.loadingHistory) null else model::older,
                            Modifier.weight(1f),
                            scrollToTopRequest = scrollToTopRequest,
                        ) { onSend ->
                            ThreadComposer(model.snapshot, model::perform, onSend) {
                                AttachmentButton(model.selectionKey, model::attach)
                            }
                        }
                    }
                }
            }
        }
    }
}

@Composable
private fun PairingScreen(
    busy: Boolean,
    pair: (String) -> Unit,
    showHosts: () -> Unit,
    scan: ((onContents: (String) -> Unit) -> Unit)?,
) {
    var contents by remember { mutableStateOf("") }
    Column(Modifier.fillMaxSize().padding(24.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
        Text("PCとペアリング", style = MaterialTheme.typography.headlineMedium)
        Text("PC Host ManagerのQRコードを読み取ります。QRの内容はこの端末に保存しません。")
        scan?.let { Button(onClick = { it { value -> contents = value } }) { Text("QRコードを読み取る") } }
        OutlinedTextField(
            contents,
            { contents = it },
            Modifier.fillMaxWidth(),
            label = { Text("ペアリングQR（手入力）") },
            minLines = 3,
        )
        Button(onClick = { pair(contents) }, enabled = contents.isNotBlank() && !busy) { Text("ペアリング") }
        Button(onClick = showHosts) { Text("戻る") }
    }
}

@Composable
private fun ProfilesScreen(profiles: List<HostProfile>, select: (String) -> Unit, pair: () -> Unit) {
    LazyColumn(Modifier.fillMaxSize(), contentPadding = PaddingValues(16.dp)) {
        item { Button(onClick = pair) { Text("PCを追加") } }
        items(profiles, key = { it.id }) { profile ->
            Card(onClick = { select(profile.id) }, modifier = Modifier.fillMaxWidth().padding(vertical = 4.dp)) {
                Column(Modifier.padding(16.dp)) {
                    Text(profile.name)
                    Text(profile.id, style = MaterialTheme.typography.bodySmall)
                }
            }
        }
    }
}

@Composable
private fun ConnectionStatus(notice: String?, busy: Boolean, reconnect: Boolean, connect: () -> Unit) {
    notice?.let { Text(it, color = MaterialTheme.colorScheme.error, modifier = Modifier.padding(12.dp)) }
    if (busy) LinearProgressIndicator(Modifier.fillMaxWidth())
    if (reconnect) Button(onClick = connect, enabled = !busy) { Text("再接続") }
}

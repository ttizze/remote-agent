package dev.remoteagent.mobile

import androidx.activity.ComponentActivity
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.Button
import androidx.compose.material3.Card
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
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import kotlin.time.Clock
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.launch

/**
 * The Android presentation receives network, storage, and QR-camera
 * capabilities explicitly while application state remains shared with iOS.
 */
@Composable
fun RemoteAgentApp(
    gateway: HostGateway = UnavailableHostGateway,
    repository: MobileRepository = InMemoryMobileRepository(),
    requestQrScan: ((onContents: (String) -> Unit) -> Unit)? = null,
    nowMs: () -> Long = { Clock.System.now().toEpochMilliseconds() },
) {
    val scope = androidx.compose.runtime.rememberCoroutineScope()
    val controller = remember(gateway, repository) { MobileController(gateway, repository) }
    var state by remember(controller) { mutableStateOf(controller.state) }
    DisposableEffect(controller) {
        val observation = controller.observe { state = it }
        onDispose(observation::cancel)
    }
    val activity = LocalContext.current as? ComponentActivity
    if (activity != null) {
        DisposableEffect(controller, activity) {
            val lifecycleObserver = AndroidConnectionLifecycle(
                onForeground = {
                    val current = controller.state
                    val profile = current.selectedProfile
                    if (profile != null && !current.showingPairing && current.connection != ConnectionPhase.Connecting) {
                        scope.launch { controller.connect(profile, scope) }
                    }
                },
                // Keep the authenticated transport alive while the Activity is
                // backgrounded. Reconnect on the next foreground event.
                onBackground = {},
            )
            activity.lifecycle.addObserver(lifecycleObserver)
            onDispose { activity.lifecycle.removeObserver(lifecycleObserver) }
        }
    }
    MaterialTheme {
        Scaffold(topBar = { TopAppBar(title = { Text("Remote Agent") }) }) { padding ->
            AppContent(
                state = state,
                controller = controller,
                scope = scope,
                requestQrScan = requestQrScan,
                nowMs = nowMs,
                modifier = Modifier.padding(padding),
            )
        }
    }
}

@Composable
private fun AppContent(
    state: AppState,
    controller: MobileController,
    scope: CoroutineScope,
    requestQrScan: ((onContents: (String) -> Unit) -> Unit)?,
    nowMs: () -> Long,
    modifier: Modifier = Modifier,
) {
    when {
        state.showingPairing || state.profiles.isEmpty() -> PairingScreen(
            pairingError = state.pairingError,
            onPair = { contents -> scope.launch { controller.pair(contents, nowMs()) } },
            onRequestScan = requestQrScan,
            onCancel = { controller.dispatch(AppAction.PairingDismissed) },
            modifier = modifier,
        )

        state.selectedProfile == null -> HostSelectionScreen(
            state = state,
            onSelect = { controller.dispatch(AppAction.ProfileSelected(it)) },
            onAddProfile = { controller.dispatch(AppAction.PairingOpened) },
            modifier = modifier,
        )

        else -> HostFlowScreen(state, controller, scope, modifier)
    }
}

@Composable
private fun PairingScreen(
    pairingError: String?,
    onPair: (String) -> Unit,
    onRequestScan: ((onContents: (String) -> Unit) -> Unit)?,
    onCancel: () -> Unit,
    modifier: Modifier,
) {
    var contents by remember { mutableStateOf("") }
    Column(
        modifier = modifier.fillMaxSize().padding(24.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Text("PCとペアリング", style = MaterialTheme.typography.headlineMedium)
        Text("PC Host ManagerのQRコードを読み取ります。QRの内容はこの端末に保存しません。")
        if (onRequestScan != null) {
            Button(onClick = { onRequestScan { scanned -> contents = scanned } }) { Text("QRコードを読み取る") }
        }
        OutlinedTextField(
            value = contents,
            onValueChange = { contents = it },
            modifier = Modifier.fillMaxWidth(),
            label = { Text("ペアリングQR（手入力）") },
            minLines = 3,
        )
        Button(onClick = { onPair(contents) }, enabled = contents.isNotBlank()) { Text("ペアリング") }
        pairingError?.let { Text(it, color = MaterialTheme.colorScheme.error) }
        Button(onClick = onCancel) { Text("戻る") }
    }
}

@Composable
private fun HostSelectionScreen(
    state: AppState,
    onSelect: (String) -> Unit,
    onAddProfile: () -> Unit,
    modifier: Modifier,
) {
    LazyColumn(
        modifier = modifier.fillMaxSize(),
        contentPadding = PaddingValues(16.dp),
        verticalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        item { Button(onClick = onAddProfile) { Text("PCを追加") } }
        items(state.profiles, key = { it.hostIdentity }) { profile ->
            Card(modifier = Modifier.fillMaxWidth(), onClick = { onSelect(profile.hostIdentity) }) {
                Column(modifier = Modifier.padding(16.dp)) {
                    Text(profile.name, style = MaterialTheme.typography.titleMedium)
                    Text(profile.hostIdentity, style = MaterialTheme.typography.bodySmall)
                }
            }
        }
    }
}

@Composable
private fun HostFlowScreen(
    state: AppState,
    controller: MobileController,
    scope: CoroutineScope,
    modifier: Modifier,
) {
    val profile = requireNotNull(state.selectedProfile)
    val view = state.selectedView
    val cache = state.cache.profile(profile.hostIdentity)
    when (view.connection) {
        ConnectionPhase.Disconnected -> ConnectScreen(
            profile = profile,
            onDiscover = { scope.launch { controller.discover(profile) } },
            onConnect = { scope.launch { controller.connect(profile, scope) } },
            onBack = { controller.dispatch(AppAction.ProfileSelectionOpened) },
            modifier = modifier,
        )

        ConnectionPhase.Connecting -> LoadingScreen("PC Hostへ接続中…", modifier)
        is ConnectionPhase.Failed -> ConnectScreen(
            profile = profile,
            error = view.connection.message,
            onDiscover = { scope.launch { controller.discover(profile) } },
            onConnect = { scope.launch { controller.connect(profile, scope) } },
            onBack = { controller.dispatch(AppAction.ProfileSelectionOpened) },
            modifier = modifier,
        )

        ConnectionPhase.Connected -> {
            // A connection always opens on the task surface. The working
            // directory is only a list filter, so an empty value means all
            // tasks rather than a separate navigation gate.
            val selectedThreadId = view.selectedThreadId
            if (selectedThreadId == null) {
                ThreadListScreen(
                    profile = profile,
                    view = view,
                    threads = cache.threadList,
                    onFilterApply = { path ->
                        controller.dispatch(AppAction.WorkingDirectoryChanged(profile.hostIdentity, path))
                        scope.launch { controller.listThreads(profile) }
                    },
                    onRefresh = { scope.launch { controller.listThreads(profile) } },
                    onStart = { path ->
                        controller.dispatch(AppAction.WorkingDirectoryChanged(profile.hostIdentity, path))
                        scope.launch { controller.startThread(profile) }
                    },
                    onSelect = { threadId -> scope.launch { controller.readThread(profile, threadId) } },
                    modifier = modifier,
                )
            } else {
                ThreadDetailScreen(
                    profile = profile,
                    view = view,
                    snapshot = cache.snapshots[selectedThreadId],
                    onBack = { controller.dispatch(AppAction.ThreadListOpened(profile.hostIdentity)) },
                    onSend = { text -> scope.launch { controller.startTurn(profile, selectedThreadId, text) } },
                    onStop = { turnId -> scope.launch { controller.interrupt(profile, selectedThreadId, turnId) } },
                    modifier = modifier,
                )
            }
        }
    }
}

@Composable
private fun ConnectScreen(
    profile: HostProfile,
    error: String? = null,
    onDiscover: () -> Unit,
    onConnect: () -> Unit,
    onBack: () -> Unit,
    modifier: Modifier,
) {
    Column(modifier = modifier.fillMaxSize().padding(24.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
        Text(profile.name, style = MaterialTheme.typography.headlineMedium)
        Text(if (profile.addresses.isEmpty()) "保存済みアドレスなし" else profile.addresses.joinToString())
        error?.let { Text(it, color = MaterialTheme.colorScheme.error) }
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            Button(onClick = onDiscover) { Text("検出") }
            Button(onClick = onConnect) { Text("接続") }
        }
        Button(onClick = onBack) { Text("PC一覧") }
    }
}

@Composable
private fun ThreadListScreen(
    profile: HostProfile,
    view: ProfileViewState,
    threads: List<ThreadSummary>,
    onFilterApply: (String) -> Unit,
    onRefresh: () -> Unit,
    onStart: (String) -> Unit,
    onSelect: (String) -> Unit,
    modifier: Modifier,
) {
    var filter by remember(profile.hostIdentity, view.workingDirectoryPath) {
        mutableStateOf(view.workingDirectoryPath)
    }
    LazyColumn(
        modifier = modifier.fillMaxSize(),
        contentPadding = PaddingValues(16.dp),
        verticalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        item {
            Text("タスク", style = MaterialTheme.typography.headlineSmall)
            Text(profile.name, style = MaterialTheme.typography.bodyMedium)
            OutlinedTextField(
                value = filter,
                onValueChange = { filter = it },
                modifier = Modifier.fillMaxWidth(),
                label = { Text("作業ディレクトリ（任意の絞り込み）") },
                placeholder = { Text("/Users/name/project") },
                singleLine = true,
            )
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                Button(onClick = { onFilterApply(filter.trim()) }, enabled = view.threadList != LoadPhase.Loading) {
                    Text("適用")
                }
                Button(onClick = onRefresh, enabled = view.threadList != LoadPhase.Loading) { Text("更新") }
            }
            Button(
                onClick = { onStart(filter.trim()) },
                enabled = filter.isNotBlank() && view.threadList != LoadPhase.Loading,
            ) { Text("新しいタスク") }
            if (filter.isBlank()) {
                Text("作業ディレクトリ未指定では、すべてのタスクを表示します。")
                Text("新しいタスクを作成するには作業ディレクトリを入力してください。")
            }
            when (val phase = view.threadList) {
                LoadPhase.Idle -> Text("適用を押してタスクを読み込みます。")
                LoadPhase.Loading -> Text("タスクを読み込み中…")
                LoadPhase.Ready -> if (threads.isEmpty()) Text("タスクがありません。")
                is LoadPhase.Failed -> {
                    Text(phase.message, color = MaterialTheme.colorScheme.error)
                    Button(onClick = onRefresh) { Text("再試行") }
                }
            }
        }
        items(threads, key = { it.id }) { thread ->
            Card(modifier = Modifier.fillMaxWidth(), onClick = { onSelect(thread.id) }) {
                Column(modifier = Modifier.padding(16.dp)) {
                    Text(thread.name ?: thread.preview.ifBlank { "無題のタスク" })
                    Text(thread.workingDirectory.path, style = MaterialTheme.typography.bodySmall)
                }
            }
        }
    }
}

@Composable
private fun ThreadDetailScreen(
    profile: HostProfile,
    view: ProfileViewState,
    snapshot: ThreadSnapshot?,
    onBack: () -> Unit,
    onSend: (String) -> Unit,
    onStop: (String) -> Unit,
    modifier: Modifier,
) {
    var composer by remember(profile.hostIdentity, view.selectedThreadId) { mutableStateOf("") }
    LazyColumn(
        modifier = modifier.fillMaxSize(),
        contentPadding = PaddingValues(16.dp),
        verticalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        item {
            Button(onClick = onBack) { Text("タスク一覧") }
            Text(snapshot?.summary?.name ?: snapshot?.summary?.preview ?: "タスクを読み込み中…", style = MaterialTheme.typography.headlineSmall)
            if (view.threadDetail is LoadPhase.Failed) Text(view.threadDetail.message, color = MaterialTheme.colorScheme.error)
        }
        snapshot?.turns?.forEach { turn ->
            item(key = turn.id) {
                Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
                    Text(turn.status.name, style = MaterialTheme.typography.labelMedium)
                    turn.items.forEach { item -> Text(item.displayText()) }
                    if (turn.status == TurnStatus.InProgress) {
                        Button(
                            onClick = { onStop(turn.id) },
                            enabled = view.interruptingTurnId != turn.id,
                        ) { Text(if (view.interruptingTurnId == turn.id) "停止中…" else "停止") }
                    }
                }
            }
        }
        item {
            OutlinedTextField(
                value = composer,
                onValueChange = { composer = it },
                modifier = Modifier.fillMaxWidth(),
                label = { Text("Codexへの入力") },
                minLines = 3,
            )
            Button(
                onClick = { onSend(composer); composer = "" },
                enabled = composer.isNotBlank() && snapshot != null,
            ) { Text("送信") }
            view.notice?.let { Text(it, color = MaterialTheme.colorScheme.error) }
        }
    }
}

@Composable
private fun LoadingScreen(text: String, modifier: Modifier) {
    Column(modifier = modifier.fillMaxSize().padding(24.dp)) { Text(text) }
}

private fun CodexItem.displayText(): String = when (this) {
    is CodexItem.UserMessage -> "You: $text"
    is CodexItem.AgentMessage -> "Codex: $text"
    is CodexItem.Reasoning -> "Reasoning: $summary"
    is CodexItem.CommandExecution -> "$ $command\n$output"
    is CodexItem.FileChange -> changes.joinToString("\n") { "${it.kind}: ${it.path}" }
    is CodexItem.Unknown -> "Codex item ($codexType): ${raw}"
}

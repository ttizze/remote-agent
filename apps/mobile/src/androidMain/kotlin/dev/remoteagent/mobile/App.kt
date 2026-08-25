package dev.remoteagent.mobile

import androidx.activity.ComponentActivity
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.snapshotFlow
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
            // A connection opens on the Host's Desktop Project/App Server
            // Thread projection. Membership still arrives as Thread.projectId.
            val selectedThreadId = view.selectedThreadId
            if (selectedThreadId == null) {
                ThreadListScreen(
                    profile = profile,
                    view = view,
                    projects = cache.projects,
                    threads = cache.threadList,
                    onRefresh = {
                        scope.launch {
                            controller.listProjects(profile)
                            controller.listThreads(profile)
                        }
                    },
                    onStart = { cwd, prompt ->
                        scope.launch { controller.startThread(profile, cwd, prompt) }
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
                    onRetry = { scope.launch { controller.readThread(profile, selectedThreadId) } },
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
    projects: List<CodexProject>,
    threads: List<ThreadSummary>,
    onRefresh: () -> Unit,
    onStart: (String, String) -> Unit,
    onSelect: (String) -> Unit,
    modifier: Modifier,
) {
    var newTaskTarget by remember(profile.hostIdentity) { mutableStateOf<NewTaskTarget?>(null) }
    val knownProjectIds = projects.mapTo(mutableSetOf()) { it.id }
    val unassigned = threads.filter { it.projectId == null || it.projectId !in knownProjectIds }
    val listPhase = when {
        view.projectList is LoadPhase.Failed -> view.projectList
        view.threadList is LoadPhase.Failed -> view.threadList
        view.projectList == LoadPhase.Loading || view.threadList == LoadPhase.Loading -> LoadPhase.Loading
        view.projectList == LoadPhase.Idle || view.threadList == LoadPhase.Idle -> LoadPhase.Idle
        else -> LoadPhase.Ready
    }
    LazyColumn(
        modifier = modifier.fillMaxSize(),
        contentPadding = PaddingValues(16.dp),
        verticalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        item {
            Text("プロジェクト", style = MaterialTheme.typography.headlineSmall)
            Text(profile.name, style = MaterialTheme.typography.bodyMedium)
            Button(
                onClick = onRefresh,
                enabled = view.threadList != LoadPhase.Loading && view.projectList != LoadPhase.Loading,
            ) { Text("更新") }
            view.notice?.let { Text(it, color = MaterialTheme.colorScheme.error) }
            when (val phase = listPhase) {
                LoadPhase.Idle -> Text("Codexのプロジェクトとタスクを読み込みます。")
                LoadPhase.Loading -> Text("プロジェクトとタスクを読み込み中…")
                LoadPhase.Ready -> Unit
                is LoadPhase.Failed -> {
                    Text(phase.message, color = MaterialTheme.colorScheme.error)
                    Button(onClick = onRefresh) { Text("再試行") }
                }
            }
        }
        projects.forEach { project ->
            item(key = "project-${project.id}") {
                Row(modifier = Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    Text("📁 ${project.name}", style = MaterialTheme.typography.titleMedium)
                    Spacer(Modifier.weight(1f))
                    TextButton(onClick = { newTaskTarget = NewTaskTarget(project) }) { Text("新規") }
                }
            }
            items(threads.filter { it.projectId == project.id }, key = { it.id }) { thread ->
                ThreadSummaryRow(thread, onSelect)
            }
        }
        item(key = "unassigned-header") {
            Row(modifier = Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                Text("チャット", style = MaterialTheme.typography.titleMedium)
                Spacer(Modifier.weight(1f))
                TextButton(onClick = { newTaskTarget = NewTaskTarget(null) }) { Text("新規") }
            }
        }
        items(unassigned, key = { it.id }) { thread ->
            ThreadSummaryRow(thread, onSelect)
        }
        if (view.threadList == LoadPhase.Ready && threads.isEmpty()) {
            item { Text("タスクがありません。", color = MaterialTheme.colorScheme.onSurfaceVariant) }
        }
    }
    newTaskTarget?.let { target ->
        NewTaskDialog(
            target = target,
            onDismiss = { newTaskTarget = null },
            onStart = { cwd, prompt ->
                newTaskTarget = null
                onStart(cwd, prompt)
            },
        )
    }
}

@Composable
private fun ThreadSummaryRow(thread: ThreadSummary, onSelect: (String) -> Unit) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .clickable(onClick = { onSelect(thread.id) })
            .padding(vertical = 8.dp),
        verticalAlignment = androidx.compose.ui.Alignment.CenterVertically,
    ) {
        Text(
            thread.name ?: thread.preview.ifBlank { "無題のタスク" },
            modifier = Modifier.weight(1f),
        )
        if (thread.status is ThreadStatus.Active) {
            CircularProgressIndicator(
                modifier = Modifier.size(18.dp),
                strokeWidth = 2.dp,
            )
        }
    }
}

private data class NewTaskTarget(val project: CodexProject?)

@Composable
private fun NewTaskDialog(
    target: NewTaskTarget,
    onDismiss: () -> Unit,
    onStart: (String, String) -> Unit,
) {
    var cwd by remember(target) { mutableStateOf(target.project?.roots?.firstOrNull()?.path.orEmpty()) }
    var prompt by remember(target) { mutableStateOf("") }
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text("新しいタスク") },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(10.dp)) {
                Text(target.project?.name ?: "プロジェクトなし")
                if (target.project?.roots.orEmpty().size > 1) {
                    target.project?.roots.orEmpty().forEach { root ->
                        TextButton(onClick = { cwd = root.path }) {
                            Text(if (cwd == root.path) "✓ ${root.path}" else root.path)
                        }
                    }
                } else if (target.project?.roots.isNullOrEmpty()) {
                    OutlinedTextField(
                        value = cwd,
                        onValueChange = { cwd = it },
                        label = { Text("作業ディレクトリ") },
                        singleLine = true,
                    )
                } else {
                    Text(cwd, style = MaterialTheme.typography.bodySmall)
                }
                OutlinedTextField(
                    value = prompt,
                    onValueChange = { prompt = it },
                    modifier = Modifier.fillMaxWidth(),
                    label = { Text("最初のメッセージ") },
                    minLines = 4,
                )
            }
        },
        confirmButton = {
            TextButton(
                onClick = { onStart(cwd.trim(), prompt.trim()) },
                enabled = cwd.isNotBlank() && prompt.isNotBlank(),
            ) { Text("開始") }
        },
        dismissButton = { TextButton(onClick = onDismiss) { Text("キャンセル") } },
    )
}

@Composable
private fun ThreadDetailScreen(
    profile: HostProfile,
    view: ProfileViewState,
    snapshot: ThreadSnapshot?,
    onBack: () -> Unit,
    onRetry: () -> Unit,
    onSend: (String) -> Unit,
    onStop: (String) -> Unit,
    modifier: Modifier,
) {
    var composer by remember(profile.hostIdentity, view.selectedThreadId) { mutableStateOf("") }
    val listState = rememberLazyListState()
    var followingLatest by remember(profile.hostIdentity, view.selectedThreadId) { mutableStateOf(true) }
    var expandedItemIds by remember(profile.hostIdentity, view.selectedThreadId) {
        mutableStateOf(emptySet<String>())
    }
    var activityExpansionOverrides by remember(profile.hostIdentity, view.selectedThreadId) {
        mutableStateOf(emptyMap<String, Boolean>())
    }
    val turnPresentations = snapshot?.turns?.map(CodexTurn::toThreadTurnPresentation).orEmpty()
    val contentVersion = snapshot?.turns?.joinToString("|") { turn ->
        val items = turn.items.joinToString(",") { "${it.id}:${it.threadItemContentVersion()}" }
        val requests = turn.pendingRequests.joinToString(",") { "${it.id}:${it.method}:${it.params.hashCode()}" }
        "${turn.id}:${turn.status}:${turn.error?.hashCode()}:$requests:$items"
    }.orEmpty()
    val detailRowCount = turnPresentations.sumOf { turn ->
        val activityExpanded = activityExpansionOverrides[turn.id] ?: turn.activityInitiallyExpanded
        turn.userMessages.size +
            (if (turn.activitySummary == null) 0 else 1) +
            (if (activityExpanded) turn.activityItems.size + if (turn.status == TurnStatus.InProgress) 1 else 0 else 0) +
            turn.pendingRequests.size +
            (if (turn.error == null) 0 else 1) +
            turn.responses.size
    }
    LaunchedEffect(listState) {
        snapshotFlow {
            val layout = listState.layoutInfo
            listState.isScrollInProgress to
                (layout.visibleItemsInfo.lastOrNull()?.index == layout.totalItemsCount - 1)
        }.collect { (isScrolling, isAtBottom) ->
            if (isScrolling) followingLatest = isAtBottom
        }
    }
    LaunchedEffect(contentVersion) {
        if (followingLatest) {
            listState.scrollToItem(detailRowCount + 1)
        }
    }
    LazyColumn(
        state = listState,
        modifier = modifier.fillMaxSize(),
        contentPadding = PaddingValues(16.dp),
        verticalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        item {
            Button(onClick = onBack) { Text("タスク一覧") }
            Text(snapshot?.summary?.name ?: snapshot?.summary?.preview ?: "タスクを読み込み中…", style = MaterialTheme.typography.headlineSmall)
            if (view.threadDetail is LoadPhase.Failed) {
                Text(view.threadDetail.message, color = MaterialTheme.colorScheme.error)
                Button(onClick = onRetry) { Text("再試行") }
            }
        }
        turnPresentations.forEach { turn ->
            items(turn.userMessages, key = { item -> "${turn.id}:user:${item.id}" }) { item ->
                ThreadMessageCard(item = item, isUser = true)
            }
            val activityExpanded = activityExpansionOverrides[turn.id] ?: turn.activityInitiallyExpanded
            turn.activitySummary?.let { summary ->
                item(key = "${turn.id}:activity") {
                    Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                        val activityModifier = if (turn.activityCanCollapse) {
                            Modifier.fillMaxWidth().clickable {
                                activityExpansionOverrides = activityExpansionOverrides +
                                    (turn.id to !activityExpanded)
                            }
                        } else {
                            Modifier.fillMaxWidth()
                        }
                        Row(
                            modifier = activityModifier.padding(vertical = 6.dp),
                            horizontalArrangement = Arrangement.spacedBy(6.dp),
                        ) {
                            Text(summary, style = MaterialTheme.typography.labelMedium)
                            if (turn.activityCanCollapse) Text(if (activityExpanded) "⌄" else "›")
                        }
                        androidx.compose.material3.HorizontalDivider()
                    }
                }
            }
            if (activityExpanded) {
                items(turn.activityItems, key = { item -> "${turn.id}:activity:${item.id}" }) { item ->
                    val expansionKey = "${turn.id}:${item.id}"
                    ThreadActivityCard(
                        item = item,
                        isExpanded = expansionKey in expandedItemIds,
                        toggleExpanded = {
                            expandedItemIds = if (expansionKey in expandedItemIds) {
                                expandedItemIds - expansionKey
                            } else {
                                expandedItemIds + expansionKey
                            }
                        },
                    )
                }
                if (turn.status == TurnStatus.InProgress) {
                    item(key = "${turn.id}:stop") {
                        Button(
                            onClick = { onStop(turn.id) },
                            enabled = view.interruptingTurnId != turn.id,
                        ) { Text(if (view.interruptingTurnId == turn.id) "停止中…" else "停止") }
                    }
                }
            }
            items(turn.pendingRequests, key = { request -> "${turn.id}:request:${request.id}" }) { request ->
                Card(modifier = Modifier.fillMaxWidth()) {
                    Column(
                        modifier = Modifier.padding(12.dp),
                        verticalArrangement = Arrangement.spacedBy(4.dp),
                    ) {
                        Text(request.title, style = MaterialTheme.typography.labelLarge)
                        Text(request.body)
                    }
                }
            }
            turn.error?.let { error ->
                item(key = "${turn.id}:error") {
                    Card(modifier = Modifier.fillMaxWidth()) {
                        Column(
                            modifier = Modifier.padding(12.dp),
                            verticalArrangement = Arrangement.spacedBy(4.dp),
                        ) {
                            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                                if (error.isReconnecting) {
                                    CircularProgressIndicator(modifier = Modifier.size(16.dp), strokeWidth = 2.dp)
                                }
                                Text(
                                    error.title,
                                    style = MaterialTheme.typography.labelLarge,
                                    color = if (error.isReconnecting) {
                                        MaterialTheme.colorScheme.onSurface
                                    } else {
                                        MaterialTheme.colorScheme.error
                                    },
                                )
                            }
                            Text(error.message)
                            error.details?.takeIf(String::isNotBlank)?.let { details ->
                                Text(details, style = MaterialTheme.typography.bodySmall)
                            }
                        }
                    }
                }
            }
            items(turn.responses, key = { item -> "${turn.id}:response:${item.id}" }) { item ->
                ThreadMessageCard(item = item, isUser = false)
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
private fun ThreadMessageCard(item: CodexItem, isUser: Boolean) {
    val message = item.toThreadItemPresentation().collapsedBody
    if (isUser) {
        Row(modifier = Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.End) {
            Card { Text(message, modifier = Modifier.padding(12.dp)) }
        }
    } else {
        Text(message, modifier = Modifier.fillMaxWidth())
    }
}

@Composable
private fun ThreadActivityCard(
    item: CodexItem,
    isExpanded: Boolean,
    toggleExpanded: () -> Unit,
) {
    val presentation = item.toThreadItemPresentation()
    Card(
        modifier = Modifier
            .fillMaxWidth()
            .then(if (presentation.isCollapsible) Modifier.clickable(onClick = toggleExpanded) else Modifier),
    ) {
        Column(
            modifier = Modifier.padding(12.dp),
            verticalArrangement = Arrangement.spacedBy(4.dp),
        ) {
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                if (presentation.isCollapsible) Text(if (isExpanded) "⌄" else "›")
                Text(presentation.title, style = MaterialTheme.typography.labelLarge, maxLines = 1)
            }
            val body = if (isExpanded) item.expandedThreadItemBody() else presentation.collapsedBody
            if (body.isNotEmpty()) {
                Text(
                    text = body,
                    maxLines = if (presentation.isCollapsible && !isExpanded) 1 else Int.MAX_VALUE,
                )
            }
        }
    }
}

@Composable
private fun LoadingScreen(text: String, modifier: Modifier) {
    Column(modifier = modifier.fillMaxSize().padding(24.dp)) { Text(text) }
}

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
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.snapshotFlow
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import kotlin.time.Clock
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.launch

/**
 * The Android presentation receives network, storage, and QR-camera
 * capabilities explicitly while application state remains shared with iOS.
 */
@Composable
fun RemoteAgentApp(
    gateway: HostGateway,
    repository: MobileRepository,
    requestQrScan: ((onContents: (String) -> Unit) -> Unit)? = null,
    nowMs: () -> Long = { Clock.System.now().toEpochMilliseconds() },
) {
    val scope = androidx.compose.runtime.rememberCoroutineScope()
    val persistenceScope = remember(gateway, repository) { CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate) }
    val controller = remember(gateway, repository) { MobileController(gateway, repository, persistenceScope) }
    var state by remember(controller) { mutableStateOf(controller.state) }
    DisposableEffect(controller) {
        val observation = controller.observe { state = it }
        controller.openApp(scope)
        val connectionObservation = controller.maintainConnection(scope)
        onDispose {
            connectionObservation.cancel()
            observation.cancel()
            persistenceScope.launch {
                try { controller.flushPersistence() } finally { persistenceScope.cancel() }
            }
        }
    }
    val activity = LocalContext.current as? ComponentActivity
    if (activity != null) {
        DisposableEffect(controller, activity) {
            val lifecycleObserver = AndroidConnectionLifecycle(
                onForeground = { controller.openApp(scope) },
                // Keep the authenticated transport alive while the Activity is
                // backgrounded. Reconnect on the next foreground event.
                onBackground = { persistenceScope.launch { controller.flushPersistence() } },
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
        items(state.profiles, key = { it.id }) { profile ->
            Card(modifier = Modifier.fillMaxWidth(), onClick = { onSelect(profile.id) }) {
                Column(modifier = Modifier.padding(16.dp)) {
                    Text(profile.name, style = MaterialTheme.typography.titleMedium)
                    Text(profile.id, style = MaterialTheme.typography.bodySmall)
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
    val cache = state.cache.profile(profile.id)
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
            if (selectedThreadId == null && view.newThreadCwd == null) {
                ThreadListScreen(
                    profile = profile,
                    view = view,
                    projects = cache.projects,
                    threads = cache.threadList,
                    onRefresh = {
                        scope.launch {
                            controller.listThreads(profile)
                        }
                    },
                    onNew = { cwd -> controller.openNewThread(profile, cwd) },
                    onSelect = { threadId -> scope.launch { controller.readThread(profile, threadId) } },
                    onExpand = { projects, projectId -> scope.launch { controller.expandTaskList(profile, projects, projectId) } },
                    modifier = modifier,
                )
            } else {
                ThreadDetailScreen(
                    profile = profile,
                    view = view,
                    snapshot = cache.snapshots[selectedThreadId],
                    onBack = { scope.launch { controller.showThreadList(profile) } },
                    onRetry = { selectedThreadId?.let { scope.launch { controller.readThread(profile, it) } } },
                    onSend = { text -> controller.sendMessage(profile, text).accepted },
                    onOlderHistory = { turnId -> scope.launch { controller.loadOlderHistory(profile, turnId) } },
                    onStop = { turnId -> selectedThreadId?.let { scope.launch { controller.interrupt(profile, it, turnId) } } },
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
        Text(profile.relayUrl)
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
    onNew: (String) -> Unit,
    onSelect: (String) -> Unit,
    onExpand: (Boolean, String?) -> Unit,
    modifier: Modifier,
) {
    val knownProjectIds = projects.mapTo(mutableSetOf()) { it.id }
    val unassigned = threads.filter { it.projectId == null || it.projectId !in knownProjectIds }
    val listPhase = view.threadList
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
                enabled = view.threadList != LoadPhase.Loading,
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
        projects.take(view.visibleProjectCount).forEach { project ->
            item(key = "project-${project.id}") {
                Row(modifier = Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    Text("📁 ${project.name}", style = MaterialTheme.typography.titleMedium)
                    Spacer(Modifier.weight(1f))
                    TextButton(onClick = { onNew(project.roots.firstOrNull()?.path.orEmpty()) }) { Text("新規") }
                }
            }
            items(threads.filter { it.projectId == project.id }, key = { it.id }) { thread ->
                ThreadSummaryRow(thread, onSelect)
            }
            if (project.id in view.moreProjectIds) {
                item(key = "project-${project.id}-more") {
                    TextButton(onClick = { onExpand(false, project.id) }, enabled = !view.loadingMoreThreads) { Text("もっと見る") }
                }
            }
        }
        if (view.hasMoreProjects) {
            item(key = "projects-more") {
                TextButton(onClick = { onExpand(true, null) }, enabled = !view.loadingMoreThreads) { Text("もっと見る") }
            }
        }
        item(key = "unassigned-header") {
            Row(modifier = Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                Text("チャット", style = MaterialTheme.typography.titleMedium)
                Spacer(Modifier.weight(1f))
                TextButton(onClick = { onNew("") }) { Text("新規") }
            }
        }
        items(unassigned, key = { it.id }) { thread ->
            ThreadSummaryRow(thread, onSelect)
        }
        if (view.hasMoreChats) {
            item(key = "chats-more") {
                TextButton(onClick = { onExpand(false, null) }, enabled = !view.loadingMoreThreads) { Text("もっと見る") }
            }
        }
        if (view.threadList == LoadPhase.Ready && threads.isEmpty()) {
            item { Text("タスクがありません。", color = MaterialTheme.colorScheme.onSurfaceVariant) }
        }
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

@Composable
private fun ThreadDetailScreen(
    profile: HostProfile,
    view: ProfileViewState,
    snapshot: ThreadSnapshot?,
    onBack: () -> Unit,
    onRetry: () -> Unit,
    onSend: suspend (String) -> Boolean,
    onOlderHistory: (String?) -> Unit,
    onStop: (String) -> Unit,
    modifier: Modifier,
) {
    var composer by remember(profile.id) { mutableStateOf("") }
    var sending by remember { mutableStateOf(false) }
    val composerScope = rememberCoroutineScope()
    val listState = rememberLazyListState()
    var followingLatest by remember(profile.id, view.selectedThreadId) { mutableStateOf(true) }
    var expandedItemIds by remember(profile.id, view.selectedThreadId) {
        mutableStateOf(emptySet<String>())
    }
    var activityExpansionOverrides by remember(profile.id, view.selectedThreadId) {
        mutableStateOf(emptyMap<String, Boolean>())
    }
    val turnPresentations = snapshot?.conversationSegments().orEmpty()
    val queuedMessages = snapshot?.submittedMessages.orEmpty().filter { it.turnId == null }
    val contentVersion = snapshot?.turns?.joinToString("|") { turn ->
        val items = turn.items.joinToString(",") { "${it.id}:${it.threadItemContentVersion()}" }
        val requests = turn.pendingRequests.joinToString(",") { "${it.id}:${it.method}:${it.params.hashCode()}" }
        "${turn.id}:${turn.status}:${turn.error?.hashCode()}:$requests:$items"
    }.orEmpty() + snapshot?.submittedMessages.orEmpty().joinToString { it.clientId + ":" + it.text }
    val openingMessages = buildMap {
        for (turn in snapshot?.turns.orEmpty()) {
            turn.raw?.get("openingUserMessage")?.let(::codexItem)
                ?.takeUnless { item -> turn.items.any { it.id == item.id } }?.let { put(turn.id, it) }
        }
    }
    val historyRows = openingMessages.size + (if (snapshot?.olderTurnsCursor != null) 1 else 0) + snapshot?.turns.orEmpty().count { it.hasOlderItems }
    val detailRowCount = historyRows + queuedMessages.size + turnPresentations.sumOf { turn ->
        val activityExpanded = activityExpansionOverrides[turn.id] ?: turn.activityInitiallyExpanded
        turn.userMessages.size +
            (if (turn.activitySummary == null) 0 else 1) +
            (if (activityExpanded) turn.activityItems.size else 0) +
            (if (turn.isLastSegment && turn.status == TurnStatus.InProgress) 1 else 0) +
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
    LaunchedEffect(listState.firstVisibleItemIndex, listState.isScrollInProgress) {
        if (listState.isScrollInProgress && !followingLatest && !view.loadingHistory) {
            val key = listState.layoutInfo.visibleItemsInfo.firstOrNull()?.key as? String
            if (key?.startsWith("history:") == true) onOlderHistory(key.removePrefix("history:").takeUnless { it == "turns" })
        }
    }
    LaunchedEffect(contentVersion) {
        if (followingLatest && !view.loadingHistory) {
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
            Text(snapshot?.summary?.name ?: snapshot?.summary?.preview ?: if (view.newThreadCwd != null) "チャット" else "タスクを読み込み中…", style = MaterialTheme.typography.headlineSmall)
            if (view.threadDetail is LoadPhase.Failed) {
                Text(view.threadDetail.message, color = MaterialTheme.colorScheme.error)
                Button(onClick = onRetry) { Text("再試行") }
            }
        }
        if (snapshot?.olderTurnsCursor != null) {
            item(key = "history:turns") {
                Button(onClick = { onOlderHistory(null) }, enabled = !view.loadingHistory) { Text("以前の会話を読み込む") }
            }
        }
        turnPresentations.forEach { turn ->
            if (turn.id == turn.turnId) openingMessages[turn.turnId]?.let { opening ->
                item(key = "opening:${turn.turnId}") { ThreadMessageCard(item = opening, isUser = true) }
            }
            if (turn.id == turn.turnId && snapshot?.turns?.firstOrNull { it.id == turn.turnId }?.hasOlderItems == true) {
                item(key = "history:${turn.turnId}") {
                    Button(onClick = { onOlderHistory(turn.turnId) }, enabled = !view.loadingHistory) { Text("途中の履歴を読み込む") }
                }
            }
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
            }
            if (turn.isLastSegment && turn.status == TurnStatus.InProgress) {
                item(key = "${turn.id}:stop") {
                    Button(
                        onClick = { onStop(turn.turnId) },
                        enabled = view.interruptingTurnId != turn.turnId,
                    ) { Text(if (view.interruptingTurnId == turn.turnId) "停止中…" else "停止") }
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
        items(queuedMessages, key = { "queued:${it.clientId}" }) { message ->
            Column {
                Text("順番待ち", style = MaterialTheme.typography.labelSmall)
                ThreadMessageCard(CodexItem.UserMessage(message.clientId, message.text, message.clientId, message.imageSources), isUser = true)
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
                onClick = {
                    val text = composer
                    sending = true
                    composerScope.launch {
                        try { if (onSend(text) && composer == text) composer = "" }
                        finally { sending = false }
                    }
                },
                enabled = !sending && composer.isNotBlank() && (snapshot != null || view.newThreadCwd != null),
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
            Card {
                Column(modifier = Modifier.padding(12.dp)) {
                    Text(message)
                    (item as? CodexItem.UserMessage)?.imageSources.orEmpty().forEach { source ->
                        Text(attachmentMessageLabel(true, source, ""))
                    }
                }
            }
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

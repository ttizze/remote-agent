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
import androidx.compose.runtime.rememberCoroutineScope
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
 * The Android presentation receives network, storage, and QR-camera capabilities explicitly while application state
 * remains shared with iOS.
 */
@Composable
fun RemoteAgentApp(
    gateway: HostGateway,
    repository: MobileRepository,
    requestQrScan: ((onContents: (String) -> Unit) -> Unit)? = null,
    nowMs: () -> Long = { Clock.System.now().toEpochMilliseconds() },
) {
    val scope = androidx.compose.runtime.rememberCoroutineScope()
    val persistenceScope =
        remember(gateway, repository) { CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate) }
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
                try {
                    controller.flushPersistence()
                } finally {
                    persistenceScope.cancel()
                }
            }
        }
    }
    val activity = LocalContext.current as? ComponentActivity
    if (activity != null) {
        DisposableEffect(controller, activity) {
            val lifecycleObserver =
                AndroidConnectionLifecycle(
                    onForeground = { controller.restoreConnection(scope) },
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
        state.showingPairing || state.profiles.isEmpty() ->
            PairingScreen(
                pairingError = state.pairingError,
                onPair = { contents -> scope.launch { controller.pair(contents, nowMs()) } },
                onRequestScan = requestQrScan,
                onCancel = { controller.dispatch(AppAction.PairingDismissed) },
                modifier = modifier,
            )

        state.selectedProfile == null ->
            HostSelectionScreen(
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
    Column(modifier = modifier.fillMaxSize().padding(24.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
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
private fun HostFlowScreen(state: AppState, controller: MobileController, scope: CoroutineScope, modifier: Modifier) {
    val profile = requireNotNull(state.selectedProfile)
    val view = state.selectedView
    when (view.connection) {
        ConnectionPhase.Disconnected ->
            ConnectScreen(
                profile = profile,
                onDiscover = { scope.launch { controller.discover(profile) } },
                onConnect = { scope.launch { controller.connect(profile, scope) } },
                onBack = { controller.dispatch(AppAction.ProfileSelectionOpened) },
                modifier = modifier,
            )

        ConnectionPhase.Connecting -> LoadingScreen("PC Hostへ接続中…", modifier)
        is ConnectionPhase.Failed ->
            ConnectScreen(
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
                ThreadListScreen(state, controller, scope, modifier)
            } else {
                ThreadDetailScreen(state, controller, scope, modifier)
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
private fun LoadingScreen(text: String, modifier: Modifier) {
    Column(modifier = modifier.fillMaxSize().padding(24.dp)) { Text(text) }
}

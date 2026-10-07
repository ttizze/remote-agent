package dev.remoteagent.mobile

import androidx.activity.ComponentActivity
import androidx.activity.compose.BackHandler
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Delete
import androidx.compose.material.icons.outlined.QrCodeScanner
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.Snackbar
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.key
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp

@Composable
internal fun RemoteAgentApp(
    activity: ComponentActivity,
    model: AndroidAppModel,
    requestQrScan: (onContents: (String) -> Unit) -> Unit,
) {
    DisposableEffect(model, activity) {
        val observer = AndroidConnectionLifecycle(model::foreground, model::background)
        activity.lifecycle.addObserver(observer)
        onDispose { activity.lifecycle.removeObserver(observer) }
    }
    BackHandler(model.stack.size > 1) { model.back() }
    AppMaterialTheme {
        val context = LocalContext.current
        val root = model.snapshot.currentDirectory()
        val markdown =
            MarkdownActions(
                root,
                { href ->
                    when (val target = dev.remoteagent.core.markdownLinkAction(href, root)) {
                        is dev.remoteagent.core.MarkdownLinkAction.WorkspaceFile ->
                            model.navigate(
                                Route.Workspace(WorkspaceTab.Files, java.io.File(root, target.path).path, target.line)
                            )
                        is dev.remoteagent.core.MarkdownLinkAction.HostFile ->
                            model.navigate(Route.Workspace(WorkspaceTab.Files, target.path, target.line))
                        is dev.remoteagent.core.MarkdownLinkAction.External ->
                            runCatching {
                                    context.startActivity(
                                        android.content.Intent(
                                            android.content.Intent.ACTION_VIEW,
                                            android.net.Uri.parse(target.url),
                                        )
                                    )
                                }
                                .onFailure { model.notice = it.message }
                        dev.remoteagent.core.MarkdownLinkAction.Nothing -> Unit
                    }
                },
                model::downloadBytes,
            )
        CompositionLocalProvider(LocalSnapshot provides model.snapshot, LocalMarkdownActions provides markdown) {
            AppSurface(model, requestQrScan)
        }
    }
}

@Composable
private fun AppSurface(model: AndroidAppModel, requestQrScan: (onContents: (String) -> Unit) -> Unit) {
    Surface(Modifier.fillMaxSize(), color = AppTheme.colors.header) {
        Box(Modifier.fillMaxSize().safeDrawingPadding()) {
            key(model.profileId) {
                when (val route = model.route) {
                    Route.Hosts -> HostsScreen(model)
                    Route.Pairing -> PairingScreen(model, requestQrScan)
                    Route.Home -> HomeScreen(model)
                    is Route.Thread -> ThreadScreen(model, route.id)
                    Route.ChooseProject -> ChooseProjectScreen(model)
                    Route.AddProject -> AddProjectScreen(model)
                    Route.AddProjectLocal -> LocalFolderScreen(model)
                    Route.NewTask -> NewTaskScreen(model)
                    is Route.Terminal ->
                        TerminalScreen(model, route.threadId, route.terminalId, route.project, route.cwd)
                    is Route.Workspace -> WorkspaceScreen(model, route.tab, route.file, route.line)
                    is Route.Settings -> SettingsScreen(model, route.projectId)
                    Route.Archived -> ArchivedScreen(model)
                }
            }
            model.notice?.let { notice ->
                Snackbar(
                    Modifier.align(Alignment.BottomCenter).padding(16.dp),
                    action = { TextButton(onClick = { model.notice = null }) { Text("Dismiss") } },
                    containerColor = AppTheme.colors.foreground,
                    contentColor = AppTheme.colors.screen,
                ) {
                    Text(notice, style = AppTheme.footnote)
                }
            }
        }
    }
}

/** Header, body and the rounded canvas the Android screens share. */
@Composable
internal fun ScreenScaffold(
    title: String,
    onBack: (() -> Unit)?,
    modifier: Modifier = Modifier,
    subtitle: String? = null,
    actions: @Composable () -> Unit = {},
    content: @Composable () -> Unit,
) {
    Column(modifier.fillMaxSize()) {
        ScreenHeader(title, subtitle, onBack, actions)
        Surface(
            Modifier.fillMaxSize(),
            color = AppTheme.colors.screen,
            shape = RoundedCornerShape(topStart = 28.dp, topEnd = 28.dp),
        ) {
            content()
        }
    }
}

@Composable
private fun HostsScreen(model: AndroidAppModel) {
    ScreenScaffold(
        "Environments",
        onBack = null,
        actions = { HeaderIconButton(Icons.Outlined.QrCodeScanner, "Add environment", onClick = model::openPairing) },
    ) {
        LazyColumn(contentPadding = PaddingValues(16.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
            items(model.profiles, key = { it.id }) { profile ->
                Surface(color = AppTheme.colors.groupedCard, shape = RoundedCornerShape(28.dp)) {
                    Row(
                        Modifier.fillMaxWidth().clickable { model.selectProfile(profile.id) }.padding(16.dp),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        Column(Modifier.weight(1f)) {
                            Text(profile.name, style = AppTheme.headline, color = AppTheme.colors.foreground)
                            Text(
                                if (profile.id == model.profileId) "Connected device" else "Paired",
                                style = AppTheme.caption,
                                color = AppTheme.colors.foregroundSecondary,
                            )
                        }
                        IconButton(onClick = { model.removeProfile(profile.id) }) {
                            Icon(Icons.Outlined.Delete, "Remove ${profile.name}", tint = AppTheme.colors.iconMuted)
                        }
                    }
                }
            }
        }
    }
}

@Composable
private fun PairingScreen(model: AndroidAppModel, scan: (onContents: (String) -> Unit) -> Unit) {
    var contents by remember { mutableStateOf("") }
    ScreenScaffold(
        if (model.invitation == null) "Add Environment" else "Pairing",
        onBack = if (model.profiles.isEmpty()) null else model::back,
    ) {
        Column(Modifier.fillMaxSize().padding(20.dp), verticalArrangement = Arrangement.spacedBy(16.dp)) {
            val invitation = model.invitation
            if (invitation == null) {
                Text(
                    "PC Host Manager の QR コードを読み取ります。",
                    style = AppTheme.footnote,
                    color = AppTheme.colors.foregroundSecondary,
                )
                PrimaryButton("QRコードを読み取る", enabled = !model.busy) { scan { model.preparePairing(it) } }
                SettingsField(contents, { contents = it }, "接続情報", minLines = 3)
                PrimaryButton("接続先を確認", enabled = contents.isNotBlank() && !model.busy) {
                    model.preparePairing(contents)
                    contents = ""
                }
            } else {
                Surface(color = AppTheme.colors.groupedCard, shape = RoundedCornerShape(28.dp)) {
                    Column(Modifier.fillMaxWidth().padding(20.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                        Text(
                            invitation.hostName,
                            style = AppTheme.title,
                            fontWeight = FontWeight.Bold,
                            color = AppTheme.colors.foreground,
                        )
                        Text("メッセージと作業に必要な内容をこの PC と AI サービスに送信します。", style = AppTheme.footnote)
                        Text("AI処理: ${invitation.aiRecipients.joinToString("、")}", style = AppTheme.footnote)
                        invitation.transcriptionRecipient?.let { Text("音声入力: $it", style = AppTheme.footnote) }
                    }
                }
                PrimaryButton(if (model.busy) "Pairing..." else "同意して接続", enabled = !model.busy, onClick = model::pair)
                TextButton(onClick = model::openPairing) { Text("変更", color = AppTheme.colors.foreground) }
            }
        }
    }
}

@Composable
internal fun Scrim(onDismiss: () -> Unit) {
    Box(Modifier.fillMaxSize().background(AppTheme.colors.backdrop).clickable(onClick = onDismiss))
}

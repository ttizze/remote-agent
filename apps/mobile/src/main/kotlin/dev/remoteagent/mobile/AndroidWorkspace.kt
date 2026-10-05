package dev.remoteagent.mobile

import android.graphics.BitmapFactory
import android.graphics.Matrix
import android.view.GestureDetector
import android.view.MotionEvent
import android.widget.ImageView
import androidx.activity.ComponentActivity
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import dev.remoteagent.core.*
import java.io.File
import java.util.UUID
import kotlinx.coroutines.*

@Composable
internal fun SettingsDialog(model: AndroidAppModel, dismiss: () -> Unit) {
    val context = LocalContext.current
    var code by remember { mutableStateOf("") }
    LaunchedEffect(Unit) { model.perform(Intent.LoadAccounts) }
    Dialog(dismiss, properties = DialogProperties(usePlatformDefaultWidth = false)) {
        Surface(Modifier.fillMaxSize()) {
            LazyColumn(contentPadding = PaddingValues(20.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
                item {
                    Row {
                        Text("Settings", Modifier.weight(1f), style = MaterialTheme.typography.titleLarge)
                        TextButton(onClick = dismiss) { Text("Done") }
                    }
                }
                item {
                    TextButton(
                        onClick = {
                            dismiss()
                            model.showHosts()
                        }
                    ) {
                        Text("Hosts and pairing")
                    }
                }
                item { Text("Provider accounts", style = MaterialTheme.typography.titleMedium) }
                items(model.snapshot.accounts()?.accounts.orEmpty(), key = { it.id }) { account ->
                    Column {
                        Text(account.email ?: account.id)
                        account.usage?.windows?.forEach { window ->
                            Text(
                                "${window.label}: ${window.remainingPercent}% remaining",
                                style = MaterialTheme.typography.bodySmall,
                            )
                            LinearProgressIndicator(
                                progress = { window.remainingPercent.toFloat() / 100f },
                                modifier = Modifier.fillMaxWidth(),
                            )
                        }
                        account.usage?.error?.let {
                            Text(accountErrorMessage(it), color = T3.color("warningForeground"))
                        }
                        Row {
                            TextButton(
                                onClick = { model.perform(Intent.SelectAccount(account.provider, account.id)) }
                            ) {
                                Text("Select")
                            }
                            TextButton(
                                onClick = { model.perform(Intent.DeleteAccount(account.provider, account.id)) }
                            ) {
                                Text("Remove")
                            }
                        }
                    }
                }
                item {
                    Button(onClick = { model.perform(Intent.StartLogin(ProviderKind.CODEX)) }) {
                        Text("Sign in to Codex")
                    }
                }
                item {
                    Button(onClick = { model.perform(Intent.StartLogin(ProviderKind.CLAUDE)) }) {
                        Text("Sign in to Claude")
                    }
                }
                model.snapshot.accountLogin()?.let { login ->
                    item {
                        Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                            if (login.userCode.isNotBlank()) {
                                Text(login.userCode)
                                CopyButton(login.userCode)
                            }
                            TextButton(
                                onClick = {
                                    val uri = android.net.Uri.parse(login.verificationUrl)
                                    if (uri.scheme == "https")
                                        context.startActivity(
                                            android.content.Intent(android.content.Intent.ACTION_VIEW, uri)
                                        )
                                }
                            ) {
                                Text("Open sign-in page")
                            }
                            if (login.requiresCodeSubmission) {
                                OutlinedTextField(
                                    code,
                                    { code = it },
                                    label = { Text("Authorization code") },
                                    visualTransformation = androidx.compose.ui.text.input.PasswordVisualTransformation(),
                                )
                                Button(
                                    onClick = {
                                        model.perform(Intent.CompleteLogin(login.provider, login.loginId, code))
                                        code = ""
                                    },
                                    enabled = code.isNotBlank(),
                                ) {
                                    Text("Complete sign in")
                                }
                            }
                            TextButton(
                                onClick = {
                                    model.perform(Intent.CancelLogin(login.provider, login.loginId))
                                    code = ""
                                }
                            ) {
                                Text("Cancel sign in")
                            }
                        }
                    }
                }
                item { Text(privacyPolicy(), style = MaterialTheme.typography.bodySmall) }
            }
        }
    }
}

@Composable
internal fun WorkspaceDialog(model: AndroidAppModel, tab: String, dismiss: () -> Unit) {
    Dialog(dismiss, properties = DialogProperties(usePlatformDefaultWidth = false)) {
        Surface(Modifier.fillMaxSize()) {
            Column {
                Row(Modifier.padding(horizontal = 20.dp), horizontalArrangement = Arrangement.SpaceBetween) {
                    Text(tab, Modifier.weight(1f), style = MaterialTheme.typography.titleLarge)
                    TextButton(onClick = dismiss) { Text("Close") }
                }
                when (tab) {
                    "Files" -> WorkspaceFiles(model, Modifier.weight(1f))
                    "Diff" -> WorkspaceDiff(model, Modifier.weight(1f))
                    "Browser" -> WorkspaceBrowser(model, Modifier.weight(1f))
                }
            }
        }
    }
}

@Composable
private fun WorkspaceFiles(model: AndroidAppModel, modifier: Modifier) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    var directory by remember { mutableStateOf(model.snapshot.currentDirectory()) }
    var path by remember { mutableStateOf(directory) }
    var selected by remember { mutableStateOf<FileEntry?>(null) }
    var error by remember { mutableStateOf<String?>(null) }
    var downloading by remember { mutableStateOf<Pair<String, String?>?>(null) }
    val download =
        rememberLauncherForActivityResult(ActivityResultContracts.CreateDocument("application/octet-stream")) { uri ->
            val source = downloading
            downloading = null
            if (uri != null && source != null && source.second == model.profileId)
                scope.launch {
                    val temporary = File(context.cacheDir, "download-${UUID.randomUUID()}")
                    try {
                        model.download(source.first, temporary.path)
                        withContext(Dispatchers.IO) {
                            context.contentResolver.openOutputStream(uri)?.use { out ->
                                temporary.inputStream().use { it.copyTo(out) }
                            } ?: error("Unable to open destination")
                        }
                    } catch (failure: Exception) {
                        error = failure.message
                    } finally {
                        temporary.delete()
                    }
                }
        }
    val upload =
        rememberLauncherForActivityResult(ActivityResultContracts.OpenDocument()) { uri ->
            if (uri != null)
                scope.launch {
                    val temporary = File(context.cacheDir, "upload-${UUID.randomUUID()}")
                    try {
                        val name =
                            withContext(Dispatchers.IO) {
                                context.contentResolver
                                    .query(
                                        uri,
                                        arrayOf(android.provider.OpenableColumns.DISPLAY_NAME),
                                        null,
                                        null,
                                        null,
                                    )
                                    ?.use { if (it.moveToFirst()) it.getString(0) else null } ?: "file"
                            }
                        withContext(Dispatchers.IO) {
                            context.contentResolver.openInputStream(uri)?.use { input ->
                                temporary.outputStream().use { input.copyTo(it) }
                            } ?: error("Unable to read selected file")
                        }
                        model.upload(temporary.path, directory, name)
                        model.perform(Intent.ListFiles(directory))
                    } catch (failure: Exception) {
                        error = failure.message
                    } finally {
                        temporary.delete()
                    }
                }
        }
    LaunchedEffect(directory) { if (directory.isNotBlank()) model.perform(Intent.ListFiles(directory)) }
    Column(modifier.padding(horizontal = 20.dp)) {
        Row {
            OutlinedTextField(
                path,
                { path = it },
                Modifier.weight(1f),
                label = { Text("Absolute path") },
                singleLine = true,
            )
            TextButton(onClick = { directory = path }) { Text("Open") }
        }
        Row {
            TextButton(
                onClick = {
                    directory = File(directory).parent ?: "/"
                    path = directory
                }
            ) {
                Text("Parent")
            }
            TextButton(onClick = { upload.launch(arrayOf("*/*")) }) { Text("Upload") }
        }
        error?.let { Text(it, color = T3.color("errorForeground")) }
        val files = model.snapshot.directory()?.takeIf { it.path == directory }
        if (files?.truncated == true)
            Text("Showing the first 2,000 entries", style = MaterialTheme.typography.bodySmall)
        LazyColumn {
            items(files?.entries.orEmpty(), key = { it.path }) { entry ->
                Row(Modifier.fillMaxWidth()) {
                    TextButton(
                        onClick = {
                            if (entry.directory) {
                                directory = entry.path
                                path = directory
                            } else {
                                selected = entry
                                model.perform(Intent.ReadFile(entry.path, false))
                            }
                        },
                        modifier = Modifier.weight(1f),
                    ) {
                        Text((if (entry.directory) "▸ " else "") + entry.name)
                    }
                    if (!entry.directory)
                        TextButton(
                            onClick = {
                                downloading = entry.path to model.profileId
                                download.launch(entry.name)
                            }
                        ) {
                            Text("↓")
                        }
                }
            }
        }
    }
    selected?.let { entry ->
        Dialog({ selected = null }, properties = DialogProperties(usePlatformDefaultWidth = false)) {
            Surface(Modifier.fillMaxSize()) {
                Column(Modifier.padding(20.dp)) {
                    Row {
                        Text(entry.name, Modifier.weight(1f))
                        TextButton(onClick = { selected = null }) { Text("Close") }
                    }
                    model.snapshot.error()?.let { Text(it, color = T3.color("errorForeground")) }
                    val file = model.snapshot.file()?.takeIf { it.path == entry.path }
                    var text by remember(entry.path) { mutableStateOf("") }
                    var pending by remember(entry.path) { mutableStateOf<Long?>(null) }
                    var revision by remember(entry.path) { mutableStateOf(0L) }
                    LaunchedEffect(file?.revision) {
                        if (pending == null && file != null) text = model.snapshot.fileDraft(entry.path) ?: file.text
                    }
                    if (file == null) CircularProgressIndicator()
                    else {
                        OutlinedTextField(
                            text,
                            { value ->
                                text = value
                                val current = ++revision
                                pending = current
                                model.perform(Intent.EditFile(entry.path, value)) {
                                    if (pending == current) pending = null
                                }
                            },
                            Modifier.weight(1f).fillMaxWidth(),
                            textStyle = MaterialTheme.typography.bodyMedium.copy(fontFamily = FontFamily.Monospace),
                        )
                        Button(onClick = { model.perform(Intent.SaveFile(entry.path)) }) { Text("Save") }
                    }
                }
            }
        }
    }
}

@Composable
private fun WorkspaceDiff(model: AndroidAppModel, modifier: Modifier) {
    val cwd = model.snapshot.currentDirectory()
    val thread = model.snapshot.selectedThreadId()
    var expanded by remember(thread) { mutableStateOf(false) }
    var selection by remember(thread) { mutableStateOf<TurnDiffOption?>(null) }
    fun refresh() {
        val range = selection
        if (range != null) model.perform(Intent.ReadTurnDiff(range.fromTurnCount, range.toTurnCount, false))
        else if (cwd.isNotBlank()) model.perform(Intent.ReviewWorkspace(cwd))
    }
    LaunchedEffect(cwd, thread, selection) { refresh() }
    val review = model.snapshot.review()
    val files = remember(review) { review?.diffFiles().orEmpty() }
    Column(modifier) {
        Row {
            Box(Modifier.weight(1f)) {
                TextButton(onClick = { expanded = true }) { Text(selection?.label ?: "Workspace changes") }
                DropdownMenu(expanded, { expanded = false }) {
                    DropdownMenuItem(
                        text = { Text("Workspace changes") },
                        onClick = {
                            selection = null
                            expanded = false
                        },
                    )
                    model.snapshot.turnDiffOptions().forEach { option ->
                        DropdownMenuItem(
                            text = { Text(option.label) },
                            onClick = {
                                selection = option
                                expanded = false
                            },
                        )
                    }
                }
            }
            TextButton(onClick = { refresh() }) { Text("Refresh") }
        }
        LazyColumn(
            Modifier.weight(1f),
            contentPadding = PaddingValues(20.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            if (review == null) item { CircularProgressIndicator() }
            else if (files.isEmpty()) item { Text("No changes") }
            items(files, key = { it.path }) { file ->
                Column {
                    Text(file.path, style = MaterialTheme.typography.labelLarge)
                    androidx.compose.foundation.text.selection.SelectionContainer {
                        Column {
                            file.rows.forEach { row ->
                                Text(
                                    row.text,
                                    fontFamily = FontFamily.Monospace,
                                    style = MaterialTheme.typography.bodySmall,
                                    color =
                                        T3.color(
                                            when (row.kind) {
                                                "+" -> "successForeground"
                                                "-" -> "errorForeground"
                                                else -> "text"
                                            }
                                        ),
                                )
                            }
                        }
                    }
                }
            }
        }
    }
}

@Composable
private fun WorkspaceBrowser(model: AndroidAppModel, modifier: Modifier) {
    val thread = model.snapshot.selectedThreadId()
    if (thread == null) {
        Text("Start a thread to open its browser")
        return
    }
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    var frame by remember(thread) { mutableStateOf<BrowserFrame?>(null) }
    var address by remember(thread) { mutableStateOf("") }
    var input by remember { mutableStateOf("") }
    var dialogText by remember { mutableStateOf("") }
    var sending by remember { mutableStateOf(false) }
    var error by remember { mutableStateOf<String?>(null) }
    var active by remember { mutableStateOf(true) }
    DisposableEffect(context) {
        val lifecycle = (context as? ComponentActivity)?.lifecycle
        val observer = LifecycleEventObserver { _, _ ->
            active = lifecycle?.currentState?.isAtLeast(Lifecycle.State.RESUMED) == true
        }
        lifecycle?.addObserver(observer)
        onDispose { lifecycle?.removeObserver(observer) }
    }
    suspend fun request(action: BrowserAction) {
        if (sending || !active) return
        sending = true
        try {
            val current = frame
            val next = model.browser(BrowserRequest(thread, current?.tabId ?: "", current?.imageId ?: "", action))
            frame = next
            error = null
            if (address.isEmpty()) address = next.tabs.firstOrNull { it.id == next.tabId }?.url.orEmpty()
        } catch (failure: Exception) {
            error = failure.message
        } finally {
            sending = false
        }
    }
    fun send(action: BrowserAction) {
        scope.launch { request(action) }
    }
    LaunchedEffect(thread, active) {
        while (active && isActive) {
            if (model.snapshot.connected()) request(BrowserAction.Read)
            delay(500)
        }
    }
    Column(modifier.padding(horizontal = 12.dp)) {
        Row {
            OutlinedTextField(
                address,
                { address = it },
                Modifier.weight(1f),
                singleLine = true,
                label = { Text("URL") },
            )
            TextButton(onClick = { send(BrowserAction.Navigate(address)) }, enabled = !sending) { Text("Go") }
        }
        Row {
            TextButton(onClick = { send(BrowserAction.Back) }) { Text("‹") }
            TextButton(onClick = { send(BrowserAction.Forward) }) { Text("›") }
            TextButton(onClick = { send(BrowserAction.Reload) }) { Text("Reload") }
            frame?.tabs?.forEach { tab ->
                TextButton(onClick = { send(BrowserAction.SelectTab(tab.id)) }) {
                    Text(tab.title.ifBlank { "Tab" }, maxLines = 1)
                }
            }
        }
        error?.let { Text(it, color = T3.color("errorForeground")) }
        frame?.dialog?.let { dialog ->
            Text(dialog.message)
            if (dialog.prompt) OutlinedTextField(dialogText, { dialogText = it })
            Row {
                TextButton(onClick = { send(BrowserAction.Dialog(false, "")) }) { Text("Cancel") }
                TextButton(
                    onClick = {
                        send(BrowserAction.Dialog(true, dialogText))
                        dialogText = ""
                    }
                ) {
                    Text("OK")
                }
            }
        }
        val bytes = frame?.image
        val bitmap =
            remember(bytes) { bytes?.takeIf { it.isNotEmpty() }?.let { BitmapFactory.decodeByteArray(it, 0, it.size) } }
        val sendLatest by rememberUpdatedState(::send)
        val acceptsInput by rememberUpdatedState(!sending && active && bitmap != null)
        AndroidView(
            factory = { nativeContext ->
                ImageView(nativeContext).apply {
                    scaleType = ImageView.ScaleType.FIT_CENTER
                    fun point(event: MotionEvent): FloatArray? {
                        val drawable = drawable ?: return null
                        val matrix = Matrix()
                        if (!imageMatrix.invert(matrix)) return null
                        val point = floatArrayOf(event.x, event.y)
                        matrix.mapPoints(point)
                        return point.takeIf {
                            it[0] >= 0 &&
                                it[1] >= 0 &&
                                it[0] < drawable.intrinsicWidth &&
                                it[1] < drawable.intrinsicHeight
                        }
                    }
                    val gestures =
                        GestureDetector(
                            nativeContext,
                            object : GestureDetector.SimpleOnGestureListener() {
                                override fun onDown(event: MotionEvent) = acceptsInput

                                override fun onSingleTapUp(event: MotionEvent): Boolean {
                                    if (acceptsInput)
                                        point(event)?.let {
                                            sendLatest(BrowserAction.Click(it[0].toDouble(), it[1].toDouble()))
                                        }
                                    return true
                                }

                                override fun onScroll(
                                    first: MotionEvent?,
                                    current: MotionEvent,
                                    distanceX: Float,
                                    distanceY: Float,
                                ): Boolean {
                                    if (acceptsInput)
                                        point(current)?.let {
                                            val inverse = Matrix()
                                            if (!imageMatrix.invert(inverse)) return true
                                            val distance = floatArrayOf(distanceX, distanceY)
                                            inverse.mapVectors(distance)
                                            sendLatest(
                                                BrowserAction.Scroll(
                                                    it[0].toDouble(),
                                                    it[1].toDouble(),
                                                    distance[0].toDouble(),
                                                    distance[1].toDouble(),
                                                )
                                            )
                                        }
                                    return true
                                }
                            },
                        )
                    setOnTouchListener { _, event -> gestures.onTouchEvent(event) }
                }
            },
            update = { it.setImageBitmap(bitmap) },
            modifier = Modifier.weight(1f).fillMaxWidth(),
        )
        Row {
            OutlinedTextField(
                input,
                { input = it },
                Modifier.weight(1f),
                label = { Text("Type in browser") },
                visualTransformation = androidx.compose.ui.text.input.PasswordVisualTransformation(),
            )
            TextButton(
                onClick = {
                    send(BrowserAction.Type(input))
                    input = ""
                }
            ) {
                Text("Type")
            }
            TextButton(onClick = { send(BrowserAction.Key(BrowserKey.ENTER)) }) { Text("Enter") }
        }
    }
}

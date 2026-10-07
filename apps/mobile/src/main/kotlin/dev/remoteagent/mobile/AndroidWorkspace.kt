@file:Suppress("TooGenericExceptionCaught")

package dev.remoteagent.mobile

import android.graphics.BitmapFactory
import android.graphics.Matrix
import android.view.GestureDetector
import android.view.MotionEvent
import android.widget.ImageView
import androidx.activity.ComponentActivity
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.key
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import dev.remoteagent.core.BrowserAction
import dev.remoteagent.core.BrowserFrame
import dev.remoteagent.core.BrowserKey
import dev.remoteagent.core.BrowserRequest
import dev.remoteagent.core.FileEntry
import dev.remoteagent.core.Intent
import java.io.File
import java.util.UUID
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext

private const val BROWSER_REFRESH_MILLIS = 500L

/** The open thread's files, diff and browser, each as its own screen. */
@Composable
internal fun WorkspaceScreen(model: AndroidAppModel, tab: WorkspaceTab) {
    ScreenScaffold(tab.name, onBack = model::back) {
        Column(Modifier.fillMaxSize()) {
            when (tab) {
                WorkspaceTab.Files -> WorkspaceFiles(model, Modifier.weight(1f))
                WorkspaceTab.Diff -> WorkspaceDiff(model, Modifier.weight(1f))
                WorkspaceTab.Browser -> WorkspaceBrowser(model, Modifier.weight(1f))
            }
        }
    }
}

@Composable
// Declarative native layout; the conversation decisions are supplied by core.
@Suppress("LongMethod", "CyclomaticComplexMethod")
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
        error?.let { Text(it, color = AppTheme.colors.dangerForeground) }
        val files = model.snapshot.directory()?.takeIf { it.path == directory }
        if (files?.truncated == true) Text("Showing the first 2,000 entries", style = AppTheme.caption)
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
                    model.snapshot.error()?.let { Text(it, color = AppTheme.colors.dangerForeground) }
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
                            textStyle = AppTheme.footnote.copy(fontFamily = FontFamily.Monospace),
                        )
                        Button(onClick = { model.perform(Intent.SaveFile(entry.path)) }) { Text("Save") }
                    }
                }
            }
        }
    }
}

/** The diff panel core describes: scope, turn, whitespace, then the files of the loaded diff. */
@Composable
@Suppress("LongMethod")
private fun WorkspaceDiff(model: AndroidAppModel, modifier: Modifier) {
    val thread = model.snapshot.selectedThreadId() ?: return
    val panel = model.snapshot.diff(thread)
    var menu by remember { mutableStateOf(false) }
    LaunchedEffect(thread, panel.request) { if (panel.request != null) model.perform(Intent.LoadDiff) }
    val review = model.snapshot.review()
    var files by remember { mutableStateOf<List<dev.remoteagent.core.WorkspaceDiffFile>>(emptyList()) }
    LaunchedEffect(model.snapshot.reviewRevision()) {
        files = withContext(Dispatchers.Default) { review?.diffFiles().orEmpty() }
    }
    val colors = AppTheme.colors
    Column(modifier) {
        Row(Modifier.padding(horizontal = 12.dp)) {
            Box(Modifier.weight(1f)) {
                TextButton(onClick = { menu = true }) { Text(panel.scopeLabel, color = colors.foreground) }
                AnchoredMenu(menu, { menu = false }) {
                    panel.scopes.forEach { scope ->
                        DropdownMenuItem(
                            text = { Text(if (scope.selected) "✓  ${scope.label}" else scope.label) },
                            onClick = {
                                menu = false
                                model.perform(Intent.SelectDiffScope(scope.choice))
                            },
                        )
                    }
                    panel.turns.forEach { turn ->
                        DropdownMenuItem(
                            text = { Text(if (turn.selected) "✓  ${turn.label}" else turn.label) },
                            onClick = {
                                menu = false
                                model.perform(Intent.SelectDiffTurn(turn.runId, null))
                            },
                        )
                    }
                }
            }
            TextButton(onClick = { model.perform(Intent.SetDiffIgnoreWhitespace(!panel.ignoreWhitespace)) }) {
                Text(panel.whitespaceToggleLabel, color = colors.foreground)
            }
        }
        LazyColumn(
            Modifier.weight(1f),
            contentPadding = PaddingValues(20.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            val empty = panel.emptyMessage
            when {
                empty != null -> item { Text(empty, color = colors.foregroundMuted) }
                review == null -> item { CircularProgressIndicator() }
                files.isEmpty() -> item { Text("No changes", color = colors.foregroundMuted) }
            }
            items(files, key = { it.path }) { file ->
                Column {
                    Text(file.path, style = AppTheme.label, color = colors.foreground)
                    androidx.compose.foundation.text.selection.SelectionContainer {
                        Column {
                            file.rows.forEach { row ->
                                Text(
                                    row.text,
                                    fontFamily = FontFamily.Monospace,
                                    style = AppTheme.caption,
                                    color =
                                        when (row.kind) {
                                            "+" -> colors.emerald
                                            "-" -> colors.rose
                                            else -> colors.foreground
                                        },
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
// Declarative native layout; the conversation decisions are supplied by core.
@Suppress("LongMethod", "CyclomaticComplexMethod")
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
    val requests = remember(thread) { kotlinx.coroutines.sync.Mutex() }
    suspend fun request(action: BrowserAction, user: Boolean = true) = requests.withLock {
        if (!active) return@withLock
        if (user) sending = true
        try {
            val current = frame
            val next = model.browser(BrowserRequest(thread, current?.tabId ?: "", current?.imageId ?: "", action))
            frame = next
            error = null
            if (address.isEmpty()) address = next.tabs.firstOrNull { it.id == next.tabId }?.url.orEmpty()
        } catch (failure: Exception) {
            error = failure.message
        } finally {
            if (user) sending = false
        }
    }
    fun send(action: BrowserAction) {
        scope.launch { request(action) }
    }
    LaunchedEffect(thread, active) {
        while (active && isActive) {
            if (model.snapshot.connected()) request(BrowserAction.Read, user = false)
            delay(BROWSER_REFRESH_MILLIS)
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
        error?.let { Text(it, color = AppTheme.colors.dangerForeground) }
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
                        return if (!imageMatrix.invert(matrix)) {
                            null
                        } else {
                            val point = floatArrayOf(event.x, event.y)
                            matrix.mapPoints(point)
                            point.takeIf {
                                it[0] >= 0 &&
                                    it[1] >= 0 &&
                                    it[0] < drawable.intrinsicWidth &&
                                    it[1] < drawable.intrinsicHeight
                            }
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
                visualTransformation = PasswordVisualTransformation(),
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

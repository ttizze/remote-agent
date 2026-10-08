@file:Suppress("TooGenericExceptionCaught")

package dev.remoteagent.mobile

import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.graphics.Matrix
import android.graphics.pdf.PdfRenderer
import android.os.ParcelFileDescriptor
import android.util.LruCache
import android.view.GestureDetector
import android.view.MotionEvent
import android.widget.ImageView
import androidx.activity.ComponentActivity
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.wrapContentWidth
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Check
import androidx.compose.material.icons.outlined.ChevronRight
import androidx.compose.material.icons.outlined.Info
import androidx.compose.material.icons.outlined.MoreHoriz
import androidx.compose.material.icons.outlined.Refresh
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.Icon
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
import androidx.compose.runtime.produceState
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import dev.remoteagent.core.BrowserAction
import dev.remoteagent.core.BrowserFrame
import dev.remoteagent.core.BrowserKey
import dev.remoteagent.core.BrowserRequest
import dev.remoteagent.core.DiffPanelView
import dev.remoteagent.core.DiffScopeChoice
import dev.remoteagent.core.FileEntry
import dev.remoteagent.core.Intent
import dev.remoteagent.core.ReviewFilesView
import dev.remoteagent.core.WorkspaceDiffFile
import java.io.File
import java.util.UUID
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.delay
import kotlinx.coroutines.ensureActive
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext

private const val BROWSER_REFRESH_MILLIS = 500L
private const val PDF_PAGE_WIDTH = 1200
private const val PDF_PAGE_HEIGHT = 1600
private const val PDF_PAGE_CACHE_SIZE = 4

/** Downloads a Host PDF into the resource cache and renders its pages locally. */
@Composable
internal fun PdfScreen(model: AndroidAppModel, path: String) {
    val context = LocalContext.current
    var local by remember(path, model.profileId) { mutableStateOf<File?>(null) }
    var error by remember(path, model.profileId) { mutableStateOf<String?>(null) }
    LaunchedEffect(path, model.profileId) {
        val temporary = File(context.cacheDir, "pdf-${UUID.randomUUID()}.pdf")
        try {
            model.download(path, temporary.path)
            local = temporary
        } catch (cancellation: CancellationException) {
            temporary.delete()
            throw cancellation
        } catch (failure: Exception) {
            temporary.delete()
            error = failure.message ?: "Unable to open PDF"
        }
    }
    DisposableEffect(local) {
        val downloaded = local
        onDispose { downloaded?.delete() }
    }
    ScreenScaffold(File(path).name, onBack = model::back) {
        when {
            error != null -> Text(error!!, Modifier.padding(20.dp), color = AppTheme.colors.dangerForeground)
            local == null ->
                Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
                    CircularProgressIndicator(color = AppTheme.colors.iconMuted)
                }
            else -> PdfPages(local!!)
        }
    }
}

@Composable
private fun PdfPages(file: File) {
    val cache = remember(file.path) { LruCache<Int, Bitmap>(PDF_PAGE_CACHE_SIZE) }
    DisposableEffect(cache) { onDispose { cache.evictAll() } }
    val pageCount by
        produceState<Result<Int>?>(null, file.path) {
            value =
                try {
                    Result.success(
                        withContext(Dispatchers.IO) {
                            ParcelFileDescriptor.open(file, ParcelFileDescriptor.MODE_READ_ONLY).use { descriptor ->
                                PdfRenderer(descriptor).use { renderer -> renderer.pageCount }
                            }
                        }
                    )
                } catch (cancellation: CancellationException) {
                    throw cancellation
                } catch (failure: Exception) {
                    Result.failure(failure)
                }
        }
    if (pageCount == null) {
        CircularProgressIndicator(Modifier.padding(20.dp), color = AppTheme.colors.iconMuted)
    } else if (pageCount!!.isFailure) {
        Text(
            pageCount!!.exceptionOrNull()?.message ?: "Unable to render PDF",
            Modifier.padding(20.dp),
            color = AppTheme.colors.dangerForeground,
        )
    } else {
        val count = pageCount!!.getOrThrow()
        LazyColumn(Modifier.fillMaxSize(), contentPadding = PaddingValues(12.dp)) {
            items(count, key = { it }) { index -> PdfPage(file, index, cache) }
        }
    }
}

@Composable
private fun PdfPage(file: File, index: Int, cache: LruCache<Int, Bitmap>) {
    val rendered by
        produceState<Result<Bitmap>?>(null, file.path, index) {
            value =
                try {
                    Result.success(renderPdfPage(file, index, cache))
                } catch (cancellation: CancellationException) {
                    throw cancellation
                } catch (failure: Exception) {
                    Result.failure(failure)
                }
        }
    when {
        rendered == null ->
            Box(Modifier.fillMaxWidth().height(180.dp), contentAlignment = Alignment.Center) {
                CircularProgressIndicator(color = AppTheme.colors.iconMuted)
            }
        rendered!!.isFailure ->
            Text(
                rendered!!.exceptionOrNull()?.message ?: "Unable to render PDF page",
                Modifier.padding(20.dp),
                color = AppTheme.colors.dangerForeground,
            )
        else ->
            Image(
                rendered!!.getOrThrow().asImageBitmap(),
                "PDF page ${index + 1}",
                Modifier.fillMaxWidth().heightIn(max = PDF_PAGE_HEIGHT.dp).padding(bottom = 12.dp),
                contentScale = androidx.compose.ui.layout.ContentScale.Fit,
            )
    }
}

private suspend fun renderPdfPage(file: File, index: Int, cache: LruCache<Int, Bitmap>): Bitmap {
    cache.get(index)?.let {
        return it
    }
    currentCoroutineContext().ensureActive()
    // Keep ownership outside withContext: its prompt cancellation can discard
    // a successfully rendered result before control returns to the UI thread.
    var pending: Bitmap? = null
    try {
        val bitmap =
            withContext(Dispatchers.IO) {
                ParcelFileDescriptor.open(file, ParcelFileDescriptor.MODE_READ_ONLY).use { descriptor ->
                    PdfRenderer(descriptor).use { renderer ->
                        require(index in 0 until renderer.pageCount) { "PDF page is unavailable" }
                        renderer.openPage(index).use { page ->
                            val size =
                                requireNotNull(
                                    dev.remoteagent.core.fitImageDisplaySize(
                                        page.width.toDouble(),
                                        page.height.toDouble(),
                                        PDF_PAGE_WIDTH.toDouble(),
                                        PDF_PAGE_HEIGHT.toDouble(),
                                    )
                                ) {
                                    "PDF page dimensions are invalid"
                                }
                            Bitmap.createBitmap(
                                    size.width.toInt().coerceAtLeast(1),
                                    size.height.toInt().coerceAtLeast(1),
                                    Bitmap.Config.ARGB_8888,
                                )
                                .also { candidate ->
                                    pending = candidate
                                    page.render(candidate, null, null, PdfRenderer.Page.RENDER_MODE_FOR_DISPLAY)
                                    currentCoroutineContext().ensureActive()
                                }
                        }
                    }
                }
            }
        currentCoroutineContext().ensureActive()
        cache.put(index, bitmap)
        pending = null
        return bitmap
    } finally {
        pending?.recycle()
    }
}

/** The open thread's files, diff and browser, each as its own screen. */
@Composable
internal fun WorkspaceScreen(model: AndroidAppModel, tab: WorkspaceTab, file: String? = null, line: ULong? = null) {
    if (tab == WorkspaceTab.Files && file != null && dev.remoteagent.core.isPdfFile(file)) {
        PdfScreen(model, file)
        return
    }
    if (tab == WorkspaceTab.Diff) {
        ReviewScreen(model)
        return
    }
    ScreenScaffold(tab.name, onBack = model::back) {
        Column(Modifier.fillMaxSize()) {
            when (tab) {
                WorkspaceTab.Files -> WorkspaceFiles(model, file, line, Modifier.weight(1f))
                WorkspaceTab.Diff -> Unit
                WorkspaceTab.Browser -> WorkspaceBrowser(model, Modifier.weight(1f))
            }
        }
    }
}

@Composable
// Declarative native layout; the conversation decisions are supplied by core.
@Suppress("LongMethod", "CyclomaticComplexMethod")
private fun WorkspaceFiles(model: AndroidAppModel, linkedFile: String?, line: ULong?, modifier: Modifier) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    var directory by remember {
        mutableStateOf(linkedFile?.let { File(it).parent } ?: model.snapshot.currentDirectory())
    }
    var path by remember { mutableStateOf(directory) }
    // A linked file opens over its folder.
    var selected by remember { mutableStateOf(linkedFile?.let { FileEntry(File(it).name, it, false, 0uL) }) }
    LaunchedEffect(linkedFile) { linkedFile?.let { model.perform(Intent.ReadFile(it, false)) } }
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
                            } else if (dev.remoteagent.core.isPdfFile(entry.path)) {
                                model.navigate(Route.Pdf(entry.path))
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
                    else if (line != null && entry.path == linkedFile) {
                        val lines = text.split('\n')
                        val scroll = androidx.compose.foundation.lazy.rememberLazyListState()
                        val horizontal = rememberScrollState()
                        val wrap = AppTheme.codeWordWrap
                        LaunchedEffect(file, line, lines.size) {
                            val target = dev.remoteagent.core.markdownLineTarget(line, lines.size.toULong())
                            if (target != null) scroll.scrollToItem((target - 1uL).toInt())
                        }
                        androidx.compose.foundation.text.selection.SelectionContainer(
                            Modifier.weight(1f).then(if (wrap) Modifier else Modifier.horizontalScroll(horizontal))
                        ) {
                            LazyColumn(
                                state = scroll,
                                modifier =
                                    if (wrap) Modifier.fillMaxWidth() else Modifier.wrapContentWidth(unbounded = true),
                            ) {
                                items(lines.size) { index ->
                                    Row(
                                        (if (wrap) Modifier.fillMaxWidth()
                                            else Modifier.wrapContentWidth(unbounded = true))
                                            .padding(horizontal = 4.dp, vertical = 2.dp),
                                        verticalAlignment = Alignment.Top,
                                    ) {
                                        Text(
                                            (index + 1).toString(),
                                            fontFamily = AppTheme.mono,
                                            fontSize = AppTheme.codeLineNumberFontSize.sp,
                                            lineHeight = AppTheme.codeLineHeight.sp,
                                            color = AppTheme.colors.foregroundTertiary,
                                            textAlign = TextAlign.End,
                                            modifier = Modifier.width(36.dp),
                                        )
                                        Text(
                                            lines[index].ifEmpty { " " },
                                            fontFamily = AppTheme.mono,
                                            fontSize = AppTheme.codeFontSize.sp,
                                            lineHeight = AppTheme.codeLineHeight.sp,
                                            softWrap = wrap,
                                            modifier =
                                                (if (wrap) Modifier.weight(1f) else Modifier).padding(start = 8.dp),
                                        )
                                    }
                                }
                            }
                        }
                    } else {
                        val horizontal = rememberScrollState()
                        BasicTextField(
                            text,
                            { value ->
                                text = value
                                val current = ++revision
                                pending = current
                                model.perform(Intent.EditFile(entry.path, value)) {
                                    if (pending == current) pending = null
                                }
                            },
                            Modifier.weight(1f)
                                .then(
                                    if (AppTheme.codeWordWrap) Modifier.fillMaxWidth()
                                    else Modifier.wrapContentWidth(unbounded = true)
                                )
                                .border(1.dp, AppTheme.colors.border, RoundedCornerShape(4.dp))
                                .horizontalScroll(horizontal, enabled = !AppTheme.codeWordWrap)
                                .padding(12.dp),
                            textStyle =
                                AppTheme.footnote.copy(
                                    fontFamily = AppTheme.mono,
                                    fontSize = AppTheme.codeFontSize.sp,
                                    lineHeight = AppTheme.codeLineHeight.sp,
                                ),
                            maxLines = Int.MAX_VALUE,
                        )
                        Button(onClick = { model.perform(Intent.SaveFile(entry.path)) }) { Text("Save") }
                    }
                }
            }
        }
    }
}

/** "Review changes": the selected diff, chosen from the header menu, then its files. */
@Composable
@Suppress("LongMethod", "CyclomaticComplexMethod")
private fun ReviewScreen(model: AndroidAppModel) {
    val thread = model.snapshot.selectedThreadId()
    val panel = thread?.let { model.snapshot.diff(it) }
    LaunchedEffect(thread, panel?.request) { if (panel?.request != null) model.perform(Intent.LoadDiff) }
    val review = model.snapshot.review()
    var files by remember { mutableStateOf<List<WorkspaceDiffFile>>(emptyList()) }
    LaunchedEffect(model.snapshot.reviewRevision()) {
        files = withContext(Dispatchers.Default) { review?.diffFiles().orEmpty() }
    }
    val colors = AppTheme.colors
    val git = panel?.git
    val choice = panel?.scopes?.firstOrNull { it.selected }?.choice
    val gitScope = choice == DiffScopeChoice.Branch || choice == DiffScopeChoice.Unstaged
    // A diff too large to send whole lists every file and reads each one on its own.
    val filesRevision = git?.filesRevision?.takeIf { gitScope }
    var lazyFiles by remember { mutableStateOf<ReviewFilesView?>(null) }
    LaunchedEffect(thread, filesRevision) {
        lazyFiles =
            if (thread == null || filesRevision == null) null
            else withContext(Dispatchers.Default) { model.snapshot.reviewFiles(thread) }
    }
    val lazy = lazyFiles?.takeIf { filesRevision != null }
    val shownFiles =
        remember(lazy, files) {
            lazy?.files?.map { WorkspaceDiffFile(it.path, it.additions, it.deletions, it.rows) } ?: files
        }
    val notices = remember(lazy) { lazy?.files?.mapNotNull { file -> file.notice?.let { file.path to it } }?.toMap() }
    val loading = (gitScope && git?.loading == true) || (panel?.request != null && review == null)
    val subtitle =
        panel?.let {
            listOf(
                    it.scopeLabel,
                    "+${shownFiles.sumOf { file -> file.additions ?: 0uL }}",
                    "-${shownFiles.sumOf { file -> file.deletions ?: 0uL }}",
                )
                .joinToString(" · ")
        } ?: "Select a diff"
    ScreenScaffold(
        "Review changes",
        onBack = model::back,
        subtitle = subtitle,
        actions = { if (panel != null) ReviewMenu(model, panel, loading) },
    ) {
        LazyColumn(
            Modifier.fillMaxSize(),
            contentPadding = PaddingValues(vertical = 8.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            val pullRequestLabel = model.snapshot.selectedPullRequestLabel()
            val pullRequestUrl = model.snapshot.selectedPullRequestUrl()
            if (pullRequestLabel != null && pullRequestUrl != null)
                item {
                    val context = LocalContext.current
                    TextButton(
                        onClick = {
                            context.startActivity(
                                android.content.Intent(
                                    android.content.Intent.ACTION_VIEW,
                                    android.net.Uri.parse(pullRequestUrl),
                                )
                            )
                        },
                        modifier = Modifier.fillMaxWidth().padding(horizontal = 8.dp),
                    ) {
                        Text("$pullRequestLabel  ↗", color = colors.primaryText)
                    }
                }
            val error = git?.error?.takeIf { gitScope }
            val empty = panel?.emptyMessage
            when {
                panel == null || empty != null ->
                    item {
                        ReviewMessage(
                            "No review diffs",
                            empty ?: "This thread has no ready turn diffs and the worktree diff is empty.",
                        )
                    }
                error != null -> item { ReviewCard("Review unavailable", error, colors.card, colors.foreground) }
                loading ->
                    item {
                        Column(
                            Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 24.dp),
                            horizontalAlignment = Alignment.CenterHorizontally,
                            verticalArrangement = Arrangement.spacedBy(12.dp),
                        ) {
                            CircularProgressIndicator(
                                Modifier.size(18.dp),
                                color = colors.iconMuted,
                                strokeWidth = 2.dp,
                            )
                            Text("Loading diff…", style = AppTheme.label, color = colors.foregroundMuted)
                        }
                    }
                shownFiles.isEmpty() ->
                    item { ReviewMessage("No changes", git?.subtitle?.takeIf { gitScope } ?: "This diff is empty.") }
            }
            if (gitScope && git?.truncated == true && !loading)
                item {
                    ReviewCard(
                        "Partial diff",
                        "Diff output hit the server size cap. Showing the available excerpt.",
                        colors.warning,
                        colors.warningForeground,
                        notice = true,
                    )
                }
            if (!loading)
                items(shownFiles, key = { it.path }) { file ->
                    if (notices != null)
                        LaunchedEffect(file.path) { model.perform(Intent.RevealDiffFile(file.path, false)) }
                    Column(Modifier.padding(horizontal = 16.dp)) {
                        Row(
                            Modifier.then(
                                if (notices != null)
                                    Modifier.clickable { model.perform(Intent.RevealDiffFile(file.path, true)) }
                                else Modifier
                            ),
                            verticalAlignment = Alignment.CenterVertically,
                        ) {
                            Text(
                                file.path,
                                Modifier.weight(1f),
                                style = AppTheme.label,
                                fontWeight = FontWeight.Medium,
                                color = colors.foreground,
                            )
                            file.deletions?.let { Text("-$it", style = AppTheme.micro, color = colors.rose) }
                            file.additions?.let {
                                Text(
                                    "+$it",
                                    Modifier.padding(start = 4.dp),
                                    style = AppTheme.micro,
                                    color = colors.emerald,
                                )
                            }
                        }
                        androidx.compose.foundation.text.selection.SelectionContainer {
                            Column(
                                Modifier.then(
                                    if (AppTheme.codeWordWrap) Modifier.fillMaxWidth()
                                    else Modifier.horizontalScroll(rememberScrollState())
                                )
                            ) {
                                file.rows.forEach { row ->
                                    Text(
                                        row.text,
                                        Modifier.then(if (AppTheme.codeWordWrap) Modifier.fillMaxWidth() else Modifier),
                                        fontFamily = AppTheme.mono,
                                        fontSize = AppTheme.codeFontSize.sp,
                                        lineHeight = AppTheme.codeLineHeight.sp,
                                        softWrap = AppTheme.codeWordWrap,
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
                        notices?.get(file.path)?.let { ReviewFileNotice(it) }
                    }
                }
        }
    }
}

/** "Select diff": Changes, Uncommitted, the latest turn, every turn, and refresh. */
@Composable
private fun ReviewMenu(model: AndroidAppModel, panel: DiffPanelView, loading: Boolean) {
    var open by remember { mutableStateOf(false) }
    var turns by remember { mutableStateOf(false) }
    val repository = panel.git?.isRepo != false
    Box {
        HeaderIconButton(Icons.Outlined.MoreHoriz, "Select diff") { open = true }
        AnchoredMenu(
            open || turns,
            {
                open = false
                turns = false
            },
        ) {
            if (turns)
                panel.turns.forEach { turn ->
                    MenuChoice(turn.label, turn.selected) {
                        turns = false
                        model.perform(Intent.SelectDiffTurn(turn.runId, null))
                    }
                }
            else {
                panel.scopes.forEach { scope ->
                    val git = scope.choice == DiffScopeChoice.Branch || scope.choice == DiffScopeChoice.Unstaged
                    MenuChoice(scope.label, scope.selected, enabled = repository || !git) {
                        open = false
                        model.perform(Intent.SelectDiffScope(scope.choice))
                    }
                }
                if (panel.turns.isNotEmpty())
                    DropdownMenuItem(
                        text = { Text("Turn", style = AppTheme.footnote) },
                        trailingIcon = { Icon(Icons.Outlined.ChevronRight, null) },
                        onClick = {
                            open = false
                            turns = true
                        },
                    )
                DropdownMenuItem(
                    text = { Text("Refresh current diff", style = AppTheme.footnote) },
                    leadingIcon = { Icon(Icons.Outlined.Refresh, null) },
                    enabled = panel.request != null && !loading,
                    onClick = {
                        open = false
                        model.perform(Intent.LoadDiff)
                    },
                )
            }
        }
    }
}

@Composable
private fun MenuChoice(label: String, selected: Boolean, enabled: Boolean = true, onClick: () -> Unit) {
    DropdownMenuItem(
        text = { Text(label, style = AppTheme.footnote) },
        trailingIcon = if (selected) ({ Icon(Icons.Outlined.Check, null) }) else null,
        enabled = enabled,
        onClick = onClick,
    )
}

/** Why a file of a diff read file by file shows no or partial rows. */
@Composable
private fun ReviewFileNotice(notice: String) {
    Row(
        Modifier.fillMaxWidth().heightIn(min = 44.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        Icon(Icons.Outlined.Info, null, Modifier.size(16.dp), tint = AppTheme.colors.foregroundMuted)
        Text(notice, style = AppTheme.caption, color = AppTheme.colors.foregroundMuted, maxLines = 1)
    }
}

@Composable
private fun ReviewMessage(title: String, detail: String) {
    Column(
        Modifier.fillMaxWidth().padding(horizontal = 24.dp, vertical = 20.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        Text(title, style = AppTheme.footnote, fontWeight = FontWeight.Bold, color = AppTheme.colors.foreground)
        Text(
            detail,
            Modifier.padding(top = 8.dp),
            style = AppTheme.label,
            color = AppTheme.colors.foregroundMuted,
            textAlign = TextAlign.Center,
        )
    }
}

@Composable
private fun ReviewCard(title: String, detail: String, container: Color, titleColor: Color, notice: Boolean = false) {
    Column(
        Modifier.fillMaxWidth()
            .padding(8.dp)
            .background(container, RoundedCornerShape(20.dp))
            .padding(horizontal = 16.dp, vertical = 12.dp)
    ) {
        Text(
            if (notice) title.uppercase() else title,
            style = if (notice) AppTheme.label else AppTheme.footnote,
            fontWeight = FontWeight.Bold,
            color = titleColor,
        )
        Text(detail, style = AppTheme.label, color = AppTheme.colors.foregroundMuted)
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

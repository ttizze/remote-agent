// Declarative native layout with the fixed mobile metrics; the terminal state is supplied by core.
@file:Suppress("LongMethod", "LongParameterList", "MagicNumber", "CyclomaticComplexMethod", "TooManyFunctions")

package dev.remoteagent.mobile

import android.view.KeyEvent
import android.view.inputmethod.InputMethodManager
import androidx.compose.foundation.background
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.isImeVisible
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.KeyboardArrowRight
import androidx.compose.material.icons.outlined.Add
import androidx.compose.material.icons.outlined.Check
import androidx.compose.material.icons.outlined.FormatSize
import androidx.compose.material.icons.outlined.Keyboard
import androidx.compose.material.icons.outlined.KeyboardHide
import androidx.compose.material.icons.outlined.Terminal
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
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
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.toArgb
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.AndroidView
import com.termux.terminal.TerminalSession
import com.termux.view.TerminalView
import dev.remoteagent.core.Intent
import dev.remoteagent.core.Outcome
import dev.remoteagent.core.Snapshot
import dev.remoteagent.core.TerminalTab
import dev.remoteagent.core.TerminalTextSize

/** The terminal a thread falls back to when `closing` goes away: the nearest lower id, else the nearest higher. */
internal fun fallbackTerminal(tabs: List<TerminalTab>, closing: String): String? {
    val others = tabs.map { it.terminalId }.filter { it != closing }
    val order = { id: String -> id.substringAfterLast('-').toIntOrNull() ?: Int.MAX_VALUE }
    val rank = order(closing)
    return others.filter { order(it) < rank }.maxByOrNull(order) ?: others.minByOrNull(order)
}

/** The last path component, as the terminal menu names a directory. */
internal fun folderName(path: String): String? = path.trimEnd('/').substringAfterLast('/').takeIf { it.isNotEmpty() }

/**
 * A thread's terminal: one shell at a time, the others in the header menu. With the keyboard hidden a bar offers
 * "Attach output" and "Show keyboard"; with it shown the extra keys sit above it.
 */
@OptIn(ExperimentalLayoutApi::class)
@Composable
internal fun TerminalScreen(
    model: AndroidAppModel,
    threadId: String,
    initialTerminalId: String,
    project: String?,
    cwd: String?,
) {
    var terminalId by remember { mutableStateOf(initialTerminalId) }
    var native by remember { mutableStateOf<NativeTerminal?>(null) }
    var captured by remember { mutableStateOf<String?>(null) }
    var noOutput by remember { mutableStateOf(false) }
    val opened = remember { mutableSetOf<String>() }
    val current by rememberUpdatedState(model)
    DisposableEffect(threadId) { onDispose { opened.forEach { current.perform(Intent.DetachTerminal(threadId, it)) } } }
    val tabs = model.snapshot.terminals(threadId)
    val selected = tabs.firstOrNull { it.terminalId == terminalId }
    val textSize = model.snapshot.terminalTextSize()
    // An exited or closed shell closes, and the screen moves to the thread's other terminal.
    LaunchedEffect(terminalId, selected?.exited, selected == null) {
        if (terminalId !in opened || (selected != null && !selected.exited)) return@LaunchedEffect
        if (selected != null) model.perform(Intent.CloseTerminal(threadId, terminalId))
        opened.remove(terminalId)
        val next = fallbackTerminal(tabs, terminalId)
        if (next != null) terminalId = next else model.back()
    }
    if (noOutput)
        AlertDialog(
            onDismissRequest = { noOutput = false },
            containerColor = AppTheme.colors.cardAlt,
            shape = RoundedCornerShape(28.dp),
            title = { Text("No terminal output", style = AppTheme.title) },
            text = {
                Text(
                    "There is no visible output to attach.",
                    style = AppTheme.footnote,
                    color = AppTheme.colors.foregroundSecondary,
                )
            },
            confirmButton = {
                TextButton(onClick = { noOutput = false }) { Text("OK", color = AppTheme.colors.foreground) }
            },
        )
    ScreenScaffold(
        "Terminal",
        onBack = model::back,
        subtitle = project,
        actions = {
            TerminalMenu(
                tabs,
                selected,
                cwd,
                textSize,
                onTextSize = { model.perform(Intent.SetTerminalFontSize(it)) },
                onSelect = { terminalId = it },
                onNew = { terminalId = "" },
            )
        },
    ) {
        Column(Modifier.fillMaxSize().imePadding()) {
            key(terminalId) {
                TerminalBody(
                    model,
                    threadId,
                    terminalId,
                    textSize.size.toFloat(),
                    onNative = { native = it },
                    onOpened = { id ->
                        opened.add(id)
                        terminalId = id
                    },
                    Modifier.weight(1f),
                )
            }
            if (WindowInsets.isImeVisible)
                TerminalKeys(native, onDismissKeyboard = { native?.hideKeyboard() }) {
                    if (terminalId.isNotEmpty()) model.perform(Intent.ClearTerminal(threadId, terminalId))
                }
            else
                KeyboardHiddenBar(
                    onAttach = {
                        val output = native?.visibleText()
                        if (output.isNullOrBlank()) noOutput = true else captured = output
                    },
                    onShowKeyboard = { native?.showKeyboard() },
                )
        }
    }
    // Over the terminal, which keeps its shell attached while the range is picked.
    captured?.let { output ->
        TerminalContextSheet(
            output,
            onClose = { captured = null },
            onAttach = { start, end ->
                model.perform(
                    Intent.AttachTerminalOutput(
                        threadId,
                        model.snapshot.terminalOutputContext(threadId, terminalId, output, start, end),
                    )
                ) { result ->
                    if (result.isSuccess) {
                        captured = null
                        model.back()
                    }
                }
            },
        )
    }
}

private fun NativeTerminal.showKeyboard() {
    view.requestFocus()
    view.context.getSystemService(InputMethodManager::class.java).showSoftInput(view, InputMethodManager.SHOW_IMPLICIT)
}

private fun NativeTerminal.hideKeyboard() {
    view.context.getSystemService(InputMethodManager::class.java).hideSoftInputFromWindow(view.windowToken, 0)
    view.clearFocus()
}

/** "Attach output" and "Show keyboard" while the keyboard is hidden. */
@Composable
private fun KeyboardHiddenBar(onAttach: () -> Unit, onShowKeyboard: () -> Unit) {
    val colors = AppTheme.colors
    Row(
        Modifier.fillMaxWidth().heightIn(min = 56.dp).background(colors.cardAlt).padding(horizontal = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        TextButton(onClick = onAttach) {
            Text("Attach output", style = AppTheme.footnote, fontWeight = FontWeight.Medium, color = colors.primaryText)
        }
        Spacer(Modifier.weight(1f))
        IconButton(onClick = onShowKeyboard) { Icon(Icons.Outlined.Keyboard, "Show keyboard", tint = colors.icon) }
    }
}

/** The header menu: the status, the text size, every live shell, and a new one. */
@Composable
private fun TerminalMenu(
    tabs: List<TerminalTab>,
    selected: TerminalTab?,
    cwd: String?,
    textSize: TerminalTextSize,
    onTextSize: (Double) -> Unit,
    onSelect: (String) -> Unit,
    onNew: () -> Unit,
) {
    val colors = AppTheme.colors
    var open by remember { mutableStateOf(false) }
    var sizes by remember { mutableStateOf(false) }
    fun close() {
        open = false
        sizes = false
    }
    val folder = cwd?.let(::folderName)
    Box {
        HeaderIconButton(Icons.Outlined.Terminal, "Terminal options") { open = true }
        AnchoredMenu(open, ::close) {
            if (sizes) {
                listOf(textSize.decrease, textSize.increase).forEach { step ->
                    DropdownMenuItem(
                        text = { Text(step.label, style = AppTheme.footnote, color = colors.foreground) },
                        enabled = step.enabled,
                        onClick = {
                            close()
                            onTextSize(step.size)
                        },
                    )
                }
                return@AnchoredMenu
            }
            Text(
                selected?.menuStatus ?: "Starting",
                Modifier.padding(horizontal = 16.dp, vertical = 8.dp),
                style = AppTheme.caption,
                color = colors.foregroundSecondary,
            )
            HorizontalDivider(color = colors.border)
            DropdownMenuItem(
                text = { Text("Text size", style = AppTheme.footnote, color = colors.foreground) },
                leadingIcon = { Icon(Icons.Outlined.FormatSize, null, tint = colors.icon) },
                trailingIcon = { Icon(Icons.AutoMirrored.Outlined.KeyboardArrowRight, null, tint = colors.icon) },
                onClick = { sizes = true },
            )
            tabs.forEach { tab ->
                MenuRow(
                    Icons.Outlined.Terminal,
                    tab.label,
                    listOfNotNull(tab.menuStatus, folderName(tab.cwd)).joinToString(" · "),
                    checked = tab.terminalId == selected?.terminalId,
                ) {
                    close()
                    onSelect(tab.terminalId)
                }
            }
            MenuRow(Icons.Outlined.Add, "Open new terminal", "Start another shell in ${folder ?: "this workspace"}") {
                close()
                onNew()
            }
        }
    }
}

@Composable
private fun MenuRow(icon: ImageVector, title: String, subtitle: String, checked: Boolean = false, onClick: () -> Unit) {
    DropdownMenuItem(
        text = {
            Column {
                Text(title, style = AppTheme.footnote, color = AppTheme.colors.foreground)
                Text(subtitle, style = AppTheme.caption, color = AppTheme.colors.foregroundMuted)
            }
        },
        leadingIcon = { Icon(icon, null, tint = AppTheme.colors.icon) },
        trailingIcon = if (checked) ({ Icon(Icons.Outlined.Check, null, tint = AppTheme.colors.icon) }) else null,
        onClick = onClick,
    )
}

@Composable
private fun TerminalBody(
    model: AndroidAppModel,
    threadId: String,
    terminalId: String,
    fontSize: Float,
    onNative: (NativeTerminal?) -> Unit,
    onOpened: (String) -> Unit,
    modifier: Modifier,
) {
    var sequence by remember { mutableStateOf(0uL) }
    var native by remember { mutableStateOf<NativeTerminal?>(null) }
    val perform by rememberUpdatedState(model::perform)
    val opened by rememberUpdatedState(onOpened)
    val colors = AppTheme.colors
    val snapshot: Snapshot = model.snapshot
    val view = if (terminalId.isEmpty()) null else snapshot.terminal(threadId, terminalId, sequence)
    DisposableEffect(native) {
        onNative(native)
        onDispose { onNative(null) }
    }
    Column(modifier.background(colors.terminalBackground)) {
        if (view?.loading == true || terminalId.isEmpty())
            Box(Modifier.fillMaxWidth().padding(8.dp), contentAlignment = Alignment.Center) {
                CircularProgressIndicator(Modifier.size(16.dp), color = colors.iconMuted, strokeWidth = 2.dp)
            }
        AndroidView(
            modifier = Modifier.weight(1f).fillMaxWidth(),
            factory = { context ->
                var started = false
                NativeTerminal(
                        context,
                        object : TerminalSession.Transport {
                            override fun write(data: ByteArray) {
                                if (terminalId.isNotEmpty())
                                    perform(Intent.WriteTerminal(threadId, terminalId, data)) {}
                            }

                            override fun resize(columns: Int, rows: Int) {
                                val cols = columns.toUShort()
                                val height = rows.toUShort()
                                when {
                                    started && terminalId.isNotEmpty() ->
                                        perform(Intent.ResizeTerminal(threadId, terminalId, cols, height)) {}
                                    started -> Unit
                                    terminalId.isEmpty() -> {
                                        started = true
                                        perform(Intent.NewTerminal(threadId, cols, height)) { result ->
                                            (result.getOrNull() as? Outcome.TerminalOpened)?.let {
                                                opened(it.terminalId)
                                            }
                                        }
                                    }
                                    else -> {
                                        started = true
                                        perform(Intent.OpenTerminal(threadId, terminalId, cols, height)) { result ->
                                            if (result.isSuccess) opened(terminalId)
                                        }
                                    }
                                }
                            }
                        },
                        colors.terminalBackground.toArgb(),
                        colors.terminalForeground.toArgb(),
                        colors.terminalCursor.toArgb(),
                        fontSize,
                    )
                    .let { terminal ->
                        native = terminal
                        terminal.view
                    }
            },
            update = {
                val terminal = native ?: return@AndroidView
                terminal.setFontSize(fontSize)
                view?.output?.forEach { chunk ->
                    terminal.feed(
                        chunk.sequence.toLong(),
                        chunk.data,
                        chunk.resetSize?.cols?.toInt() ?: 0,
                        chunk.resetSize?.rows?.toInt() ?: 0,
                    )
                    sequence = chunk.sequence
                }
            },
        )
    }
}

private sealed interface ExtraKey {
    val label: String

    data class Code(override val label: String, val code: Int) : ExtraKey

    data class Symbol(override val label: String) : ExtraKey

    data class Toggle(override val label: String, val control: Boolean) : ExtraKey

    data object Paste : ExtraKey {
        override val label = "paste"
    }

    data object Clear : ExtraKey {
        override val label = "CLEAR"
    }
}

private val EXTRA_KEYS =
    listOf(
        ExtraKey.Code("esc", KeyEvent.KEYCODE_ESCAPE),
        ExtraKey.Toggle("CTRL", control = true),
        ExtraKey.Toggle("ALT", control = false),
        ExtraKey.Code("tab", KeyEvent.KEYCODE_TAB),
        ExtraKey.Paste,
        ExtraKey.Clear,
        ExtraKey.Code("↑", KeyEvent.KEYCODE_DPAD_UP),
        ExtraKey.Code("↓", KeyEvent.KEYCODE_DPAD_DOWN),
        ExtraKey.Code("←", KeyEvent.KEYCODE_DPAD_LEFT),
        ExtraKey.Code("→", KeyEvent.KEYCODE_DPAD_RIGHT),
        ExtraKey.Symbol("~"),
        ExtraKey.Symbol("|"),
        ExtraKey.Symbol("/"),
        ExtraKey.Symbol("-"),
    )

/** The extra keys row over the keyboard: keys, one-shot modifiers, paste, clear, then "Dismiss keyboard". */
@Composable
private fun TerminalKeys(terminal: NativeTerminal?, onDismissKeyboard: () -> Unit, onClear: () -> Unit) {
    val colors = AppTheme.colors
    var control by remember { mutableStateOf(false) }
    var alt by remember { mutableStateOf(false) }
    DisposableEffect(terminal) {
        terminal?.onModifiersReleased = Runnable {
            control = false
            alt = false
        }
        onDispose { terminal?.onModifiersReleased = null }
    }
    fun press(key: ExtraKey) {
        val native = terminal ?: return
        when (key) {
            is ExtraKey.Code -> {
                native.view.onKeyDown(key.code, KeyEvent(0, 0, KeyEvent.ACTION_DOWN, key.code, 0))
                native.releaseModifiers()
            }
            is ExtraKey.Symbol ->
                native.view.inputCodePoint(
                    TerminalView.KEY_EVENT_SOURCE_SOFT_KEYBOARD,
                    key.label.codePointAt(0),
                    false,
                    false,
                )
            is ExtraKey.Toggle ->
                if (key.control) {
                    native.controlArmed = !native.controlArmed
                    control = native.controlArmed
                } else {
                    native.altArmed = !native.altArmed
                    alt = native.altArmed
                }
            ExtraKey.Paste -> native.onPasteTextFromClipboard(native.session)
            ExtraKey.Clear -> onClear()
        }
    }
    HorizontalDivider(color = colors.border)
    Row(
        Modifier.fillMaxWidth()
            .height(52.dp)
            .background(colors.terminalBackground)
            .padding(horizontal = 8.dp, vertical = 4.dp),
        horizontalArrangement = Arrangement.spacedBy(4.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Row(
            Modifier.weight(1f).horizontalScroll(rememberScrollState()),
            horizontalArrangement = Arrangement.spacedBy(4.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            EXTRA_KEYS.forEach { key ->
                val armed = key is ExtraKey.Toggle && (if (key.control) control else alt)
                Surface(
                    onClick = { press(key) },
                    shape = RoundedCornerShape(10.dp),
                    color = if (armed) colors.secondary else colors.terminalBackground,
                    modifier =
                        Modifier.height(44.dp).widthIn(min = if (key.label.length > 1) 56.dp else 44.dp, max = 120.dp),
                ) {
                    Box(contentAlignment = Alignment.Center) {
                        Text(
                            key.label,
                            style = AppTheme.footnote,
                            fontWeight = if (armed) FontWeight.Bold else FontWeight.Medium,
                            color = colors.terminalForeground,
                        )
                    }
                }
            }
        }
        IconButton(onClick = onDismissKeyboard, modifier = Modifier.size(44.dp)) {
            Icon(Icons.Outlined.KeyboardHide, "Dismiss keyboard", tint = colors.terminalForeground)
        }
    }
}

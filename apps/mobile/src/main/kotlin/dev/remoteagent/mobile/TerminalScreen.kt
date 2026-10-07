package dev.remoteagent.mobile

import android.view.KeyEvent
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Add
import androidx.compose.material.icons.outlined.Close
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.key
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.toArgb
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.AndroidView
import com.termux.terminal.TerminalSession
import dev.remoteagent.core.Intent
import dev.remoteagent.core.Outcome
import dev.remoteagent.core.Snapshot
import dev.remoteagent.core.TerminalTab

private const val TERMINAL_FONT_SIZE = 12

/** A thread's terminals: tabs over one native terminal, keyed by (thread, terminal id). */
@Composable
internal fun TerminalScreen(model: AndroidAppModel, threadId: String, initialTerminalId: String) {
    var terminalId by remember { mutableStateOf(initialTerminalId) }
    val opened = remember { mutableSetOf<String>() }
    val current by rememberUpdatedState(model)
    DisposableEffect(threadId) { onDispose { opened.forEach { current.perform(Intent.DetachTerminal(threadId, it)) } } }
    val tabs = model.snapshot.terminals(threadId)
    val selected = tabs.firstOrNull { it.terminalId == terminalId }
    ScreenScaffold(selected?.label ?: "Terminal", onBack = model::back, subtitle = selected?.status) {
        Column(Modifier.fillMaxSize().imePadding()) {
            TerminalTabs(tabs, terminalId, onSelect = { terminalId = it }, onNew = { terminalId = "" }) { id ->
                opened.remove(id)
                model.perform(Intent.CloseTerminal(threadId, id))
                if (id == terminalId) terminalId = tabs.firstOrNull { it.terminalId != id }?.terminalId ?: ""
            }
            key(terminalId) {
                TerminalBody(
                    model,
                    threadId,
                    terminalId,
                    onOpened = { id ->
                        opened.add(id)
                        terminalId = id
                    },
                    Modifier.weight(1f),
                )
            }
        }
    }
}

@Composable
private fun TerminalTabs(
    tabs: List<TerminalTab>,
    selected: String,
    onSelect: (String) -> Unit,
    onNew: () -> Unit,
    onClose: (String) -> Unit,
) {
    val colors = AppTheme.colors
    Row(
        Modifier.fillMaxWidth().horizontalScroll(rememberScrollState()).padding(horizontal = 8.dp, vertical = 6.dp),
        horizontalArrangement = Arrangement.spacedBy(6.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        tabs.forEach { tab ->
            Row(
                Modifier.heightIn(min = 32.dp)
                    .background(
                        if (tab.terminalId == selected) colors.secondary else colors.screen,
                        RoundedCornerShape(10.dp),
                    )
                    .clickable { onSelect(tab.terminalId) }
                    .padding(start = 10.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Text(
                    tab.label,
                    style = AppTheme.caption,
                    fontWeight = FontWeight.Medium,
                    color = if (tab.running) colors.foreground else colors.foregroundMuted,
                )
                Box(Modifier.size(28.dp).clickable { onClose(tab.terminalId) }, contentAlignment = Alignment.Center) {
                    Icon(Icons.Outlined.Close, "Close ${tab.label}", Modifier.size(12.dp), tint = colors.iconMuted)
                }
            }
        }
        Box(Modifier.size(32.dp).clickable(onClick = onNew), contentAlignment = Alignment.Center) {
            Icon(Icons.Outlined.Add, "New terminal", Modifier.size(16.dp), tint = colors.icon)
        }
    }
}

@Composable
private fun TerminalBody(
    model: AndroidAppModel,
    threadId: String,
    terminalId: String,
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
                        TERMINAL_FONT_SIZE,
                    )
                    .let { terminal ->
                        native = terminal
                        terminal.view
                    }
            },
            update = {
                val terminal = native ?: return@AndroidView
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
        TerminalKeys(native)
    }
}

@Composable
private fun TerminalKeys(terminal: NativeTerminal?) {
    val keys =
        listOf(
            "Esc" to KeyEvent.KEYCODE_ESCAPE,
            "Tab" to KeyEvent.KEYCODE_TAB,
            "Ctrl+C" to KeyEvent.KEYCODE_C,
            "←" to KeyEvent.KEYCODE_DPAD_LEFT,
            "↓" to KeyEvent.KEYCODE_DPAD_DOWN,
            "↑" to KeyEvent.KEYCODE_DPAD_UP,
            "→" to KeyEvent.KEYCODE_DPAD_RIGHT,
        )
    Row(Modifier.fillMaxWidth().background(AppTheme.colors.screen), horizontalArrangement = Arrangement.SpaceEvenly) {
        keys.forEach { (label, code) ->
            TextButton(
                onClick = {
                    val modifiers = if (label == "Ctrl+C") KeyEvent.META_CTRL_ON else 0
                    terminal?.view?.onKeyDown(code, KeyEvent(0, 0, KeyEvent.ACTION_DOWN, code, 0, modifiers))
                },
                contentPadding = PaddingValues(),
            ) {
                Text(label, style = AppTheme.caption, color = AppTheme.colors.foreground)
            }
        }
    }
}

package dev.remoteagent.mobile

import android.view.KeyEvent
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.imePadding
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.viewinterop.AndroidView
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import com.termux.terminal.TerminalSession
import dev.remoteagent.core.DetachTerminal
import dev.remoteagent.core.Intent
import dev.remoteagent.core.KillTerminal
import dev.remoteagent.core.Outcome
import dev.remoteagent.core.ResizeTerminal
import dev.remoteagent.core.Snapshot
import dev.remoteagent.core.StartTerminal
import dev.remoteagent.core.TerminalSize
import dev.remoteagent.core.WriteTerminal
import dev.remoteagent.core.terminalHandle

@Composable
internal fun TerminalDialog(
    snapshot: Snapshot,
    perform: (Intent, (Result<Outcome>) -> Unit) -> Unit,
    dismiss: () -> Unit,
) {
    val cwd = snapshot.navigation().cwd
    val handle = remember(cwd) { terminalHandle(cwd) }
    var terminated by remember { mutableStateOf(false) }
    val currentPerform by rememberUpdatedState(perform)
    DisposableEffect(handle) {
        onDispose { if (!terminated) currentPerform(Intent.DetachTerminal(DetachTerminal(handle))) {} }
    }
    Dialog(onDismissRequest = dismiss, properties = DialogProperties(usePlatformDefaultWidth = false)) {
        Surface(Modifier.fillMaxSize().imePadding()) {
            Column {
                Text(snapshot.terminalView(handle)?.status ?: "接続中…")
                Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween) {
                    TextButton(onClick = dismiss) { Text("閉じる") }
                    TextButton(onClick = {
                        perform(Intent.KillTerminal(KillTerminal(handle))) {
                            if (it.isSuccess) { terminated = true; dismiss() }
                        }
                    }) { Text("終了") }
                }
                TerminalBody(snapshot, handle, cwd, perform, Modifier.weight(1f))
            }
        }
    }
}

@Composable
private fun TerminalBody(
    snapshot: Snapshot,
    handle: String,
    cwd: String,
    perform: (Intent, (Result<Outcome>) -> Unit) -> Unit,
    modifier: Modifier,
) {
    var nativeTerminal by remember { mutableStateOf<NativeTerminal?>(null) }
    val currentPerform by rememberUpdatedState(perform)
    Column(modifier) {
        AndroidView(
            modifier = Modifier.weight(1f).fillMaxWidth(),
            factory = { context ->
                var started = false
                NativeTerminal(context, object : TerminalSession.Transport {
                    override fun write(data: ByteArray) {
                        currentPerform(Intent.WriteTerminal(WriteTerminal(handle, data))) {}
                    }
                    override fun resize(columns: Int, rows: Int) {
                        val size = TerminalSize(columns.toUShort(), rows.toUShort())
                        if (!started) {
                            started = true
                            currentPerform(Intent.StartTerminal(StartTerminal(handle, cwd, size))) {}
                        } else currentPerform(Intent.ResizeTerminal(ResizeTerminal(handle, size))) {}
                    }
                }).let { terminal -> nativeTerminal = terminal; terminal.view }
            },
            update = { view ->
                val terminal = requireNotNull(nativeTerminal)
                snapshot.terminalView(handle)?.output?.forEach { chunk ->
                    if (terminal.feed(
                        chunk.sequence.toLong(), chunk.data,
                        chunk.resetSize?.cols?.toInt() ?: 0, chunk.resetSize?.rows?.toInt() ?: 0,
                    )) {
                        view.post { currentPerform(Intent.AcknowledgeTerminal(handle, chunk.sequence)) {} }
                    }
                }
            },
        )
        TerminalKeys(nativeTerminal)
    }
}

@Composable
private fun TerminalKeys(terminal: NativeTerminal?) {
    val keys = listOf(
        "Esc" to KeyEvent.KEYCODE_ESCAPE, "Tab" to KeyEvent.KEYCODE_TAB,
        "Ctrl+C" to KeyEvent.KEYCODE_C, "←" to KeyEvent.KEYCODE_DPAD_LEFT,
        "↓" to KeyEvent.KEYCODE_DPAD_DOWN, "↑" to KeyEvent.KEYCODE_DPAD_UP,
        "→" to KeyEvent.KEYCODE_DPAD_RIGHT,
    )
    Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceEvenly) {
        keys.forEach { (label, code) ->
            TextButton(onClick = {
                val modifiers = if (label == "Ctrl+C") KeyEvent.META_CTRL_ON else 0
                terminal?.view?.onKeyDown(code, KeyEvent(0, 0, KeyEvent.ACTION_DOWN, code, 0, modifiers))
            }, contentPadding = PaddingValues()) { Text(label) }
        }
    }
}

@Composable
internal fun TerminalLauncher(snapshot: Snapshot, perform: (Intent, (Result<Outcome>) -> Unit) -> Unit) {
    var visible by remember { mutableStateOf(false) }
    TextButton(onClick = { visible = true }) { Text("ターミナル") }
    if (visible) TerminalDialog(snapshot, perform) { visible = false }
}

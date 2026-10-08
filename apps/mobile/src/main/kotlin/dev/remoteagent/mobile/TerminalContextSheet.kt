// Declarative native layout with the fixed mobile metrics; the line range rules are supplied by core.
@file:Suppress("MagicNumber", "LongMethod")

package dev.remoteagent.mobile

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.remoteagent.core.visibleOutputLines
import dev.remoteagent.core.visibleOutputSelection

/** The captured viewport's lines; tapping the first and then the last line picks the range to attach. */
@Composable
internal fun TerminalContextSheet(output: String, onClose: () -> Unit, onAttach: (UInt, UInt) -> Unit) {
    val colors = AppTheme.colors
    val lines = remember(output) { visibleOutputLines(output) }
    var range by remember(output) { mutableStateOf(0 to lines.lastIndex) }
    var anchor by remember(output) { mutableStateOf<Int?>(null) }
    val selection =
        remember(lines, range) { visibleOutputSelection(lines, range.first.toUInt(), range.second.toUInt()) }
    BackHandler(onBack = onClose)
    Surface(Modifier.fillMaxSize(), color = colors.sheet) {
        Column(Modifier.fillMaxSize()) {
            Row(Modifier.fillMaxWidth().padding(16.dp), verticalAlignment = Alignment.CenterVertically) {
                Text(
                    "Visible terminal output",
                    Modifier.weight(1f),
                    style = AppTheme.headline,
                    color = colors.foreground,
                )
                TextButton(onClick = onClose) { Text("Cancel", color = colors.foreground) }
            }
            Text(
                "Tap the first and last line to select a range.",
                Modifier.padding(start = 16.dp, end = 16.dp, bottom = 12.dp),
                style = AppTheme.body,
                color = colors.foregroundMuted,
            )
            LazyColumn(Modifier.weight(1f), contentPadding = PaddingValues(16.dp)) {
                itemsIndexed(lines) { index, line ->
                    val selected = index in range.first..range.second
                    Text(
                        "${index + 1} ${line.ifEmpty { " " }}",
                        Modifier.fillMaxWidth()
                            .then(if (selected) Modifier.background(colors.subtle) else Modifier)
                            .clickable {
                                val start = anchor
                                if (start == null) {
                                    anchor = index
                                    range = index to index
                                } else {
                                    range = minOf(start, index) to maxOf(start, index)
                                    anchor = null
                                }
                            }
                        .padding(vertical = 4.dp),
                        style = AppTheme.footnote,
                        fontFamily = AppTheme.mono,
                        fontSize = AppTheme.terminalFontSize.sp,
                        color = colors.foreground,
                    )
                }
            }
            if (selection.tooLarge)
                Text(
                    "Select fewer lines to fit the context limit.",
                    Modifier.padding(horizontal = 16.dp),
                    style = AppTheme.body,
                    color = colors.foregroundMuted,
                )
            Surface(
                onClick = { onAttach(range.first.toUInt(), range.second.toUInt()) },
                enabled = selection.canAttach,
                shape = RoundedCornerShape(12.dp),
                color = colors.subtle,
                modifier = Modifier.fillMaxWidth().padding(start = 16.dp, end = 16.dp, top = 16.dp, bottom = 40.dp),
            ) {
                Text(
                    "Attach selected output",
                    Modifier.padding(16.dp),
                    style = AppTheme.body,
                    textAlign = TextAlign.Center,
                    color = colors.foreground,
                )
            }
        }
    }
}

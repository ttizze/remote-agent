package dev.remoteagent.mobile

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Card
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.unit.dp

private const val ADDITION_COLOR = 0xFF37CF77
private const val DELETION_COLOR = 0xFFFF6259

@Composable
internal fun ChangedFilesSummary(
    files: List<dev.remoteagent.core.WorkspaceDiffFile>,
    load: () -> Unit,
) {
    var open by remember { mutableStateOf(false) }
    Card(
        Modifier.fillMaxWidth().clickable {
            load()
            open = true
        }
    ) {
        Column(Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Text("変更ファイル", style = MaterialTheme.typography.labelMedium)
            files.forEach { file ->
                Row {
                    Text(
                        file.path,
                        Modifier.weight(1f),
                        maxLines = 1,
                        overflow = androidx.compose.ui.text.style.TextOverflow.Ellipsis,
                    )
                    file.additions?.let { Text(" +$it", color = Color(ADDITION_COLOR)) }
                    file.deletions?.let { Text(" −$it", color = Color(DELETION_COLOR)) }
                }
            }
        }
    }
    if (open)
        androidx.compose.material3.AlertDialog(
            onDismissRequest = { open = false },
            title = { Text("変更ファイル") },
            confirmButton = {
                androidx.compose.material3.TextButton(onClick = { open = false }) { Text("閉じる") }
            },
            text = { ChangeDetails(files) },
        )
}

@Composable
private fun ChangeDetails(files: List<dev.remoteagent.core.WorkspaceDiffFile>) {
    SelectionContainer {
        Column(Modifier.verticalScroll(rememberScrollState())) {
            files.forEach { file ->
                Text(file.path, style = MaterialTheme.typography.titleSmall)
                file.rows.forEach { row ->
                    Text(
                        row.text,
                        Modifier.fillMaxWidth()
                            .background(
                                when (row.kind) {
                                    "+" -> Color.Green.copy(alpha = 0.12f)
                                    "-" -> Color.Red.copy(alpha = 0.12f)
                                    else -> Color.Transparent
                                }
                            )
                            .padding(vertical = 3.dp),
                        fontFamily = FontFamily.Monospace,
                        style = MaterialTheme.typography.bodySmall,
                    )
                }
                if (file.rows.isEmpty())
                    Text("差分はまだ取得されていません", style = MaterialTheme.typography.bodySmall)
            }
        }
    }
}

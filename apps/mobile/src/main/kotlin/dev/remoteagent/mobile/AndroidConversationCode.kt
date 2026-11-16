package dev.remoteagent.mobile

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.unit.dp
import dev.remoteagent.core.MarkdownBlock

private const val COPY_FEEDBACK_MILLIS = 1200L

@Composable
internal fun MarkdownCode(block: MarkdownBlock.Paragraph, index: Int) {
    val language = LocalUiLanguage.current
    val clipboard = androidx.compose.ui.platform.LocalClipboardManager.current
    var copied by remember { mutableStateOf(false) }
    var wrapped by remember { mutableStateOf(false) }
    val text = block.runs.joinToString("") { it.text }
    androidx.compose.runtime.LaunchedEffect(copied) {
        if (copied) {
            kotlinx.coroutines.delay(COPY_FEEDBACK_MILLIS)
            copied = false
        }
    }
    Column(
        Modifier.fillMaxWidth()
            .clip(androidx.compose.foundation.shape.RoundedCornerShape(12.dp))
            .border(
                1.dp,
                MaterialTheme.colorScheme.outlineVariant,
                androidx.compose.foundation.shape.RoundedCornerShape(12.dp),
            )
            .background(MaterialTheme.colorScheme.surfaceContainer)
    ) {
        Row(Modifier.fillMaxWidth().padding(horizontal = 12.dp), verticalAlignment = Alignment.CenterVertically) {
            Text(
                block.style.filename ?: (block.style.language ?: "CODE").uppercase(),
                Modifier.weight(1f),
                style = MaterialTheme.typography.labelMedium,
                maxLines = 1,
                overflow = androidx.compose.ui.text.style.TextOverflow.Ellipsis,
            )
            androidx.compose.material3.TextButton(onClick = { wrapped = !wrapped }) {
                Text(if (wrapped) language.text("折り返し解除") else language.text("折り返し"))
            }
            androidx.compose.material3.TextButton(
                onClick = {
                    clipboard.setText(androidx.compose.ui.text.AnnotatedString(text))
                    copied = true
                },
                modifier = Modifier.testTag("markdown.code.copy.$index"),
            ) {
                Text(if (copied) "✓" else language.text("コピー"))
            }
        }
        HorizontalDivider()
        Box(
            Modifier.fillMaxWidth()
                .then(if (wrapped) Modifier else Modifier.horizontalScroll(rememberScrollState()))
                .padding(12.dp)
        ) {
            Text(
                markdownText(block.runs),
                style = MaterialTheme.typography.bodyMedium,
                fontFamily = FontFamily.Monospace,
            )
        }
    }
}

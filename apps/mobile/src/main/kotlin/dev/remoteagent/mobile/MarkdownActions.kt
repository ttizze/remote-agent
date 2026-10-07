package dev.remoteagent.mobile

import androidx.compose.runtime.staticCompositionLocalOf

internal class MarkdownActions(
    val workspaceRoot: String?,
    val open: (String) -> Unit,
    val loadFile: suspend (String) -> ByteArray,
)

internal val LocalMarkdownActions = staticCompositionLocalOf {
    MarkdownActions(null, {}, { error("No connected Host") })
}

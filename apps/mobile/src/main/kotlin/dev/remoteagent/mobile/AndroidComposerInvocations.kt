package dev.remoteagent.mobile

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import dev.remoteagent.core.ComposerSuggestions
import dev.remoteagent.core.Invocation
import dev.remoteagent.core.Intent
import dev.remoteagent.core.Outcome
import dev.remoteagent.core.Snapshot
import dev.remoteagent.core.insertInvocation

@Composable
internal fun ComposerInvocationPicker(suggestions: ComposerSuggestions?, select: (Invocation) -> Unit) {
    suggestions?.status?.let { Text(it, style = MaterialTheme.typography.bodySmall) }
    val candidates = suggestions?.candidates.orEmpty()
    if (candidates.isNotEmpty()) {
        Column(Modifier.heightIn(max = 180.dp).verticalScroll(rememberScrollState())) {
            candidates.forEach { candidate ->
                TextButton(onClick = { select(candidate.invocation) }) {
                    Column {
                        Text(candidate.invocation.name)
                        Text(candidate.description, style = MaterialTheme.typography.bodySmall, maxLines = 2)
                    }
                }
            }
        }
    }
}

@Composable
internal fun ComposerInvocations(
    snapshot: Snapshot,
    perform: (Intent, (Result<Outcome>) -> Unit) -> Unit,
) {
    val navigation = snapshot.navigation()
    val draft = snapshot.draft(navigation.draftKey)
    if (!snapshot.composerControls().canInvokeLocalTools) return
    val cursor = draft.text.toByteArray(Charsets.UTF_8).size.toUInt()
    ComposerInvocationPicker(snapshot.composerSuggestions(draft.text, cursor)) { invocation ->
        insertInvocation(draft.text, cursor, invocation.kind, invocation.name)?.let {
            perform(Intent.InsertInvocation(navigation.draftKey, it.text, invocation)) {}
        }
    }
}

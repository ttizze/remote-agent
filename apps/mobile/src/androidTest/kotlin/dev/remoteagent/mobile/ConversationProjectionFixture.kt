package dev.remoteagent.mobile

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import dev.remoteagent.core.ConversationRowPolicy
import dev.remoteagent.core.ConversationRows
import dev.remoteagent.core.ProgressRows
import dev.remoteagent.core.Snapshot

private val fixtureRowPolicy = ConversationRowPolicy(requests = true, progress = ProgressRows.ALWAYS)

/** Construct native action fixtures through the production core projector. */
internal fun projectConversationRows(snapshot: Snapshot): ConversationRows? {
    val value = dev.remoteagent.core.Presentation.fromSnapshot(snapshot, "")
    return value.conversation()?.let { rendered ->
        ConversationRows.project(
            rendered,
            snapshot.panelScope("fixture", value.conversationContext().navigation().draftKey),
            emptyMap(),
            fixtureRowPolicy,
            null,
        )
    }
}

@androidx.compose.runtime.Composable
internal fun ThreadDetailFixture(
    snapshot: Snapshot,
    projection: ConversationRows?,
    perform: (dev.remoteagent.core.Intent, (Result<dev.remoteagent.core.Outcome>) -> Unit) -> Unit,
    modifier: androidx.compose.ui.Modifier = androidx.compose.ui.Modifier,
    scrollToTopRequest: Int = 0,
    composer: @androidx.compose.runtime.Composable (() -> Unit) -> Unit,
) {
    var current by remember(projection) { mutableStateOf(projection) }
    var expansion by remember(projection) { mutableStateOf(emptyMap<String, String>()) }
    ThreadDetailScreen(
        dev.remoteagent.core.Presentation.fromSnapshot(snapshot, "").conversationContext(),
        current,
        ConversationActions(
            perform,
            { id ->
                current?.let { before ->
                    before.toggleActivity(id, expansion)?.let { change ->
                        expansion = change
                        current =
                            ConversationRows.project(
                                before.source(),
                                before.scope(),
                                change,
                                fixtureRowPolicy,
                                before,
                            )
                    }
                }
            },
            snapshot.terminals(),
        ),
        modifier,
        scrollToTopRequest,
        composer,
    )
}

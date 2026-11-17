package dev.remoteagent.mobile

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import dev.remoteagent.core.ConversationRows
import dev.remoteagent.core.Snapshot

/** Construct native action fixtures through the production core projector. */
internal fun projectConversationRows(snapshot: Snapshot): ConversationRows? {
    val value = dev.remoteagent.core.Presentation.fromSnapshot(snapshot, "")
    return value.conversation()?.let { rendered ->
        ConversationRows.project(
            dev.remoteagent.core.TimelineStyle(
                dev.remoteagent.core.WorkDisclosure.SETTLED,
                dev.remoteagent.core.PlanPresentation.CARD,
                false,
                false,
            ),
            rendered,
            snapshot.panelScope("fixture", value.conversationContext().navigation().draftKey),
            null,
            emptyMap(),
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
    val context = dev.remoteagent.core.Presentation.fromSnapshot(snapshot, "").conversationContext()
    ThreadDetailScreen(
        context,
        current,
        ConversationActions(
            perform,
            { id ->
                current?.let { before ->
                    before.toggleDisclosure(id, expansion)?.let { change ->
                        expansion = change.expansion
                        change.load?.let { perform(it) {} }
                        current =
                            ConversationRows.project(
                                dev.remoteagent.core.TimelineStyle(
                                    dev.remoteagent.core.WorkDisclosure.SETTLED,
                                    dev.remoteagent.core.PlanPresentation.CARD,
                                    false,
                                    false,
                                ),
                                before.source(),
                                before.scope(),
                                null,
                                change.expansion,
                                before,
                            )
                    }
                }
            },
            snapshot.terminals(),
        ),
        modifier,
        scrollToTopRequest,
        dockedRequests = context.navigation().threadId?.let(snapshot::dockedRequests).orEmpty(),
        composer = composer,
    )
}

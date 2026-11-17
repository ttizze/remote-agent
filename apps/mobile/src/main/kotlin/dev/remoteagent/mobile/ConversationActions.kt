package dev.remoteagent.mobile

import dev.remoteagent.core.Intent
import dev.remoteagent.core.Outcome
import dev.remoteagent.core.Terminals

internal class ConversationActions(
    val perform: (Intent, (Result<Outcome>) -> Unit) -> Unit,
    val toggleDisclosure: (String) -> Unit,
    val terminals: Terminals,
)

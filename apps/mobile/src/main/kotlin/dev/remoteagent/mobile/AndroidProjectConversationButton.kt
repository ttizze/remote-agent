package dev.remoteagent.mobile

import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.res.painterResource
import dev.remoteagent.core.Intent
import dev.remoteagent.core.ProjectSummary
import dev.remoteagent.core.localizedUiFormat

@Composable
internal fun ProjectConversationButton(project: ProjectSummary, perform: (Intent) -> Unit) {
    val language = LocalUiLanguage.current
    val openLink = rememberExternalLinkOpener()
    val coordinatorUrl = project.coordinatorUrl
    if (coordinatorUrl != null) {
        IconButton(onClick = { openLink(coordinatorUrl) }) {
            Icon(
                painterResource(R.drawable.ic_chat),
                localizedUiFormat(language, "{0}のメイン会話をClaudeで開く", listOf(project.name)),
            )
        }
    } else {
        IconButton(onClick = { perform(Intent.NewChat(project.roots.firstOrNull()?.path.orEmpty())) }) {
            Icon(
                painterResource(R.drawable.ic_add),
                localizedUiFormat(language, "{0}で新しいチャット", listOf(project.name)),
            )
        }
    }
}

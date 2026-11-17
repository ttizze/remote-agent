package dev.remoteagent.mobile

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.FilledIconButton
import androidx.compose.material3.IconButtonDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import dev.remoteagent.core.Intent
import dev.remoteagent.core.Interrupt
import dev.remoteagent.core.Outcome

@Composable
internal fun ComposerStopButton(
    interrupt: Interrupt,
    phase: dev.remoteagent.core.OperationPhase?,
    enabled: Boolean,
    perform: (Intent, (Result<Outcome>) -> Unit) -> Unit,
) {
    val language = LocalUiLanguage.current
    val stopping = phase is dev.remoteagent.core.OperationPhase.Running
    Column {
        FilledIconButton(
            onClick = { perform(Intent.Interrupt(interrupt)) {} },
            enabled = enabled,
            colors =
                IconButtonDefaults.filledIconButtonColors(containerColor = Color.White, contentColor = Color.Black),
            modifier =
                Modifier.testTag("composer.stop").semantics {
                    contentDescription = if (stopping) language.text("停止中") else language.text("停止")
                },
        ) {
            if (stopping) CircularProgressIndicator(Modifier.size(20.dp), strokeWidth = 2.dp)
            else Box(Modifier.size(16.dp).background(Color.Black, RoundedCornerShape(3.dp)))
        }
        if (phase is dev.remoteagent.core.OperationPhase.Failed)
            Text(language.text("停止できませんでした。再試行してください。"), color = MaterialTheme.colorScheme.error)
    }
}

package dev.remoteagent.mobile

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.FilterChip
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import dev.remoteagent.core.FollowUpBehavior
import dev.remoteagent.core.Intent

@Composable
internal fun ClientSettings(model: AndroidAppModel) {
    var providers by remember(model.profileId) { mutableStateOf(false) }
    if (providers) {
        ProviderSettings(model) { providers = false }
        return
    }
    val snapshot = model.snapshot
    Column(Modifier.padding(24.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
        Text("Follow-ups", style = MaterialTheme.typography.headlineSmall)
        Text("この端末の全Hostに適用されます。", style = MaterialTheme.typography.bodySmall)
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            for ((label, behavior) in listOf("Queue" to FollowUpBehavior.QUEUE, "Steer" to FollowUpBehavior.STEER)) {
                FilterChip(
                    selected = snapshot.followUpBehavior() == behavior,
                    onClick = { model.perform(Intent.SetFollowUpBehavior(behavior)) },
                    label = { Text(label) },
                )
            }
        }
        Text("Queueは次のターンまで待機し、Steerは実行中のターンへ送ります。送信ボタンの長押しで、今回だけ動作を選べます。")
        TextButton(onClick = { providers = true }, enabled = snapshot.connected()) { Text("Providers") }
    }
}

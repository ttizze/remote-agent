package dev.remoteagent.mobile

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.FilterChip
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import dev.remoteagent.core.FollowUpBehavior
import dev.remoteagent.core.Intent
import dev.remoteagent.core.Snapshot

@Composable
internal fun ClientSettings(snapshot: Snapshot, perform: (Intent) -> Unit) {
    Column(Modifier.padding(24.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
        Text("Follow-ups", style = MaterialTheme.typography.headlineSmall)
        Text("この端末の全Hostに適用されます。", style = MaterialTheme.typography.bodySmall)
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            for ((label, behavior) in listOf("Queue" to FollowUpBehavior.QUEUE, "Steer" to FollowUpBehavior.STEER)) {
                FilterChip(
                    selected = snapshot.followUpBehavior() == behavior,
                    onClick = { perform(Intent.SetFollowUpBehavior(behavior)) },
                    label = { Text(label) },
                )
            }
        }
        Text("Queueは次のターンまで待機し、Steerは実行中のターンへ送ります。送信ボタンの長押しで、今回だけ動作を選べます。")
    }
}

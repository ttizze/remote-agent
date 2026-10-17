package dev.remoteagent.mobile

import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.material3.Text
import androidx.compose.material3.TextField
import androidx.compose.material3.TextFieldDefaults
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import dev.remoteagent.core.Intent
import dev.remoteagent.core.Outcome
import dev.remoteagent.core.Snapshot

@Composable
internal fun ComposerTextInput(snapshot: Snapshot, perform: (Intent, (Result<Outcome>) -> Unit) -> Unit) {
    val navigation = snapshot.navigation()
    val draft = snapshot.draft(navigation.draftKey)
    TextField(
        draft.text,
        { perform(Intent.EditComposer(navigation.draftKey, it, it.toByteArray(Charsets.UTF_8).size.toUInt())) {} },
        Modifier.fillMaxWidth(),
        placeholder = { Text("AIに依頼する…") },
        colors =
            TextFieldDefaults.colors(
                focusedContainerColor = Color.Transparent,
                unfocusedContainerColor = Color.Transparent,
                focusedIndicatorColor = Color.Transparent,
                unfocusedIndicatorColor = Color.Transparent,
            ),
        minLines = 2,
        maxLines = 5,
    )
}

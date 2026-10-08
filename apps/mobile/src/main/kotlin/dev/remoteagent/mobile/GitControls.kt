package dev.remoteagent.mobile

import androidx.compose.foundation.layout.size
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.AltRoute
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import dev.remoteagent.core.Intent

/** Git status is subscribed once for the checkout shown by the thread. */
@Composable
internal fun GitControls(model: AndroidAppModel, cwd: String, onOpen: () -> Unit) {
    LaunchedEffect(cwd) {
        if (cwd.isNotEmpty()) {
            model.perform(Intent.SubscribeVcsStatus(cwd))
            model.perform(Intent.RefreshVcsStatus(cwd))
        }
    }
    IconButton(onClick = onOpen, modifier = Modifier.size(48.dp), enabled = cwd.isNotEmpty()) {
        Icon(Icons.AutoMirrored.Outlined.AltRoute, "Git")
    }
}

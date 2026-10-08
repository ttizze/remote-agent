package dev.remoteagent.mobile

import android.graphics.BitmapFactory
import androidx.compose.foundation.Image
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.unit.dp
import dev.remoteagent.core.DeviceActionIntent
import dev.remoteagent.core.Intent

/** Native device picker, setup and live frame surface for a conversation. */
@Composable
internal fun DeviceScreen(model: AndroidAppModel, threadId: String) {
    val view = model.snapshot.device()
    val threadSessions = view.sessions.filter { it.threadId == threadId }
    val sessionKey = threadSessions.joinToString(",") { "${it.hostId}:${it.deviceId}" }
    LaunchedEffect(threadId) {
        model.perform(Intent.OpenThread(threadId))
        model.perform(Intent.LoadDevices)
        model.perform(Intent.SubscribeDevice)
    }
    LaunchedEffect(threadId, sessionKey) {
        threadSessions.forEach { session ->
            model.perform(Intent.LoadDeviceDetail(session.hostId, session.deviceId))
        }
    }
    DisposableEffect(threadId) {
        onDispose { model.perform(Intent.UnsubscribeDevice) }
    }
    ScreenScaffold("Device", onBack = model::back) {
        if (!view.enabled) {
            Column(Modifier.fillMaxSize().padding(20.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
                Text("Device support is off")
                Text("Enable it to discover simulators and emulators on the Host.")
                Button(onClick = {
                    model.perform(Intent.ConfigureDevices(true, null, false))
                }) { Text("Enable device support") }
            }
            return@ScreenScaffold
        }
        LazyColumn(
            Modifier.fillMaxSize().padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(10.dp),
        ) {
            item { Text("Host status: ${view.status}") }
            view.statusDetail?.let { detail -> item { Text(detail) } }
            items(view.hosts, key = { it.id }) { host ->
                Text(host.label + " · " + host.kind)
                host.unavailableReasons.forEach { reason -> Text(reason) }
            }
            if (view.enabled && !view.agentAccessEnabled) {
                item {
                    Button(onClick = {
                        model.perform(Intent.ConfigureDevices(null, true, true))
                    }) { Text("Enable agent device access") }
                }
            }
            items(view.devices, key = { "${it.hostId}:${it.id}" }) { device ->
                val opened = threadSessions.any { it.hostId == device.hostId && it.deviceId == device.id }
                Card(Modifier.fillMaxWidth()) {
                    Row(Modifier.fillMaxWidth().padding(12.dp), horizontalArrangement = Arrangement.spacedBy(10.dp)) {
                        Column(Modifier.weight(1f)) {
                            Text(device.name)
                            Text("${device.platform} · ${device.version}")
                        }
                        Button(onClick = {
                            if (opened) {
                                model.perform(Intent.CloseDevice(device.hostId, device.id, false))
                            } else {
                                model.perform(Intent.OpenDevice(device.hostId, device.id, device.platform, true))
                            }
                        }) { Text(if (opened) "Close" else "Open") }
                    }
                }
            }
            items(
                threadSessions,
                key = { session -> "controls:${session.hostId}:${session.deviceId}" },
            ) { session ->
                val detail = view.details.firstOrNull {
                    it.hostId == session.hostId && it.deviceId == session.deviceId
                }
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    Button(onClick = {
                        model.perform(
                            Intent.DeviceAction(
                                session.hostId,
                                session.deviceId,
                                DeviceActionIntent.SetAppearance(true),
                            )
                        )
                    }) { Text("Dark") }
                    Button(onClick = {
                        model.perform(
                            Intent.DeviceAction(
                                session.hostId,
                                session.deviceId,
                                DeviceActionIntent.SetAppearance(false),
                            )
                        )
                    }) { Text("Light") }
                    Button(onClick = {
                        model.perform(
                            Intent.DeviceAction(
                                session.hostId,
                                session.deviceId,
                                DeviceActionIntent.SetTextSize("large"),
                            )
                        )
                    }) { Text("Text +") }
                    if (session.platform == "android") {
                        Button(onClick = {
                            model.perform(
                                Intent.DeviceAction(
                                    session.hostId,
                                    session.deviceId,
                                    DeviceActionIntent.SetOrientation("portrait"),
                                )
                            )
                        }) { Text("Portrait") }
                    }
                    Button(onClick = {
                        model.perform(Intent.CloseDevice(session.hostId, session.deviceId, true))
                    }) { Text("Power off") }
                }
                detail?.foregroundApp?.let { app -> Text("Foreground: " + app) }
            }
            view.frames.filter { it.threadId == threadId }.maxByOrNull { it.sequence }?.let { frame ->
                item(key = "frame-${frame.sequence}") {
                    val bitmap = remember(frame.sequence) {
                        BitmapFactory.decodeByteArray(frame.png, 0, frame.png.size)?.asImageBitmap()
                    }
                    bitmap?.let {
                        Image(it, "Live device frame", Modifier.fillMaxWidth().size(320.dp), contentScale = ContentScale.Fit)
                    }
                }
            }
        }
    }
}

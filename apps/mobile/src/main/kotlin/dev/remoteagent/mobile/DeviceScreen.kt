package dev.remoteagent.mobile

import android.view.KeyEvent as AndroidKeyEvent
import android.graphics.BitmapFactory
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.Image
import androidx.compose.foundation.gestures.awaitEachGesture
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
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
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.input.pointer.changedToUp
import androidx.compose.ui.input.pointer.consume
import androidx.compose.ui.input.pointer.awaitFirstDown
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.input.pointer.positionChanged
import androidx.compose.ui.input.key.KeyEventType
import androidx.compose.ui.input.key.onPreviewKeyEvent
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.unit.dp
import androidx.compose.ui.platform.LocalContext
import dev.remoteagent.core.DeviceActionIntent
import dev.remoteagent.core.DeviceDuoCommandIntent
import dev.remoteagent.core.DeviceDuoPoseIntent
import dev.remoteagent.core.DeviceFoldPostureIntent
import dev.remoteagent.core.DeviceView
import dev.remoteagent.core.Intent
import dev.remoteagent.core.projectDevicePoint

/** Native device picker, setup and live frame surface for a conversation. */
@Composable
internal fun DeviceScreen(model: AndroidAppModel, threadId: String) {
    val view = model.snapshot.device()
    val context = LocalContext.current
    val threadSessions = view.sessions.filter { it.threadId == threadId }
    val sessionKey = threadSessions.joinToString(",") { "${it.hostId}:${it.deviceId}:${it.sessionEpoch}" }
    LaunchedEffect(threadId) {
        model.perform(Intent.OpenThread(threadId))
        model.perform(Intent.LoadDevices)
        model.perform(Intent.SubscribeDevice)
    }
    LaunchedEffect(threadId, sessionKey) {
        threadSessions.forEach { session ->
            model.perform(Intent.LoadDeviceDetail(session.hostId, session.deviceId))
            model.perform(Intent.LoadDeviceAccessibility(session.hostId, session.deviceId))
            if (session.platform == "ios") {
                model.perform(Intent.LoadDeviceEventLog(session.hostId, session.deviceId, 100u.toUShort()))
            }
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
            item {
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    Button(onClick = { model.perform(Intent.InspectDevices(null)) }) { Text("Inspect tools") }
                    Button(onClick = { model.perform(Intent.UpdateDeviceTool(null, "hub")) }) { Text("Update hub") }
                    Button(onClick = { model.perform(Intent.UpdateDeviceTool(null, "agent")) }) { Text("Update agent") }
                }
            }
            view.statusDetail?.let { detail -> item { Text(detail) } }
            items(view.hosts, key = { it.id }) { host ->
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    Text(host.label + " · " + host.kind, Modifier.weight(1f))
                    Button(onClick = { model.perform(Intent.RetryDeviceHost(host.id)) }) { Text("Retry") }
                }
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
                        Button(onClick = {
                            model.perform(
                                Intent.DeviceAction(
                                    session.hostId,
                                    session.deviceId,
                                    DeviceActionIntent.Fold(DeviceFoldPostureIntent.Opened),
                                )
                            )
                        }) { Text("Open fold") }
                    } else {
                        Button(onClick = {
                            model.perform(
                                Intent.DeviceAction(
                                    session.hostId,
                                    session.deviceId,
                                    DeviceActionIntent.Duo(DeviceDuoCommandIntent.Pose(DeviceDuoPoseIntent.Book)),
                                )
                            )
                        }) { Text("Book") }
                        Button(onClick = {
                            model.perform(
                                Intent.DeviceAction(
                                    session.hostId,
                                    session.deviceId,
                                    DeviceActionIntent.Duo(DeviceDuoCommandIntent.Table(true)),
                                )
                            )
                        }) { Text("Table") }
                    }
                    Button(onClick = {
                        model.perform(
                            Intent.DeviceAction(
                                session.hostId,
                                session.deviceId,
                                DeviceActionIntent.HardwareButton("home"),
                            )
                        )
                    }) { Text("Home") }
                    Button(onClick = {
                        model.perform(
                            Intent.DeviceAction(
                                session.hostId,
                                session.deviceId,
                                DeviceActionIntent.Rotate,
                            )
                        )
                    }) { Text("Rotate") }
                    Button(onClick = {
                        model.perform(Intent.StartDeviceRecording(session.hostId, session.deviceId, "mp4"))
                    }) { Text("Record") }
                    Button(onClick = {
                        model.perform(Intent.StopDeviceRecording(session.hostId, session.deviceId))
                    }) { Text("Stop record") }
                    Button(onClick = {
                        model.perform(Intent.CloseDevice(session.hostId, session.deviceId, true))
                    }) { Text("Power off") }
                }
                detail?.foregroundApp?.let { app -> Text("Foreground: " + app) }
                view.duoControls.firstOrNull {
                    it.threadId == threadId && it.hostId == session.hostId && it.deviceId == session.deviceId
                }?.let { duo ->
                    if (duo.pending || duo.error != null) {
                        Text(duo.error ?: "Duo control pending")
                    }
                }
            }
            view.frames.filter { it.threadId == threadId }.maxByOrNull { it.sequence }?.let { frame ->
                item(key = "frame-${frame.sequence}") {
                    val bitmap = remember(frame.sequence) {
                        BitmapFactory.decodeByteArray(frame.png, 0, frame.png.size)?.asImageBitmap()
                    }
                    bitmap?.let {
                        Box(
                            Modifier
                                .fillMaxWidth()
                                .size(320.dp)
                                .deviceKeyInput(model, frame.hostId, frame.deviceId)
                                .deviceTouchInput(model, frame.hostId, frame.deviceId, frame.width, frame.height, frame.sequence),
                        ) {
                            Image(it, "Live device frame", Modifier.fillMaxSize(), contentScale = ContentScale.Fit)
                            DeviceAccessibilityOverlay(view, frame.hostId, frame.deviceId)
                        }
                    }
                }
            }
            view.videoFrames
                .filter { it.threadId == threadId && (it.encoding == "jpeg" || it.encoding == "mjpeg") }
                .maxByOrNull { it.sequence }?.let { frame ->
                    item(key = "video-frame-${frame.sequence}") {
                        val bitmap = remember(frame.sequence) {
                            BitmapFactory.decodeByteArray(frame.payload, 0, frame.payload.size)?.asImageBitmap()
                        }
                        bitmap?.let {
                            Box(
                                Modifier
                                    .fillMaxWidth()
                                    .size(320.dp)
                                    .deviceKeyInput(model, frame.hostId, frame.deviceId)
                                    .deviceTouchInput(model, frame.hostId, frame.deviceId, frame.width, frame.height, frame.sequence),
                            ) {
                                Image(it, "Live device video frame", Modifier.fillMaxSize(), contentScale = ContentScale.Fit)
                                DeviceAccessibilityOverlay(view, frame.hostId, frame.deviceId)
                            }
                        }
                }
            }
            view.videoFrames
                .firstOrNull { it.threadId == threadId && (it.encoding == "h264" || it.encoding == "semu") }
                ?.let {
                    item(key = "unsupported-video-${it.sequence}") {
                        Text("Live H.264 device video is unavailable in this native decoder")
                    }
                }
            view.accessibility
                .filter { tree -> threadSessions.any { it.hostId == tree.hostId && it.deviceId == tree.deviceId } }
                .forEach { tree ->
                    item(key = "accessibility-${tree.hostId}-${tree.deviceId}") {
                        Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
                            Text("Accessibility overlay")
                            tree.elements.filter { it.label.isNotEmpty() }.take(20).forEach { element ->
                                Text("${element.role} · ${element.label}")
                            }
                        }
                    }
                }
            view.eventLog
                .filter { entry -> threadSessions.any { it.hostId == entry.hostId && it.deviceId == entry.deviceId } }
                .takeLast(20)
                .forEach { entry ->
                    item(key = "event-log-${entry.hostId}-${entry.deviceId}-${entry.id}") {
                        Text("${entry.kind} · ${entry.summary}")
                    }
                }
            view.lastRecording?.takeIf { it.threadId == threadId }?.let { recording ->
                item(key = "last-recording-${recording.deviceId}-${recording.byteCount}") {
                    Text("Recording ready · ${recording.frameCount} frames · ${recording.byteCount} bytes")
                    Button(onClick = {
                        val type = deviceRecordingFileType(recording.fileName, recording.mimeType, recording.bytes)
                        if (type == null) {
                            model.notice = "The device recording is incomplete or has an unsupported format."
                        } else {
                            val (extension, mime) = type
                            val file = java.io.File(context.cacheDir, "device-${recording.deviceId}-${recording.byteCount}.$extension")
                            file.writeBytes(recording.bytes)
                            model.perform(
                                Intent.AttachFiles(
                                    threadId,
                                    listOf(dev.remoteagent.core.LocalFile(file.path, file.name, mime)),
                                ),
                            )
                        }
                    }) { Text("Attach recording") }
                }
            }
        }
        }
    }

@Composable
private fun DeviceAccessibilityOverlay(view: DeviceView, hostId: String, deviceId: String) {
    val tree = view.accessibility.firstOrNull { it.hostId == hostId && it.deviceId == deviceId }
    if (tree == null) return
    Canvas(Modifier.fillMaxSize()) {
        tree.elements.filter { it.label.isNotEmpty() }.forEach { element ->
            drawRect(
                color = Color(0xFF4F8CFF),
                topLeft = Offset(size.width * element.x, size.height * element.y),
                size = Size(size.width * element.width, size.height * element.height),
                style = Stroke(width = 2f),
            )
        }
    }
}

private fun deviceRecordingFileType(fileName: String, mimeType: String, bytes: ByteArray): Pair<String, String>? {
    if (!fileName.lowercase().endsWith(".mp4") || mimeType.lowercase() != "video/mp4" || bytes.size < 12) return null
    if (!bytes.copyOfRange(4, 8).contentEquals(byteArrayOf('f'.code.toByte(), 't'.code.toByte(), 'y'.code.toByte(), 'p'.code.toByte()))) return null
    return "mp4" to mimeType
}

private fun Modifier.deviceTouchInput(
    model: AndroidAppModel,
    hostId: String,
    deviceId: String,
    frameWidth: UInt,
    frameHeight: UInt,
    sequence: ULong,
): Modifier = pointerInput(sequence) {
    awaitEachGesture {
        var lastPoint: Pair<Float, Float>? = null
        var ended = false
        try {
            awaitPointerEventScope {
                val down = awaitFirstDown()
            fun send(phase: String, position: androidx.compose.ui.geometry.Offset): Boolean {
                val point = projectDevicePoint(
                    size.width,
                    size.height,
                    frameWidth.toFloat(),
                    frameHeight.toFloat(),
                    position.x,
                    position.y,
                ) ?: return false
                val x = point.x
                val y = point.y
                lastPoint = x to y
                model.perform(
                    Intent.DeviceAction(
                        hostId,
                        deviceId,
                        DeviceActionIntent.Touch(phase, x, y),
                    ),
                )
                return true
            }
            send("begin", down.position)
            while (true) {
                val event = awaitPointerEvent()
                val change = event.changes.first()
                when {
                    change.changedToUp() -> {
                        if (!send("end", change.position)) {
                            lastPoint?.let { (x, y) ->
                                model.perform(
                                    Intent.DeviceAction(
                                        hostId,
                                        deviceId,
                                        DeviceActionIntent.Touch("end", x, y),
                                    ),
                                )
                            }
                        }
                        ended = true
                        lastPoint = null
                        break
                    }
                    change.positionChanged() -> {
                        change.consume()
                        send("move", change.position)
                    }
                }
            }
            }
        } finally {
            if (!ended) {
                lastPoint?.let { (x, y) ->
                    model.perform(
                        Intent.DeviceAction(
                            hostId,
                            deviceId,
                            DeviceActionIntent.Touch("end", x, y),
                        ),
                    )
                }
            }
        }
    }
}

private fun Modifier.deviceKeyInput(model: AndroidAppModel, hostId: String, deviceId: String): Modifier =
    onPreviewKeyEvent { event ->
        val code = when (event.nativeKeyEvent.keyCode) {
            AndroidKeyEvent.KEYCODE_DPAD_UP -> "ArrowUp"
            AndroidKeyEvent.KEYCODE_DPAD_DOWN -> "ArrowDown"
            AndroidKeyEvent.KEYCODE_DPAD_LEFT -> "ArrowLeft"
            AndroidKeyEvent.KEYCODE_DPAD_RIGHT -> "ArrowRight"
            AndroidKeyEvent.KEYCODE_ENTER -> "Enter"
            AndroidKeyEvent.KEYCODE_DEL -> "Backspace"
            AndroidKeyEvent.KEYCODE_TAB -> "Tab"
            AndroidKeyEvent.KEYCODE_ESCAPE -> "Escape"
            in AndroidKeyEvent.KEYCODE_A..AndroidKeyEvent.KEYCODE_Z -> "Key${('A'.code + event.nativeKeyEvent.keyCode - AndroidKeyEvent.KEYCODE_A).toChar()}"
            in AndroidKeyEvent.KEYCODE_0..AndroidKeyEvent.KEYCODE_9 -> "Digit${(event.nativeKeyEvent.keyCode - AndroidKeyEvent.KEYCODE_0)}"
            else -> return@onPreviewKeyEvent false
        }
        model.perform(
            Intent.DeviceAction(
                hostId,
                deviceId,
                        DeviceActionIntent.Key(
                            code,
                            event.nativeKeyEvent.getUnicodeChar(event.nativeKeyEvent.metaState)
                                .takeIf { it != 0 }
                                ?.let { String(Character.toChars(it)) }
                                ?: code,
                            event.type == KeyEventType.KeyDown,
                            event.nativeKeyEvent.isMetaPressed,
                            event.nativeKeyEvent.isCtrlPressed,
                        ),
            ),
        )
        true
    }

package dev.remoteagent.mobile

import android.view.KeyEvent as AndroidKeyEvent
import android.graphics.BitmapFactory
import android.os.Environment
import android.view.TextureView
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
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
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
import androidx.compose.ui.focus.focusable
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.unit.dp
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.viewinterop.AndroidView
import dev.remoteagent.core.DeviceActionIntent
import dev.remoteagent.core.DeviceView
import dev.remoteagent.core.Intent
import java.io.File

/** Native device picker, setup and live frame surface for a conversation. */
@Composable
internal fun DeviceScreen(model: AndroidAppModel, threadId: String) {
    val view = model.snapshot.device()
    val context = LocalContext.current
    val hostProfile = model.profileId
    val threadSessions = view.sessions.filter { it.threadId == threadId }
    val sessionKey = threadSessions.joinToString(",") { "${it.hostId}:${it.deviceId}" }
    val liveFrames = view.videoFrames.filter { it.threadId == threadId }
    LaunchedEffect(threadId, hostProfile) {
        model.perform(Intent.OpenThread(threadId))
        model.perform(Intent.LoadDevices)
        model.perform(Intent.SubscribeDevice)
    }
    LaunchedEffect(threadId, hostProfile, sessionKey) {
        threadSessions.forEach { session ->
            model.perform(Intent.LoadDeviceDetail(session.hostId, session.deviceId))
            model.perform(Intent.LoadDeviceAccessibility(session.hostId, session.deviceId))
            if (session.platform == "ios") {
                model.perform(Intent.LoadDeviceEventLog(session.hostId, session.deviceId, 100u.toUShort()))
            }
        }
    }
    DisposableEffect(threadId, hostProfile) {
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
            view.error?.let { error -> item { Text("Device error: $error") } }
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
                val foreground = view.foreground.firstOrNull {
                    it.hostId == session.hostId && it.deviceId == session.deviceId
                }?.appId ?: detail?.foregroundApp
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
                        model.perform(Intent.StartDeviceRecording(session.hostId, session.deviceId, "avcc"))
                    }) { Text("Record") }
                    Button(onClick = {
                        model.perform(Intent.StopDeviceRecording(session.hostId, session.deviceId))
                    }) { Text("Stop record") }
                    if (session.platform == "android") {
                        Button(onClick = {
                            model.perform(
                                Intent.DeviceAction(
                                    session.hostId,
                                    session.deviceId,
                                    DeviceActionIntent.Fold("table"),
                                ),
                            )
                        }) { Text("Fold") }
                        Button(onClick = {
                            model.perform(
                                Intent.DeviceAction(
                                    session.hostId,
                                    session.deviceId,
                                    DeviceActionIntent.Duo("toggle"),
                                ),
                            )
                        }) { Text("Duo") }
                    }
                    Button(onClick = {
                        model.perform(Intent.CloseDevice(session.hostId, session.deviceId, true))
                    }) { Text("Power off") }
                }
                foreground?.let { app -> Text("Foreground: " + app) }
            }
            view.screens
                .filter { it.threadId == threadId }
                .sortedBy { it.screenId ?: 0 }
                .forEach { screen ->
                    item(key = "screen-${screen.hostId}-${screen.deviceId}-${screen.screenId ?: 0}") {
                        Text(
                            "Screen ${screen.screenId ?: 0}: ${screen.width}×${screen.height} · " +
                                "${screen.orientation}" +
                                (screen.hingeAngle?.let { " · hinge ${it.toInt()}°" } ?: ""),
                        )
                        if (screen.tableModeAvailable) {
                            Text(if (screen.tableMode) "Table mode on" else "Table mode off")
                        }
                    }
                }
            if (liveFrames.isEmpty()) {
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
                                    .deviceTouchInput(model, frame.hostId, frame.deviceId, frame.sequence),
                            ) {
                                Image(it, "Live device frame", Modifier.fillMaxSize(), contentScale = ContentScale.Fit)
                                DeviceAccessibilityOverlay(view, frame.hostId, frame.deviceId)
                            }
                        }
                    }
                }
            } else {
                liveFrames
                    .groupBy { it.screenId ?: 0 }
                    .toSortedMap()
                    .forEach { (screenId, frames) ->
                        frames.maxByOrNull { it.sequence }?.let { frame ->
                            item(key = "video-frame-${frame.hostId}-${frame.deviceId}-$screenId-${frame.sequence}") {
                                if (frames.size > 1 || screenId != 0) Text("Live screen $screenId")
                                when (frame.encoding) {
                                    "jpeg", "mjpeg" -> DeviceJpegFrame(model, view, frame)
                                    "h264", "semu", "avcc-description" -> DeviceH264Frame(model, view, frame)
                                    else -> Text("Unsupported live device frame format: ${frame.encoding}")
                                }
                            }
                        }
                    }
            }
            view.accessibility
                .filter { tree -> threadSessions.any { it.hostId == tree.hostId && it.deviceId == tree.deviceId } }
                .forEach { tree ->
                    item(key = "accessibility-${tree.hostId}-${tree.deviceId}") {
                        Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
                            Text("Accessibility overlay")
                            tree.errors.forEach { error -> Text("Accessibility error: $error") }
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
                    recording.error?.let { error -> Text("Recording failed: $error") }
                    if (recording.error == null && recording.bytes.isNotEmpty()) {
                        val artifact = recordingArtifact(recording.format)
                        var savedPath by remember(recording.byteCount) { mutableStateOf<String?>(null) }
                        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                            Button(onClick = {
                                val directory = context.getExternalFilesDir(Environment.DIRECTORY_MOVIES)
                                    ?: context.filesDir
                                val file = File(
                                    directory,
                                    "device-${recording.deviceId}-${recording.byteCount}.${artifact.extension}",
                                )
                                runCatching {
                                    directory.mkdirs()
                                    file.writeBytes(recording.bytes)
                                    savedPath = file.absolutePath
                                }.onFailure { model.notice = "Could not save recording: ${it.message}" }
                            }) { Text("Save") }
                            Button(onClick = {
                                val file = File(
                                    context.cacheDir,
                                    "device-${recording.deviceId}-${recording.byteCount}.${artifact.extension}",
                                )
                                runCatching { file.writeBytes(recording.bytes) }
                                    .onSuccess {
                                        model.perform(
                                            Intent.AttachFiles(
                                                threadId,
                                                listOf(dev.remoteagent.core.LocalFile(file.path, file.name, artifact.mimeType)),
                                            ),
                                        ) { result ->
                                            result.exceptionOrNull()?.let { model.notice = "Could not attach recording: ${it.message}" }
                                        }
                                    }
                                    .onFailure { model.notice = "Could not prepare recording: ${it.message}" }
                            }) { Text("Attach") }
                        }
                        savedPath?.let { Text("Saved to $it") }
                    }
                }
            }
        }
        }
    }

@Composable
private fun DeviceJpegFrame(
    model: AndroidAppModel,
    view: DeviceView,
    frame: dev.remoteagent.core.DeviceVideoFrameView,
) {
    val bitmap = remember(frame.sequence) {
        BitmapFactory.decodeByteArray(frame.payload, 0, frame.payload.size)?.asImageBitmap()
    }
    bitmap?.let {
        Box(
            Modifier
                .fillMaxWidth()
                .size(320.dp)
                .deviceKeyInput(model, frame.hostId, frame.deviceId)
                .deviceTouchInput(model, frame.hostId, frame.deviceId, frame.sequence),
        ) {
            Image(it, "Live device video frame", Modifier.fillMaxSize(), contentScale = ContentScale.Fit)
            DeviceAccessibilityOverlay(view, frame.hostId, frame.deviceId)
        }
    }
}

@Composable
private fun DeviceH264Frame(
    model: AndroidAppModel,
    view: DeviceView,
    frame: dev.remoteagent.core.DeviceVideoFrameView,
) {
    val decoder = remember { DeviceVideoDecoder() }
    val streamKey = "${model.profileId}:${frame.hostId}:${frame.deviceId}:${frame.screenId ?: 0}"
    DisposableEffect(decoder) {
        onDispose { decoder.close() }
    }
    Box(
        Modifier
            .fillMaxWidth()
            .size(320.dp)
            .deviceKeyInput(model, frame.hostId, frame.deviceId)
            .deviceTouchInput(model, frame.hostId, frame.deviceId, frame.sequence),
    ) {
        AndroidView(
            modifier = Modifier.fillMaxSize(),
            factory = { context ->
                TextureView(context).also { decoder.attach(it) }
            },
            update = {
                decoder.reset(streamKey, frame.width.toInt(), frame.height.toInt())
                decoder.submit(
                    payload = frame.payload.toByteArray(),
                    encoding = frame.encoding,
                    width = frame.width.toInt(),
                    height = frame.height.toInt(),
                    sequence = frame.sequence,
                    timestampUs = frame.timestampUs,
                    keyframe = frame.keyframe,
                )
            },
        )
        DeviceAccessibilityOverlay(view, frame.hostId, frame.deviceId)
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

private fun Modifier.deviceTouchInput(
    model: AndroidAppModel,
    hostId: String,
    deviceId: String,
    sequence: ULong,
): Modifier = pointerInput(sequence) {
    awaitEachGesture {
        awaitPointerEventScope {
            val down = awaitFirstDown()
            fun send(phase: String, position: androidx.compose.ui.geometry.Offset) {
                val x = (position.x / size.width).coerceIn(0f, 1f)
                val y = (position.y / size.height).coerceIn(0f, 1f)
                model.perform(
                    Intent.DeviceAction(
                        hostId,
                        deviceId,
                        DeviceActionIntent.Touch(phase, x, y),
                    ),
                )
            }
            send("begin", down.position)
            while (true) {
                val event = awaitPointerEvent()
                val change = event.changes.first()
                when {
                    change.changedToUp() -> {
                        send("end", change.position)
                        break
                    }
                    change.positionChanged() -> {
                        change.consume()
                        send("move", change.position)
                    }
                }
            }
        }
    }
}

private fun Modifier.deviceKeyInput(model: AndroidAppModel, hostId: String, deviceId: String): Modifier =
    focusable().onPreviewKeyEvent { event ->
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
                DeviceActionIntent.Key(code, event.type == KeyEventType.KeyDown),
            ),
        )
        true
    }

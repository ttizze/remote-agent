package dev.remoteagent.mobile

import android.graphics.BitmapFactory
import android.os.Environment
import android.view.KeyEvent as AndroidKeyEvent
import android.view.TextureView
import android.widget.FrameLayout
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.Image
import androidx.compose.foundation.gestures.awaitEachGesture
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.aspectRatio
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
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.focusable
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.input.key.KeyEventType
import androidx.compose.ui.input.key.onPreviewKeyEvent
import androidx.compose.ui.input.pointer.awaitFirstDown
import androidx.compose.ui.input.pointer.changedToUp
import androidx.compose.ui.input.pointer.consume
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.input.pointer.positionChanged
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.AndroidView
import dev.remoteagent.core.DeviceActionIntent
import dev.remoteagent.core.DeviceDuoCommandIntent
import dev.remoteagent.core.DeviceDuoOrientationIntent
import dev.remoteagent.core.DeviceDuoPhysicalIntent
import dev.remoteagent.core.DeviceDuoPoseIntent
import dev.remoteagent.core.DeviceFoldPostureIntent
import dev.remoteagent.core.DeviceView
import dev.remoteagent.core.Intent
import dev.remoteagent.core.projectTouchPoint
import java.io.File

private data class StreamKey(val hostId: String, val deviceId: String, val screenId: Int, val sessionEpoch: String)

private data class DeviceKeyFacts(val code: String, val key: String, val meta: Boolean, val ctrl: Boolean)

/** Native device picker, setup and live frame surface for a conversation. */
@Composable
internal fun DeviceScreen(model: AndroidAppModel, threadId: String) {
    val view = model.snapshot.device()
    val context = LocalContext.current
    val hostProfile = model.profileId
    val threadSessions = view.sessions.filter { it.threadId == threadId }
    val sessionKey = threadSessions.joinToString(",") { "${it.hostId}:${it.deviceId}:${it.sessionEpoch}" }
    val liveEvents = view.videoEvents.filter { it.threadId == threadId }
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
    DisposableEffect(threadId, hostProfile) { onDispose { model.perform(Intent.UnsubscribeDevice) } }
    DisposableEffect(threadId, sessionKey) {
        onDispose {
            threadSessions.forEach { session ->
                model.perform(Intent.ReleaseDeviceInput(session.hostId, session.deviceId, session.sessionEpoch))
            }
        }
    }
    ScreenScaffold("Device", onBack = model::back) {
        if (!view.enabled) {
            Column(Modifier.fillMaxSize().padding(20.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
                Text("Device support is off")
                Text("Enable it to discover simulators and emulators on the Host.")
                Button(onClick = { model.perform(Intent.ConfigureDevices(true, null, false)) }) {
                    Text("Enable device support")
                }
            }
            return@ScreenScaffold
        }
        LazyColumn(Modifier.fillMaxSize().padding(16.dp), verticalArrangement = Arrangement.spacedBy(10.dp)) {
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
                    Button(onClick = { model.perform(Intent.ConfigureDevices(null, true, true)) }) {
                        Text("Enable agent device access")
                    }
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
                        Button(
                            onClick = {
                                if (opened) {
                                    model.perform(
                                        Intent.ReleaseDeviceInput(
                                            device.hostId,
                                            device.id,
                                            threadSessions
                                                .firstOrNull { it.hostId == device.hostId && it.deviceId == device.id }
                                                ?.sessionEpoch,
                                        )
                                    )
                                    model.perform(Intent.CloseDevice(device.hostId, device.id, false))
                                } else {
                                    model.perform(Intent.OpenDevice(device.hostId, device.id, device.platform, true))
                                }
                            }
                        ) {
                            Text(if (opened) "Close" else "Open")
                        }
                    }
                }
            }
            items(
                threadSessions,
                key = { session -> "controls:${session.hostId}:${session.deviceId}:${session.sessionEpoch}" },
            ) { session ->
                val detail = view.details.firstOrNull { it.hostId == session.hostId && it.deviceId == session.deviceId }
                val activeRecording =
                    view.recordings.firstOrNull {
                        it.threadId == threadId &&
                            it.hostId == session.hostId &&
                            it.deviceId == session.deviceId &&
                            it.sessionEpoch == session.sessionEpoch
                    }
                val foreground =
                    view.foreground
                        .firstOrNull { it.hostId == session.hostId && it.deviceId == session.deviceId }
                        ?.appId ?: detail?.foregroundApp
                val duo =
                    view.duoControls.firstOrNull {
                        it.threadId == threadId &&
                            it.hostId == session.hostId &&
                            it.deviceId == session.deviceId &&
                            it.sessionEpoch == session.sessionEpoch
                    }
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    Button(
                        onClick = {
                            model.perform(
                                Intent.DeviceAction(
                                    session.hostId,
                                    session.deviceId,
                                    DeviceActionIntent.SetAppearance(true),
                                )
                            )
                        }
                    ) {
                        Text("Dark")
                    }
                    Button(
                        onClick = {
                            model.perform(
                                Intent.DeviceAction(
                                    session.hostId,
                                    session.deviceId,
                                    DeviceActionIntent.SetAppearance(false),
                                )
                            )
                        }
                    ) {
                        Text("Light")
                    }
                    Button(
                        onClick = {
                            model.perform(
                                Intent.DeviceAction(
                                    session.hostId,
                                    session.deviceId,
                                    DeviceActionIntent.SetTextSize("large"),
                                )
                            )
                        }
                    ) {
                        Text("Text +")
                    }
                    if (session.platform == "android") {
                        Button(
                            onClick = {
                                model.perform(
                                    Intent.DeviceAction(
                                        session.hostId,
                                        session.deviceId,
                                        DeviceActionIntent.SetOrientation("portrait"),
                                    )
                                )
                            }
                        ) {
                            Text("Portrait")
                        }
                    }
                    Button(
                        onClick = {
                            model.perform(
                                Intent.DeviceAction(
                                    session.hostId,
                                    session.deviceId,
                                    DeviceActionIntent.HardwareButton("home"),
                                )
                            )
                        }
                    ) {
                        Text("Home")
                    }
                    Button(
                        onClick = {
                            model.perform(
                                Intent.DeviceAction(session.hostId, session.deviceId, DeviceActionIntent.Rotate)
                            )
                        }
                    ) {
                        Text("Rotate")
                    }
                    Button(
                        onClick = {
                            model.perform(Intent.StartDeviceRecording(session.hostId, session.deviceId, "mp4"))
                        }
                    ) {
                        Text("Record")
                    }
                    Button(
                        onClick = {
                            activeRecording?.let { recording ->
                                model.perform(
                                    Intent.StopDeviceRecording(
                                        session.hostId,
                                        session.deviceId,
                                        recording.recordingId,
                                        recording.sessionEpoch,
                                    )
                                )
                            } ?: run { model.notice = "No active device recording" }
                        }
                    ) {
                        Text("Stop record")
                    }
                    if (session.platform == "android") {
                        Button(
                            onClick = {
                                model.perform(
                                    Intent.DeviceAction(
                                        session.hostId,
                                        session.deviceId,
                                        DeviceActionIntent.Fold(DeviceFoldPostureIntent.Closed),
                                    )
                                )
                            }
                        ) {
                            Text("Close fold")
                        }
                        Button(
                            onClick = {
                                model.perform(
                                    Intent.DeviceAction(
                                        session.hostId,
                                        session.deviceId,
                                        DeviceActionIntent.Fold(DeviceFoldPostureIntent.Opened),
                                    )
                                )
                            }
                        ) {
                            Text("Open fold")
                        }
                    }
                    if (session.platform == "ios") {
                        Button(
                            onClick = {
                                model.perform(
                                    Intent.DeviceAction(
                                        session.hostId,
                                        session.deviceId,
                                        DeviceActionIntent.Duo(DeviceDuoCommandIntent.Angle(90f)),
                                    )
                                )
                            }
                        ) {
                            Text("Duo 90°")
                        }
                        Button(
                            onClick = {
                                model.perform(
                                    Intent.DeviceAction(
                                        session.hostId,
                                        session.deviceId,
                                        DeviceActionIntent.Duo(DeviceDuoCommandIntent.Angle(180f)),
                                    )
                                )
                            }
                        ) {
                            Text("Duo 180°")
                        }
                        listOf(
                                DeviceDuoPoseIntent.Closed to "Duo closed",
                                DeviceDuoPoseIntent.Book to "Duo book",
                                DeviceDuoPoseIntent.Open to "Duo open",
                                DeviceDuoPoseIntent.Laptop to "Duo laptop",
                                DeviceDuoPoseIntent.Tent to "Duo tent",
                            )
                            .forEach { (pose, label) ->
                                Button(
                                    onClick = {
                                        model.perform(
                                            Intent.DeviceAction(
                                                session.hostId,
                                                session.deviceId,
                                                DeviceActionIntent.Duo(DeviceDuoCommandIntent.Pose(pose)),
                                            )
                                        )
                                    }
                                ) {
                                    Text(label)
                                }
                            }
                        Button(
                            onClick = {
                                model.perform(
                                    Intent.DeviceAction(
                                        session.hostId,
                                        session.deviceId,
                                        DeviceActionIntent.Duo(DeviceDuoCommandIntent.Table(true)),
                                    )
                                )
                            }
                        ) {
                            Text("Table on")
                        }
                        Button(
                            onClick = {
                                model.perform(
                                    Intent.DeviceAction(
                                        session.hostId,
                                        session.deviceId,
                                        DeviceActionIntent.Duo(DeviceDuoCommandIntent.Table(false)),
                                    )
                                )
                            }
                        ) {
                            Text("Table off")
                        }
                        listOf(
                                DeviceDuoPhysicalIntent.Faceup to "Face up",
                                DeviceDuoPhysicalIntent.Facedown to "Face down",
                            )
                            .forEach { (physical, label) ->
                                Button(
                                    onClick = {
                                        model.perform(
                                            Intent.DeviceAction(
                                                session.hostId,
                                                session.deviceId,
                                                DeviceActionIntent.Duo(DeviceDuoCommandIntent.Physical(physical)),
                                            )
                                        )
                                    }
                                ) {
                                    Text(label)
                                }
                            }
                        listOf(
                                DeviceDuoOrientationIntent.Portrait to "Portrait",
                                DeviceDuoOrientationIntent.LandscapeLeft to "Landscape left",
                                DeviceDuoOrientationIntent.PortraitUpsideDown to "Portrait upside down",
                                DeviceDuoOrientationIntent.LandscapeRight to "Landscape right",
                            )
                            .forEach { (orientation, label) ->
                                Button(
                                    onClick = {
                                        model.perform(
                                            Intent.DeviceAction(
                                                session.hostId,
                                                session.deviceId,
                                                DeviceActionIntent.Duo(DeviceDuoCommandIntent.Orientation(orientation)),
                                            )
                                        )
                                    }
                                ) {
                                    Text(label)
                                }
                            }
                    }
                    Button(
                        onClick = {
                            model.perform(
                                Intent.ReleaseDeviceInput(session.hostId, session.deviceId, session.sessionEpoch)
                            )
                            model.perform(Intent.CloseDevice(session.hostId, session.deviceId, true))
                        }
                    ) {
                        Text("Power off")
                    }
                }
                foreground?.let { app -> Text("Foreground: " + app) }
                duo?.let { control ->
                    when {
                        control.pending -> Text("Duo control pending" + (control.requested?.let { ": $it" } ?: ""))
                        control.error != null -> Text("Duo control failed: ${control.error}")
                    }
                }
            }
            view.screens
                .filter { screen ->
                    screen.threadId == threadId &&
                        threadSessions.any { session ->
                            session.hostId == screen.hostId &&
                                session.deviceId == screen.deviceId &&
                                session.sessionEpoch == screen.sessionEpoch
                        }
                }
                .sortedBy { it.screenId ?: 0 }
                .forEach { screen ->
                    item(
                        key =
                            "screen-${screen.hostId}-${screen.deviceId}-${screen.screenId ?: 0}-${screen.sessionEpoch}"
                    ) {
                        Text(
                            "Screen ${screen.screenId ?: 0}: ${screen.width}×${screen.height} · " +
                                "${screen.orientation}" +
                                (screen.hingeAngle?.let { " · hinge ${it.toInt()}°" } ?: "")
                        )
                        if (screen.hingePose != null || screen.tableModeAvailable) {
                            Text(
                                "Duo readback: " +
                                    listOfNotNull(
                                            screen.hingePose?.let { "pose $it" },
                                            screen.hingeAngle?.let { "angle ${it.toInt()}°" },
                                            if (screen.tableModeAvailable) {
                                                "table ${if (screen.tableMode) "on" else "off"}"
                                            } else {
                                                null
                                            },
                                        )
                                        .joinToString(" · ")
                            )
                        }
                        if (screen.tableModeAvailable) {
                            Text(if (screen.tableMode) "Table mode on" else "Table mode off")
                        }
                    }
                }
            liveEvents
                .groupBy { StreamKey(it.hostId, it.deviceId, it.screenId?.toInt() ?: 0, it.sessionEpoch) }
                .toSortedMap(compareBy({ it.hostId }, { it.deviceId }, { it.screenId }, { it.sessionEpoch }))
                .forEach { (stream, events) ->
                    val ordered = events.sortedBy { it.sequence }
                    val frame = ordered.lastOrNull() ?: return@forEach
                    val epoch = frame.sessionEpoch
                    item(key = "video-frame-${stream.hostId}-${stream.deviceId}-${stream.screenId}-$epoch") {
                        if (ordered.size > 1 || stream.screenId != 0) Text("Live screen ${stream.screenId}")
                        when (frame.encoding) {
                            "jpeg",
                            "mjpeg" -> DeviceJpegFrame(model, view, frame, epoch, stream.screenId)
                            "h264",
                            "semu",
                            "avcc-description" -> DeviceH264Frame(model, view, ordered, epoch, stream.screenId)
                            else -> Text("Unsupported live device frame format: ${frame.encoding}")
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
                            tree.elements
                                .filter { it.label.isNotEmpty() }
                                .take(20)
                                .forEach { element -> Text("${element.role} · ${element.label}") }
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
            view.lastRecording
                ?.takeIf { it.threadId == threadId }
                ?.let { recording ->
                    item(key = "last-recording-${recording.deviceId}-${recording.byteCount}") {
                        Text("Recording ready · ${recording.frameCount} frames · ${recording.byteCount} bytes")
                        recording.error?.let { error -> Text("Recording failed: $error") }
                        val fileName = recording.fileName
                        val mimeType = recording.mimeType
                        if (
                            recording.error == null &&
                                recording.bytes.isNotEmpty() &&
                                fileName.isNotBlank() &&
                                mimeType.isNotBlank()
                        ) {
                            var savedPath by remember(recording.byteCount) { mutableStateOf<String?>(null) }
                            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                                Button(
                                    onClick = {
                                        val directory =
                                            context.getExternalFilesDir(Environment.DIRECTORY_MOVIES)
                                                ?: context.filesDir
                                        val file = File(directory, fileName)
                                        runCatching {
                                                directory.mkdirs()
                                                file.writeBytes(recording.bytes)
                                                savedPath = file.absolutePath
                                            }
                                            .onFailure { model.notice = "Could not save recording: ${it.message}" }
                                    }
                                ) {
                                    Text("Save")
                                }
                                Button(
                                    onClick = {
                                        val file = File(context.cacheDir, fileName)
                                        runCatching { file.writeBytes(recording.bytes) }
                                            .onSuccess {
                                                model.perform(
                                                    Intent.AttachFiles(
                                                        threadId,
                                                        listOf(
                                                            dev.remoteagent.core.LocalFile(
                                                                file.path,
                                                                file.name,
                                                                mimeType,
                                                            )
                                                        ),
                                                    )
                                                ) { result ->
                                                    result.exceptionOrNull()?.let {
                                                        model.notice = "Could not attach recording: ${it.message}"
                                                    }
                                                }
                                            }
                                            .onFailure { model.notice = "Could not prepare recording: ${it.message}" }
                                    }
                                ) {
                                    Text("Attach")
                                }
                            }
                            savedPath?.let { Text("Saved to $it") }
                        }
                        if (
                            recording.error == null &&
                                recording.bytes.isNotEmpty() &&
                                (fileName.isBlank() || mimeType.isBlank())
                        ) {
                            Text("Recording is not a playable artifact; save and attach are unavailable")
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
    sessionEpoch: String,
    screenId: Int,
) {
    val bitmap =
        remember(frame.sequence) {
            BitmapFactory.decodeByteArray(frame.payload, 0, frame.payload.size)?.asImageBitmap()
        }
    bitmap?.let {
        Box(
            Modifier.fillMaxWidth()
                .aspectRatio(frame.width.toFloat() / frame.height.toFloat().coerceAtLeast(1f))
                .deviceKeyInput(model, frame.hostId, frame.deviceId)
                .deviceTouchInput(
                    model,
                    frame.hostId,
                    frame.deviceId,
                    sessionEpoch,
                    screenId,
                    frame.width.toInt(),
                    frame.height.toInt(),
                )
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
    frames: List<dev.remoteagent.core.DeviceVideoFrameView>,
    sessionEpoch: String,
    screenId: Int,
) {
    val frame = frames.lastOrNull() ?: return
    val decoder = remember { DeviceVideoDecoder() }
    val streamKey = "${model.profileId}:$sessionEpoch:${frame.hostId}:${frame.deviceId}:$screenId"
    DisposableEffect(decoder) { onDispose { decoder.close() } }
    Box(
        Modifier.fillMaxWidth()
            .aspectRatio(frame.width.toFloat() / frame.height.toFloat().coerceAtLeast(1f))
            .deviceKeyInput(model, frame.hostId, frame.deviceId)
            .deviceTouchInput(
                model,
                frame.hostId,
                frame.deviceId,
                sessionEpoch,
                screenId,
                frame.width.toInt(),
                frame.height.toInt(),
            )
    ) {
        AndroidView(
            modifier = Modifier.fillMaxSize(),
            factory = { context ->
                FrameLayout(context).apply {
                    addView(
                        TextureView(context).also { decoder.attach(it) },
                        FrameLayout.LayoutParams(
                            FrameLayout.LayoutParams.MATCH_PARENT,
                            FrameLayout.LayoutParams.MATCH_PARENT,
                        ),
                    )
                    addView(
                        DeviceAccessibilityOverlayView(context),
                        FrameLayout.LayoutParams(
                            FrameLayout.LayoutParams.MATCH_PARENT,
                            FrameLayout.LayoutParams.MATCH_PARENT,
                        ),
                    )
                }
            },
            update = { container ->
                val tree = view.accessibility.firstOrNull { it.hostId == frame.hostId && it.deviceId == frame.deviceId }
                (container.getChildAt(1) as? DeviceAccessibilityOverlayView)?.rects =
                    tree
                        ?.elements
                        ?.filter { it.label.isNotEmpty() }
                        ?.map { element ->
                            android.graphics.RectF(
                                element.x,
                                element.y,
                                element.x + element.width,
                                element.y + element.height,
                            )
                        } ?: emptyList()
                decoder.reset(streamKey, frame.width.toInt(), frame.height.toInt())
                frames.forEach { event ->
                    decoder.submit(
                        payload = event.payload.toByteArray(),
                        encoding = event.encoding,
                        sequence = event.sequence,
                        timestampUs = event.timestampUs,
                        keyframe = event.keyframe,
                    )
                }
            },
        )
    }
}

@Composable
private fun DeviceAccessibilityOverlay(view: DeviceView, hostId: String, deviceId: String) {
    val tree = view.accessibility.firstOrNull { it.hostId == hostId && it.deviceId == deviceId }
    if (tree == null) return
    Canvas(Modifier.fillMaxSize()) {
        tree.elements
            .filter { it.label.isNotEmpty() }
            .forEach { element ->
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
    sessionEpoch: String,
    screenId: Int,
    contentWidth: Int,
    contentHeight: Int,
): Modifier =
    pointerInput(hostId, deviceId, sessionEpoch, screenId, contentWidth, contentHeight) {
        awaitEachGesture {
            var lastPoint: Pair<Float, Float>? = null
            var active = false
            var ended = false
            try {
                awaitPointerEventScope {
                    val down = awaitFirstDown()
                    fun send(phase: String, position: androidx.compose.ui.geometry.Offset): Boolean {
                        val point =
                            projectTouchPoint(
                                position.x,
                                position.y,
                                size.width,
                                size.height,
                                frameWidth.toFloat(),
                                frameHeight.toFloat(),
                            ) ?: return false
                        val x = point.x
                        val y = point.y
                        lastPoint = x to y
                        model.perform(
                            Intent.DeviceAction(hostId, deviceId, DeviceActionIntent.Touch(phase, point.x, point.y))
                        )
                        return true
                    }
                    active = send("begin", down.position)
                    while (true) {
                        val event = awaitPointerEvent()
                        val change = event.changes.first()
                        when {
                            change.changedToUp() -> {
                                if (active && !send("end", change.position)) {
                                    lastPoint?.let { (x, y) ->
                                        model.perform(
                                            Intent.DeviceAction(hostId, deviceId, DeviceActionIntent.Touch("end", x, y))
                                        )
                                    }
                                }
                                ended = true
                                active = false
                                lastPoint = null
                                break
                            }
                            change.positionChanged() -> {
                                change.consume()
                                if (active) {
                                    send("move", change.position)
                                } else {
                                    active = send("begin", change.position)
                                }
                            }
                        }
                    }
                }
            } finally {
                if (!ended) {
                    if (active)
                        lastPoint?.let { (x, y) ->
                            model.perform(Intent.DeviceAction(hostId, deviceId, DeviceActionIntent.Touch("end", x, y)))
                        }
                }
            }
        }
    }

private fun Modifier.deviceKeyInput(model: AndroidAppModel, hostId: String, deviceId: String): Modifier =
    focusable().onPreviewKeyEvent { event ->
        // Android's device stream sends key actions on keydown only. The Host
        // maps the actual key value to either a text or navigation event and
        // does not emit a second action for keyup.
        if (event.type != KeyEventType.KeyDown) return@onPreviewKeyEvent false
        val facts = deviceKeyFacts(event.nativeKeyEvent) ?: return@onPreviewKeyEvent true
        model.perform(
            Intent.DeviceAction(
                hostId,
                deviceId,
                DeviceActionIntent.Key(facts.code, facts.key, true, facts.meta, facts.ctrl),
            )
        )
        true
    }

private fun deviceKeyFacts(event: AndroidKeyEvent): DeviceKeyFacts? {
    val key =
        when (event.keyCode) {
            AndroidKeyEvent.KEYCODE_DPAD_UP -> "ArrowUp"
            AndroidKeyEvent.KEYCODE_DPAD_DOWN -> "ArrowDown"
            AndroidKeyEvent.KEYCODE_DPAD_LEFT -> "ArrowLeft"
            AndroidKeyEvent.KEYCODE_DPAD_RIGHT -> "ArrowRight"
            AndroidKeyEvent.KEYCODE_ENTER -> "Enter"
            AndroidKeyEvent.KEYCODE_DEL -> "Backspace"
            AndroidKeyEvent.KEYCODE_FORWARD_DEL -> "Delete"
            AndroidKeyEvent.KEYCODE_TAB -> "Tab"
            AndroidKeyEvent.KEYCODE_ESCAPE -> "Escape"
            AndroidKeyEvent.KEYCODE_MOVE_HOME -> "Home"
            AndroidKeyEvent.KEYCODE_MOVE_END -> "End"
            AndroidKeyEvent.KEYCODE_PAGE_UP -> "PageUp"
            AndroidKeyEvent.KEYCODE_PAGE_DOWN -> "PageDown"
            else -> {
                val unicode = event.unicodeChar
                // The Android transport accepts the same UTF-16 single-unit key
                // values as the reference stream. Supplementary characters are
                // represented by two units and must not be sent as one text key.
                if (!Character.isValidCodePoint(unicode) || Character.charCount(unicode) != 1 || unicode == 0)
                    return null
                String(Character.toChars(unicode))
            }
        }
    return DeviceKeyFacts(
        code = AndroidKeyEvent.keyCodeToString(event.keyCode),
        key = key,
        meta = event.isMetaPressed,
        ctrl = event.isCtrlPressed,
    )
}

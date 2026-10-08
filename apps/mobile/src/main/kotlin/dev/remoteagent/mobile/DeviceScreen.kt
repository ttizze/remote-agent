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
import androidx.compose.foundation.lazy.LazyListScope
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
import dev.remoteagent.core.DeviceEntryView
import dev.remoteagent.core.DeviceFoldPostureIntent
import dev.remoteagent.core.DeviceRecordingView
import dev.remoteagent.core.DeviceSessionView
import dev.remoteagent.core.DeviceView
import dev.remoteagent.core.Intent
import dev.remoteagent.core.projectTouchPoint
import java.io.File

private data class StreamKey(val hostId: String, val deviceId: String, val screenId: Int, val sessionEpoch: String)

private data class DeviceKeyFacts(val code: String, val key: String, val meta: Boolean, val ctrl: Boolean)

private const val DEVICE_EVENT_LOG_LOAD_LIMIT = 100
private const val DEVICE_DUO_HALF_OPEN_ANGLE = 90f
private const val DEVICE_DUO_FULL_OPEN_ANGLE = 180f
private const val DEVICE_ACCESSIBILITY_ELEMENT_LIMIT = 20
private const val DEVICE_EVENT_LOG_PREVIEW_LIMIT = 20
private const val DEVICE_ACCESSIBILITY_STROKE_WIDTH = 2f
// This is the native overlay's fixed accessibility accent, matching the Host UI token.
@Suppress("MagicNumber") private val DEVICE_ACCESSIBILITY_COLOR = Color(0xFF4F8CFF)

/** Native device picker, setup and live frame surface for a conversation. */
@Composable
internal fun DeviceScreen(model: AndroidAppModel, threadId: String) {
    val view = model.snapshot.device()
    val hostProfile = model.profileId
    val threadSessions = view.sessions.filter { it.threadId == threadId }
    val sessionKey = threadSessions.joinToString(",") { "${it.hostId}:${it.deviceId}:${it.sessionEpoch}" }
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
                model.perform(
                    Intent.LoadDeviceEventLog(session.hostId, session.deviceId, DEVICE_EVENT_LOG_LOAD_LIMIT.toUShort())
                )
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
            DeviceOverviewSections.supportDisabled(model)
            return@ScreenScaffold
        }
        LazyColumn(Modifier.fillMaxSize().padding(16.dp), verticalArrangement = Arrangement.spacedBy(10.dp)) {
            with(DeviceOverviewSections) {
                statusItems(model, view)
                pickerItems(model, view.devices, threadSessions)
                sessionItems(model, view, threadId, threadSessions)
            }
            with(DeviceStreamSections) {
                screenItems(view, threadId, threadSessions)
                liveItems(model, view, threadId)
                accessibilityItems(view, threadSessions)
                eventLogItems(view, threadSessions)
                recordingItems(model, threadId, view.lastRecording)
            }
        }
    }
}

private object DeviceOverviewSections {
    @Composable
    fun supportDisabled(model: AndroidAppModel) {
        Column(Modifier.fillMaxSize().padding(20.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
            Text("Device support is off")
            Text("Enable it to discover simulators and emulators on the Host.")
            Button(onClick = { model.perform(Intent.ConfigureDevices(true, null, false)) }) {
                Text("Enable device support")
            }
        }
    }

    fun LazyListScope.statusItems(model: AndroidAppModel, view: DeviceView) {
        item { Text("Host status: ${view.status}") }
        view.error?.let { error -> item { Text("Device error: ${error}") } }
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
        if (!view.agentAccessEnabled) {
            item {
                Button(onClick = { model.perform(Intent.ConfigureDevices(null, true, true)) }) {
                    Text("Enable agent device access")
                }
            }
        }
    }

    fun LazyListScope.pickerItems(
        model: AndroidAppModel,
        devices: List<DeviceEntryView>,
        threadSessions: List<DeviceSessionView>,
    ) {
        items(devices, key = { "${it.hostId}:${it.id}" }) { device ->
            val opened = threadSessions.any { it.hostId == device.hostId && it.deviceId == device.id }
            Card(Modifier.fillMaxWidth()) {
                Row(Modifier.fillMaxWidth().padding(12.dp), horizontalArrangement = Arrangement.spacedBy(10.dp)) {
                    Column(Modifier.weight(1f)) {
                        Text(device.name)
                        Text("${device.platform} · ${device.version}")
                    }
                    Button(
                        onClick = {
                            val session = threadSessions.firstOrNull {
                                it.hostId == device.hostId && it.deviceId == device.id
                            }
                            if (opened) {
                                model.perform(
                                    Intent.ReleaseDeviceInput(device.hostId, device.id, session?.sessionEpoch)
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
    }

    fun LazyListScope.sessionItems(
        model: AndroidAppModel,
        view: DeviceView,
        threadId: String,
        threadSessions: List<DeviceSessionView>,
    ) {
        items(
            threadSessions,
            key = { session -> "controls:${session.hostId}:${session.deviceId}:${session.sessionEpoch}" },
        ) { session ->
            val detail = view.details.firstOrNull { it.hostId == session.hostId && it.deviceId == session.deviceId }
            val activeRecording =
                view.recordings.firstOrNull {
                    when {
                        it.threadId != threadId -> false
                        it.hostId != session.hostId -> false
                        it.deviceId != session.deviceId -> false
                        else -> it.sessionEpoch == session.sessionEpoch
                    }
                }
            val foreground =
                view.foreground.firstOrNull { it.hostId == session.hostId && it.deviceId == session.deviceId }?.appId
                    ?: detail?.foregroundApp
            val duo =
                view.duoControls.firstOrNull {
                    when {
                        it.threadId != threadId -> false
                        it.hostId != session.hostId -> false
                        it.deviceId != session.deviceId -> false
                        else -> it.sessionEpoch == session.sessionEpoch
                    }
                }
            sessionControls(model, session, activeRecording, foreground, duo)
        }
    }

    @Composable
    fun sessionControls(
        model: AndroidAppModel,
        session: DeviceSessionView,
        activeRecording: DeviceRecordingView?,
        foreground: String?,
        duo: dev.remoteagent.core.DeviceDuoControlView?,
    ) {
        Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
            appearanceControls(model, session)
            recordingControls(model, session, activeRecording)
            if (session.platform == "android") foldControls(model, session)
            if (session.platform == "ios") duoControls(model, session)
            powerControl(model, session)
            foreground?.let { app -> Text("Foreground: ${app}") }
            duo?.let { control ->
                when {
                    control.pending -> Text("Duo control pending" + (control.requested?.let { ": ${it}" } ?: ""))
                    control.error != null -> Text("Duo control failed: ${control.error}")
                }
            }
        }
    }

    @Composable
    fun appearanceControls(model: AndroidAppModel, session: DeviceSessionView) {
        fun send(action: DeviceActionIntent) {
            model.perform(Intent.DeviceAction(session.hostId, session.deviceId, action))
        }
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            Button(onClick = { send(DeviceActionIntent.SetAppearance(true)) }) { Text("Dark") }
            Button(onClick = { send(DeviceActionIntent.SetAppearance(false)) }) { Text("Light") }
            Button(onClick = { send(DeviceActionIntent.SetTextSize("large")) }) { Text("Text +") }
            if (session.platform == "android") {
                Button(onClick = { send(DeviceActionIntent.SetOrientation("portrait")) }) { Text("Portrait") }
            }
            Button(onClick = { send(DeviceActionIntent.HardwareButton("home")) }) { Text("Home") }
            Button(onClick = { send(DeviceActionIntent.Rotate) }) { Text("Rotate") }
        }
    }

    @Composable
    fun recordingControls(model: AndroidAppModel, session: DeviceSessionView, activeRecording: DeviceRecordingView?) {
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            Button(onClick = { model.perform(Intent.StartDeviceRecording(session.hostId, session.deviceId, "mp4")) }) {
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
        }
    }

    @Composable
    fun foldControls(model: AndroidAppModel, session: DeviceSessionView) {
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
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
    }

    @Composable
    fun duoControls(model: AndroidAppModel, session: DeviceSessionView) {
        fun send(command: DeviceDuoCommandIntent) {
            model.perform(Intent.DeviceAction(session.hostId, session.deviceId, DeviceActionIntent.Duo(command)))
        }
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            Button(onClick = { send(DeviceDuoCommandIntent.Angle(DEVICE_DUO_HALF_OPEN_ANGLE)) }) { Text("Duo 90°") }
            Button(onClick = { send(DeviceDuoCommandIntent.Angle(DEVICE_DUO_FULL_OPEN_ANGLE)) }) { Text("Duo 180°") }
            listOf(
                    DeviceDuoPoseIntent.Closed to "Duo closed",
                    DeviceDuoPoseIntent.Book to "Duo book",
                    DeviceDuoPoseIntent.Open to "Duo open",
                    DeviceDuoPoseIntent.Laptop to "Duo laptop",
                    DeviceDuoPoseIntent.Tent to "Duo tent",
                )
                .forEach { (pose, label) ->
                    Button(onClick = { send(DeviceDuoCommandIntent.Pose(pose)) }) { Text(label) }
                }
            Button(onClick = { send(DeviceDuoCommandIntent.Table(true)) }) { Text("Table on") }
            Button(onClick = { send(DeviceDuoCommandIntent.Table(false)) }) { Text("Table off") }
            listOf(DeviceDuoPhysicalIntent.Faceup to "Face up", DeviceDuoPhysicalIntent.Facedown to "Face down")
                .forEach { (physical, label) ->
                    Button(onClick = { send(DeviceDuoCommandIntent.Physical(physical)) }) { Text(label) }
                }
            listOf(
                    DeviceDuoOrientationIntent.Portrait to "Portrait",
                    DeviceDuoOrientationIntent.LandscapeLeft to "Landscape left",
                    DeviceDuoOrientationIntent.PortraitUpsideDown to "Portrait upside down",
                    DeviceDuoOrientationIntent.LandscapeRight to "Landscape right",
                )
                .forEach { (orientation, label) ->
                    Button(onClick = { send(DeviceDuoCommandIntent.Orientation(orientation)) }) { Text(label) }
                }
        }
    }

    @Composable
    fun powerControl(model: AndroidAppModel, session: DeviceSessionView) {
        Button(
            onClick = {
                model.perform(Intent.ReleaseDeviceInput(session.hostId, session.deviceId, session.sessionEpoch))
                model.perform(Intent.CloseDevice(session.hostId, session.deviceId, true))
            }
        ) {
            Text("Power off")
        }
    }
}

private object DeviceStreamSections {
    fun LazyListScope.screenItems(view: DeviceView, threadId: String, threadSessions: List<DeviceSessionView>) {
        view.screens
            .filter { screen ->
                screen.threadId == threadId &&
                    threadSessions.any {
                        when {
                            it.hostId != screen.hostId -> false
                            it.deviceId != screen.deviceId -> false
                            else -> it.sessionEpoch == screen.sessionEpoch
                        }
                    }
            }
            .sortedBy { it.screenId ?: 0 }
            .forEach { screen ->
                item(
                    key = "screen-${screen.hostId}-${screen.deviceId}-${screen.screenId ?: 0}-${screen.sessionEpoch}"
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
                                        screen.hingePose?.let { "pose ${it}" },
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
    }

    fun LazyListScope.liveItems(model: AndroidAppModel, view: DeviceView, threadId: String) {
        view.videoEvents
            .filter { it.threadId == threadId }
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
    }

    fun LazyListScope.accessibilityItems(view: DeviceView, threadSessions: List<DeviceSessionView>) {
        view.accessibility
            .filter { tree -> threadSessions.any { it.hostId == tree.hostId && it.deviceId == tree.deviceId } }
            .forEach { tree ->
                item(key = "accessibility-${tree.hostId}-${tree.deviceId}-${tree.sessionEpoch}") {
                    Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
                        Text("Accessibility overlay")
                        tree.errors.forEach { error -> Text("Accessibility error: ${error}") }
                        tree.elements
                            .filter { it.label.isNotEmpty() }
                            .take(DEVICE_ACCESSIBILITY_ELEMENT_LIMIT)
                            .forEach { element -> Text("${element.role} · ${element.label}") }
                    }
                }
            }
    }

    fun LazyListScope.eventLogItems(view: DeviceView, threadSessions: List<DeviceSessionView>) {
        view.eventLog
            .filter { entry -> threadSessions.any { it.hostId == entry.hostId && it.deviceId == entry.deviceId } }
            .takeLast(DEVICE_EVENT_LOG_PREVIEW_LIMIT)
            .forEach { entry ->
                item(key = "event-log-${entry.hostId}-${entry.deviceId}-${entry.sessionEpoch}-${entry.id}") {
                    Text("${entry.kind} · ${entry.summary}")
                }
            }
    }

    fun LazyListScope.recordingItems(model: AndroidAppModel, threadId: String, recording: DeviceRecordingView?) {
        recording
            ?.takeIf { it.threadId == threadId }
            ?.let { value ->
                item(key = "last-recording-${value.deviceId}-${value.byteCount}-${value.recordingId}") {
                    recordingItem(model, threadId, value)
                }
            }
    }

    @Composable
    fun recordingItem(model: AndroidAppModel, threadId: String, recording: DeviceRecordingView) {
        Text("Recording ready · ${recording.frameCount} frames · ${recording.byteCount} bytes")
        recording.error?.let { error -> Text("Recording failed: ${error}") }
        val fileName = recording.fileName
        val mimeType = recording.mimeType
        if (recording.error == null && recording.bytes.isNotEmpty()) {
            if (fileName.isNotBlank() && mimeType.isNotBlank()) {
                recordingActions(model, threadId, recording, fileName, mimeType)
            } else {
                Text("Recording is not a playable artifact; save and attach are unavailable")
            }
        }
    }

    @Composable
    fun recordingActions(
        model: AndroidAppModel,
        threadId: String,
        recording: DeviceRecordingView,
        fileName: String,
        mimeType: String,
    ) {
        val context = LocalContext.current
        var savedPath by remember(recording.byteCount) { mutableStateOf<String?>(null) }
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            Button(
                onClick = {
                    val directory = context.getExternalFilesDir(Environment.DIRECTORY_MOVIES) ?: context.filesDir
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
                                    listOf(dev.remoteagent.core.LocalFile(file.path, file.name, mimeType)),
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
        savedPath?.let { Text("Saved to ${it}") }
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
        DeviceH264Surface(decoder, view, frame, frames, streamKey)
    }
}

@Composable
private fun DeviceH264Surface(
    decoder: DeviceVideoDecoder,
    view: DeviceView,
    frame: dev.remoteagent.core.DeviceVideoFrameView,
    frames: List<dev.remoteagent.core.DeviceVideoFrameView>,
    streamKey: String,
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

@Composable
private fun DeviceAccessibilityOverlay(view: DeviceView, hostId: String, deviceId: String) {
    val tree = view.accessibility.firstOrNull { it.hostId == hostId && it.deviceId == deviceId }
    if (tree == null) return
    Canvas(Modifier.fillMaxSize()) {
        tree.elements
            .filter { it.label.isNotEmpty() }
            .forEach { element ->
                drawRect(
                    color = DEVICE_ACCESSIBILITY_COLOR,
                    topLeft = Offset(size.width * element.x, size.height * element.y),
                    size = Size(size.width * element.width, size.height * element.height),
                    style = Stroke(width = DEVICE_ACCESSIBILITY_STROKE_WIDTH),
                )
            }
    }
}

// Keep the frame dimensions and session identity explicit: core owns the
// orientation/letterbox projection and must receive the exact gesture target.
@Suppress("LongParameterList")
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
                                contentWidth.toFloat(),
                                contentHeight.toFloat(),
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

private fun androidSpecialKey(keyCode: Int): String? =
    when (keyCode) {
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
        else -> null
    }

private fun deviceKeyFacts(event: AndroidKeyEvent): DeviceKeyFacts? {
    val key =
        androidSpecialKey(event.keyCode)
            ?: run {
                val unicode = event.unicodeChar
                // The Android transport accepts the same UTF-16 single-unit key
                // values as the reference stream. Supplementary characters are
                // represented by two units and must not be sent as one text key.
                if (!Character.isValidCodePoint(unicode) || Character.charCount(unicode) != 1 || unicode == 0)
                    return null
                String(Character.toChars(unicode))
            }
    return DeviceKeyFacts(
        code = AndroidKeyEvent.keyCodeToString(event.keyCode),
        key = key,
        meta = event.isMetaPressed,
        ctrl = event.isCtrlPressed,
    )
}

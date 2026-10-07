// Microphone capture and the composer's dictation controls; the phases' presentation is supplied by core.
@file:Suppress("MagicNumber", "TooGenericExceptionCaught")

package dev.remoteagent.mobile

import android.Manifest
import android.annotation.SuppressLint
import android.app.Activity
import android.content.Context
import android.content.ContextWrapper
import android.content.pm.PackageManager
import android.media.AudioFormat
import android.media.AudioRecord
import android.media.MediaRecorder
import android.net.Uri
import android.provider.Settings
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Check
import androidx.compose.material.icons.outlined.Close
import androidx.compose.material.icons.outlined.Mic
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateListOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import dev.remoteagent.core.DictationFailure
import dev.remoteagent.core.DictationPhase
import dev.remoteagent.core.DictationPresentation
import dev.remoteagent.core.DictationPreparation
import dev.remoteagent.core.Intent
import dev.remoteagent.core.dictationElapsedLabel
import dev.remoteagent.core.dictationPresentation
import dev.remoteagent.core.normalizeVoiceInputDecibels
import dev.remoteagent.core.voiceRecordingLimitSeconds
import dev.remoteagent.core.voiceWaveformSampleCount
import java.io.ByteArrayOutputStream
import kotlin.math.log10
import kotlin.math.sqrt
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

private const val SAMPLE_RATE = 24_000
private const val BYTES_PER_SECOND = SAMPLE_RATE * 2
/** One waveform sample per 80 ms of audio. */
private const val METERING_BYTES = BYTES_PER_SECOND * 80 / 1000

/** One dictation into a draft: capture on device, transcription on the Host. */
internal class Dictation(private val model: AndroidAppModel, private val scope: CoroutineScope) {
    var phase by mutableStateOf<DictationPhase>(DictationPhase.Idle)
        private set

    var elapsed by mutableIntStateOf(0)
        private set

    val levels = mutableStateListOf<Float>().apply { repeat(voiceWaveformSampleCount().toInt()) { add(0f) } }

    val presentation: DictationPresentation
        get() = dictationPresentation(phase, elapsed.toUInt())

    private var draftKey: String = ""
    private var recorder: AudioRecord? = null
    private var capture: Job? = null
    private var preparation: DictationPreparation? = null
    private var operation = 0

    fun preparing(draftKey: String) {
        operation += 1
        this.draftKey = draftKey
        elapsed = 0
        levels.indices.forEach { levels[it] = 0f }
        phase = DictationPhase.Preparing
    }

    fun fail(failure: DictationFailure) {
        release()
        phase = DictationPhase.Error(failure)
    }

    @SuppressLint("MissingPermission") // Called once RECORD_AUDIO is granted.
    fun record() {
        if (phase != DictationPhase.Preparing) return
        val token = operation
        val size = maxOf(
            AudioRecord.getMinBufferSize(SAMPLE_RATE, AudioFormat.CHANNEL_IN_MONO, AudioFormat.ENCODING_PCM_16BIT),
            METERING_BYTES,
        )
        val audio =
            try {
                AudioRecord(
                        MediaRecorder.AudioSource.VOICE_RECOGNITION,
                        SAMPLE_RATE,
                        AudioFormat.CHANNEL_IN_MONO,
                        AudioFormat.ENCODING_PCM_16BIT,
                        size * 2,
                    )
                    .also { it.startRecording() }
            } catch (_: Exception) {
                return fail(DictationFailure.CouldNotStart)
            }
        if (audio.recordingState != AudioRecord.RECORDSTATE_RECORDING) {
            audio.release()
            return fail(DictationFailure.CouldNotStart)
        }
        recorder = audio
        preparation = model.prepareDictation()
        finishing = false
        phase = DictationPhase.Recording
        val samples = ByteArrayOutputStream()
        capture = scope.launch {
            val chunk = ByteArray(METERING_BYTES)
            val limit = voiceRecordingLimitSeconds().toInt() * BYTES_PER_SECOND
            while (isActive && token == operation && !finishing && samples.size() < limit) {
                val read = withContext(Dispatchers.IO) { audio.read(chunk, 0, chunk.size) }
                if (token != operation) return@launch
                if (read < 0) return@launch fail(DictationFailure.Interrupted)
                samples.write(chunk, 0, read)
                levels.removeAt(0)
                levels.add(normalizeVoiceInputDecibels(decibels(chunk, read)).toFloat())
                elapsed = samples.size() / BYTES_PER_SECOND
            }
            if (token == operation) {
                capture = null
                transcribe(samples.toByteArray())
            }
        }
    }

    // Confirm was pressed; the capture loop sends what it has.
    private var finishing = false

    /** Confirm: the recording so far goes to the Host for transcription. */
    fun finish() {
        if (phase == DictationPhase.Recording) finishing = true
    }

    private fun transcribe(audio: ByteArray) {
        val prepared = preparation
        stopCapture()
        phase = DictationPhase.Transcribing
        val token = operation
        model.perform(Intent.Transcribe(draftKey, prepared?.id(), audio)) { result ->
            prepared?.destroy()
            if (preparation === prepared) preparation = null
            if (token != operation) return@perform
            if (result.isFailure) {
                model.notice = null
                phase = DictationPhase.Error(DictationFailure.TranscriptionFailed)
            } else phase = DictationPhase.Idle
        }
    }

    /** Cancel: nothing is sent, and an error clears. */
    fun cancel() {
        operation += 1
        release()
        phase = DictationPhase.Idle
    }

    /** The app left the foreground. */
    fun backgrounded() {
        when (phase) {
            DictationPhase.Preparing -> fail(DictationFailure.Backgrounded)
            DictationPhase.Recording -> fail(DictationFailure.Interrupted)
            else -> Unit
        }
    }

    private fun stopCapture() {
        capture?.cancel()
        capture = null
        recorder?.let {
            runCatching { it.stop() }
            it.release()
        }
        recorder = null
    }

    private fun release() {
        operation += 1
        stopCapture()
        preparation?.destroy()
        preparation = null
    }

    private fun decibels(chunk: ByteArray, length: Int): Double? {
        val count = length / 2
        if (count == 0) return null
        var sum = 0.0
        for (index in 0 until count) {
            val sample = (chunk[index * 2].toInt() and 0xff) or (chunk[index * 2 + 1].toInt() shl 8)
            val value = sample.toShort() / 32768.0
            sum += value * value
        }
        val rms = sqrt(sum / count)
        return if (rms <= 0.0) null else 20 * log10(rms)
    }
}

private tailrec fun Context.activity(): Activity? =
    when (this) {
        is Activity -> this
        is ContextWrapper -> baseContext.activity()
        else -> null
    }

/** The composer's dictation for `draftKey`; it ends when the draft changes or the app leaves the foreground. */
@Composable
internal fun rememberDictation(model: AndroidAppModel, draftKey: String): Pair<Dictation, () -> Unit> {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    val dictation = remember(model) { Dictation(model, scope) }
    val key by rememberUpdatedState(draftKey)
    val permission =
        rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) { granted ->
            if (granted) dictation.record()
            else {
                val askAgain =
                    context.activity()?.shouldShowRequestPermissionRationale(Manifest.permission.RECORD_AUDIO) ?: true
                dictation.fail(DictationFailure.MicrophoneDenied(openSettings = !askAgain))
            }
        }
    LaunchedEffect(draftKey) { if (dictation.phase != DictationPhase.Idle) dictation.cancel() }
    LaunchedEffect(model.backgrounds) { dictation.backgrounded() }
    DisposableEffect(dictation) { onDispose { dictation.cancel() } }
    val start = {
        if (dictation.presentation.opensSettings) {
            dictation.cancel()
            context.startActivity(
                android.content.Intent(
                        Settings.ACTION_APPLICATION_DETAILS_SETTINGS,
                        Uri.parse("package:${context.packageName}"),
                    )
                    .addFlags(android.content.Intent.FLAG_ACTIVITY_NEW_TASK)
            )
        } else {
            dictation.preparing(key)
            if (
                context.checkSelfPermission(Manifest.permission.RECORD_AUDIO) == PackageManager.PERMISSION_GRANTED
            )
                dictation.record()
            else permission.launch(Manifest.permission.RECORD_AUDIO)
        }
    }
    return dictation to start
}

/** The mic, or the confirm button while dictating (a spinner until the recording can finish). */
@Composable
internal fun DictationPrimaryAction(dictation: Dictation, available: Boolean, onStart: () -> Unit) {
    val colors = AppTheme.colors
    val presentation = dictation.presentation
    if (presentation.confirm) {
        Box(
            Modifier.size(44.dp)
                .clickable(enabled = presentation.confirmationEnabled) { dictation.finish() }
                .semantics {
                    contentDescription =
                        if (presentation.confirmationEnabled) "Finish dictation"
                        else presentation.status ?: "Preparing voice input"
                },
            contentAlignment = Alignment.Center,
        ) {
            Box(
                Modifier.size(30.dp)
                    .background(if (presentation.confirmationEnabled) colors.primary else colors.subtle, CircleShape),
                contentAlignment = Alignment.Center,
            ) {
                if (presentation.confirmationEnabled)
                    Icon(Icons.Outlined.Check, null, Modifier.size(16.dp), tint = colors.primaryForeground)
                else CircularProgressIndicator(Modifier.size(14.dp), color = colors.iconMuted, strokeWidth = 2.dp)
            }
        }
        return
    }
    if (!available) return
    val settings = presentation.opensSettings
    Box(
        Modifier.size(44.dp).clickable(onClick = onStart).semantics {
            contentDescription = if (settings) "Open microphone settings" else "Start dictation"
        },
        contentAlignment = Alignment.Center,
    ) {
        Icon(Icons.Outlined.Mic, null, Modifier.size(20.dp), tint = colors.icon)
    }
}

/** "Cancel dictation" at the start of the dictating toolbar. */
@Composable
internal fun DictationCancelAction(dictation: Dictation) {
    if (!dictation.presentation.cancel) return
    Box(
        Modifier.size(44.dp).clickable { dictation.cancel() }.semantics { contentDescription = "Cancel dictation" },
        contentAlignment = Alignment.Center,
    ) {
        Icon(Icons.Outlined.Close, null, Modifier.size(20.dp), tint = AppTheme.colors.icon)
    }
}

/** The waveform and time while recording, the phase otherwise, or the error with its dismiss button. */
@Composable
internal fun DictationStatus(dictation: Dictation, modifier: Modifier) {
    val colors = AppTheme.colors
    val presentation = dictation.presentation
    val status = presentation.status ?: return
    if (presentation.error) {
        Row(
            modifier.height(44.dp).padding(horizontal = 8.dp),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(6.dp),
        ) {
            Text(status, Modifier.weight(1f), style = AppTheme.footnote, color = colors.dangerForeground, maxLines = 2)
            Box(
                Modifier.size(28.dp).clickable { dictation.cancel() }.semantics {
                    contentDescription = "Dismiss voice input error"
                },
                contentAlignment = Alignment.Center,
            ) {
                Icon(Icons.Outlined.Close, null, Modifier.size(12.dp), tint = colors.iconMuted)
            }
        }
        return
    }
    if (dictation.phase == DictationPhase.Recording)
        Row(
            modifier.height(44.dp).padding(horizontal = 4.dp).semantics { contentDescription = status },
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Waveform(dictation.levels, Modifier.weight(1f))
            Text(
                dictationElapsedLabel(dictation.elapsed.toUInt()),
                style = AppTheme.caption,
                fontFamily = FontFamily.Default,
                color = colors.foregroundMuted,
            )
        }
    else
        Text(
            status,
            modifier.padding(horizontal = 8.dp),
            style = AppTheme.footnote,
            color = colors.foregroundMuted,
            textAlign = TextAlign.Center,
            maxLines = 1,
        )
}

/** The newest levels as 2-wide bars every 5 dp, as many as fit. */
@Composable
private fun Waveform(levels: List<Float>, modifier: Modifier) {
    val color = AppTheme.colors.foreground
    BoxWithConstraints(modifier.height(32.dp)) {
        val count = (maxWidth.value / 5).toInt().coerceIn(1, levels.size)
        Row(Modifier.fillMaxHeight(), horizontalArrangement = Arrangement.SpaceBetween) {
            levels.takeLast(count).forEach { level ->
                Box(Modifier.fillMaxHeight(), contentAlignment = Alignment.Center) {
                    Box(
                        Modifier.width(2.dp)
                            .height((2 + level * 30).dp)
                            .alpha(0.22f + level * 0.78f)
                            .background(color, RoundedCornerShape(1.dp))
                    )
                }
            }
        }
    }
}

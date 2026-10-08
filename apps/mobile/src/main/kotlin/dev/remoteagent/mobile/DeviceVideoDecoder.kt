// The decoder file owns ingress gates, lifecycle, and codec format parsing together;
// splitting these private operations would obscure their shared ownership.
@file:Suppress("TooManyFunctions")

package dev.remoteagent.mobile

import android.content.Context
import android.graphics.Canvas
import android.graphics.Paint
import android.graphics.RectF
import android.graphics.SurfaceTexture
import android.media.MediaCodec
import android.media.MediaFormat
import android.os.Handler
import android.os.HandlerThread
import android.view.Surface
import android.view.TextureView
import android.view.View
import java.io.ByteArrayOutputStream
import java.nio.ByteBuffer
import java.util.ArrayDeque

private const val DEVICE_VIDEO_MAX_PENDING_FRAMES = 8
private const val DEVICE_VIDEO_MAX_INGRESS_BYTES = 16 * 1024 * 1024
// This is the native accessibility overlay's fixed accent, shared with the Compose surface.
@Suppress("MagicNumber") private val DEVICE_ACCESSIBILITY_COLOR = android.graphics.Color.rgb(79, 140, 255)
private const val DEVICE_ACCESSIBILITY_STROKE_WIDTH = 2f
private const val AVC_LENGTH_PREFIX_BYTES = 4
private const val AVC_PARAMETER_SET_LENGTH_BYTES = 2
private const val AVC_CONFIGURATION_MIN_BYTES = 7
private const val AVC_CONFIGURATION_VERSION = 1
private const val AVC_SPS_COUNT_OFFSET = 5
private const val AVC_NAL_TYPE_MASK = 0x1f
private const val AVC_SPS_NAL_TYPE = 7
private const val AVC_PPS_NAL_TYPE = 8
private const val BYTE_MASK = 0xff
private const val BITS_PER_BYTE = 8
private const val U32_LAST_BYTE_OFFSET = 3
private const val U16_HIGH_BYTE_SHIFT = BITS_PER_BYTE
private const val U32_FIRST_BYTE_SHIFT = BITS_PER_BYTE * U32_LAST_BYTE_OFFSET
private const val U32_SECOND_BYTE_SHIFT = BITS_PER_BYTE * 2
private const val U32_THIRD_BYTE_SHIFT = BITS_PER_BYTE
private const val ANNEX_B_SHORT_START_CODE_BYTES = 3
private const val ANNEX_B_LONG_START_CODE_BYTES = 4
private const val ANNEX_B_START_CODE_ONE_BYTE = 1
private const val NAL_LENGTH_BUFFER_SLACK = 16

/** Bounds UI-to-worker handoff before payload copies and Handler posts occur. */
internal class DeviceVideoIngressGate(
    private val maxFrames: Int = DEVICE_VIDEO_MAX_PENDING_FRAMES,
    private val maxBytes: Int = DEVICE_VIDEO_MAX_INGRESS_BYTES,
) {
    data class Admission(val generation: Long, val bytes: Int)

    data class Completion(val current: Boolean, val resync: Boolean)

    private var streamKey: String? = null
    private var generation = 0L
    private var lastSequence: ULong? = null
    private var inFlightFrames = 0
    private var inFlightBytes = 0
    private var overflowed = false
    private var needsKeyframe = true
    private var closed = false

    @Synchronized
    fun reset(streamKey: String) {
        if (this.streamKey == streamKey) return
        this.streamKey = streamKey
        generation += 1
        lastSequence = null
        overflowed = false
        needsKeyframe = true
    }

    // Admission intentionally keeps every rejection guard beside its counter update so
    // overflow, stale sequence, and keyframe recovery remain one atomic policy.
    @Synchronized
    @Suppress("CyclomaticComplexMethod", "ReturnCount")
    fun offer(sequence: ULong, encoding: String, keyframe: Boolean, bytes: Int): Admission? {
        if (closed || bytes < 0 || bytes > maxBytes) return null
        if (encoding != "h264" && encoding != "semu" && encoding != "avcc-description") return null
        if (lastSequence?.let { sequence <= it } == true) return null
        if (overflowed) return null
        val actualKeyframe = keyframe && encoding != "avcc-description"
        if (needsKeyframe && !actualKeyframe && encoding != "avcc-description") return null
        if (inFlightFrames >= maxFrames || inFlightBytes > maxBytes - bytes) {
            overflowed = true
            needsKeyframe = true
            return null
        }
        lastSequence = sequence
        if (actualKeyframe) needsKeyframe = false
        inFlightFrames += 1
        inFlightBytes += bytes
        return Admission(generation, bytes)
    }

    @Synchronized
    fun complete(admission: Admission): Completion {
        inFlightFrames = (inFlightFrames - 1).coerceAtLeast(0)
        inFlightBytes = (inFlightBytes - admission.bytes).coerceAtLeast(0)
        val current = admission.generation == generation && !closed
        val resync = current && overflowed
        if (resync) overflowed = false
        return Completion(current, resync)
    }

    @Synchronized fun isCurrent(admission: Admission): Boolean = admission.generation == generation && !closed

    @Synchronized
    fun forceResync() {
        needsKeyframe = true
    }

    @Synchronized
    fun close() {
        closed = true
        generation += 1
        inFlightFrames = 0
        inFlightBytes = 0
        overflowed = false
    }
}

internal data class DeviceResetRequest(val streamKey: String, val width: Int, val height: Int)

/** Coalesces recomposition-driven decoder resets before they reach the worker queue. */
internal class DeviceVideoResetGate {
    private var pending: DeviceResetRequest? = null
    private var applied: DeviceResetRequest? = null
    private var scheduled = false

    // A request may replace pending work while a callback is already scheduled; the
    // three-state transition is kept explicit for the exact callback ownership.
    @Synchronized
    fun request(request: DeviceResetRequest): Boolean =
        when {
            pending == request || (applied == request && !scheduled) -> false
            else -> {
                pending = request
                if (scheduled) {
                    false
                } else {
                    scheduled = true
                    true
                }
            }
        }

    @Synchronized fun next(): DeviceResetRequest? = pending

    @Synchronized
    fun finish(request: DeviceResetRequest): Boolean {
        applied = request
        if (pending == request) {
            pending = null
            scheduled = false
            return false
        }
        return true
    }

    @Synchronized
    fun cancel() {
        pending = null
        applied = null
        scheduled = false
    }
}

/** Owns the one delayed output command so release can remove the exact queued callback. */
internal class DeviceVideoPumpGate {
    private var pending: Runnable? = null

    @Synchronized
    fun admit(command: Runnable): Boolean {
        if (pending != null) return false
        pending = command
        return true
    }

    @Synchronized
    fun begin(command: Runnable): Boolean {
        if (pending !== command) return false
        pending = null
        return true
    }

    @Synchronized
    fun cancel(): Runnable? {
        val command = pending
        pending = null
        return command
    }
}

/** A bounded, stateful H.264 decoder for the Host's live device transport. */
// This class is the single lifecycle owner for the TextureView, MediaCodec, pump, and queue.
@Suppress("TooManyFunctions")
internal class DeviceVideoDecoder : TextureView.SurfaceTextureListener {
    private data class Frame(val payload: ByteArray, val encoding: String, val timestampUs: Long, val keyframe: Boolean)

    private val worker = HandlerThread("device-video-decoder").apply { start() }
    private val handler = Handler(worker.looper)
    private var textureView: TextureView? = null
    private var surfaceTexture: SurfaceTexture? = null
    private var surface: Surface? = null
    private var codec: MediaCodec? = null
    private var streamKey: String? = null
    private var width = 0
    private var height = 0
    private var codecDescription: ByteArray? = null
    private var needsKeyframe = true
    private var lastSequence: ULong? = null
    @Volatile private var closed = false
    private val pending = ArrayDeque<Frame>()
    private val ingress = DeviceVideoIngressGate()
    private val resetGate = DeviceVideoResetGate()
    private val outputPumpGate = DeviceVideoPumpGate()

    fun attach(textureView: TextureView) {
        if (this.textureView === textureView) return
        this.textureView?.surfaceTextureListener = null
        this.textureView = textureView
        textureView.surfaceTextureListener = this
        if (textureView.isAvailable)
            textureView.surfaceTexture?.let { onSurfaceTextureAvailable(it, textureView.width, textureView.height) }
    }

    fun reset(streamKey: String, width: Int, height: Int) {
        if (closed) return
        val request =
            DeviceResetRequest(streamKey = streamKey, width = width.coerceAtLeast(1), height = height.coerceAtLeast(1))
        // Dimensions are part of the ingress identity so a resize invalidates queued frames
        // even when the Host keeps the same stream key.
        ingress.reset("${request.streamKey}\u0000${request.width}x${request.height}")
        if (!resetGate.request(request)) return
        if (
            !handler.post {
                while (!closed) {
                    val pendingReset = resetGate.next() ?: return@post
                    closeCodec()
                    this.streamKey = pendingReset.streamKey
                    this.width = pendingReset.width
                    this.height = pendingReset.height
                    codecDescription = null
                    needsKeyframe = true
                    ingress.forceResync()
                    lastSequence = null
                    pending.clear()
                    if (!resetGate.finish(pendingReset)) return@post
                }
            }
        ) {
            resetGate.cancel()
        }
    }

    fun submit(payload: ByteArray, encoding: String, sequence: ULong, timestampUs: ULong?, keyframe: Boolean) {
        if (closed) return
        val admission = ingress.offer(sequence, encoding, keyframe, payload.size) ?: return
        val copy = payload.copyOf()
        if (
            !handler.post {
                if (ingress.isCurrent(admission)) submitOnWorker(copy, encoding, sequence, timestampUs, keyframe)
                val completion = ingress.complete(admission)
                if (completion.resync) requestKeyframeResync()
            }
        ) {
            ingress.complete(admission)
        }
    }

    private fun submitOnWorker(
        payload: ByteArray,
        encoding: String,
        sequence: ULong,
        timestampUs: ULong?,
        keyframe: Boolean,
    ) {
        if (closed) return
        if (lastSequence?.let { sequence <= it } == true) return
        if (lastSequence?.let { sequence - it > 1uL } == true) requestKeyframeResync()
        lastSequence = sequence
        when (encoding) {
            "avcc-description" -> {
                if (codecDescription?.contentEquals(payload) != true) {
                    codecDescription = payload.copyOf()
                    requestKeyframeResync()
                }
            }

            "h264",
            "semu" -> {
                pending.addLast(
                    Frame(
                        payload = payload,
                        encoding = encoding,
                        timestampUs = timestampUs?.toLong() ?: sequence.toLong(),
                        keyframe = keyframe,
                    )
                )
                while (pending.size > DEVICE_VIDEO_MAX_PENDING_FRAMES) {
                    pending.removeFirst()
                    requestKeyframeResync()
                }
                drainPending()
            }

            else -> Unit
        }
    }

    private fun requestKeyframeResync() {
        ingress.forceResync()
        closeCodec()
        pending.clear()
        needsKeyframe = true
    }

    override fun onSurfaceTextureAvailable(texture: SurfaceTexture, width: Int, height: Int) {
        if (closed) return
        handler.post {
            if (this.surfaceTexture === texture && this.surface != null) return@post
            if (this.surfaceTexture != null || this.surface != null) {
                releaseSurfaceOnWorker()
            } else {
                closeCodec()
            }
            this.surfaceTexture = texture
            this.surface = Surface(texture)
            if (this.width <= 0) this.width = width.coerceAtLeast(1)
            if (this.height <= 0) this.height = height.coerceAtLeast(1)
            configureCodecIfPossible()
            drainPending()
        }
    }

    override fun onSurfaceTextureSizeChanged(surface: SurfaceTexture, width: Int, height: Int) = Unit

    override fun onSurfaceTextureDestroyed(surface: SurfaceTexture): Boolean {
        if (!closed) handler.post { releaseSurfaceOnWorker() }
        return true
    }

    override fun onSurfaceTextureUpdated(surface: SurfaceTexture) = Unit

    fun close() {
        if (closed) return
        closed = true
        ingress.close()
        resetGate.cancel()
        cancelOutputPump()
        textureView?.surfaceTextureListener = null
        textureView = null
        handler.post {
            releaseSurfaceOnWorker()
            pending.clear()
            codecDescription = null
            streamKey = null
            lastSequence = null
        }
        worker.quitSafely()
    }

    private fun configureCodecIfPossible() {
        if (codec != null || width <= 0 || height <= 0) return
        val outputSurface = surface?.takeIf { it.isValid } ?: return
        runCatching {
                val format = MediaFormat.createVideoFormat(MediaFormat.MIMETYPE_VIDEO_AVC, width, height)
                format.setInteger(MediaFormat.KEY_MAX_INPUT_SIZE, MAX_INPUT_SIZE)
                codecDescription?.let { description ->
                    val (sps, pps) = splitCodecDescription(description)
                    sps?.let { format.setByteBuffer("csd-0", ByteBuffer.wrap(it)) }
                    pps?.let { format.setByteBuffer("csd-1", ByteBuffer.wrap(it)) }
                }
                val decoder = MediaCodec.createDecoderByType(MediaFormat.MIMETYPE_VIDEO_AVC)
                runCatching {
                        decoder.configure(format, outputSurface, null, 0)
                        decoder.start()
                    }
                    .onFailure {
                        runCatching { decoder.stop() }
                        runCatching { decoder.release() }
                    }
                    .getOrThrow()
                codec = decoder
            }
            .onFailure { closeCodec() }
    }

    // The queue state machine deliberately keeps codec backpressure, stale
    // frame drops, and resync exits adjacent so each ownership transition is
    // visible before the next frame is admitted.
    @Suppress("CyclomaticComplexMethod", "ReturnCount", "LoopWithTooManyJumpStatements")
    private fun drainPending() {
        configureCodecIfPossible()
        val decoder = codec ?: return
        while (pending.isNotEmpty()) {
            val frame = pending.first()
            if (frame.encoding == "avcc-description") {
                pending.removeFirst()
                continue
            }
            if (needsKeyframe && !frame.keyframe) {
                pending.removeFirst()
                continue
            }
            val index =
                runCatching { decoder.dequeueInputBuffer(0L) }
                    .getOrElse {
                        closeCodec()
                        return
                    }
            if (index < 0) break
            val input = decoder.getInputBuffer(index)
            val payload = normalizeH264Payload(frame.payload)
            if (input == null || payload.isEmpty()) {
                runCatching { decoder.queueInputBuffer(index, 0, 0, frame.timestampUs, 0) }.onFailure { closeCodec() }
                pending.removeFirst()
                if (codec == null) return
                continue
            }
            if (payload.size > input.capacity()) {
                runCatching { decoder.queueInputBuffer(index, 0, 0, frame.timestampUs, 0) }.onFailure { closeCodec() }
                pending.removeFirst()
                if (codec == null) return
                requestKeyframeResync()
                continue
            }
            input.clear()
            input.put(payload)
            var queued = false
            runCatching {
                    decoder.queueInputBuffer(index, 0, payload.size, frame.timestampUs, 0)
                    queued = true
                }
                .onFailure { closeCodec() }
            pending.removeFirst()
            if (!queued || codec == null) return
            if (frame.keyframe) needsKeyframe = false
            drainOutput(decoder)
            if (codec == null) return
        }
        drainOutput(decoder)
        scheduleOutputPump()
    }

    private fun scheduleOutputPump() {
        if (closed || codec == null) return
        val pump =
            object : Runnable {
                override fun run() {
                    if (outputPumpGate.begin(this) && !closed) {
                        codec?.let {
                            drainOutput(it)
                            drainPending()
                        }
                    }
                }
            }
        if (!outputPumpGate.admit(pump)) return
        if (closed || !handler.postDelayed(pump, OUTPUT_PUMP_INTERVAL_MS)) {
            outputPumpGate.cancel()?.let { handler.removeCallbacks(it) }
            handler.removeCallbacks(pump)
        }
    }

    private fun drainOutput(decoder: MediaCodec) {
        val info = MediaCodec.BufferInfo()
        var draining = true
        while (draining) {
            val result = runCatching { decoder.dequeueOutputBuffer(info, 0L) }
            if (result.isFailure) {
                closeCodec()
                draining = false
                continue
            }
            when (val index = result.getOrThrow()) {
                MediaCodec.INFO_TRY_AGAIN_LATER -> draining = false
                MediaCodec.INFO_OUTPUT_FORMAT_CHANGED -> Unit
                else -> {
                    if (index >= 0 && runCatching { decoder.releaseOutputBuffer(index, true) }.isFailure) {
                        closeCodec()
                        draining = false
                    }
                }
            }
        }
    }

    private fun closeCodec() {
        cancelOutputPump()
        val decoder = codec ?: return
        codec = null
        runCatching { decoder.stop() }
        runCatching { decoder.release() }
        needsKeyframe = true
    }

    private fun releaseSurfaceOnWorker() {
        closeCodec()
        pending.clear()
        lastSequence = null
        surface?.release()
        surface = null
        surfaceTexture = null
    }

    private fun cancelOutputPump() {
        outputPumpGate.cancel()?.let { handler.removeCallbacks(it) }
    }

    private companion object {
        const val MAX_INPUT_SIZE = 8 * 1024 * 1024
        const val OUTPUT_PUMP_INTERVAL_MS = 16L
    }
}

/** Native sibling overlay so accessibility bounds stay above a TextureView. */
internal class DeviceAccessibilityOverlayView(context: Context) : View(context) {
    private val paint =
        Paint(Paint.ANTI_ALIAS_FLAG).apply {
            color = DEVICE_ACCESSIBILITY_COLOR
            style = Paint.Style.STROKE
            strokeWidth = DEVICE_ACCESSIBILITY_STROKE_WIDTH
        }

    var rects: List<RectF> = emptyList()
        set(value) {
            field = value
            invalidate()
        }

    override fun onDraw(canvas: Canvas) {
        super.onDraw(canvas)
        rects.forEach { rect ->
            canvas.drawRect(
                rect.left * width,
                rect.top * height,
                (rect.left + rect.width()) * width,
                (rect.top + rect.height()) * height,
                paint,
            )
        }
    }
}

/** Convert length-prefixed AVC access units to the Annex-B form MediaCodec accepts. */
internal fun normalizeH264Payload(payload: ByteArray): ByteArray =
    when {
        payload.startsWithAnnexB() -> payload
        payload.size < AVC_LENGTH_PREFIX_BYTES -> payload
        else -> normalizeLengthPrefixedPayload(payload)
    }

private fun normalizeLengthPrefixedPayload(payload: ByteArray): ByteArray {
    val converted = ByteArrayOutputStream(payload.size + NAL_LENGTH_BUFFER_SLACK)
    var offset = 0
    while (offset + AVC_LENGTH_PREFIX_BYTES <= payload.size) {
        val length = readUInt32(payload, offset)
        offset += AVC_LENGTH_PREFIX_BYTES
        if (length <= 0 || offset + length > payload.size) return payload
        converted.write(ANNEX_B_START_CODE)
        converted.write(payload, offset, length)
        offset += length
    }
    return if (offset == payload.size && converted.size() > 0) converted.toByteArray() else payload
}

/** Return the SPS and PPS buffers accepted by MediaFormat's AVC CSD fields. */
internal fun splitCodecDescription(payload: ByteArray): Pair<ByteArray?, ByteArray?> {
    if (isAvcConfiguration(payload)) {
        return parseAvcConfiguration(payload) ?: (null to null)
    }
    val nalUnits = annexBNalUnits(payload)
    return nalUnits.firstOrNull { it.isNotEmpty() && (it[0].toInt() and AVC_NAL_TYPE_MASK) == AVC_SPS_NAL_TYPE } to
        nalUnits.firstOrNull { it.isNotEmpty() && (it[0].toInt() and AVC_NAL_TYPE_MASK) == AVC_PPS_NAL_TYPE }
}

private data class AvcParameterSetRead(val first: ByteArray?, val nextOffset: Int)

private fun isAvcConfiguration(payload: ByteArray): Boolean =
    payload.size >= AVC_CONFIGURATION_MIN_BYTES && payload[0].toInt() == AVC_CONFIGURATION_VERSION

private fun parseAvcConfiguration(payload: ByteArray): Pair<ByteArray?, ByteArray?>? {
    val sps =
        readAvcParameterSets(
            payload,
            AVC_SPS_COUNT_OFFSET + 1,
            payload[AVC_SPS_COUNT_OFFSET].toInt() and AVC_NAL_TYPE_MASK,
        )
    return sps?.let { value ->
        if (value.nextOffset >= payload.size) {
            value.first?.withAnnexBPrefix() to null
        } else {
            val ppsOffset = value.nextOffset
            readAvcParameterSets(payload, ppsOffset + 1, payload[ppsOffset].toInt() and BYTE_MASK)?.let { pps ->
                value.first?.withAnnexBPrefix() to pps.first?.withAnnexBPrefix()
            }
        }
    }
}

private fun readAvcParameterSets(payload: ByteArray, startOffset: Int, count: Int): AvcParameterSetRead? {
    var offset = startOffset
    var first: ByteArray? = null
    var valid = true
    repeat(count) {
        if (valid) {
            if (offset + AVC_PARAMETER_SET_LENGTH_BYTES > payload.size) {
                valid = false
            } else {
                val length = readUInt16(payload, offset)
                offset += AVC_PARAMETER_SET_LENGTH_BYTES
                if (length <= 0 || offset + length > payload.size) {
                    valid = false
                } else {
                    if (first == null) first = payload.copyOfRange(offset, offset + length)
                    offset += length
                }
            }
        }
    }
    return if (valid) AvcParameterSetRead(first, offset) else null
}

private fun readUInt16(payload: ByteArray, offset: Int): Int =
    ((payload[offset].toInt() and BYTE_MASK) shl U16_HIGH_BYTE_SHIFT) or (payload[offset + 1].toInt() and BYTE_MASK)

private fun readUInt32(payload: ByteArray, offset: Int): Int =
    ((payload[offset].toInt() and BYTE_MASK) shl U32_FIRST_BYTE_SHIFT) or
        ((payload[offset + 1].toInt() and BYTE_MASK) shl U32_SECOND_BYTE_SHIFT) or
        ((payload[offset + 2].toInt() and BYTE_MASK) shl U32_THIRD_BYTE_SHIFT) or
        (payload[offset + U32_LAST_BYTE_OFFSET].toInt() and BYTE_MASK)

private fun annexBNalUnits(payload: ByteArray): List<ByteArray> {
    val starts = mutableListOf<Pair<Int, Int>>()
    var index = 0
    while (index + ANNEX_B_SHORT_START_CODE_BYTES - 1 < payload.size) {
        val length =
            when {
                payload[index] == 0.toByte() &&
                    payload[index + 1] == 0.toByte() &&
                    payload[index + 2] == ANNEX_B_START_CODE_ONE_BYTE.toByte() -> ANNEX_B_SHORT_START_CODE_BYTES
                index + ANNEX_B_LONG_START_CODE_BYTES <= payload.size &&
                    payload[index] == 0.toByte() &&
                    payload[index + 1] == 0.toByte() &&
                    payload[index + 2] == 0.toByte() &&
                    payload[index + ANNEX_B_LONG_START_CODE_BYTES - ANNEX_B_START_CODE_ONE_BYTE] ==
                        ANNEX_B_START_CODE_ONE_BYTE.toByte() -> ANNEX_B_LONG_START_CODE_BYTES
                else -> 0
            }
        if (length > 0) {
            starts += index to (index + length)
            index += length
        } else {
            index++
        }
    }
    return starts.mapIndexedNotNull { position, (_, start) ->
        val end = starts.getOrNull(position + 1)?.first ?: payload.size
        if (start < end) payload.copyOfRange(start, end) else null
    }
}

private fun ByteArray.withAnnexBPrefix(): ByteArray = ANNEX_B_START_CODE + this

private fun ByteArray.startsWithAnnexB(): Boolean =
    size >= ANNEX_B_SHORT_START_CODE_BYTES &&
        ((this[0] == 0.toByte() && this[1] == 0.toByte() && this[2] == ANNEX_B_START_CODE_ONE_BYTE.toByte()) ||
            (size >= ANNEX_B_LONG_START_CODE_BYTES &&
                this[0] == 0.toByte() &&
                this[1] == 0.toByte() &&
                this[2] == 0.toByte() &&
                this[ANNEX_B_LONG_START_CODE_BYTES - ANNEX_B_START_CODE_ONE_BYTE] ==
                    ANNEX_B_START_CODE_ONE_BYTE.toByte()))

private val ANNEX_B_START_CODE = byteArrayOf(0, 0, 0, 1)

package dev.remoteagent.mobile

import android.media.MediaCodec
import android.media.MediaFormat
import android.graphics.SurfaceTexture
import android.view.Surface
import android.view.TextureView
import java.io.ByteArrayOutputStream
import java.nio.ByteBuffer
import java.util.ArrayDeque

/** A bounded, stateful H.264 decoder for the Host's live device transport. */
internal class DeviceVideoDecoder : TextureView.SurfaceTextureListener {
    private data class Frame(
        val payload: ByteArray,
        val encoding: String,
        val width: Int,
        val height: Int,
        val timestampUs: Long,
        val keyframe: Boolean,
    )

    private var textureView: TextureView? = null
    private var surface: Surface? = null
    private var codec: MediaCodec? = null
    private var streamKey: String? = null
    private var width = 0
    private var height = 0
    private var codecDescription: ByteArray? = null
    private var needsKeyframe = true
    private val pending = ArrayDeque<Frame>()

    fun attach(textureView: TextureView) {
        if (this.textureView === textureView) return
        this.textureView?.surfaceTextureListener = null
        this.textureView = textureView
        textureView.surfaceTextureListener = this
        if (textureView.isAvailable) textureView.surfaceTexture?.let { onSurfaceTextureAvailable(it, textureView.width, textureView.height) }
    }

    fun reset(streamKey: String, width: Int, height: Int) {
        if (this.streamKey == streamKey && this.width == width && this.height == height) return
        closeCodec()
        this.streamKey = streamKey
        this.width = width.coerceAtLeast(1)
        this.height = height.coerceAtLeast(1)
        codecDescription = null
        needsKeyframe = true
        pending.clear()
    }

    fun submit(
        payload: ByteArray,
        encoding: String,
        width: Int,
        height: Int,
        sequence: ULong,
        timestampUs: ULong?,
        keyframe: Boolean,
    ) {
        if (encoding == "avcc-description") {
            codecDescription = payload.copyOf()
            if (codec != null) closeCodec()
        }
        if (encoding != "h264" && encoding != "semu" && encoding != "avcc-description") return
        pending.addLast(
            Frame(
                payload = payload.copyOf(),
                encoding = encoding,
                width = width,
                height = height,
                timestampUs = timestampUs?.toLong() ?: sequence.toLong(),
                keyframe = keyframe,
            ),
        )
        while (pending.size > MAX_PENDING_FRAMES) pending.removeFirst()
        drainPending()
    }

    override fun onSurfaceTextureAvailable(surface: SurfaceTexture, width: Int, height: Int) {
        this.surface = Surface(surface)
        configureCodecIfPossible()
        drainPending()
    }

    override fun onSurfaceTextureSizeChanged(surface: SurfaceTexture, width: Int, height: Int) = Unit

    override fun onSurfaceTextureDestroyed(surface: SurfaceTexture): Boolean {
        closeCodec()
        this.surface?.release()
        this.surface = null
        return true
    }

    override fun onSurfaceTextureUpdated(surface: SurfaceTexture) = Unit

    fun close() {
        textureView?.surfaceTextureListener = null
        textureView = null
        surface?.release()
        surface = null
        closeCodec()
        pending.clear()
        codecDescription = null
        streamKey = null
    }

    private fun configureCodecIfPossible() {
        if (codec != null || width <= 0 || height <= 0) return
        val outputSurface = surface ?: return
        if (!outputSurface.isValid) return
        runCatching {
            val format = MediaFormat.createVideoFormat(MediaFormat.MIMETYPE_VIDEO_AVC, width, height)
            format.setInteger(MediaFormat.KEY_MAX_INPUT_SIZE, MAX_INPUT_SIZE)
            codecDescription?.let { description ->
                val (sps, pps) = splitCodecDescription(description)
                sps?.let { format.setByteBuffer("csd-0", ByteBuffer.wrap(it)) }
                pps?.let { format.setByteBuffer("csd-1", ByteBuffer.wrap(it)) }
            }
            MediaCodec.createDecoderByType(MediaFormat.MIMETYPE_VIDEO_AVC).also { decoder ->
                decoder.configure(format, outputSurface, null, 0)
                decoder.start()
                codec = decoder
            }
        }.onFailure {
            closeCodec()
        }
    }

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
            val index = runCatching { decoder.dequeueInputBuffer(0L) }.getOrElse {
                closeCodec()
                return
            }
            if (index < 0) break
            val input = decoder.getInputBuffer(index)
            if (input == null || frame.payload.isEmpty()) {
                pending.removeFirst()
                continue
            }
            val payload = normalizeH264Payload(frame.payload)
            if (payload.size > input.capacity()) {
                pending.removeFirst()
                continue
            }
            input.clear()
            input.put(payload)
            val flags = if (frame.encoding == "avcc-description") {
                MediaCodec.BUFFER_FLAG_CODEC_CONFIG
            } else {
                0
            }
            runCatching {
                decoder.queueInputBuffer(index, 0, payload.size, frame.timestampUs, flags)
            }.onFailure {
                closeCodec()
            }
            pending.removeFirst()
            if (frame.keyframe) needsKeyframe = false
            drainOutput(decoder)
            if (codec == null) return
        }
        drainOutput(decoder)
    }

    private fun drainOutput(decoder: MediaCodec) {
        val info = MediaCodec.BufferInfo()
        while (true) {
            val index = runCatching { decoder.dequeueOutputBuffer(info, 0L) }.getOrElse {
                closeCodec()
                return
            }
            when {
                index == MediaCodec.INFO_TRY_AGAIN_LATER -> return
                index == MediaCodec.INFO_OUTPUT_FORMAT_CHANGED -> Unit
                index >= 0 -> runCatching { decoder.releaseOutputBuffer(index, true) }
                    .onFailure { closeCodec(); return }
            }
        }
    }

    private fun closeCodec() {
        val decoder = codec ?: return
        codec = null
        runCatching { decoder.stop() }
        runCatching { decoder.release() }
        needsKeyframe = true
    }

    private companion object {
        const val MAX_PENDING_FRAMES = 8
        const val MAX_INPUT_SIZE = 8 * 1024 * 1024
    }
}

/** Convert length-prefixed AVC access units to the Annex-B form MediaCodec accepts. */
internal fun normalizeH264Payload(payload: ByteArray): ByteArray {
    if (payload.startsWithAnnexB()) return payload
    if (payload.size < 4) return payload
    val converted = ByteArrayOutputStream(payload.size + 16)
    var offset = 0
    while (offset + 4 <= payload.size) {
        val length =
            ((payload[offset].toInt() and 0xff) shl 24) or
                ((payload[offset + 1].toInt() and 0xff) shl 16) or
                ((payload[offset + 2].toInt() and 0xff) shl 8) or
                (payload[offset + 3].toInt() and 0xff)
        offset += 4
        if (length <= 0 || offset + length > payload.size) return payload
        converted.write(ANNEX_B_START_CODE)
        converted.write(payload, offset, length)
        offset += length
    }
    return if (offset == payload.size && converted.size() > 0) converted.toByteArray() else payload
}

/** Return the SPS and PPS buffers accepted by MediaFormat's AVC CSD fields. */
internal fun splitCodecDescription(payload: ByteArray): Pair<ByteArray?, ByteArray?> {
    if (payload.size >= 7 && payload[0].toInt() == 1) {
        var offset = 5
        val spsCount = payload[5].toInt() and 0x1f
        offset++
        var sps: ByteArray? = null
        var valid = true
        repeat(spsCount) {
            if (!valid) return@repeat
            if (offset + 2 > payload.size) {
                valid = false
                return@repeat
            }
            val length = ((payload[offset].toInt() and 0xff) shl 8) or (payload[offset + 1].toInt() and 0xff)
            offset += 2
            if (length <= 0 || offset + length > payload.size) {
                valid = false
                return@repeat
            }
            if (sps == null) sps = payload.copyOfRange(offset, offset + length)
            offset += length
        }
        if (!valid) return null to null
        if (offset >= payload.size) return sps to null
        val ppsCount = payload[offset].toInt() and 0xff
        offset++
        var pps: ByteArray? = null
        repeat(ppsCount) {
            if (!valid) return@repeat
            if (offset + 2 > payload.size) {
                valid = false
                return@repeat
            }
            val length = ((payload[offset].toInt() and 0xff) shl 8) or (payload[offset + 1].toInt() and 0xff)
            offset += 2
            if (length <= 0 || offset + length > payload.size) {
                valid = false
                return@repeat
            }
            if (pps == null) pps = payload.copyOfRange(offset, offset + length)
            offset += length
        }
        if (!valid) return null to null
        return sps to pps
    }
    val nalUnits = annexBNalUnits(payload)
    return nalUnits.firstOrNull { it.isNotEmpty() && (it[0].toInt() and 0x1f) == 7 } to
        nalUnits.firstOrNull { it.isNotEmpty() && (it[0].toInt() and 0x1f) == 8 }
}

private fun annexBNalUnits(payload: ByteArray): List<ByteArray> {
    val starts = mutableListOf<Int>()
    var index = 0
    while (index + 3 < payload.size) {
        val length = when {
            payload[index] == 0.toByte() && payload[index + 1] == 0.toByte() && payload[index + 2] == 1.toByte() -> 3
            index + 4 <= payload.size && payload[index] == 0.toByte() && payload[index + 1] == 0.toByte() && payload[index + 2] == 0.toByte() && payload[index + 3] == 1.toByte() -> 4
            else -> 0
        }
        if (length > 0) {
            starts += index + length
            index += length
        } else {
            index++
        }
    }
    return starts.mapIndexedNotNull { position, start ->
        val end = starts.getOrNull(position + 1) ?: payload.size
        if (start < end) payload.copyOfRange(start, end) else null
    }
}

private fun ByteArray.startsWithAnnexB(): Boolean =
    size >= 3 && ((this[0] == 0.toByte() && this[1] == 0.toByte() && this[2] == 1.toByte()) ||
        (size >= 4 && this[0] == 0.toByte() && this[1] == 0.toByte() && this[2] == 0.toByte() && this[3] == 1.toByte()))

private val ANNEX_B_START_CODE = byteArrayOf(0, 0, 0, 1)

internal data class DeviceRecordingArtifact(val extension: String, val mimeType: String)

internal fun recordingArtifact(format: String): DeviceRecordingArtifact = when (format.lowercase()) {
    "mjpeg" -> DeviceRecordingArtifact("mjpeg", "multipart/x-mixed-replace; boundary=remote-agent-device")
    "avcc" -> DeviceRecordingArtifact("avcc", "video/avc")
    else -> DeviceRecordingArtifact("bin", "application/octet-stream")
}

package dev.remoteagent.mobile

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Test

class DeviceVideoDecoderTest {
    @Test
    fun lengthPrefixedAccessUnitsBecomeAnnexB() {
        val payload = byteArrayOf(0, 0, 0, 2, 0x65, 0x01, 0, 0, 0, 1, 0x41)
        assertArrayEquals(
            byteArrayOf(0, 0, 0, 1, 0x65, 0x01, 0, 0, 0, 1, 0x41),
            normalizeH264Payload(payload),
        )
    }

    @Test
    fun codecDescriptionSplitsAnnexBSpsAndPps() {
        val (sps, pps) = splitCodecDescription(
            byteArrayOf(0, 0, 0, 1, 0x67, 0x64, 0, 0, 0, 1, 0x68, 0xEE.toByte()),
        )
        assertArrayEquals(byteArrayOf(0, 0, 0, 1, 0x67, 0x64), sps)
        assertArrayEquals(byteArrayOf(0, 0, 0, 1, 0x68, 0xEE.toByte()), pps)
    }

    @Test
    fun codecDescriptionSplitsAvccSpsAndPpsWithAnnexBPrefixes() {
        val (sps, pps) = splitCodecDescription(
            byteArrayOf(
                1, 0x64, 0, 0x1f, 0xff.toByte(), 0xe1.toByte(), 0, 2, 0x67, 0x64,
                1, 0, 2, 0x68, 0xee.toByte(),
            ),
        )
        assertArrayEquals(byteArrayOf(0, 0, 0, 1, 0x67, 0x64), sps)
        assertArrayEquals(byteArrayOf(0, 0, 0, 1, 0x68, 0xee.toByte()), pps)
    }

    @Test
    fun recordingAttachmentKeepsTheTransportFormat() {
        assertEquals(
            DeviceRecordingArtifact("avcc", "video/avc"),
            recordingArtifact("avcc"),
        )
        assertEquals(
            DeviceRecordingArtifact("mjpeg", "multipart/x-mixed-replace; boundary=remote-agent-device"),
            recordingArtifact("mjpeg"),
        )
    }
}

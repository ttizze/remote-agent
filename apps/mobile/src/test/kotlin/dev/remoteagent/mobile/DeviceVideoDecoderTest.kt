package dev.remoteagent.mobile

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Test

class DeviceVideoDecoderTest {
    @Test
    fun pumpGateRemovesOnlyTheOwnedQueuedWorkerCommand() {
        val gate = DeviceVideoPumpGate()
        val first = Runnable {}
        val second = Runnable {}

        assertTrue(gate.admit(first))
        assertTrue(!gate.admit(second))
        assertSame(first, gate.cancel())
        assertTrue(!gate.begin(first))

        assertTrue(gate.admit(second))
        assertTrue(gate.begin(second))
        assertTrue(!gate.begin(second))
    }

    @Test
    fun resetGateBoundsWorkerCommandsWhileLatestDimensionsWin() {
        val gate = DeviceVideoResetGate()
        val first = DeviceResetRequest("host/device/0", 1080, 1920)
        val resized = DeviceResetRequest("host/device/0", 1920, 1080)
        val fakeWorker = mutableListOf<() -> Unit>()
        val applied = mutableListOf<DeviceResetRequest>()
        var posted = 0

        fun request(value: DeviceResetRequest) {
            if (gate.request(value)) {
                posted += 1
                fakeWorker += {
                    while (true) {
                        val current = gate.next() ?: break
                        applied += current
                        if (current == first) repeat(100) { request(resized) }
                        if (!gate.finish(current)) break
                    }
                }
            }
        }

        repeat(100) { request(first) }
        assertEquals(1, posted)
        assertEquals(1, fakeWorker.size)
        assertEquals(first, gate.next())

        // A resize arrives while the one queued worker command is applying. It updates
        // the pending request and is consumed by that same command.
        val worker = fakeWorker.removeFirst()
        assertEquals(1, posted)
        worker()
        assertEquals(listOf(first, resized), applied)
        assertTrue(fakeWorker.isEmpty())
        assertEquals(1, posted)

        repeat(100) { request(resized) }
        assertEquals(1, posted)
    }

    @Test
    fun ingressGateDeduplicatesAndResyncsAfterBoundedOverflow() {
        val gate = DeviceVideoIngressGate(maxFrames = 2, maxBytes = 8)
        gate.reset("host/device/screen")

        val keyframe = gate.offer(1uL, "h264", keyframe = true, bytes = 4)
        assertNotNull(keyframe)
        assertNull(gate.offer(1uL, "h264", keyframe = true, bytes = 4))

        val delta = gate.offer(2uL, "h264", keyframe = false, bytes = 4)
        assertNotNull(delta)
        assertNull(gate.offer(3uL, "h264", keyframe = false, bytes = 1))
        assertNull(gate.offer(3uL, "h264", keyframe = true, bytes = 1))

        assertEquals(DeviceVideoIngressGate.Completion(current = true, resync = true), gate.complete(keyframe!!))
        assertEquals(DeviceVideoIngressGate.Completion(current = true, resync = false), gate.complete(delta!!))
        assertNull(gate.offer(4uL, "h264", keyframe = false, bytes = 1))
        val recoveryKeyframe = gate.offer(5uL, "h264", keyframe = true, bytes = 1)
        assertNotNull(recoveryKeyframe)
        assertTrue(gate.complete(recoveryKeyframe!!).current)
    }

    @Test
    fun ingressGateDropsCompletionsFromAnOldStreamGeneration() {
        val gate = DeviceVideoIngressGate(maxFrames = 2, maxBytes = 8)
        gate.reset("first")
        val oldFrame = gate.offer(9uL, "h264", keyframe = true, bytes = 2)
        assertNotNull(oldFrame)

        gate.reset("second")
        val currentFrame = gate.offer(1uL, "h264", keyframe = true, bytes = 2)
        assertNotNull(currentFrame)

        assertEquals(DeviceVideoIngressGate.Completion(current = false, resync = false), gate.complete(oldFrame!!))
        assertTrue(gate.complete(currentFrame!!).current)
    }

    @Test
    fun ingressGateRejectsUnsupportedAndOversizedPayloadsBeforeCopying() {
        val gate = DeviceVideoIngressGate(maxFrames = 2, maxBytes = 8)
        gate.reset("stream")

        assertNull(gate.offer(1uL, "jpeg", keyframe = true, bytes = 1))
        assertNull(gate.offer(2uL, "h264", keyframe = true, bytes = 9))
        val valid = gate.offer(3uL, "h264", keyframe = true, bytes = 8)
        assertNotNull(valid)
        assertTrue(gate.complete(valid!!).current)
    }

    @Test
    fun lengthPrefixedAccessUnitsBecomeAnnexB() {
        val payload = byteArrayOf(0, 0, 0, 2, 0x65, 0x01, 0, 0, 0, 1, 0x41)
        assertArrayEquals(byteArrayOf(0, 0, 0, 1, 0x65, 0x01, 0, 0, 0, 1, 0x41), normalizeH264Payload(payload))
    }

    @Test
    fun codecDescriptionSplitsAnnexBSpsAndPps() {
        val (sps, pps) = splitCodecDescription(byteArrayOf(0, 0, 0, 1, 0x67, 0x64, 0, 0, 0, 1, 0x68, 0xEE.toByte()))
        assertArrayEquals(byteArrayOf(0, 0, 0, 1, 0x67, 0x64), sps)
        assertArrayEquals(byteArrayOf(0, 0, 0, 1, 0x68, 0xEE.toByte()), pps)
    }

    @Test
    fun codecDescriptionSplitsAvccSpsAndPpsWithAnnexBPrefixes() {
        val (sps, pps) =
            splitCodecDescription(
                byteArrayOf(
                    1,
                    0x64,
                    0,
                    0x1f,
                    0xff.toByte(),
                    0xe1.toByte(),
                    0,
                    2,
                    0x67,
                    0x64,
                    1,
                    0,
                    2,
                    0x68,
                    0xee.toByte(),
                )
            )
        assertArrayEquals(byteArrayOf(0, 0, 0, 1, 0x67, 0x64), sps)
        assertArrayEquals(byteArrayOf(0, 0, 0, 1, 0x68, 0xee.toByte()), pps)
    }
}

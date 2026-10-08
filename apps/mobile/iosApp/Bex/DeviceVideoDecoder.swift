import AgentCore
import AVFoundation
import CoreGraphics
import CoreImage
import CoreMedia
import Foundation
import UIKit
import VideoToolbox

/// Decodes one ordered device feed. The Host sends complete AVCC/SEMU access
/// units; this object retains codec configuration and waits for a keyframe
/// after a gap or decoder reset instead of presenting a delta as a picture.
/// It is owned by `DeviceFrameDecoderWorker`, so VideoToolbox and Core Image
/// work never runs in the SwiftUI main-actor render path.
final class DeviceVideoDecoder {
    private static let maxAccessUnitBytes = 8 * 1024 * 1024
    private static let maxImageBytes = 16 * 1024 * 1024
    private var formatDescription: CMVideoFormatDescription?
    private var session: VTDecompressionSession?
    private var awaitingKeyframe = true
    private var latestPixelBuffer: CVPixelBuffer?
    private let lock = NSLock()
    private let context = CIContext()

    deinit {
        session.map(VTDecompressionSessionInvalidate)
    }

    /// Drops the decoder and its codec description. Used when a stream is
    /// removed or the Host reports a fatal format error.
    func reset() {
        session.map(VTDecompressionSessionInvalidate)
        session = nil
        formatDescription = nil
        awaitingKeyframe = true
        clearLatestPixelBuffer()
    }

    /// Resynchronizes a live stream while preserving SPS/PPS. The next
    /// keyframe recreates the VideoToolbox session from the retained format.
    func resync() {
        session.map(VTDecompressionSessionInvalidate)
        session = nil
        awaitingKeyframe = true
        clearLatestPixelBuffer()
    }

    func consume(_ frame: DeviceVideoFrameView) -> Data? {
        switch frame.encoding {
        case "jpeg", "mjpeg", "png": consumeImage(frame)
        case "avcc-description": consumeDescription(frame)
        case "h264", "semu": consumeVideo(frame)
        default: nil
        }
    }

    private func consumeImage(_ frame: DeviceVideoFrameView) -> Data? {
        guard frame.payload.count <= Self.maxImageBytes else { return nil }
        return frame.payload
    }

    private func consumeDescription(_ frame: DeviceVideoFrameView) -> Data? {
        guard frame.payload.count <= Self.maxAccessUnitBytes,
              configure(description: frame.payload) else {
            resync()
            return nil
        }
        awaitingKeyframe = true
        return nil
    }

    private func consumeVideo(_ frame: DeviceVideoFrameView) -> Data? {
        guard frame.keyframe || !awaitingKeyframe else { return nil }
        if frame.keyframe, session == nil {
            if let formatDescription {
                guard createSession(formatDescription: formatDescription) else { return nil }
            } else {
                configureFromAccessUnit(frame.payload)
            }
        }
        guard session != nil, formatDescription != nil else { return nil }
        let accessUnit = annexBToAvcc(frame.payload)
        guard !accessUnit.isEmpty, accessUnit.count <= Self.maxAccessUnitBytes else {
            resync()
            return nil
        }
        let timestamp = frame.timestampUs ?? frame.sequence.saturatingMultiply(16667)
        guard decode(accessUnit, timestampUs: timestamp) else {
            resync()
            return nil
        }
        awaitingKeyframe = false
        guard let data = imageDataFromLatestPixelBuffer(), data.count <= Self.maxImageBytes else { return nil }
        return data
    }

    private func clearLatestPixelBuffer() {
        lock.lock()
        latestPixelBuffer = nil
        lock.unlock()
    }

    private func configure(description bytes: Data) -> Bool {
        guard let parameterSets = avcParameterSets(from: bytes) else { return false }
        return configure(parameterSets: parameterSets)
    }

    private func configureFromAccessUnit(_ bytes: Data) {
        let nals = annexBNALUnits(bytes)
        let parameterSets = nals.filter {
            guard let first = $0.first else { return false }
            return first & 0x1F == 7 || first & 0x1F == 8
        }
        guard parameterSets.count >= 2 else { return }
        _ = configure(parameterSets: [parameterSets[0], parameterSets[1]])
    }

    private func configure(parameterSets: [Data]) -> Bool {
        guard parameterSets.count >= 2 else { return false }
        var description: CMVideoFormatDescription?
        let sps = parameterSets[0]
        let pps = parameterSets[1]
        let status = sps.withUnsafeBytes { spsRaw in
            pps.withUnsafeBytes { ppsRaw in
                let pointers: [UnsafePointer<UInt8>] = [
                    spsRaw.bindMemory(to: UInt8.self).baseAddress!,
                    ppsRaw.bindMemory(to: UInt8.self).baseAddress!
                ]
                let sizes = [sps.count, pps.count]
                return pointers.withUnsafeBufferPointer { pointerBuffer in
                    sizes.withUnsafeBufferPointer { sizeBuffer in
                        CMVideoFormatDescriptionCreateFromH264ParameterSets(
                            allocator: kCFAllocatorDefault,
                            parameterSetCount: 2,
                            parameterSetPointers: pointerBuffer.baseAddress!,
                            parameterSetSizes: sizeBuffer.baseAddress!,
                            nalUnitHeaderLength: 4,
                            formatDescriptionOut: &description
                        )
                    }
                }
            }
        }
        guard status == noErr, let description else { return false }
        return createSession(formatDescription: description)
    }

    private func createSession(formatDescription description: CMVideoFormatDescription) -> Bool {
        session.map(VTDecompressionSessionInvalidate)
        var callback = VTDecompressionOutputCallbackRecord(
            decompressionOutputCallback: Self.outputCallback,
            decompressionOutputRefCon: Unmanaged.passUnretained(self).toOpaque()
        )
        var newSession: VTDecompressionSession?
        let sessionStatus = VTDecompressionSessionCreate(
            allocator: kCFAllocatorDefault,
            formatDescription: description,
            decoderSpecification: nil,
            imageBufferAttributes: nil,
            outputCallback: &callback,
            decompressionSessionOut: &newSession
        )
        guard sessionStatus == noErr, let newSession else {
            session = nil
            return false
        }
        formatDescription = description
        session = newSession
        awaitingKeyframe = true
        return true
    }

    private func decode(_ bytes: Data, timestampUs: UInt64) -> Bool {
        guard let session, let formatDescription,
              let sampleBuffer = makeSampleBuffer(bytes, formatDescription: formatDescription,
                                                  timestampUs: timestampUs) else {
            return false
        }
        var flags = VTDecodeInfoFlags()
        let status = VTDecompressionSessionDecodeFrame(
            session,
            sampleBuffer: sampleBuffer,
            flags: [],
            frameRefcon: nil,
            infoFlagsOut: &flags
        )
        guard status == noErr else { return false }
        _ = VTDecompressionSessionWaitForAsynchronousFrames(session)
        return true
    }

    private func makeSampleBuffer(
        _ bytes: Data,
        formatDescription: CMVideoFormatDescription,
        timestampUs: UInt64
    ) -> CMSampleBuffer? {
        var blockBuffer: CMBlockBuffer?
        let blockStatus = CMBlockBufferCreateWithMemoryBlock(
            allocator: kCFAllocatorDefault,
            memoryBlock: nil,
            blockLength: bytes.count,
            blockAllocator: kCFAllocatorDefault,
            customBlockSource: nil,
            offsetToData: 0,
            dataLength: bytes.count,
            flags: 0,
            blockBufferOut: &blockBuffer
        )
        guard blockStatus == kCMBlockBufferNoErr, let blockBuffer else { return nil }
        let copyStatus = bytes.withUnsafeBytes { raw in
            CMBlockBufferReplaceDataBytes(
                with: raw.baseAddress!,
                blockBuffer: blockBuffer,
                offsetIntoDestination: 0,
                dataLength: bytes.count
            )
        }
        guard copyStatus == kCMBlockBufferNoErr else { return nil }
        var timing = CMSampleTimingInfo(
            duration: .invalid,
            presentationTimeStamp: CMTime(value: CMTimeValue(timestampUs), timescale: 1_000_000),
            decodeTimeStamp: .invalid
        )
        var sampleBuffer: CMSampleBuffer?
        var size = bytes.count
        let sampleStatus = CMSampleBufferCreateReady(
            allocator: kCFAllocatorDefault,
            dataBuffer: blockBuffer,
            formatDescription: formatDescription,
            sampleCount: 1,
            sampleTimingEntryCount: 1,
            sampleTimingArray: &timing,
            sampleSizeEntryCount: 1,
            sampleSizeArray: &size,
            sampleBufferOut: &sampleBuffer
        )
        guard sampleStatus == noErr else { return nil }
        return sampleBuffer
    }

    private func imageDataFromLatestPixelBuffer() -> Data? {
        lock.lock()
        let pixelBuffer = latestPixelBuffer
        lock.unlock()
        guard let pixelBuffer else { return nil }
        let bounds = CGRect(
            x: 0,
            y: 0,
            width: CVPixelBufferGetWidth(pixelBuffer),
            height: CVPixelBufferGetHeight(pixelBuffer)
        )
        guard let image = context.createCGImage(CIImage(cvPixelBuffer: pixelBuffer), from: bounds) else {
            return nil
        }
        return context.jpegRepresentation(
            of: CIImage(cgImage: image),
            colorSpace: CGColorSpaceCreateDeviceRGB(),
            options: [:]
        )
    }

    private static let outputCallback: VTDecompressionOutputCallback = { refCon, _, status, _, imageBuffer, _, _ in
        guard status == noErr, let refCon, let imageBuffer else { return }
        let decoder = Unmanaged<DeviceVideoDecoder>.fromOpaque(refCon).takeUnretainedValue()
        decoder.lock.lock()
        decoder.latestPixelBuffer = imageBuffer
        decoder.lock.unlock()
    }
}

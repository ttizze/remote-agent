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
        case "jpeg", "mjpeg", "png":
            guard frame.payload.count <= Self.maxImageBytes else { return nil }
            return Data(frame.payload)
        case "avcc-description":
            guard frame.payload.count <= Self.maxAccessUnitBytes else {
                resync()
                return nil
            }
            guard configure(description: frame.payload) else {
                resync()
                return nil
            }
            awaitingKeyframe = true
            return nil
        case "h264", "semu":
            guard frame.keyframe || !awaitingKeyframe else { return nil }
            if frame.keyframe {
                if session == nil {
                    if let formatDescription {
                        guard createSession(formatDescription: formatDescription) else { return nil }
                    } else {
                        configureFromAccessUnit(frame.payload)
                    }
                }
            }
            guard session != nil, formatDescription != nil else { return nil }
            let accessUnit = annexBToAvcc(frame.payload)
            guard !accessUnit.isEmpty, accessUnit.count <= Self.maxAccessUnitBytes else {
                resync()
                return nil
            }
            guard decode(accessUnit, timestampUs: frame.timestampUs ?? frame.sequence.saturatingMultiply(16_667)) else {
                resync()
                return nil
            }
            awaitingKeyframe = false
            guard let data = imageDataFromLatestPixelBuffer(), data.count <= Self.maxImageBytes else {
                return nil
            }
            return data
        default:
            return nil
        }
    }

    private func clearLatestPixelBuffer() {
        lock.lock()
        latestPixelBuffer = nil
        lock.unlock()
    }

    private func configure(description bytes: [UInt8]) -> Bool {
        guard let parameterSets = avcParameterSets(from: bytes) else { return false }
        return configure(parameterSets: parameterSets)
    }

    private func configureFromAccessUnit(_ bytes: [UInt8]) {
        let nals = annexBNALUnits(bytes)
        let parameterSets = nals.filter {
            guard let first = $0.first else { return false }
            return first & 0x1f == 7 || first & 0x1f == 8
        }
        guard parameterSets.count >= 2 else { return }
        _ = configure(parameterSets: [parameterSets[0], parameterSets[1]])
    }

    private func configure(parameterSets: [[UInt8]]) -> Bool {
        guard parameterSets.count >= 2 else { return false }
        var description: CMVideoFormatDescription?
        let sps = parameterSets[0]
        let pps = parameterSets[1]
        let status = sps.withUnsafeBytes { spsRaw in
            pps.withUnsafeBytes { ppsRaw in
                let pointers: [UnsafePointer<UInt8>] = [
                    spsRaw.bindMemory(to: UInt8.self).baseAddress!,
                    ppsRaw.bindMemory(to: UInt8.self).baseAddress!,
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

    private func decode(_ bytes: [UInt8], timestampUs: UInt64) -> Bool {
        guard let session, let formatDescription else { return false }
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
        guard blockStatus == kCMBlockBufferNoErr, let blockBuffer else { return false }
        let copyStatus = bytes.withUnsafeBytes { raw in
            CMBlockBufferReplaceDataBytes(
                with: raw.baseAddress!,
                blockBuffer: blockBuffer,
                offsetIntoDestination: 0,
                dataLength: bytes.count
            )
        }
        guard copyStatus == kCMBlockBufferNoErr else { return false }
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
        guard sampleStatus == noErr, let sampleBuffer else { return false }
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

private struct DecodedDeviceFrame {
    let key: String
    let threadId: String
    let sessionEpoch: String
    let hostId: String
    let deviceId: String
    let screenId: Int
    let width: Int
    let height: Int
    let data: Data
}

private struct DeviceDecodeRequest {
    let input: [DeviceVideoFrameView]
    let threadId: String
    let generation: UInt64
}

/// Serializes decoder state away from SwiftUI. The actor owns VideoToolbox,
/// Core Image and all sequence checks; the main actor only turns the returned
/// bounded image data into a view image.
private actor DeviceFrameDecoderWorker {
    private var decoders: [String: DeviceVideoDecoder] = [:]
    private var sequences: [String: UInt64] = [:]

    func consume(_ input: [DeviceVideoFrameView], threadId: String) -> [DecodedDeviceFrame] {
        var output: [DecodedDeviceFrame] = []
        let ordered = input
            .filter { $0.threadId == threadId }
            .sorted { lhs, rhs in
                if lhs.hostId != rhs.hostId { return lhs.hostId < rhs.hostId }
                if lhs.deviceId != rhs.deviceId { return lhs.deviceId < rhs.deviceId }
                if lhs.sessionEpoch != rhs.sessionEpoch { return lhs.sessionEpoch < rhs.sessionEpoch }
                let lhsScreen = lhs.screenId ?? 0
                let rhsScreen = rhs.screenId ?? 0
                if lhsScreen != rhsScreen { return lhsScreen < rhsScreen }
                return lhs.sequence < rhs.sequence
            }
        for frame in ordered {
            if Task.isCancelled { return output }
            let screenId = Int(frame.screenId ?? 0)
            let key = "\(threadId):\(frame.hostId):\(frame.deviceId):\(frame.sessionEpoch):\(screenId)"
            if sequences[key].map({ frame.sequence <= $0 }) == true { continue }
            if let previous = sequences[key], previous < UInt64.max, frame.sequence > previous + 1 {
                decoders[key]?.resync()
            }
            sequences[key] = frame.sequence
            let decoder = decoders[key] ?? {
                let decoder = DeviceVideoDecoder()
                decoders[key] = decoder
                return decoder
            }()
            guard let data = decoder.consume(frame), !data.isEmpty else { continue }
            output.append(DecodedDeviceFrame(
                key: key,
                threadId: threadId,
                sessionEpoch: frame.sessionEpoch,
                hostId: frame.hostId,
                deviceId: frame.deviceId,
                screenId: screenId,
                width: Int(frame.width),
                height: Int(frame.height),
                data: data
            ))
        }
        return output
    }

    func reset(threadId: String) {
        let prefix = "\(threadId):"
        let keys = Set(
            decoders.keys.filter { $0.hasPrefix(prefix) }
                + sequences.keys.filter { $0.hasPrefix(prefix) }
        )
        for key in keys {
            decoders.removeValue(forKey: key)
            sequences.removeValue(forKey: key)
        }
    }
}

/// Owns one decoder per device screen. Screen 1 and 3 are independent feeds,
/// so a delayed delta from one panel cannot corrupt the other panel's image.
@MainActor
final class DeviceFrameStore: ObservableObject {
    private static let maxAdmittedBytes = 16 * 1024 * 1024
    private static let maxAdmittedFrameBytes = 8 * 1024 * 1024

    struct RenderedFrame: Identifiable {
        let id: String
        let threadId: String
        let sessionEpoch: String
        let image: UIImage
        let screenId: Int
        let hostId: String
        let deviceId: String
        let width: Int
        let height: Int
    }

    @Published private(set) var frames: [String: RenderedFrame] = [:]
    private let worker = DeviceFrameDecoderWorker()
    private var generation: UInt64 = 0
    private var consumeTask: Task<Void, Never>?
    private var decodeInFlight = false
    private var pendingRequest: DeviceDecodeRequest?
    private var resetTask: Task<Void, Never>?
    private var resetSerial: UInt64 = 0

    func consume(_ input: [DeviceVideoFrameView], threadId: String) {
        generation = generation &+ 1
        let request = DeviceDecodeRequest(
            input: Self.admit(input, threadId: threadId),
            threadId: threadId,
            generation: generation
        )
        if decodeInFlight || resetTask != nil {
            // A frame revision is a snapshot, so only the newest snapshot has
            // any value once a decode is already running. Replacing this one
            // slot keeps both request count and retained payload bytes bounded.
            pendingRequest = request
        } else {
            start(request)
        }
    }

    private static func admit(_ input: [DeviceVideoFrameView], threadId: String) -> [DeviceVideoFrameView] {
        var selected: [(Int, DeviceVideoFrameView)] = []
        var bytes = 0
        for (index, frame) in input.enumerated().reversed() {
            guard frame.threadId == threadId,
                  !frame.payload.isEmpty,
                  frame.payload.count <= maxAdmittedFrameBytes,
                  bytes <= maxAdmittedBytes - frame.payload.count else { continue }
            selected.append((index, frame))
            bytes += frame.payload.count
        }
        selected.sort { $0.0 < $1.0 }
        return selected.map { $0.1 }
    }

    private func start(_ request: DeviceDecodeRequest) {
        decodeInFlight = true
        let worker = worker
        consumeTask = Task { [weak self] in
            let decoded = await worker.consume(request.input, threadId: request.threadId)
            guard let self else { return }
            self.finish(request, decoded: decoded)
        }
    }

    private func finish(_ request: DeviceDecodeRequest, decoded: [DecodedDeviceFrame]) {
        if request.generation == generation {
            for frame in decoded {
                guard let image = UIImage(data: frame.data) else { continue }
                frames[frame.key] = RenderedFrame(
                    id: frame.key,
                    threadId: frame.threadId,
                    sessionEpoch: frame.sessionEpoch,
                    image: image,
                    screenId: frame.screenId,
                    hostId: frame.hostId,
                    deviceId: frame.deviceId,
                    width: frame.width,
                    height: frame.height
                )
            }
        }
        decodeInFlight = false
        consumeTask = nil
        startPendingIfReady()
    }

    private func startPendingIfReady() {
        guard !decodeInFlight, resetTask == nil, let next = pendingRequest else { return }
        pendingRequest = nil
        start(next)
    }

    func frames(for threadId: String) -> [RenderedFrame] {
        frames.values.filter { $0.threadId == threadId }.sorted { $0.id < $1.id }
    }

    func reset(threadId: String) {
        generation = generation &+ 1
        pendingRequest = nil
        consumeTask?.cancel()
        let prefix = "\(threadId):"
        frames = frames.filter { !$0.key.hasPrefix(prefix) }
        resetTask?.cancel()
        resetSerial = resetSerial &+ 1
        let serial = resetSerial
        let worker = worker
        resetTask = Task { [weak self] in
            await worker.reset(threadId: threadId)
            guard let self, self.resetSerial == serial else { return }
            self.resetTask = nil
            self.startPendingIfReady()
        }
    }

    deinit {
        consumeTask?.cancel()
        resetTask?.cancel()
    }
}

private func annexBNALUnits(_ bytes: [UInt8]) -> [[UInt8]] {
    var starts: [(nal: Int, code: Int)] = []
    var index = 0
    while index + 3 < bytes.count {
        if bytes[index...].starts(with: [0, 0, 0, 1]) {
            starts.append((index + 4, index))
            index += 4
        } else if bytes[index...].starts(with: [0, 0, 1]) {
            starts.append((index + 3, index))
            index += 3
        } else {
            index += 1
        }
    }
    return starts.enumerated().compactMap { offset, start in
        let end = offset + 1 < starts.count ? starts[offset + 1].code : bytes.count
        guard start.nal < end else { return nil }
        return Array(bytes[start.nal..<end])
    }
}

private func annexBToAvcc(_ bytes: [UInt8]) -> [UInt8] {
    let nals = annexBNALUnits(bytes)
    if !nals.isEmpty {
        return nals.reduce(into: []) { result, nal in
            var length = UInt32(nal.count).bigEndian
            withUnsafeBytes(of: &length) { result.append(contentsOf: $0) }
            result.append(contentsOf: nal)
        }
    }
    var result: [UInt8] = []
    var index = 0
    while index + 4 <= bytes.count {
        let length = Int(bytes[index]) << 24 | Int(bytes[index + 1]) << 16 | Int(bytes[index + 2]) << 8 | Int(bytes[index + 3])
        index += 4
        guard length > 0, index + length <= bytes.count else { return [] }
        result.append(contentsOf: bytes[index - 4..<index + length])
        index += length
    }
    return index == bytes.count ? result : []
}

private func avcParameterSets(from bytes: [UInt8]) -> [[UInt8]]? {
    if bytes.first == 1, bytes.count >= 7 {
        var index = 5
        let spsCount = Int(bytes[index] & 0x1f)
        index += 1
        var sets: [[UInt8]] = []
        for _ in 0..<spsCount {
            guard index + 2 <= bytes.count else { return nil }
            let length = Int(bytes[index]) << 8 | Int(bytes[index + 1])
            index += 2
            guard length > 0, index + length <= bytes.count else { return nil }
            sets.append(Array(bytes[index..<index + length]))
            index += length
        }
        guard index < bytes.count else { return nil }
        let ppsCount = Int(bytes[index]); index += 1
        for _ in 0..<ppsCount {
            guard index + 2 <= bytes.count else { return nil }
            let length = Int(bytes[index]) << 8 | Int(bytes[index + 1])
            index += 2
            guard length > 0, index + length <= bytes.count else { return nil }
            sets.append(Array(bytes[index..<index + length]))
            index += length
        }
        return sets.count >= 2 ? sets : nil
    }
    let sets = annexBNALUnits(bytes).filter {
        guard let first = $0.first else { return false }
        return first & 0x1f == 7 || first & 0x1f == 8
    }
    return sets.count >= 2 ? sets : nil
}

private extension UInt64 {
    func saturatingAdd(_ value: Int) -> UInt64 {
        addingReportingOverflow(UInt64(max(value, 0))).overflow ? UInt64.max : self + UInt64(max(value, 0))
    }

    func saturatingMultiply(_ value: UInt64) -> UInt64 {
        multipliedReportingOverflow(by: value).overflow ? UInt64.max : self * value
    }
}

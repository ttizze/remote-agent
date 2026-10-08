import AgentCore
import Foundation
import UIKit

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
        for frame in orderedFrames(input, threadId: threadId) {
            guard !Task.isCancelled else { return output }
            if let decoded = consume(frame, threadId: threadId) {
                output.append(decoded)
            }
        }
        return output
    }

    private func orderedFrames(_ input: [DeviceVideoFrameView], threadId: String) -> [DeviceVideoFrameView] {
        input.filter { $0.threadId == threadId }.sorted { lhs, rhs in
            if lhs.hostId != rhs.hostId {
                return lhs.hostId < rhs.hostId
            }
            if lhs.deviceId != rhs.deviceId {
                return lhs.deviceId < rhs.deviceId
            }
            if lhs.sessionEpoch != rhs.sessionEpoch {
                return lhs.sessionEpoch < rhs.sessionEpoch
            }
            let lhsScreen = lhs.screenId ?? 0
            let rhsScreen = rhs.screenId ?? 0
            if lhsScreen != rhsScreen {
                return lhsScreen < rhsScreen
            }
            return lhs.sequence < rhs.sequence
        }
    }

    private func consume(_ frame: DeviceVideoFrameView, threadId: String) -> DecodedDeviceFrame? {
        let screenId = Int(frame.screenId ?? 0)
        let key = "\(threadId):\(frame.hostId):\(frame.deviceId):\(frame.sessionEpoch):\(screenId)"
        guard sequences[key].map({ frame.sequence > $0 }) ?? true else { return nil }
        if let previous = sequences[key], previous < UInt64.max, frame.sequence > previous + 1 {
            decoders[key]?.resync()
        }
        sequences[key] = frame.sequence
        guard let data = decoder(for: key).consume(frame), !data.isEmpty else { return nil }
        return DecodedDeviceFrame(
            key: key,
            threadId: threadId,
            sessionEpoch: frame.sessionEpoch,
            hostId: frame.hostId,
            deviceId: frame.deviceId,
            screenId: screenId,
            width: Int(frame.width),
            height: Int(frame.height),
            data: data
        )
    }

    private func decoder(for key: String) -> DeviceVideoDecoder {
        if let decoder = decoders[key] {
            return decoder
        }
        let decoder = DeviceVideoDecoder()
        decoders[key] = decoder
        return decoder
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
        return selected.map(\.1)
    }

    private func start(_ request: DeviceDecodeRequest) {
        decodeInFlight = true
        let worker = worker
        consumeTask = Task { [weak self] in
            let decoded = await worker.consume(request.input, threadId: request.threadId)
            guard let self else { return }
            finish(request, decoded: decoded)
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
            guard let self, resetSerial == serial else { return }
            resetTask = nil
            startPendingIfReady()
        }
    }

    deinit {
        consumeTask?.cancel()
        resetTask?.cancel()
    }
}

func annexBNALUnits(_ bytes: [UInt8]) -> [[UInt8]] {
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
        return Array(bytes[start.nal ..< end])
    }
}

func annexBToAvcc(_ bytes: [UInt8]) -> [UInt8] {
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
        let length = Int(bytes[index]) << 24 | Int(bytes[index + 1]) << 16 | Int(bytes[index + 2]) << 8 |
            Int(bytes[index + 3])
        index += 4
        guard length > 0, index + length <= bytes.count else { return [] }
        result.append(contentsOf: bytes[index - 4 ..< index + length])
        index += length
    }
    return index == bytes.count ? result : []
}

func avcParameterSets(from bytes: [UInt8]) -> [[UInt8]]? {
    if bytes.first == 1, bytes.count >= 7 {
        var index = 5
        let spsCount = Int(bytes[index] & 0x1F)
        index += 1
        var sets: [[UInt8]] = []
        for _ in 0 ..< spsCount {
            guard index + 2 <= bytes.count else { return nil }
            let length = Int(bytes[index]) << 8 | Int(bytes[index + 1])
            index += 2
            guard length > 0, index + length <= bytes.count else { return nil }
            sets.append(Array(bytes[index ..< index + length]))
            index += length
        }
        guard index < bytes.count else { return nil }
        let ppsCount = Int(bytes[index]); index += 1
        for _ in 0 ..< ppsCount {
            guard index + 2 <= bytes.count else { return nil }
            let length = Int(bytes[index]) << 8 | Int(bytes[index + 1])
            index += 2
            guard length > 0, index + length <= bytes.count else { return nil }
            sets.append(Array(bytes[index ..< index + length]))
            index += length
        }
        return sets.count >= 2 ? sets : nil
    }
    let sets = annexBNALUnits(bytes).filter {
        guard let first = $0.first else { return false }
        return first & 0x1F == 7 || first & 0x1F == 8
    }
    return sets.count >= 2 ? sets : nil
}

extension UInt64 {
    func saturatingMultiply(_ value: UInt64) -> UInt64 {
        multipliedReportingOverflow(by: value).overflow ? UInt64.max : self * value
    }
}

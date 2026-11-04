import AVFoundation
import Foundation

// A generated, silent movie keeps the isolated photo library free of
// personal media and avoids checking a binary fixture into the repository.
let url = URL(fileURLWithPath: CommandLine.arguments[1])
let writer = try AVAssetWriter(outputURL: url, fileType: url.pathExtension == "mp4" ? .mp4 : .mov)
let lastSecond = CommandLine.arguments.count > 2 ? Int(CommandLine.arguments[2])! : 1
let input = AVAssetWriterInput(mediaType: .video, outputSettings: [
    AVVideoCodecKey: AVVideoCodecType.h264,
    AVVideoWidthKey: 64, AVVideoHeightKey: 64
])
let adaptor = AVAssetWriterInputPixelBufferAdaptor(assetWriterInput: input, sourcePixelBufferAttributes: [
    kCVPixelBufferPixelFormatTypeKey as String: kCVPixelFormatType_32ARGB,
    kCVPixelBufferWidthKey as String: 64, kCVPixelBufferHeightKey as String: 64
])
writer.add(input)
precondition(writer.startWriting())
writer.startSession(atSourceTime: .zero)
var buffer: CVPixelBuffer?
precondition(CVPixelBufferCreate(kCFAllocatorDefault, 64, 64, kCVPixelFormatType_32ARGB, nil, &buffer) ==
    kCVReturnSuccess)
let frame = buffer!
CVPixelBufferLockBaseAddress(frame, [])
memset(CVPixelBufferGetBaseAddress(frame), 0x88, CVPixelBufferGetDataSize(frame))
CVPixelBufferUnlockBaseAddress(frame, [])
let deadline = Date().addingTimeInterval(15)
for second in 0 ... lastSecond {
    while !input.isReadyForMoreMediaData, Date() < deadline {
        Thread.sleep(forTimeInterval: 0.01)
    }
    precondition(input.isReadyForMoreMediaData)
    precondition(adaptor.append(frame, withPresentationTime: CMTime(value: Int64(second), timescale: 1)))
}

input.markAsFinished()
let finished = DispatchSemaphore(value: 0)
writer.finishWriting { finished.signal() }
precondition(finished.wait(timeout: .now() + 15) == .success)
precondition(writer.status == .completed, "Movie fixture encoding failed")

import AVFoundation
import CoreGraphics
import CoreText
import Foundation
import ImageIO
import UniformTypeIdentifiers

/// Synthetic files for the native renderer boundary. The video and large text
/// exceed the editor limit; no personal media or binary assets are checked in.
let directory = URL(fileURLWithPath: CommandLine.arguments[1])
try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
let movie = directory.appendingPathComponent("fixture video.mp4")
try FileManager.default.copyItem(at: URL(fileURLWithPath: CommandLine.arguments[2]), to: movie)
let handle = try FileHandle(forWritingTo: movie)
try handle.seekToEnd()
/// A legal ISO-BMFF free box pads a real H.264 movie beyond 1 MiB.
var box = UInt32(1024 * 1024 + 8).bigEndian
try withUnsafeBytes(of: &box) { try handle.write(contentsOf: Data($0)) }
try handle.write(contentsOf: Data("free".utf8))
try handle.write(contentsOf: Data(repeating: 0, count: 1024 * 1024))
try handle.close()

let format = AVAudioFormat(standardFormatWithSampleRate: 16000, channels: 1)!
let audio = try AVAudioFile(
    forWriting: directory.appendingPathComponent("fixture audio.wav"),
    settings: format.settings
)
let buffer = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: 160_000)!
buffer.frameLength = buffer.frameCapacity
memset(buffer.floatChannelData![0], 0, Int(buffer.frameLength) * MemoryLayout<Float>.size)
try audio.write(from: buffer)

let gif = CGImageDestinationCreateWithURL(
    directory.appendingPathComponent("fixture animation.gif") as CFURL, UTType.gif.identifier as CFString, 2, nil
)!
CGImageDestinationSetProperties(gif, [kCGImagePropertyGIFDictionary: [kCGImagePropertyGIFLoopCount: 0]] as CFDictionary)
let canvas = CGContext(data: nil, width: 64, height: 64, bitsPerComponent: 8, bytesPerRow: 0,
                       space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
for red in [0.0, 1.0] {
    canvas.setFillColor(red: red, green: 1 - red, blue: 0.2, alpha: 1)
    canvas.fill(CGRect(x: 0, y: 0, width: 64, height: 64))
    CGImageDestinationAddImage(gif, canvas.makeImage()!,
                               [kCGImagePropertyGIFDictionary: [kCGImagePropertyGIFDelayTime: 0.5]] as CFDictionary)
}

precondition(CGImageDestinationFinalize(gif))

try """
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 320 240" role="img" aria-label="SVG preview fixture">
<rect width="320" height="240" fill="#62cc88"/>
<text x="20" y="120" font-size="24">SVG preview fixture</text>
</svg>
""".write(to: directory.appendingPathComponent("fixture vector.svg"), atomically: true, encoding: .utf8)

var bounds = CGRect(x: 0, y: 0, width: 400, height: 500)
let pdf = CGContext(directory.appendingPathComponent("fixture document.pdf") as CFURL, mediaBox: &bounds, nil)!
pdf.beginPDFPage(nil)
pdf.setFillColor(red: 0.8, green: 0.95, blue: 0.85, alpha: 1)
pdf.fill(bounds)
let label = NSAttributedString(string: "PDF preview fixture", attributes: [
    kCTFontAttributeName as NSAttributedString.Key: CTFontCreateWithName("Helvetica" as CFString, 24, nil)
])
pdf.textPosition = CGPoint(x: 32, y: 400)
CTLineDraw(CTLineCreateWithAttributedString(label), pdf)
pdf.endPDFPage()
pdf.closePDF()

try ("Large text fixture\n" + String(repeating: "x", count: 1024 * 1024) + "END OF FULL FILE")
    .write(to: directory.appendingPathComponent("fixture large.txt"), atomically: true, encoding: .utf8)
try Data([0, 0xFF, 1, 2]).write(to: directory.appendingPathComponent("fixture unknown.bin"))
// A valid empty ZIP stays downloadable even when the OS has no previewer.
try Data([0x50, 0x4B, 0x05, 0x06] + Array(repeating: UInt8(0), count: 18))
    .write(to: directory.appendingPathComponent("fixture archive.zip"))

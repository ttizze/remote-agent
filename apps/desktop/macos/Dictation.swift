import AppKit
import AVFoundation

/// Like FilePicker, this helper owns its AppKit loop independently of GPUI.
/// stdin EOF cancels recording even if the parent exits unexpectedly. Audio stays
/// in the parent's private temporary directory; stdout contains only status.
final class Dictation: NSObject, AVAudioRecorderDelegate {
    private let directory: URL
    private var recorder: AVAudioRecorder?
    private var finished = false

    init(directory: URL) {
        self.directory = directory
    }

    func start() {
        AVCaptureDevice.requestAccess(for: .audio) { granted in
            DispatchQueue.main.async {
                guard !self.finished else { return }
                guard granted else {
                    self.complete(error: "システム設定の「プライバシーとセキュリティ → マイク」で Bex のアクセスを許可してください。")
                    return
                }
                do {
                    let recorder = try AVAudioRecorder(
                        url: self.directory.appendingPathComponent("recording.wav"),
                        settings: [
                            AVFormatIDKey: kAudioFormatLinearPCM,
                            AVSampleRateKey: 24000,
                            AVNumberOfChannelsKey: 1,
                            AVLinearPCMBitDepthKey: 16,
                            AVLinearPCMIsBigEndianKey: false,
                            AVLinearPCMIsFloatKey: false
                        ]
                    )
                    self.recorder = recorder
                    recorder.delegate = self
                    guard recorder.record() else {
                        self.complete(error: "録音を開始できませんでした。マイクの接続を確認してください。")
                        return
                    }
                    self.emit(["recording": true])
                } catch { self.complete(error: error.localizedDescription) }
            }
        }
    }

    func command(_ command: String?) {
        if command == "stop", let recorder {
            recorder.stop()
        } else {
            complete(error: "録音を中止しました。")
        }
    }

    func audioRecorderDidFinishRecording(_ recorder: AVAudioRecorder, successfully success: Bool) {
        guard !finished else { return }
        guard success else { complete(error: "録音が中断されました。もう一度録音してください。"); return }
        do {
            let file = try AVAudioFile(forReading: recorder.url, commonFormat: .pcmFormatInt16, interleaved: true)
            guard file.length > 0,
                  let buffer = AVAudioPCMBuffer(pcmFormat: file.processingFormat, frameCapacity: 4096) else {
                complete(error: "音声を録音できませんでした。もう一度録音してください。")
                return
            }
            let output = directory.appendingPathComponent("recording.pcm")
            guard FileManager.default.createFile(atPath: output.path, contents: nil) else {
                complete(error: "録音データを保存できませんでした。")
                return
            }
            let writer = try FileHandle(forWritingTo: output)
            defer { try? writer.close() }
            while file.framePosition < file.length {
                try file.read(into: buffer)
                guard buffer.frameLength > 0, let samples = buffer.int16ChannelData?.pointee else {
                    complete(error: "録音データを読み込めませんでした。")
                    return
                }
                try writer.write(contentsOf: Data(bytesNoCopy: samples,
                                                  count: Int(buffer.frameLength) * MemoryLayout<Int16>.size,
                                                  deallocator: .none))
            }
            complete(error: nil)
        } catch { complete(error: error.localizedDescription) }
    }

    func audioRecorderEncodeErrorDidOccur(_: AVAudioRecorder, error: Error?) {
        complete(error: error?.localizedDescription ?? "音声を録音できませんでした。")
    }

    private func emit(_ value: [String: Any]) {
        do {
            let data = try JSONSerialization.data(withJSONObject: value)
            FileHandle.standardOutput.write(data)
            FileHandle.standardOutput.write(Data([10]))
        } catch {
            FileHandle.standardError.write(Data("Dictation status encoding failed: \(error)\n".utf8))
            exit(1)
        }
    }

    private func complete(error: String?) {
        guard !finished else { return }
        finished = true
        recorder?.delegate = nil
        recorder?.stop()
        if let error {
            emit(["error": error])
        } else {
            emit(["complete": true])
        }
        exit(error == nil ? 0 : 1)
    }
}

guard CommandLine.arguments.count == 2 else { exit(2) }
let application = NSApplication.shared
application.setActivationPolicy(.prohibited)
let dictation = Dictation(directory: URL(fileURLWithPath: CommandLine.arguments[1], isDirectory: true))
DispatchQueue.global().async {
    while let command = readLine() {
        DispatchQueue.main.async { dictation.command(command) }
    }
    DispatchQueue.main.async { dictation.command(nil) }
}

dictation.start()
application.run()

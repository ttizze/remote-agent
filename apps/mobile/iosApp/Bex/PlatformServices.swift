import AVFoundation
import Combine
import RemoteAgentMobile
import UIKit

@MainActor
final class BexPlatformBridge {
    func didEnterBackground() {
        IosLifecycleBridge.shared.didEnterBackground()
    }

    /// iOS may stop the process while backgrounded. Recovery always obtains a
    /// fresh Host snapshot rather than trusting the on-device cache.
    func restoreAfterForeground() {
        IosLifecycleBridge.shared.restoreAfterForeground()
    }
}

@MainActor
final class DictationRecorder: NSObject, ObservableObject, AVAudioRecorderDelegate {
    @Published private(set) var isRecording = false
    @Published private(set) var requestingPermission = false
    private var recorder: AVAudioRecorder?
    private var fileURL: URL?
    private var requestID: UUID?
    private var audioSessionActive = false
    private var previousAudioCategory: (AVAudioSession.Category, AVAudioSession.Mode, AVAudioSession.CategoryOptions)?
    private var completion: ((Result<Data, Error>) -> Void)?

    override init() {
        super.init()
        NotificationCenter.default.addObserver(self, selector: #selector(cancel), name: UIApplication.didEnterBackgroundNotification, object: nil)
        NotificationCenter.default.addObserver(self, selector: #selector(interrupted(_:)), name: AVAudioSession.interruptionNotification, object: nil)
    }

    deinit {
        NotificationCenter.default.removeObserver(self)
        recorder?.stop()
        if let fileURL { try? FileManager.default.removeItem(at: fileURL) }
        if audioSessionActive { try? AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation) }
        if let previousAudioCategory {
            try? AVAudioSession.sharedInstance().setCategory(previousAudioCategory.0, mode: previousAudioCategory.1, options: previousAudioCategory.2)
        }
    }

    func start(completion: @escaping (Result<Data, Error>) -> Void) {
        guard requestID == nil else { return }
        let id = UUID()
        requestID = id
        self.completion = completion
        requestingPermission = true
        AVAudioSession.sharedInstance().requestRecordPermission { [weak self] granted in
            DispatchQueue.main.async {
                guard let self, self.requestID == id else { return }
                self.requestingPermission = false
                guard granted else {
                    self.complete(.failure(Self.error("設定アプリでBexのマイクへのアクセスを許可してください。")))
                    return
                }
                do {
                    let session = AVAudioSession.sharedInstance()
                    self.previousAudioCategory = (session.category, session.mode, session.categoryOptions)
                    try session.setCategory(.record, mode: .measurement)
                    try session.setActive(true)
                    self.audioSessionActive = true
                    let url = FileManager.default.temporaryDirectory.appendingPathComponent("dictation-\(id.uuidString).wav")
                    self.fileURL = url
                    let recorder = try AVAudioRecorder(url: url, settings: [
                        AVFormatIDKey: kAudioFormatLinearPCM,
                        AVSampleRateKey: 24_000,
                        AVNumberOfChannelsKey: 1,
                        AVLinearPCMBitDepthKey: 16,
                        AVLinearPCMIsBigEndianKey: false,
                        AVLinearPCMIsFloatKey: false
                    ])
                    self.recorder = recorder
                    recorder.delegate = self
                    guard recorder.record(forDuration: 30) else { throw Self.error("録音を開始できませんでした。") }
                    self.isRecording = true
                } catch { self.complete(.failure(error)) }
            }
        }
    }

    func finish() { recorder?.stop() }

    @objc func cancel() {
        guard requestID != nil else { return }
        completion = nil
        cleanup()
    }

    @objc private func interrupted(_ notification: Notification) {
        guard let type = notification.userInfo?[AVAudioSessionInterruptionTypeKey] as? UInt,
              type == AVAudioSession.InterruptionType.began.rawValue else { return }
        if requestID != nil { complete(.failure(Self.error("録音が中断されました。もう一度録音してください。"))) }
    }

    func audioRecorderDidFinishRecording(_ recorder: AVAudioRecorder, successfully flag: Bool) {
        guard self.recorder === recorder else { return }
        complete(Result {
            guard flag else { throw Self.error("録音を完了できませんでした。") }
            let file = try AVAudioFile(forReading: recorder.url, commonFormat: .pcmFormatInt16, interleaved: true)
            guard file.length > 0, file.length <= 24_000 * 30,
                  let buffer = AVAudioPCMBuffer(pcmFormat: file.processingFormat, frameCapacity: AVAudioFrameCount(file.length)) else {
                throw Self.error("音声を録音できませんでした。もう一度録音してください。")
            }
            try file.read(into: buffer)
            guard let samples = buffer.int16ChannelData?.pointee else { throw Self.error("録音データを読み込めませんでした。") }
            return Data(bytes: samples, count: Int(buffer.frameLength) * MemoryLayout<Int16>.size)
        })
    }

    func audioRecorderEncodeErrorDidOccur(_ recorder: AVAudioRecorder, error: Error?) {
        guard self.recorder === recorder else { return }
        complete(.failure(error ?? Self.error("音声を録音できませんでした。")))
    }

    private func complete(_ result: Result<Data, Error>) {
        let callback = completion
        completion = nil
        cleanup()
        callback?(result)
    }

    private func cleanup() {
        requestID = nil
        recorder?.delegate = nil
        recorder?.stop()
        recorder = nil
        isRecording = false
        requestingPermission = false
        if let fileURL { try? FileManager.default.removeItem(at: fileURL) }
        fileURL = nil
        if audioSessionActive {
            try? AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation)
            audioSessionActive = false
        }
        if let previousAudioCategory {
            try? AVAudioSession.sharedInstance().setCategory(previousAudioCategory.0, mode: previousAudioCategory.1, options: previousAudioCategory.2)
            self.previousAudioCategory = nil
        }
    }

    private static func error(_ message: String) -> Error {
        NSError(domain: "BexDictation", code: 1, userInfo: [NSLocalizedDescriptionKey: message])
    }
}

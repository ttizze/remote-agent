import AgentCore
import AVFoundation
import Combine
import Security
import SwiftUI
import UIKit

/// Keychain and private file I/O; core owns restoration and save scheduling.
final class MobileSessionStorage: ClientStorage, ClientPlatform, @unchecked Sendable {
    let host: String

    init(host: String) {
        self.host = host
    }

    private func location() throws -> URL {
        let directory = try FileManager.default.url(
            for: .applicationSupportDirectory, in: .userDomainMask,
            appropriateFor: nil, create: true
        ).appendingPathComponent("snapshots", isDirectory: true)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        let name = Data(host.utf8).base64EncodedString().replacingOccurrences(of: "/", with: "_")
        return directory.appendingPathComponent(name).appendingPathExtension("v3.json")
    }

    private func access<T>(_ body: () throws -> T) throws -> T {
        do { return try body() } catch { throw AgentError.Failed(reason: error.localizedDescription) }
    }

    func load() throws -> Data {
        try access {
            let url = try location()
            return FileManager.default.fileExists(atPath: url.path) ? try Data(contentsOf: url) : Data()
        }
    }

    func save(bytes: Data) throws {
        try access { try bytes.write(to: location(), options: .atomic) }
    }

    func identity() throws -> Data {
        try access { try DeviceIdentity.loadOrGenerate(host) }
    }

    func diagnosticsDirectory() throws -> String? {
        try access { try location().deletingPathExtension().appendingPathExtension("diagnostics").path }
    }

    func conversationDirectory() throws -> String? {
        try access { try conversations().path }
    }

    /// Saved conversation copies for this Host; removing the Host removes them.
    func conversations() throws -> URL {
        try location().deletingPathExtension().appendingPathExtension("conversations")
    }
}

/// These locations belong to the application, including before its first Host opens.
final class MobilePreferenceStorage: PreferenceStorage, @unchecked Sendable {
    func load(key: AgentCore.PreferenceKey) throws -> Data {
        switch key {
        case .models: return UserDefaults.standard.data(forKey: "bex.model-preferences") ?? Data()
        case .editor: return UserDefaults.standard.data(forKey: "bex.editor-preferences") ?? Data()
        case .composer: return UserDefaults.standard.data(forKey: "bex.composer-preferences") ?? Data()
        case .contrast: return UserDefaults.standard.data(forKey: "bex.contrast") ?? Data()
        case .webLinks: return UserDefaults.standard.data(forKey: "bex.web-links") ?? Data()
        case .language:
            guard let language = UserDefaults.standard.string(forKey: "bex.language") else { return Data() }
            return try JSONEncoder().encode(language)
        }
    }

    func save(key: AgentCore.PreferenceKey, bytes: Data) throws {
        switch key {
        case .models: UserDefaults.standard.set(bytes, forKey: "bex.model-preferences")
        case .editor: UserDefaults.standard.set(bytes, forKey: "bex.editor-preferences")
        case .composer: UserDefaults.standard.set(bytes, forKey: "bex.composer-preferences")
        case .contrast: UserDefaults.standard.set(bytes, forKey: "bex.contrast")
        case .webLinks: UserDefaults.standard.set(bytes, forKey: "bex.web-links")
        case .language:
            try UserDefaults.standard.set(JSONDecoder().decode(String.self, from: bytes), forKey: "bex.language")
        }
    }
}

enum DeviceIdentity {
    static func remove(_ reference: String) throws {
        let status = SecItemDelete([
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: "app.bex.iroh.identity",
            kSecAttrAccount as String: reference
        ] as CFDictionary)
        guard status == errSecSuccess || status == errSecItemNotFound else {
            throw NSError(domain: NSOSStatusErrorDomain, code: Int(status))
        }
    }

    static func loadOrGenerate(_ reference: String) throws -> Data {
        let query: [String: CFTypeRef] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: "app.bex.iroh.identity" as CFString,
            kSecAttrAccount as String: reference as CFString,
            kSecReturnData as String: kCFBooleanTrue!
        ]
        var result: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &result)
        if status == errSecSuccess, let data = result as? Data {
            return data
        }
        guard status == errSecItemNotFound else { throw NSError(domain: NSOSStatusErrorDomain, code: Int(status)) }
        let data = generateIdentity()
        var record = query
        record.removeValue(forKey: kSecReturnData as String)
        record[kSecValueData as String] = data as CFData
        record[kSecAttrAccessible as String] = kSecAttrAccessibleWhenUnlockedThisDeviceOnly
        let saved = SecItemAdd(record as CFDictionary, nil)
        guard saved == errSecSuccess else { throw NSError(domain: NSOSStatusErrorDomain, code: Int(saved)) }
        return data
    }
}

@MainActor
final class DictationRecorder: NSObject, ObservableObject, AVAudioRecorderDelegate {
    @Published private(set) var isRecording = false
    @Published private(set) var requestingPermission = false
    @Published private(set) var levels: [Float] = Array(repeating: 0, count: 40)
    private var meteringTimer: Timer?
    private var recorder: AVAudioRecorder?
    private var fileURL: URL?
    private var requestID: UUID?
    private var audioSessionActive = false
    private var previousAudioConfiguration: AudioSessionConfiguration?
    private var completion: ((Result<Data, Error>) -> Void)?

    override init() {
        super.init()
        NotificationCenter.default.addObserver(
            self,
            selector: #selector(cancel),
            name: UIApplication.didEnterBackgroundNotification,
            object: nil
        )
        NotificationCenter.default.addObserver(
            self,
            selector: #selector(interrupted(_:)),
            name: AVAudioSession.interruptionNotification,
            object: nil
        )
    }

    deinit {
        NotificationCenter.default.removeObserver(self)
        meteringTimer?.invalidate()
        recorder?.stop()
        if let fileURL {
            try? FileManager.default.removeItem(at: fileURL)
        }
        if audioSessionActive {
            try? AVAudioSession.sharedInstance().setActive(
                false,
                options: .notifyOthersOnDeactivation
            )
        }
        if let previousAudioConfiguration {
            try? previousAudioConfiguration.restore()
        }
    }

    func start(started: @escaping () -> Void, completion: @escaping (Result<Data, Error>) -> Void) {
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
                    self.previousAudioConfiguration = AudioSessionConfiguration(
                        category: session.category, mode: session.mode, options: session.categoryOptions
                    )
                    try session.setCategory(.record, mode: .measurement)
                    try session.setActive(true)
                    self.audioSessionActive = true
                    let url = FileManager.default.temporaryDirectory
                        .appendingPathComponent("dictation-\(id.uuidString).wav")
                    self.fileURL = url
                    let recorder = try AVAudioRecorder(url: url, settings: [
                        AVFormatIDKey: kAudioFormatLinearPCM,
                        AVSampleRateKey: 24000,
                        AVNumberOfChannelsKey: 1,
                        AVLinearPCMBitDepthKey: 16,
                        AVLinearPCMIsBigEndianKey: false,
                        AVLinearPCMIsFloatKey: false
                    ])
                    self.recorder = recorder
                    recorder.delegate = self
                    recorder.isMeteringEnabled = true
                    guard recorder.record() else { throw Self.error("録音を開始できませんでした。") }
                    self.isRecording = true
                    started()
                    self.meteringTimer = Timer.scheduledTimer(withTimeInterval: 0.05, repeats: true) { [weak self] _ in
                        Task { @MainActor [weak self] in
                            guard let self, isRecording, let recorder = self.recorder else { return }
                            recorder.updateMeters()
                            levels.removeFirst()
                            levels.append(pow(10, min(0, recorder.averagePower(forChannel: 0)) / 20))
                        }
                    }
                } catch { self.complete(.failure(error)) }
            }
        }
    }

    func finish() {
        recorder?.stop()
    }

    @objc func cancel() {
        guard requestID != nil else { return }
        completion = nil
        cleanup()
    }

    @objc private func interrupted(_ notification: Notification) {
        guard let type = notification.userInfo?[AVAudioSessionInterruptionTypeKey] as? UInt,
              type == AVAudioSession.InterruptionType.began.rawValue else { return }
        if requestID != nil {
            complete(.failure(Self.error("録音が中断されました。もう一度録音してください。")))
        }
    }

    func audioRecorderDidFinishRecording(_ recorder: AVAudioRecorder, successfully flag: Bool) {
        guard self.recorder === recorder else { return }
        complete(Result {
            guard flag else { throw Self.error("録音を完了できませんでした。") }
            let file = try AVAudioFile(forReading: recorder.url, commonFormat: .pcmFormatInt16, interleaved: true)
            guard file.length > 0, file.length <= Int64(AVAudioFrameCount.max),
                  let buffer = AVAudioPCMBuffer(
                      pcmFormat: file.processingFormat,
                      frameCapacity: AVAudioFrameCount(file.length)
                  )
            else {
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
        meteringTimer?.invalidate()
        meteringTimer = nil
        levels = Array(repeating: 0, count: 40)
        recorder?.delegate = nil
        recorder?.stop()
        recorder = nil
        isRecording = false
        requestingPermission = false
        if let fileURL {
            try? FileManager.default.removeItem(at: fileURL)
        }
        fileURL = nil
        if audioSessionActive {
            try? AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation)
            audioSessionActive = false
        }
        if let previousAudioConfiguration {
            try? previousAudioConfiguration.restore()
            self.previousAudioConfiguration = nil
        }
    }

    private static func error(_ message: String) -> Error {
        NSError(domain: "Dictation", code: 1, userInfo: [NSLocalizedDescriptionKey: message])
    }
}

struct AudioSessionConfiguration {
    let category: AVAudioSession.Category
    let mode: AVAudioSession.Mode
    let options: AVAudioSession.CategoryOptions

    func restore() throws {
        try AVAudioSession.sharedInstance().setCategory(category, mode: mode, options: options)
    }
}

/// Owns one temporary directory while a preview or share sheet still uses its file.
final class SharedFile: Identifiable {
    let id = UUID()
    let url: URL

    init(url: URL) {
        self.url = url
    }

    deinit {
        try? FileManager.default.removeItem(at: url.deletingLastPathComponent())
    }
}

/// Keep UIKit's Copy command available beside primary actions without duplicating it.
@MainActor func splitCopyCommand(_ elements: [UIMenuElement]) -> (copy: UICommand?, others: [UIMenuElement]) {
    var copy: UICommand?
    let others = elements.compactMap { element -> UIMenuElement? in
        if let command = element as? UICommand,
           command.action == #selector(UIResponderStandardEditActions.copy(_:)) {
            copy = copy ?? command
            return nil
        }
        if let menu = element as? UIMenu {
            let children = splitCopyCommand(menu.children)
            copy = copy ?? children.copy
            return children.others.isEmpty ? nil : menu.replacingChildren(children.others)
        }
        return element
    }
    return (copy, others)
}

struct ShareSheet: UIViewControllerRepresentable {
    let file: SharedFile

    func makeUIViewController(context _: Context) -> UIActivityViewController {
        UIActivityViewController(activityItems: [file.url], applicationActivities: nil)
    }

    func updateUIViewController(_: UIActivityViewController, context _: Context) {}
}

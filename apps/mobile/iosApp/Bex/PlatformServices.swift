import AgentCore
import AVFoundation
import Combine
import AudioToolbox
import Security
import SwiftUI
import UIKit
import UserNotifications

enum LocalNotifications {
    private static var authorized: Bool?
    private static var authorizationRequestInFlight = false
    private struct Pending {
        let title: String
        let body: String
        let sound: Bool
        let threadId: String?
        let deepLink: String?
        let badgeCount: UInt32
        let kind: String?
        let soundKind: String?
    }
    private static var pending: [Pending] = []

    private static func schedule(_ pendingRequest: Pending) {
        let content = UNMutableNotificationContent()
        content.title = pendingRequest.title
        content.body = pendingRequest.body
        content.sound = pendingRequest.sound ? .default : nil
        content.badge = NSNumber(value: pendingRequest.badgeCount)
        if let threadId = pendingRequest.threadId {
            // Keep same-named threads from different Hosts in separate
            // notification groups; the deep link carries the owning route.
            content.threadIdentifier = pendingRequest.deepLink ?? threadId
            var userInfo: [AnyHashable: Any] = [
                "threadId": threadId,
                "deeplink": pendingRequest.deepLink ?? "remote-agent://thread/\(threadId)"
            ]
            if let kind = pendingRequest.kind { userInfo["kind"] = kind }
            if let soundKind = pendingRequest.soundKind { userInfo["soundKind"] = soundKind }
            content.userInfo = userInfo
        }
        let trigger = UNTimeIntervalNotificationTrigger(timeInterval: 0.1, repeats: false)
        let notificationRequest = UNNotificationRequest(
            identifier: "remoteagent.local.\(UUID().uuidString)", content: content, trigger: trigger
        )
        UNUserNotificationCenter.current().add(notificationRequest)
    }

    static func deliver(
        title: String,
        body: String,
        sound: Bool,
        threadId: String? = nil,
        deepLink: String? = nil,
        badgeCount: UInt32 = 0,
        kind: String? = nil,
        soundKind: String? = nil
    ) {
        let request = Pending(
            title: title, body: body, sound: sound, threadId: threadId,
            deepLink: deepLink, badgeCount: badgeCount, kind: kind, soundKind: soundKind
        )
        if authorized == true {
            schedule(request)
            return
        }
        guard authorized != false else {
            pending.removeAll()
            return
        }
        if let deepLink {
            pending.removeAll { $0.deepLink == deepLink }
        } else {
            pending.removeAll { $0.threadId == threadId }
        }
        pending.append(request)
        guard authorized == nil, !authorizationRequestInFlight else { return }
        authorizationRequestInFlight = true
        UNUserNotificationCenter.current().requestAuthorization(options: [.alert, .sound, .badge]) {
            granted, _ in
            DispatchQueue.main.async {
                Self.authorizationRequestInFlight = false
                Self.authorized = granted
                if !granted { Self.pending.removeAll() }
                Self.flushPendingIfAuthorized()
            }
        }
    }

    static func refreshAuthorization() {
        UNUserNotificationCenter.current().getNotificationSettings { settings in
            DispatchQueue.main.async {
                switch settings.authorizationStatus {
                case .authorized, .provisional:
                    Self.authorized = true
                case .denied:
                    Self.authorized = false
                    Self.pending.removeAll()
                case .notDetermined:
                    // The permission sheet may still be visible. Keep
                    // queued events until the request completion reports a
                    // real grant or denial.
                    if Self.authorized != true { Self.authorized = nil }
                @unknown default:
                    Self.authorized = false
                    Self.pending.removeAll()
                }
                Self.flushPendingIfAuthorized()
            }
        }
    }

    private static func flushPendingIfAuthorized() {
        guard authorized == true else { return }
        let requests = pending
        pending.removeAll()
        requests.forEach(schedule)
    }

    /// Applies the aggregate core attention count even when focus/selection
    /// cleared it without producing a new notification event.
    static func updateBadge(_ count: UInt32) {
        UIApplication.shared.applicationIconBadgeNumber = Int(count)
    }

    static func clearDelivered() {
        UNUserNotificationCenter.current().removeAllDeliveredNotifications()
        updateBadge(0)
    }

    static func playSound(soundKind: String? = nil) {
        // The foreground path has no UNNotificationContent to carry the
        // event's sound kind, so select the corresponding system cue here.
        let kind = soundKind?.split(separator: ".").last.map(String.init)?.uppercased()
        AudioServicesPlaySystemSound(kind == "COMPLETION" ? 1004 : 1007)
    }
}

struct RemoteAgentSharePayload: Codable {
    let text: String
    let urls: [String]

    var content: ShareContent {
        ShareContent(text: text, urls: urls)
    }
}

enum RemoteAgentShareInbox {
    static let appGroupIdentifier = "group.com.ttizze.b-codex"
    private static let directoryName = "incoming-shares"

    struct Pending {
        let file: URL
        let content: ShareContent
    }

    private static func directory() -> URL? {
        FileManager.default
            .containerURL(forSecurityApplicationGroupIdentifier: appGroupIdentifier)?
            .appendingPathComponent(directoryName, isDirectory: true)
    }

    static func pending() -> [Pending] {
        guard let directory,
              let files = try? FileManager.default.contentsOfDirectory(
                  at: directory,
                  includingPropertiesForKeys: [.contentModificationDateKey],
                  options: [.skipsHiddenFiles]
              )
        else { return [] }
        return files
            .filter { $0.pathExtension == "json" }
            .sorted { $0.lastPathComponent < $1.lastPathComponent }
            .compactMap { file in
                guard let data = try? Data(contentsOf: file),
                      let payload = try? JSONDecoder().decode(RemoteAgentSharePayload.self, from: data)
                else { return nil }
                return Pending(file: file, content: payload.content)
            }
    }

    static func remove(_ file: URL) {
        try? FileManager.default.removeItem(at: file)
    }
}

/// Where each Host's state lives; core keeps the state file written. The model
/// preferences every Host shares stay in the app's defaults.
enum SnapshotFiles {
    private static func location(_ host: String) throws -> URL {
        let directory = try FileManager.default.url(
            for: .applicationSupportDirectory, in: .userDomainMask,
            appropriateFor: nil, create: true
        ).appendingPathComponent("orchestration", isDirectory: true)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        let name = Data(host.utf8).base64EncodedString()
            .replacingOccurrences(of: "/", with: "_")
        return directory.appendingPathComponent(name).appendingPathExtension("json")
    }

    static func stateFile(_ host: String) throws -> String {
        try location(host).path
    }

    static func cacheDirectory(_ host: String) throws -> String {
        let directory = try location(host).deletingPathExtension().appendingPathExtension("cache")
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        return directory.path
    }

    static func diagnosticsDirectory(_ host: String) throws -> String {
        try location(host).deletingPathExtension().appendingPathExtension("diagnostics").path
    }

    static func modelDefaults() -> Data {
        UserDefaults.standard.data(forKey: "bex.orchestration-model-defaults") ?? Data()
    }

    static func saveModelPreferences(_ snapshot: AgentCore.Snapshot) async throws {
        try await Task.detached(priority: .utility) {
            try UserDefaults.standard.set(
                snapshot.serializeModelPreferences(),
                forKey: "bex.orchestration-model-defaults"
            )
        }.value
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
final class DictationRecorder: NSObject, ObservableObject, @preconcurrency AVAudioRecorderDelegate {
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
        AVAudioApplication.requestRecordPermission { [weak self] granted in
            DispatchQueue.main.async {
                guard let self, self.requestID == id else { return }
                self.requestingPermission = false
                guard granted else {
                    self.complete(.failure(Self.error("設定アプリでBexのマイクへのアクセスを許可してください。")))
                    return
                }
                do {
                    let session = AVAudioSession.sharedInstance()
                    self.previousAudioConfiguration = AudioSessionConfiguration(session)
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
        NSError(domain: "BexDictation", code: 1, userInfo: [NSLocalizedDescriptionKey: message])
    }
}

private struct AudioSessionConfiguration {
    let category: AVAudioSession.Category
    let mode: AVAudioSession.Mode
    let options: AVAudioSession.CategoryOptions

    init(_ session: AVAudioSession) {
        category = session.category
        mode = session.mode
        options = session.categoryOptions
    }

    func restore() throws {
        try AVAudioSession.sharedInstance().setCategory(category, mode: mode, options: options)
    }
}

/// The taps that confirm actions, as the mobile app plays them.
@MainActor
enum Haptics {
    /// A thread list action or a copy.
    static func light() {
        UIImpactFeedbackGenerator(style: .light).impactOccurred()
    }

    /// A swipe reaching the action a full swipe commits.
    static func medium() {
        UIImpactFeedbackGenerator(style: .medium).impactOccurred()
    }

    /// A choice, a disclosure, a navigation or streaming text.
    static func selection() {
        UISelectionFeedbackGenerator().selectionChanged()
    }

    /// Copies `text` to the clipboard with the light tap.
    static func copy(_ text: String) {
        UIPasteboard.general.string = text
        light()
    }
}

extension UIResponder {
    private nonisolated(unsafe) weak static var found: UIResponder?

    /// The focused text input is still composing (marked) text, such as kana
    /// awaiting conversion, so Return confirms it rather than sending.
    @MainActor static var isComposingText: Bool {
        found = nil
        UIApplication.shared.sendAction(#selector(UIResponder.captureFirstResponder), to: nil, from: nil, for: nil)
        return (found as? UITextInput)?.markedTextRange != nil
    }

    @objc private func captureFirstResponder() {
        UIResponder.found = self
    }
}

struct SharedFile: Identifiable {
    let id = UUID()
    let url: URL
}

struct FileShareSheet: UIViewControllerRepresentable {
    let url: URL
    func makeUIViewController(context _: Context) -> UIActivityViewController {
        let controller = UIActivityViewController(activityItems: [url], applicationActivities: nil)
        controller.completionWithItemsHandler = { _, _, _, _ in
            try? FileManager.default.removeItem(at: url.deletingLastPathComponent())
        }
        return controller
    }

    func updateUIViewController(_: UIActivityViewController, context _: Context) {}
}

/// Runs `body` with a fresh private temporary directory and removes the
/// directory when `body` fails, so partial files never outlive an error.
func inTemporaryDirectory<T>(_ body: (URL) throws -> T) throws -> T {
    let directory = try makeTemporaryDirectory()
    do {
        return try body(directory)
    } catch {
        try? FileManager.default.removeItem(at: directory)
        throw error
    }
}

func inTemporaryDirectory<T>(_ body: (URL) async throws -> T) async throws -> T {
    let directory = try makeTemporaryDirectory()
    do {
        return try await body(directory)
    } catch {
        try? FileManager.default.removeItem(at: directory)
        throw error
    }
}

private func makeTemporaryDirectory() throws -> URL {
    let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString, isDirectory: true)
    try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
    return directory
}

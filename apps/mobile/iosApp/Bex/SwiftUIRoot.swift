import AVFoundation
import Combine
import ImageIO
import PhotosUI
import SwiftUI
import UIKit
import UniformTypeIdentifiers
import RemoteAgentMobile

@MainActor
final class BexConversationModel: ObservableObject {
    @Published var thread: IosThreadView?

    init(thread: IosThreadView?) { self.thread = thread }
}

/// Native presentation only: state transitions, cache reconciliation, and RPC
/// orchestration remain inside IosAppController.
@MainActor
final class BexAppViewModel: ObservableObject {
    @Published private(set) var state: IosAppViewState
    let conversation: BexConversationModel
    @Published var isScanning = false
    @Published var transferError: String?
    @Published var transferring = false
    @Published var sending = false
    @Published private(set) var transcribing = false
    @Published private(set) var models: [CodexModel] = []
    @Published private(set) var modelError: String?
    @Published private(set) var loadingModels = false
    @Published private var modelChoices = UserDefaults.standard.dictionary(forKey: "bex.models.v1") as? [String: String] ?? [:]
    @Published private var effortChoices = UserDefaults.standard.dictionary(forKey: "bex.efforts.v1") as? [String: String] ?? [:]
    var selectedModel: String { modelChoices[state.selectedProfileId ?? ""] ?? "" }
    var selectedEffort: String { effortChoices[state.selectedProfileId ?? ""] ?? "" }
    var currentModel: CodexModel? { models.first { $0.model == selectedModel } }

    func chooseModel(_ value: String) {
        guard let host = state.selectedProfileId else { return }
        modelChoices[host] = value
        effortChoices[host] = models.first { $0.model == value }?.defaultReasoningEffort ?? ""
        persistTurnOptions()
    }

    func chooseEffort(_ value: String) {
        guard let host = state.selectedProfileId else { return }
        effortChoices[host] = value.isEmpty ? (currentModel?.defaultReasoningEffort ?? "") : value
        persistTurnOptions()
    }

    private func persistTurnOptions() {
        UserDefaults.standard.set(modelChoices, forKey: "bex.models.v1")
        UserDefaults.standard.set(effortChoices, forKey: "bex.efforts.v1")
        applyTurnOptions()
    }

    private func applyTurnOptions() {
        guard let host = state.selectedProfileId else { return }
        controller.setTurnOptions(hostIdentity: host, model: selectedModel.isEmpty ? nil : selectedModel,
                                  effort: selectedEffort.isEmpty ? currentModel?.defaultReasoningEffort : selectedEffort)
    }

    func loadModels() {
        guard state.isConnected, let host = state.selectedProfileId, !loadingModels else { return }
        loadingModels = true
        modelError = nil
        controller.listModels { [weak self] models, error in
            guard let self, self.state.selectedProfileId == host else { return }
            self.loadingModels = false
            self.models = models ?? []
            self.modelError = error
        }
    }

    @Published private var drafts = UserDefaults.standard.dictionary(forKey: "bex.drafts.v4") as? [String: String] ?? [:]
    @Published private var staged: [String: [StagedAttachment]] = {
        guard let data = UserDefaults.standard.data(forKey: "bex.attachments.v4") else { return [:] }
        return (try? JSONDecoder().decode([String: [StagedAttachment]].self, from: data)) ?? [:]
    }() {
        didSet { UserDefaults.standard.set(try? JSONEncoder().encode(staged), forKey: "bex.attachments.v4") }
    }

    var draftKey: String { (state.selectedProfileId ?? "") + ":" + (state.selectedThreadId ?? "new:\(state.workingDirectory)") }
    var draft: String {
        get { drafts[draftKey] ?? "" }
        set { drafts[draftKey] = newValue; UserDefaults.standard.set(drafts, forKey: "bex.drafts.v4") }
    }
    var attachments: [StagedAttachment] { staged[draftKey] ?? [] }
    var cwd: String { state.threads.first { $0.id == state.selectedThreadId }?.workingDirectory ?? state.workingDirectory }


    private let controller = IosAppController()

    init() {
        state = controller.currentState()
        conversation = BexConversationModel(thread: controller.currentThread())
        applyTurnOptions()
        if state.isConnected { loadModels() }
        controller.observe { [weak self] state, thread in
            DispatchQueue.main.async {
                guard let self else { return }
                let changedHost = self.state.selectedProfileId != state.selectedProfileId
                let connected = !self.state.isConnected && state.isConnected
                if self.state !== state { self.state = state }
                if self.conversation.thread !== thread { self.conversation.thread = thread }
                if changedHost {
                    self.models = []
                    self.loadingModels = false
                    self.modelError = nil
                }
                if changedHost { self.applyTurnOptions() }
                if changedHost || connected { self.loadModels() }
            }
        }
    }

    deinit {
        controller.close()
    }

    func openPairing() { controller.openPairing() }
    func dismissPairing() { controller.dismissPairing() }
    func showProfiles() { controller.showProfiles() }
    func selectProfile(_ id: String) { controller.selectProfile(hostIdentity: id) }
    func pair(_ contents: String) { controller.pair(contents: contents, nowMs: Int64(Date().timeIntervalSince1970 * 1_000)) }
    func connect() { controller.connect() }
    func refreshTaskList() { controller.refreshTaskList() }
    func expandTaskList(projects: Bool = false, projectId: String? = nil) {
        controller.expandTaskList(projects: projects, projectId: projectId)
    }
    func searchTaskList(_ term: String) { controller.searchTaskList(term: term) }
    func openNewThread(cwd: String) { controller.openNewThread(cwd: cwd) }
    fileprivate func openNewThread(on profileId: String) {
        guard profileId != state.selectedProfileId else { return }
        controller.selectProfile(hostIdentity: profileId)
        controller.openNewThread(cwd: "")
    }
    func openThread(_ id: String) { controller.openThread(threadId: id) }
    func loadOlderHistory(_ turnId: String?) { controller.loadOlderHistory(turnId: turnId) }
    func showThreadList() { controller.showThreadList() }
    func send() {
        let text = draft
        send(text, originalDraft: text, dictatedText: nil, files: attachments,
             key: draftKey, host: state.selectedProfileId ?? "")
    }

    private func send(_ text: String, originalDraft: String, dictatedText: String?, files: [StagedAttachment], key: String, host: String) {
        applyTurnOptions()
        sending = true
        controller.sendTurn(text: text, attachments: files.map { CodexAttachment(path: $0.path, name: $0.name, isImage: $0.isImage) }) { [weak self] accepted, threadId in
            guard let self else { return }
            self.sending = false
            let destination = threadId.map { host + ":" + $0 } ?? key
            if accepted.boolValue {
                for draftKey in Set([key, destination]) {
                    if self.drafts[draftKey] == originalDraft { self.drafts[draftKey] = "" }
                    self.staged[draftKey]?.removeAll { attachment in files.contains { $0.id == attachment.id } }
                }
            } else {
                if destination != key {
                    if self.drafts[destination, default: ""].isEmpty { self.drafts[destination] = originalDraft }
                    if self.staged[destination, default: []].isEmpty { self.staged[destination] = files }
                }
                if let dictatedText {
                    for draftKey in Set([key, destination]) {
                        self.drafts[draftKey] = Self.appendingDictation(dictatedText, to: self.drafts[draftKey, default: ""])
                    }
                }
            }
            UserDefaults.standard.set(self.drafts, forKey: "bex.drafts.v4")
        }
    }

    func removeAttachment(_ id: UUID) { staged[draftKey]?.removeAll { $0.id == id } }

    func transcribe(_ audio: Data, draftKey key: String, sendImmediately: Bool) {
        guard !transcribing, key == draftKey else { return }
        let originalDraft = draft
        let files = attachments
        let host = state.selectedProfileId ?? ""
        transcribing = true
        transferError = nil
        controller.transcribeAudio(audio: audio.base64EncodedString()) { [weak self] text, error in
            guard let self else { return }
            self.transcribing = false
            if let text {
                if sendImmediately && self.draftKey == key {
                    self.send(Self.appendingDictation(text, to: originalDraft), originalDraft: originalDraft,
                              dictatedText: text, files: files, key: key, host: host)
                } else {
                    self.drafts[key] = Self.appendingDictation(text, to: self.drafts[key, default: ""])
                    UserDefaults.standard.set(self.drafts, forKey: "bex.drafts.v4")
                    if sendImmediately { self.transferError = "会話が切り替わったため送信せず、元の会話の下書きに文字起こしを保存しました。" }
                }
            } else if self.draftKey == key {
                self.transferError = error ?? "文字起こしできませんでした。"
            }
        }
    }

    private static func appendingDictation(_ text: String, to draft: String) -> String {
        draft + (draft.isEmpty || draft.last?.isWhitespace == true ? "" : "\n") + text
    }

    func attach(_ url: URL, temporaryDirectory: URL? = nil, completion: @escaping () -> Void = {}) {
        let key = draftKey
        let directory = cwd
        let access = url.startAccessingSecurityScopedResource()
        transferring = true
        transferError = nil
        controller.transfer(paramsJson: jsonString(["direction": "upload", "source": url.path, "directory": directory, "fileName": url.lastPathComponent])) { [weak self] result, error in
            if access { url.stopAccessingSecurityScopedResource() }
            if let temporaryDirectory { try? FileManager.default.removeItem(at: temporaryDirectory) }
            defer { completion() }
            guard let self else { return }
            self.transferring = false
            self.transferError = error
            if let result, let path = jsonObject(result)["path"] as? String {
                let type = UTType(filenameExtension: url.pathExtension)
                self.staged[key, default: []].append(StagedAttachment(name: url.lastPathComponent, path: path, isImage: type?.conforms(to: .image) == true))
            }
        }
    }

    func respond(_ request: IosTurnRequestView, result: [String: Any], completion: @escaping (String?) -> Void) {
        controller.respond(requestIdJson: request.requestIdJson, responseJson: jsonString(result), completion: completion)
    }

    func workspace(_ method: String, _ params: [String: Any], completion: @escaping ([String: Any]?, String?) -> Void) {
        controller.workspaceRequest(method: method, paramsJson: jsonString(params)) { result, error in
            completion(result.map(jsonObject), error)
        }
    }

    func readItemDetails(threadId: String, turnId: String, itemId: String) async -> (String?, String?) {
        await withCheckedContinuation { continuation in
            controller.readItemDetails(threadId: threadId, turnId: turnId, itemId: itemId) { body, error in
                continuation.resume(returning: (body, error))
            }
        }
    }

    func download(_ path: String, completion: @escaping (URL?, String?) -> Void) {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString, isDirectory: true)
        do { try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true) }
        catch { completion(nil, error.localizedDescription); return }
        let target = directory.appendingPathComponent(URL(fileURLWithPath: path).lastPathComponent)
        controller.transfer(paramsJson: jsonString(["direction": "download", "source": path, "destination": target.path])) { result, error in
            completion(result == nil ? nil : target, error)
        }
    }
    func interrupt(_ turnId: String) { controller.interrupt(turnId: turnId) }

    func scanned(_ contents: String?) {
        isScanning = false
        guard let contents, !contents.isEmpty else { return }
        pair(contents)
    }
}

struct BexSwiftUIRoot: View {
    @StateObject private var model = BexAppViewModel()

    var body: some View {
        BexScreen(state: model.state, model: model)
            .sheet(isPresented: $model.isScanning) {
                BexQrScannerSheet { model.scanned($0) }
                    .interactiveDismissDisabled()
            }
    }
}

private struct BexScreen: View {
    let state: IosAppViewState
    @ObservedObject var model: BexAppViewModel

    var body: some View {
        NavigationView {
            Group {
                if state.profiles.isEmpty {
                    PairingScreen(state: state, model: model)
                } else {
                    ProfilesScreen(state: state, model: model)
                        .background(
                            NavigationLink(isActive: Binding(
                                get: { state.screen == .threads || state.screen == .thread },
                                set: { if !$0 && model.state.selectedProfileId != nil { model.showProfiles() } }
                            )) {
                                threadList
                            } label: { EmptyView() }
                        )
                        .sheet(isPresented: Binding(
                            get: { state.screen == .pairing },
                            set: { if !$0 && model.state.screen == .pairing { model.dismissPairing() } }
                        )) {
                            NavigationView { PairingScreen(state: state, model: model) }
                                .navigationViewStyle(StackNavigationViewStyle())
                        }
                }
            }
            .navigationBarTitleDisplayMode(.inline)
        }
        .navigationViewStyle(StackNavigationViewStyle())
        .preferredColorScheme(.dark)
    }

    private var threadList: some View {
        ThreadsScreen(state: state, model: model)
            .background(
                NavigationLink(isActive: Binding(
                    get: { state.screen == .thread },
                    set: { if !$0 && model.state.screen == .thread { model.showThreadList() } }
                )) {
                    ThreadScreen(state: state, model: model, conversation: model.conversation)
                        .safeAreaInset(edge: .top, spacing: 0) { connectionErrorBanner }
                } label: { EmptyView() }
            )
            .safeAreaInset(edge: .top, spacing: 0) { connectionErrorBanner }
    }

    @ViewBuilder private var connectionErrorBanner: some View {
        if state.selectedProfileId != nil && state.screen != .pairing,
           let error = state.connectionError {
            HStack(spacing: 8) {
                Text(error).font(.caption).lineLimit(2)
                Spacer()
                Button("再接続") { model.connect() }
            }
            .padding(10).background(.ultraThinMaterial)
            .accessibilityIdentifier("connection.error")
        }
    }

}

private struct PairingScreen: View {
    let state: IosAppViewState
    @ObservedObject var model: BexAppViewModel
    @State private var contents = ""
    @State private var showsManualPairing = false

    var body: some View {
        ScrollView {
            VStack(spacing: 24) {
                VStack(spacing: 14) {
                    Image(systemName: "terminal.fill")
                        .font(.system(size: 36, weight: .semibold))
                        .foregroundColor(.accentColor)
                        .frame(width: 76, height: 76)
                        .background(Color.accentColor.opacity(0.12))
                        .clipShape(RoundedRectangle(cornerRadius: 20, style: .continuous))
                    Text("PCとペアリング")
                        .font(.title2.weight(.bold))
                    Text("PCに表示されたQRコードを読み取ると、このiPhoneからCodexを操作できます。")
                        .font(.subheadline)
                        .multilineTextAlignment(.center)
                        .foregroundColor(.secondary)
                }

                Button { model.isScanning = true } label: {
                    Label("QRコードを読み取る", systemImage: "qrcode.viewfinder")
                        .font(.headline)
                        .frame(maxWidth: .infinity)
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.large)
                .accessibilityIdentifier("pairing.scan")

                DisclosureGroup("QRの内容を手入力", isExpanded: $showsManualPairing) {
                    VStack(alignment: .leading, spacing: 12) {
                        SecureField("ペアリング情報を貼り付け", text: $contents)
                            .font(.system(.footnote, design: .monospaced))
                            .frame(minHeight: 44)
                            .textFieldStyle(.roundedBorder)
                            .accessibilityIdentifier("pairing.contents")
                        Button("入力内容でペアリング") { model.pair(contents) }
                            .buttonStyle(.bordered)
                            .disabled(contents.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                            .accessibilityIdentifier("pairing.submit")
                    }
                    .padding(.top, 12)
                }
                .padding(16)
                .background(Color(UIColor.secondarySystemBackground))
                .clipShape(RoundedRectangle(cornerRadius: 16, style: .continuous))

                Text("ペアリング情報は接続時だけ使用し、QRの内容そのものは保存しません。")
                    .font(.caption)
                    .multilineTextAlignment(.center)
                    .foregroundColor(.secondary)

                if let error = state.pairingError {
                    BexNotice(text: error)
                }


            }
            .padding(.horizontal, 24)
            .padding(.vertical, 32)
        }
        .background(Color(UIColor.systemGroupedBackground).ignoresSafeArea())
        .navigationTitle("Bex")
        .toolbar {
            ToolbarItem(placement: .cancellationAction) {
                if !state.profiles.isEmpty {
                    Button("キャンセル") { model.dismissPairing() }
                        .accessibilityIdentifier("pairing.cancel")
                }
            }
        }
    }
}

private struct ProfilesScreen: View {
    let state: IosAppViewState
    @ObservedObject var model: BexAppViewModel

    var body: some View {
        List {
            ForEach(state.profiles, id: \.id) { profile in
                Button { model.selectProfile(profile.id) } label: {
                    VStack(alignment: .leading, spacing: 4) {
                        Text(profile.name).font(.headline)
                        Text(profile.hostIdentity).font(.caption).foregroundColor(.secondary)
                    }
                }
                .accessibilityIdentifier("profiles.\(profile.id)")
            }
            Section {
                Button("PCを追加") { model.openPairing() }
                    .accessibilityIdentifier("profiles.add")
            }
        }
        .navigationTitle("PC Hosts")
    }
}

private struct ThreadsScreen: View {
    let state: IosAppViewState
    @ObservedObject var model: BexAppViewModel
    @State private var search = ""
    @State private var collapsedProjectIds = Set<String>()

    @ViewBuilder var body: some View {
        if #available(iOS 26.0, *) {
            taskList.toolbar {
                DefaultToolbarItem(kind: .search, placement: .bottomBar)
                ToolbarSpacer(.flexible, placement: .bottomBar)
            }
        } else {
            taskList
        }
    }

    @ViewBuilder private var taskList: some View {
        let knownProjects = Set(state.projects.map(\.id))
        let groupedThreads = Dictionary(grouping: visibleThreads) { thread in
            thread.projectId.flatMap { knownProjects.contains($0) ? $0 : nil }
        }
        let projects = search.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            ? state.projects : state.projects.filter { groupedThreads[$0.id] != nil }
        let chats = groupedThreads[nil] ?? []
        List {
            if state.threadLoadState == .failed {
                Section {
                    if let error = state.threadLoadError {
                        BexNotice(text: error)
                            .taskListRowStyle()
                    }
                    Button("再試行") { model.refreshTaskList() }
                        .accessibilityIdentifier("tasks.retry")
                        .taskListRowStyle()
                }
                .listSectionSeparator(.hidden)
            }
            if let notice = state.notice {
                Section {
                    BexNotice(text: notice)
                        .taskListRowStyle()
                }
                .listSectionSeparator(.hidden)
            }

            Section {
                Text("プロジェクト")
                    .font(.title2.weight(.bold))
                    .textCase(nil)
                    .foregroundColor(.primary)
                    .accessibilityIdentifier("tasks.projects")
                    .taskListRowStyle()
            }
            .listSectionSeparator(.hidden)

            ForEach(projects.prefix(Int(state.visibleProjectCount)), id: \.id) { project in
                Section {
                    HStack(spacing: 16) {
                        Button {
                            if collapsedProjectIds.contains(project.id) { collapsedProjectIds.remove(project.id) }
                            else { collapsedProjectIds.insert(project.id) }
                        } label: {
                            HStack(spacing: 16) {
                                Image(systemName: "folder")
                                    .font(.title3).frame(width: 24)
                                Text(project.name).font(.title3).lineLimit(1)
                                Spacer(minLength: 0)
                            }
                            .frame(minHeight: 44)
                            .contentShape(Rectangle())
                        }
                        .buttonStyle(.plain)
                        .accessibilityIdentifier("tasks.project.\(project.id)")
                        .accessibilityValue(collapsedProjectIds.contains(project.id) ? "閉じています" : "開いています")
                        Button { model.openNewThread(cwd: project.roots.first ?? "") } label: {
                            Image(systemName: "square.and.pencil").font(.title3)
                                .foregroundColor(.secondary).frame(width: 44, height: 44)
                        }
                        .buttonStyle(.plain)
                        .accessibilityLabel("\(project.name)で新しいタスク")
                        .accessibilityIdentifier("tasks.new.project.\(project.id)")
                    }
                    .taskListRowStyle()
                    if !collapsedProjectIds.contains(project.id) {
                        let threads = groupedThreads[project.id] ?? []
                        ForEach(threads, id: \.id) { thread in
                            ThreadListRow(thread: thread, indented: true) { model.openThread(thread.id) }
                        }
                        if state.moreProjectIds.contains(project.id) {
                            Button("もっと見る") { model.expandTaskList(projectId: project.id) }
                                .padding(.leading, 40)
                                .disabled(state.loadingMoreThreads)
                                .accessibilityIdentifier("tasks.project.\(project.id).more")
                                .taskListRowStyle()
                        }
                    }
                }
                .listSectionSeparator(.hidden)
            }

            if state.hasMoreProjects {
                Section {
                    Button("もっと見る") { model.expandTaskList(projects: true) }
                        .disabled(state.loadingMoreThreads)
                        .accessibilityIdentifier("tasks.projects.more")
                        .taskListRowStyle()
                }
                .listSectionSeparator(.hidden)
            }

            if state.threadLoadState == .ready && state.projects.isEmpty {
                Section {
                    Text("Codexに登録されたプロジェクトはありません")
                        .font(.subheadline)
                        .foregroundColor(.secondary)
                        .taskListRowStyle()
                }
                .listSectionSeparator(.hidden)
            }

            Section {
                if state.threadLoadState == .ready && chats.isEmpty {
                    Text("プロジェクトに属さないチャットはありません")
                        .font(.subheadline)
                        .foregroundColor(.secondary)
                        .accessibilityIdentifier("tasks.empty")
                        .taskListRowStyle()
                } else {
                    ForEach(chats, id: \.id) { thread in
                        ThreadListRow(thread: thread) { model.openThread(thread.id) }
                    }
                    if state.hasMoreChats {
                        Button("もっと見る") { model.expandTaskList() }
                            .disabled(state.loadingMoreThreads)
                            .accessibilityIdentifier("tasks.chats.more")
                            .taskListRowStyle()
                    }
                }
            } header: {
                Text("チャット")
                    .font(.title3.weight(.semibold))
                    .textCase(nil)
                    .foregroundColor(.primary)
            }
            .listSectionSeparator(.hidden)
        }
        .listStyle(.plain)
        .environment(\.defaultMinListRowHeight, 52)
        .background(Color(UIColor.systemBackground))
        .accessibilityIdentifier("tasks.list")
        .searchable(text: $search, placement: .toolbar, prompt: "チャットを検索")
        .refreshable { model.refreshTaskList() }
        .task(id: search) {
            do { try await Task.sleep(nanoseconds: 200_000_000) } catch { return }
            model.searchTaskList(search)
        }
        .navigationBarBackButtonHidden(true)
        .navigationTitle("リモート")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .principal) {
                VStack(spacing: 1) {
                    Text("リモート").font(.headline)
                    HStack(spacing: 5) {
                        if state.isConnecting || state.threadLoadState == .loading {
                            ProgressView().controlSize(.mini)
                                .accessibilityLabel(state.isConnecting ? "接続中" : "読み込み中")
                                .accessibilityIdentifier("connection.progress")
                        } else {
                            Circle().fill(state.isConnected ? Color.green : Color.secondary).frame(width: 6, height: 6)
                        }
                        Image(systemName: "laptopcomputer")
                        Text(state.selectedProfileName ?? "PC Host").lineLimit(1)
                    }
                    .font(.caption)
                    .foregroundColor(.secondary)
                    .accessibilityElement(children: .combine)
                    .accessibilityLabel("\(state.selectedProfileName ?? "PC Host")、\(state.isConnected ? "接続済み" : "未接続")")
                }
            }
            ToolbarItem(placement: .bottomBar) {
                Button { model.openNewThread(cwd: "") } label: {
                    Image(systemName: "square.and.pencil")
                }
                .buttonStyle(.borderedProminent)
                .tint(.primary)
                .accessibilityLabel("プロジェクトなしで新しいタスク")
                .accessibilityIdentifier("tasks.new.chat")
            }
            ToolbarItem(placement: .navigationBarLeading) {
                Button { model.showProfiles() } label: { Image(systemName: "line.3.horizontal") }
                    .accessibilityLabel("PC一覧")
                    .accessibilityIdentifier("tasks.hosts")
            }
            ToolbarItem(placement: .navigationBarTrailing) {
                Menu {
                    Button { model.refreshTaskList() } label: { Label("更新", systemImage: "arrow.clockwise") }
                        .disabled(state.threadLoadState == .loading)
                        .accessibilityIdentifier("tasks.refresh")
                    Button { model.showProfiles() } label: { Label("PC一覧", systemImage: "laptopcomputer") }
                } label: { Image(systemName: "ellipsis") }
                .accessibilityLabel("その他")
                .accessibilityIdentifier("tasks.menu")
            }
        }
    }

    private var visibleThreads: [IosThreadSummaryView] {
        let query = search.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !query.isEmpty else { return state.threads }
        return state.threads.filter {
            $0.title.localizedCaseInsensitiveContains(query) ||
                $0.preview.localizedCaseInsensitiveContains(query)
        }
    }


}

private extension View {
    func taskListRowStyle() -> some View {
        listRowSeparator(.hidden)
            .listRowInsets(EdgeInsets(top: 0, leading: 20, bottom: 0, trailing: 20))
            .listRowBackground(Color.clear)
    }
}

private struct ThreadListRow: View {
    let thread: IosThreadSummaryView
    var indented = false
    let open: () -> Void

    var body: some View {
        Button(action: open) {
            HStack(spacing: 10) {
                Text(thread.title).font(.title3).foregroundColor(.primary).lineLimit(1)
                Spacer()
                if thread.isActive { ProgressView().controlSize(.small) }
            }
            .padding(.leading, indented ? 40 : 0)
            .contentShape(Rectangle())
        }
        .accessibilityIdentifier("tasks.row.\(thread.id)")
        .taskListRowStyle()
    }
}

private struct ModelSettingsSheet: View {
    @ObservedObject var model: BexAppViewModel
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationView {
            Form {
                Section {
                    NavigationLink {
                        Form { TurnOptionsPicker(model: model, conversation: model.conversation) }
                            .navigationTitle("モデルの詳細設定")
                            .navigationBarTitleDisplayMode(.inline)
                            .toolbar { ToolbarItem(placement: .confirmationAction) { doneButton } }
                    } label: {
                        VStack(alignment: .leading, spacing: 4) {
                            Text(model.currentModel?.displayName ?? (model.selectedModel.isEmpty ? "現在の設定" : model.selectedModel))
                            Text(model.selectedEffort.isEmpty ? (model.currentModel?.defaultReasoningEffort ?? "") : model.selectedEffort)
                                .foregroundColor(.secondary)
                        }
                    }
                    .accessibilityIdentifier("model.details")
                }
                if let current = model.currentModel, !current.reasoningEfforts.isEmpty {
                    Section("推論の強度") {
                        Picker("推論の強度", selection: Binding(
                            get: { model.selectedEffort.isEmpty ? current.defaultReasoningEffort : model.selectedEffort },
                            set: model.chooseEffort
                        )) {
                            ForEach(current.reasoningEfforts, id: \.self) { effort in
                                Text(effort).tag(effort)
                            }
                        }
                        .pickerStyle(.segmented)
                        .accessibilityIdentifier("model.quick.effort")
                    }
                }
            }
            .navigationTitle("モデル設定")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .confirmationAction) { doneButton } }
        }
        .navigationViewStyle(StackNavigationViewStyle())
    }

    private var doneButton: some View {
        Button("完了") { dismiss() }.accessibilityIdentifier("model.close")
    }
}

private struct TurnOptionsPicker: View {
    @ObservedObject var model: BexAppViewModel
    @ObservedObject var conversation: BexConversationModel

    var body: some View {
        Group {
            Picker("モデル", selection: Binding(get: { model.selectedModel }, set: model.chooseModel)) {
                Text("現在の設定").tag("")
                if !model.selectedModel.isEmpty && model.currentModel == nil {
                    Text(model.selectedModel).tag(model.selectedModel)
                }
                ForEach(model.models, id: \.id) { Text($0.displayName).tag($0.model) }
            }
            .pickerStyle(.menu).accessibilityIdentifier("model.picker")
            if let current = model.currentModel {
                Picker("推論の強度", selection: Binding(get: { model.selectedEffort }, set: model.chooseEffort)) {
                    Text("既定 (\(current.defaultReasoningEffort))").tag("")
                    ForEach(current.reasoningEfforts, id: \.self) { Text($0).tag($0) }
                }
                .pickerStyle(.menu).accessibilityIdentifier("model.effort")
            }
            if model.loadingModels { ProgressView() }
            if let error = model.modelError {
                HStack {
                    Text(error).font(.caption).foregroundColor(.secondary).lineLimit(2)
                    Button("再読込") { model.loadModels() }
                }
            }
            if conversation.thread?.turns.contains(where: { $0.isInProgress }) == true {
                Text("実行中の追加入力には現在の設定が使われます")
                    .font(.caption2).foregroundColor(.secondary)
            }
        }
    }
}

private struct ThreadScreen: View {
    let state: IosAppViewState
    @ObservedObject var model: BexAppViewModel
    @ObservedObject var conversation: BexConversationModel
    @StateObject private var dictation = DictationRecorder()
    @State private var sendRecordedText = false
    @State private var importing = false
    @State private var showingPhotos = false
    @State private var showingCamera = false
    @State private var preparingMedia = false
    @State private var showingFiles = false
    @State private var showingModelSettings = false
    @State private var scrollViewportHeight: CGFloat = 0
    @State private var isFollowingLatest = true
    @StateObject private var scrollPosition = ConversationScrollPosition()
    @State private var scrollingToOlder = false
    @State private var historyBoundaries = [String: CGFloat]()
    @State private var historyRequestPending = false
    @State private var expandedItemIds = Set<String>()
    @State private var activityExpansionOverrides = [String: (status: String, expanded: Bool)]()
    @State private var opensDiff = false
    @State private var review: (files: Int, additions: Int, deletions: Int)?
    @FocusState private var composerFocused: Bool

    private var reviewVersion: String {
        model.cwd + (conversation.thread?.turns.map { "\($0.id):\($0.status)" }.joined(separator: ",") ?? "")
    }
    private var project: IosProjectView? {
        state.projects.first { $0.roots.contains(model.cwd) }
    }


    // Give native list virtualization one stable identity per conversation row.
    // Whole-turn containers and size-change anchoring loop on large histories.
    private func activityIsExpanded(_ turn: IosTurnView) -> Bool {
        guard let override = activityExpansionOverrides[turn.id], override.status == turn.status else {
            return turn.activityInitiallyExpanded
        }
        return override.expanded
    }

    private func conversationRows(_ thread: IosThreadView) -> [ThreadConversationRow] {
        var rows = [ThreadConversationRow]()
        rows.reserveCapacity(thread.turns.reduce(0) { $0 + $1.userMessages.count + $1.activityItems.count + $1.responses.count + 4 })
        if thread.hasOlderTurns { rows.append(.init(id: "history-older-turns", content: .olderTurns)) }
        for turn in thread.turns {
            if let opening = turn.openingUserMessage { rows.append(.init(id: "opening:" + turn.id, content: .user(opening))) }
            if turn.hasOlderItems { rows.append(.init(id: "history-gap:" + turn.id, content: .olderItems(turn.turnId))) }
            for item in turn.userMessages { rows.append(.init(id: "history-item:" + item.id, content: .user(item))) }
            if turn.activitySummary != nil {
                rows.append(.init(id: turn.id, content: .activityHeader(turn)))
                if activityIsExpanded(turn) {
                    for item in turn.activityItems { rows.append(.init(id: "history-item:" + item.id, content: .activity(item, turn.turnId))) }
                }
            }
            for request in turn.pendingRequests { rows.append(.init(id: "history-request:" + request.id, content: .request(request))) }
            if let error = turn.error { rows.append(.init(id: "history-error:" + turn.id, content: .error(error))) }
            for item in turn.responses { rows.append(.init(id: "history-item:" + item.id, content: .response(item))) }
        }
        for item in thread.queuedMessages { rows.append(.init(id: item.id, content: .queued(item))) }
        return rows
    }

    @ViewBuilder
    private func conversationRow(_ row: ThreadConversationRow) -> some View {
        switch row.content {
        case .olderTurns: historyBoundary(nil)
        case .olderItems(let turnId): historyBoundary(turnId)
        case .user(let item): ThreadMessageRow(item: item, isUser: true, model: model).padding(.top, 16)
        case .response(let item): ThreadMessageRow(item: item, isUser: false, model: model)
        case .activityHeader(let turn):
            let expanded = activityIsExpanded(turn)
            if turn.activityCanCollapse {
                Button {
                    scrollPosition.stopFollowingLatest()
                    activityExpansionOverrides[turn.id] = (turn.status, !expanded)
                } label: {
                    ThreadActivityHeader(turn: turn, expanded: expanded)
                }
                .buttonStyle(.plain)
                .accessibilityIdentifier("turn.activity." + turn.id)
            } else {
                ThreadActivityHeader(turn: turn, expanded: expanded).accessibilityIdentifier("turn.activity." + turn.id)
            }
        case .activity(let item, let turnId):
            ThreadItemRow(item: item, model: model, isExpanded: expandedItemIds.contains(item.id),
                toggleExpanded: {
                    scrollPosition.stopFollowingLatest()
                    if expandedItemIds.contains(item.id) { expandedItemIds.remove(item.id) }
                    else { expandedItemIds.insert(item.id) }
                },
                loadDetails: { await model.readItemDetails(threadId: conversation.thread?.id ?? "", turnId: turnId, itemId: item.id) })
        case .request(let request): ThreadRequestRow(request: request, model: model)
        case .error(let error): ThreadErrorRow(error: error)
        case .queued(let item):
            VStack(alignment: .leading, spacing: 6) {
                Text("順番待ち").font(.caption).foregroundColor(.secondary)
                ThreadMessageRow(item: item, isUser: true, model: model)
            }
        }
    }

    private func loadVisibleHistory() {
        guard scrollingToOlder, !historyRequestPending, !state.loadingHistory else { return }
        if let boundary = historyBoundaries.filter({ $0.value >= 0 && $0.value < scrollViewportHeight * 0.6 })
            .min(by: { $0.value < $1.value }) {
            let turnId = boundary.key == "older-turns" ? nil : boundary.key
            requestHistory(turnId)
        }
    }

    private func requestHistory(_ turnId: String?) {
        guard !state.loadingHistory else { return }
        scrollPosition.stopFollowingLatest()
        scrollingToOlder = false
        historyRequestPending = true
        model.loadOlderHistory(turnId)
    }

    private func historyBoundary(_ turnId: String?) -> some View {
        Button { requestHistory(turnId) } label: {
            HStack {
                if state.loadingHistory { ProgressView() }
                Text(turnId == nil ? "以前の会話を読み込む" : "途中の履歴を読み込む")
            }.frame(maxWidth: .infinity)
        }
        .disabled(state.loadingHistory)
        .accessibilityIdentifier("history.\(turnId ?? "older-turns")")
        .background(GeometryReader { geometry in
            Color.clear.preference(key: HistoryBoundaryPreferenceKey.self,
                value: [turnId ?? "older-turns": geometry.frame(in: .named("thread-scroll")).minY])
        })
    }

    var body: some View {
        VStack(spacing: 0) {
            if let notice = state.notice { BexNotice(text: notice).padding(.horizontal).padding(.top, 8) }
            if let thread = conversation.thread {
                List {
                    ForEach(conversationRows(thread)) { row in
                        conversationRow(row)
                            .taskListRowStyle()
                            .listRowInsets(EdgeInsets(top: 6, leading: 16, bottom: 6, trailing: 16))
                    }
                }
                .listStyle(.plain)
                .buttonStyle(.plain)
                .environment(\.defaultMinListRowHeight, 0)
                .background(ConversationScrollViewObserver(position: scrollPosition,
                    accessibilityValue: threadAccessibilityValue(thread),
                    onFollowingLatest: { isFollowingLatest = $0 },
                    onDirection: { upward in
                        if scrollingToOlder != upward { scrollingToOlder = upward }
                        loadVisibleHistory()
                    }))
                .overlay(alignment: .bottom) {
                    if !isFollowingLatest {
                        Button { scrollPosition.scrollToLatest(animated: true) } label: {
                            Image(systemName: "arrow.down").font(.title3.weight(.medium))
                        }
                        .buttonStyle(.bordered)
                        .buttonBorderShape(.capsule)
                        .controlSize(.large)
                        .accessibilityLabel("最新のメッセージへ")
                        .accessibilityIdentifier("task.latest")
                        .padding(.bottom, 6)
                    }
                }
                .coordinateSpace(name: "thread-scroll")
                .background(GeometryReader { geometry in
                    Color.clear.preference(key: ScrollViewportPreferenceKey.self,
                        value: geometry.size.height - geometry.safeAreaInsets.top - geometry.safeAreaInsets.bottom)
                })
                .onPreferenceChange(HistoryBoundaryPreferenceKey.self) { boundaries in
                    historyBoundaries = boundaries
                    loadVisibleHistory()
                }
                .onChange(of: state.loadingHistory) { if !$0 { historyRequestPending = false } }
                .onPreferenceChange(ScrollViewportPreferenceKey.self) { scrollViewportHeight = $0 }
                .onChange(of: thread.id) { _ in
                    scrollingToOlder = false
                    historyRequestPending = false
                    historyBoundaries.removeAll()
                    expandedItemIds.removeAll()
                    activityExpansionOverrides.removeAll()
                    scrollPosition.scrollToLatest(animated: false)
                }
            } else if state.isNewThread {
                Color.clear.frame(maxWidth: .infinity, maxHeight: .infinity)
                    .accessibilityIdentifier("task.empty")
            } else {
                ProgressView("タスクを読み込み中…")
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
        }
        .background(Color(UIColor.systemBackground))
        .safeAreaInset(edge: .bottom, spacing: 0) { composer }
        .onDisappear { dictation.cancel() }
        .onChange(of: model.draftKey) { _ in dictation.cancel() }
        .onChange(of: state.isConnected) { if !$0 { dictation.cancel() } }
        .fileImporter(isPresented: $importing, allowedContentTypes: [.item]) { result in
            switch result {
            case .success(let url): model.attach(url)
            case .failure(let error): model.transferError = error.localizedDescription
            }
        }
        .sheet(isPresented: $showingPhotos) {
            ChatPhotoPicker { providers in
                showingPhotos = false
                guard !providers.isEmpty else { return }
                preparingMedia = true
                model.transferError = nil
                importPhotos(providers[...], draftKey: model.draftKey)
            }
        }
        .fullScreenCover(isPresented: $showingCamera) {
            ChatCameraPicker { result in
                showingCamera = false
                finishMediaImport(result)
            }.ignoresSafeArea()
        }
        .sheet(isPresented: $showingFiles, onDismiss: refreshReview) {
            WorkspaceSheet(model: model, root: model.cwd, opensDiff: opensDiff)
        }
        .sheet(isPresented: $showingModelSettings) { ModelSettingsSheet(model: model) }
        .onAppear { if state.isNewThread { composerFocused = true } }
        .onChange(of: state.isNewThread) { if $0 { composerFocused = true } }
        .task(id: reviewVersion) { refreshReview() }
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .principal) {
                if !state.isNewThread { conversationTitle }
            }
            ToolbarItem(placement: .navigationBarTrailing) {
                if !state.isNewThread { conversationActions }
            }
        }
    }

    private func importPhotos(_ providers: ArraySlice<NSItemProvider>, draftKey: String) {
        guard model.draftKey == draftKey else {
            preparingMedia = false
            model.transferError = "チャットが切り替わったため、写真・動画をもう一度選択してください。"
            return
        }
        guard let provider = providers.first else { preparingMedia = false; return }
        let type = provider.hasItemConformingToTypeIdentifier(UTType.movie.identifier) ? UTType.movie : UTType.image
        provider.loadFileRepresentation(forTypeIdentifier: type.identifier) { url, error in
            let result = Result {
                guard let url else { throw error ?? CocoaError(.fileReadUnknown) }
                return try retainChatMedia(url)
            }
            DispatchQueue.main.async {
                guard model.draftKey == draftKey else {
                    if case .success(let url) = result { try? FileManager.default.removeItem(at: url.deletingLastPathComponent()) }
                    preparingMedia = false
                    model.transferError = "チャットが切り替わったため、写真・動画をもう一度選択してください。"
                    return
                }
                switch result {
                case .success(let url):
                    model.attach(url, temporaryDirectory: url.deletingLastPathComponent()) {
                        if model.transferError != nil { preparingMedia = false; return }
                        importPhotos(providers.dropFirst(), draftKey: draftKey)
                    }
                case .failure(let error):
                    preparingMedia = false
                    model.transferError = error.localizedDescription
                }
            }
        }
    }

    private func finishMediaImport(_ result: Result<URL?, Error>) {
        preparingMedia = false
        switch result {
        case .success(let url):
            if let url { model.attach(url, temporaryDirectory: url.deletingLastPathComponent()) }
        case .failure(let error): model.transferError = error.localizedDescription
        }
    }

    private func openCamera() {
        composerFocused = false
        guard UIImagePickerController.isSourceTypeAvailable(.camera) else {
            model.transferError = "この端末ではカメラを利用できません。"
            return
        }
        switch AVCaptureDevice.authorizationStatus(for: .video) {
        case .authorized: showingCamera = true
        case .notDetermined:
            AVCaptureDevice.requestAccess(for: .video) { granted in
                DispatchQueue.main.async {
                    if granted { showingCamera = true }
                    else { model.transferError = "設定アプリでBexのカメラへのアクセスを許可してください。" }
                }
            }
        default: model.transferError = "設定アプリでBexのカメラへのアクセスを許可してください。"
        }
    }

    private var conversationTitle: some View {
        VStack(alignment: .leading, spacing: 3) {
            HStack(spacing: 6) {
                Text(conversation.thread?.title ?? (state.isNewThread ? "チャット" : "タスク"))
                    .font(.headline).lineLimit(1)
                if state.isConnecting {
                    ProgressView().controlSize(.small)
                        .accessibilityLabel("接続中")
                        .accessibilityIdentifier("connection.progress")
                }
            }
            Text([project?.name ?? (model.cwd.isEmpty ? "" : URL(fileURLWithPath: model.cwd).lastPathComponent), state.selectedProfileName ?? "Mac"].filter { !$0.isEmpty }.joined(separator: " · "))
                .font(.subheadline).foregroundColor(.secondary).lineLimit(1)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private var conversationActions: some View {
        HStack(spacing: 0) {
            Button { model.openNewThread(cwd: model.cwd) } label: {
                Image(systemName: "square.and.pencil").font(.title2).frame(width: 44, height: 44)
            }.accessibilityLabel("新しい会話").accessibilityIdentifier("task.new")
            Menu {
                Button { opensDiff = false; showingFiles = true } label: {
                    Label("ファイル", systemImage: "folder")
                }.accessibilityIdentifier("task.files")
                Button { opensDiff = true; showingFiles = true } label: {
                    Label("変更を表示", systemImage: "plus.forwardslash.minus")
                }
                Button {
                    if let id = conversation.thread?.id { model.openThread(id) }
                    refreshReview()
                } label: { Label("更新", systemImage: "arrow.clockwise") }
            } label: {
                Image(systemName: "ellipsis").font(.title2.weight(.semibold)).frame(width: 44, height: 44)
            }
            .accessibilityLabel("その他")
            .accessibilityIdentifier("task.more")
        }
        .buttonStyle(.plain)
    }

    private var newThreadContext: some View {
        VStack(alignment: .leading, spacing: 4) {
            Menu {
                ForEach(state.profiles, id: \.id) { profile in
                    Button { model.openNewThread(on: profile.id) } label: {
                        if profile.id == state.selectedProfileId {
                            Label(profile.name, systemImage: "checkmark")
                        } else { Text(profile.name) }
                    }
                }
            } label: {
                contextLabel(state.selectedProfileName ?? "環境を選択", icon: "laptopcomputer")
                if state.isConnecting { ProgressView().controlSize(.small) }
            }
            .accessibilityLabel("環境: \(state.selectedProfileName ?? "未選択")")
            .accessibilityIdentifier("task.environment")
            Menu {
                Button { model.openNewThread(cwd: "") } label: {
                    Label("チャット", systemImage: "bubble.left.and.bubble.right")
                }
                ForEach(state.projects, id: \.id) { project in
                    ForEach(project.roots, id: \.self) { root in
                        Button { model.openNewThread(cwd: root) } label: {
                            Label(project.roots.count == 1 ? project.name : root,
                                  systemImage: root == model.cwd ? "checkmark" : "folder")
                        }
                    }
                }
                if state.hasMoreProjects {
                    Button("さらにプロジェクトを読み込む") { model.expandTaskList(projects: true) }
                }
            } label: {
                contextLabel(project?.name ?? (model.cwd.isEmpty ? "チャット" : URL(fileURLWithPath: model.cwd).lastPathComponent),
                             icon: model.cwd.isEmpty ? "bubble.left.and.bubble.right" : "folder")
            }
            .accessibilityLabel("フォルダ: \(project?.name ?? (model.cwd.isEmpty ? "チャット" : model.cwd))")
            .accessibilityIdentifier("task.folder")
        }
        .font(.title3)
        .foregroundColor(.secondary)
        .buttonStyle(.plain)
        .frame(maxWidth: .infinity, alignment: .leading)
        .disabled(model.transferring || preparingMedia || model.sending || dictation.isRecording || dictation.requestingPermission || model.transcribing)
    }

    private func contextLabel(_ title: String, icon: String) -> some View {
        HStack(spacing: 12) {
            Image(systemName: icon).frame(width: 28)
            Text(title).lineLimit(1).truncationMode(.middle)
            Image(systemName: "chevron.up.chevron.down").font(.caption.weight(.semibold))
        }
        .frame(minHeight: 44)
        .padding(.horizontal, 8)
    }

    private var composer: some View {
        VStack(spacing: 12) {
            if state.isNewThread { newThreadContext }
            if !state.isNewThread, let review, review.files > 0 {
                Button { opensDiff = true; showingFiles = true } label: {
                    HStack(spacing: 10) {
                        Text("\(review.files)件のファイル")
                        Text("+\(review.additions)").foregroundColor(.green)
                        Text("−\(review.deletions)").foregroundColor(.red)
                    }
                    .font(.subheadline.monospacedDigit())
                }
                .buttonStyle(.bordered)
                .buttonBorderShape(.capsule)
                .accessibilityIdentifier("task.diff")
            }
            if let error = model.transferError { BexNotice(text: error) }
            if dictation.isRecording {
                Text("録音中（最大30秒）").font(.caption).foregroundColor(.red)
                    .accessibilityIdentifier("dictation.recording")
            }
            if model.transcribing {
                ProgressView(sendRecordedText ? "文字起こしして送信中…" : "文字起こし中…").font(.caption)
                    .accessibilityIdentifier("dictation.processing")
            }
            ForEach(model.attachments) { attachment in
                HStack {
                    Label(attachment.name, systemImage: attachment.isImage ? "photo" : "doc")
                        .lineLimit(1)
                    Button { model.removeAttachment(attachment.id) } label: { Image(systemName: "xmark.circle.fill") }
                        .accessibilityLabel("\(attachment.name)を外す")
                }
                .font(.subheadline).padding(10)
                .background(Color(UIColor.secondarySystemBackground), in: Capsule())
            }
            VStack(alignment: .leading, spacing: 8) {
                messageField
                    .font(.system(size: 18))
                    .focused($composerFocused)
                    .padding(.horizontal, 8)
                    .padding(.top, 10)
                    .padding(.bottom, 4)
                    .accessibilityIdentifier("task.message")
                HStack(spacing: 8) {
                    Menu {
                        Button { composerFocused = false; showingPhotos = true } label: {
                            Label("写真・動画", systemImage: "photo.on.rectangle")
                        }.accessibilityIdentifier("task.attach.photos")
                        Button { openCamera() } label: {
                            Label("カメラ", systemImage: "camera")
                        }.accessibilityIdentifier("task.attach.camera")
                        Button { composerFocused = false; importing = true } label: {
                            Label("ファイル", systemImage: "doc")
                        }.accessibilityIdentifier("task.attach.file")
                    } label: {
                        Image(systemName: "plus").font(.title2.weight(.regular)).frame(width: 40, height: 40)
                    }
                    .disabled(model.transferring || preparingMedia || model.sending || dictation.isRecording || dictation.requestingPermission || model.transcribing)
                    .accessibilityLabel("添付").accessibilityIdentifier("task.attach")
                    if model.transferring || preparingMedia { ProgressView().frame(height: 40) }
                    Spacer(minLength: 0)
                    Button { showingModelSettings = true } label: {
                        Image(systemName: "speedometer")
                            .font(.system(size: 23, weight: .regular))
                            .frame(width: 44, height: 44)
                    }
                    .accessibilityLabel("モデル設定")
                    .accessibilityIdentifier("model.settings")
                    if dictation.isRecording || dictation.requestingPermission {
                        Button { dictation.cancel() } label: {
                            Image(systemName: "xmark").frame(width: 40, height: 40)
                        }
                        .accessibilityLabel("録音を中止")
                        .accessibilityIdentifier("dictation.cancel")
                    }
                    Button {
                        if dictation.isRecording { dictation.finish() }
                        else { startDictation() }
                    } label: {
                        if dictation.requestingPermission { ProgressView().frame(width: 40, height: 40) }
                        else {
                            Image(systemName: dictation.isRecording ? "stop.circle.fill" : "mic")
                                .font(.system(size: 23)).foregroundColor(dictation.isRecording ? .red : .primary)
                                .frame(width: 40, height: 40)
                        }
                    }
                    .disabled(!state.isConnected || (!state.isNewThread && conversation.thread == nil) || model.transcribing || dictation.requestingPermission || model.sending || model.transferring || preparingMedia)
                    .accessibilityLabel(dictation.isRecording ? "録音を終了して文字起こし" : "音声をCodexで文字起こし")
                    .accessibilityIdentifier("dictation.toggle")
                    if let running = conversation.thread?.turns.last(where: { $0.isInProgress }),
                       !dictation.isRecording && !dictation.requestingPermission && !model.transcribing &&
                       model.draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty && model.attachments.isEmpty {
                        Button { model.interrupt(running.turnId) } label: {
                            Image(systemName: "stop.fill").font(.system(size: 15))
                        }
                        .buttonStyle(.borderedProminent)
                        .buttonBorderShape(.capsule)
                        .controlSize(.large)
                        .disabled(state.interruptingTurnId == running.turnId)
                        .accessibilityLabel(state.interruptingTurnId == running.turnId ? "停止中" : "停止")
                        .accessibilityIdentifier("turn.interrupt.\(running.id)")
                    } else {
                        Button {
                            if dictation.isRecording {
                                sendRecordedText = true
                                dictation.finish()
                            } else { model.send() }
                            composerFocused = false
                        } label: {
                            Image(systemName: "arrow.up").font(.title2.weight(.semibold))
                        }
                        .buttonStyle(.borderedProminent)
                        .buttonBorderShape(.capsule)
                        .controlSize(.large)
                        .accessibilityLabel(dictation.isRecording ? "文字起こしして送信" : "送信")
                        .disabled(!state.isConnected || (!state.isNewThread && conversation.thread == nil) || model.sending || model.transferring || preparingMedia || dictation.requestingPermission || model.transcribing ||
                                  (!dictation.isRecording && model.draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty && model.attachments.isEmpty))
                        .accessibilityIdentifier("task.send")
                    }
                }
            }
            .buttonStyle(.plain)
            .padding(7)
            .background(.ultraThinMaterial, in: RoundedRectangle(cornerRadius: 30))
            .overlay(RoundedRectangle(cornerRadius: 30).stroke(Color.white.opacity(0.12)))
        }
        .padding(.horizontal, 20).padding(.top, 8).padding(.bottom, 8)
        .background(LinearGradient(colors: [.clear, Color(UIColor.systemBackground)], startPoint: .top, endPoint: .bottom))
    }

    private func startDictation() {
        composerFocused = false
        model.transferError = nil
        sendRecordedText = false
        let sendIntent = $sendRecordedText
        let key = model.draftKey
        dictation.start { [weak model] result in
            guard let model, model.draftKey == key else { return }
            switch result {
            case .success(let audio): model.transcribe(audio, draftKey: key, sendImmediately: sendIntent.wrappedValue)
            case .failure(let error): model.transferError = error.localizedDescription
            }
        }
    }

    @ViewBuilder private var messageField: some View {
        let binding = Binding(get: { model.draft }, set: { model.draft = $0 })
        if #available(iOS 16.0, *) {
            TextField(state.isNewThread ? "メッセージを入力" : "追加の指示を入力", text: binding, axis: .vertical).lineLimit(1...6)
        } else {
            TextField(state.isNewThread ? "メッセージを入力" : "追加の指示を入力", text: binding)
        }
    }

    private func refreshReview() {
        let directory = model.cwd
        guard !directory.isEmpty, let threadId = conversation.thread?.id else { review = nil; return }
        model.workspace("host/workspace/review", ["cwd": directory]) { result, _ in
            guard model.cwd == directory, model.state.selectedThreadId == threadId else { return }
            if let result, let files = result["files"] as? [[String: Any]],
               let additions = result["additions"] as? Int, let deletions = result["deletions"] as? Int {
                review = (files.count, additions, deletions)
            } else { review = nil }
        }
    }

    private func threadAccessibilityValue(_ thread: IosThreadView) -> String {
        let itemCount = thread.turns.reduce(0) { total, turn in
            total + turn.userMessages.count + turn.activityItems.count + turn.responses.count
                + turn.pendingRequests.count + (turn.error == nil ? 0 : 1)
        }
        return "turns=\(Set(thread.turns.map { $0.turnId }).count);items=\(itemCount)"
    }

}

private struct ScrollViewportPreferenceKey: PreferenceKey {
    static var defaultValue: CGFloat = 0
    static func reduce(value: inout CGFloat, nextValue: () -> CGFloat) { value = max(value, nextValue()) }
}

private struct ThreadConversationRow: Identifiable {
    let id: String
    let content: Content
    enum Content {
        case olderTurns, olderItems(String)
        case user(IosItemView), response(IosItemView), queued(IosItemView)
        case activityHeader(IosTurnView), activity(IosItemView, String)
        case request(IosTurnRequestView), error(IosTurnErrorView)
    }
}

private struct ThreadActivityHeader: View {
    let turn: IosTurnView
    let expanded: Bool
    var body: some View {
        HStack(spacing: 5) {
            Text((turn.activitySummary ?? "").replacingOccurrences(of: "h", with: "時間")
                .replacingOccurrences(of: "m", with: "分")
                .replacingOccurrences(of: "s", with: "秒")
                .replacingOccurrences(of: "間作業", with: " 作業"))
                .font(.system(size: 16)).foregroundColor(.secondary)
                .lineLimit(1).truncationMode(.tail)
            if turn.activityCanCollapse {
                Image(systemName: expanded ? "chevron.down" : "chevron.right")
                    .font(.caption2.weight(.semibold)).foregroundColor(.secondary)
            }
            if turn.isInProgress { ProgressView().controlSize(.small) }
            Spacer(minLength: 0)
        }.contentShape(Rectangle())
    }
}

private struct ThreadRequestRow: View {
    let request: IosTurnRequestView
    let model: BexAppViewModel
    @State private var answers: [String: String] = [:]
    @State private var rawResponse = "{}"
    @State private var busy = false
    @State private var error: String?
    @State private var resolved = false

    private var params: [String: Any] { jsonObject(request.paramsJson) }
    private var questions: [[String: Any]] { params["questions"] as? [[String: Any]] ?? [] }

    var body: some View {
        if !resolved {
            VStack(alignment: .leading, spacing: 10) {
                Text(request.title).font(.subheadline.weight(.semibold)).accessibilityIdentifier("request.\(request.id)")
                Text(request.body).textSelection(.enabled)
                DisclosureGroup("詳細") {
                    Text(request.paramsJson).font(.caption.monospaced()).textSelection(.enabled)
                }
                if request.method == "item/tool/requestUserInput" {
                    ForEach(Array(questions.enumerated()), id: \.offset) { _, question in
                        questionView(question)
                    }
                    Button("回答を送信") {
                        var result: [String: Any] = [:]
                        for question in questions {
                            if let id = question["id"] as? String { result[id] = ["answers": [answers[id] ?? ""]] }
                        }
                        submit(["answers": result])
                    }.disabled(questions.contains { (answers[$0["id"] as? String ?? ""] ?? "").isEmpty })
                } else if request.method == "item/permissions/requestApproval" {
                    HStack {
                        Button("このターンで許可") { submit(["permissions": params["permissions"] ?? [:], "scope": "turn"]) }
                        Button("拒否") { submit(["permissions": [:], "scope": "turn"]) }
                    }
                } else if request.method == "item/commandExecution/requestApproval" || request.method == "item/fileChange/requestApproval" {
                    HStack {
                        Button("承認") { submit(["decision": "accept"]) }.accessibilityIdentifier("request.accept")
                        Button("拒否") { submit(["decision": "decline"]) }
                    }
                } else {
                    Text("応答 JSON").font(.caption)
                    TextEditor(text: $rawResponse).font(.body.monospaced()).frame(minHeight: 100)
                    Button("応答を送信") {
                        guard let data = rawResponse.data(using: .utf8),
                              let value = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else {
                            error = "JSON オブジェクトを入力してください"; return
                        }
                        submit(value)
                    }
                }
                if busy { ProgressView() }
                if let error { Text(error).foregroundColor(.red) }
            }
            .disabled(busy)
            .padding(10)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(Color.orange.opacity(0.14))
            .clipShape(RoundedRectangle(cornerRadius: 10))
        }
    }

    @ViewBuilder private func questionView(_ question: [String: Any]) -> some View {
        let id = question["id"] as? String ?? ""
        let binding = Binding<String>(get: { answers[id] ?? "" }, set: { answers[id] = $0 })
        Text(question["question"] as? String ?? "回答")
        if let options = question["options"] as? [[String: Any]] {
            ForEach(Array(options.enumerated()), id: \.offset) { _, option in
                Button(option["label"] as? String ?? "") { answers[id] = option["label"] as? String }
                    .buttonStyle(.bordered)
            }
        }
        if question["isSecret"] as? Bool == true { SecureField("回答", text: binding) }
        else { TextField("回答", text: binding).textFieldStyle(.roundedBorder).accessibilityIdentifier("request.answer") }
    }

    private func submit(_ result: [String: Any]) {
        busy = true
        error = nil
        model.respond(request, result: result) { message in
            busy = false; error = message; resolved = message == nil
        }
    }
}

private struct ThreadErrorRow: View {
    let error: IosTurnErrorView

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack(spacing: 6) {
                if error.isReconnecting { ProgressView().controlSize(.small) }
                Text(error.title).font(.subheadline.weight(.semibold))
            }
            Text(error.message).textSelection(.enabled)
            if let details = error.details, !details.isEmpty {
                Text(details).font(.caption).foregroundColor(.secondary).textSelection(.enabled)
            }
        }
        .foregroundColor(error.isReconnecting ? .primary : .red)
        .padding(10)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color.red.opacity(error.isReconnecting ? 0.06 : 0.12))
        .clipShape(RoundedRectangle(cornerRadius: 10))
        .accessibilityIdentifier("turn.error")
    }
}

private struct ThreadMessageRow: View {
    let item: IosItemView
    let isUser: Bool
    let model: BexAppViewModel
    @State private var sharing = false
    @State private var expanded = false
    @State private var copied = false

    var body: some View {
        VStack(alignment: isUser ? .trailing : .leading, spacing: 14) {
            VStack(alignment: isUser ? .trailing : .leading, spacing: 12) {
                let sources = item.imageSources
                ForEach(sources.indices, id: \.self) { index in
                    ConversationImage(source: sources[index], label: "添付画像", identifier: "message.image.\(item.id).\(index)", model: model)
                }
                if !item.collapsedBody.isEmpty {
                    if isUser { Text(item.collapsedBody).font(.system(size: 18)).textSelection(.enabled) }
                    else { ConversationMarkdown(text: item.collapsedBody, model: model) }
                }
            }
            .padding(isUser ? 14 : 0)
            .background(isUser ? Color(UIColor.secondarySystemBackground) : Color.clear, in: RoundedRectangle(cornerRadius: 22))
            .padding(.leading, isUser ? 42 : 0)
            .frame(maxWidth: .infinity, alignment: isUser ? .trailing : .leading)
            .accessibilityElement(children: .contain)
            .accessibilityIdentifier("item.\(item.id)")
            if item.kind == "agent" {
                HStack(spacing: 20) {
                    Button { UIPasteboard.general.string = item.collapsedBody; copied = true } label: {
                        Image(systemName: copied ? "checkmark" : "doc.on.doc")
                    }.accessibilityLabel(copied ? "コピーしました" : "回答をコピー")
                    Button { sharing = true } label: { Image(systemName: "square.and.arrow.up") }
                        .accessibilityLabel("回答を共有")
                    Button { expanded = true } label: { Image(systemName: "arrow.up.left.and.arrow.down.right") }
                        .accessibilityLabel("回答を広げて表示")
                }
                .font(.system(size: 19)).foregroundColor(.secondary).buttonStyle(.plain)
                .padding(.vertical, 4)
            }
        }
        .padding(.bottom, isUser ? 12 : 8)
        .sheet(isPresented: $sharing) { ResponseShareSheet(text: item.collapsedBody) }
        .sheet(isPresented: $expanded) {
            NavigationView {
                ScrollView { ConversationMarkdown(text: item.collapsedBody, model: model).padding(20) }
                    .navigationTitle("回答").navigationBarTitleDisplayMode(.inline)
                    .toolbar { Button("閉じる") { expanded = false } }
            }.preferredColorScheme(.dark)
        }
    }
}

private struct ResponseShareSheet: UIViewControllerRepresentable {
    let text: String
    func makeUIViewController(context: Context) -> UIActivityViewController {
        UIActivityViewController(activityItems: [text], applicationActivities: nil)
    }
    func updateUIViewController(_ controller: UIActivityViewController, context: Context) {}
}

private struct ThreadItemRow: View {
    let item: IosItemView
    let model: BexAppViewModel
    let isExpanded: Bool
    let toggleExpanded: () -> Void
    let loadDetails: () async -> (String?, String?)
    @State private var loadedBody: String?
    @State private var detailError: String?
    @State private var loadedVersion: String?
    @State private var retry = 0

    private var icon: String {
        switch item.kind {
        case "command": return "terminal"
        case "fileChange": return "doc.badge.gearshape"
        case "webSearch": return "globe"
        default: return "chevron.left.forwardslash.chevron.right"
        }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            if item.isCollapsible {
                DisclosureGroup(isExpanded: Binding(
                    get: { isExpanded },
                    set: { if $0 != isExpanded { toggleExpanded() } }
                )) {
                    if isExpanded {
                        let body = item.isDeferred ? loadedBody ?? "" : item.expandedBody()
                        if item.isDeferred && loadedBody == nil {
                            if let detailError {
                                Text(detailError).font(.caption).foregroundColor(.red)
                                Button("再読み込み") { retry += 1 }
                            } else { ProgressView("詳細を読み込み中…") }
                        }
                        if !body.isEmpty {
                            Text(body).font(.system(.subheadline, design: .monospaced)).textSelection(.enabled)
                                .padding(12).frame(maxWidth: .infinity, alignment: .leading)
                                .background(Color(UIColor.secondarySystemBackground), in: RoundedRectangle(cornerRadius: 12))
                        }
                    }
                } label: {
                    Label {
                        Text(item.kind == "command" && item.title.hasPrefix("$ ") ? String(item.title.dropFirst(2)) : item.title)
                            .lineLimit(1)
                    } icon: { Image(systemName: icon) }
                    .font(.system(size: 17))
                    .foregroundColor(.secondary)
                    .padding(.vertical, 5)
                    .accessibilityIdentifier("item.\(item.id)")
                }
            } else {
                ConversationMarkdown(text: item.collapsedBody, model: model)
                    .foregroundColor(item.kind == "agent" || item.kind == "user" ? .primary : .secondary)
                    .accessibilityIdentifier("item.\(item.id)")
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .accessibilityElement(children: .contain)
        .task(id: "\(isExpanded):\(item.contentVersion):\(retry)") {
            if loadedVersion != item.contentVersion { loadedBody = nil; detailError = nil }
            guard isExpanded && item.isDeferred && loadedBody == nil else { return }
            detailError = nil
            let (body, error) = await loadDetails()
            guard !Task.isCancelled else { return }
            loadedBody = body
            detailError = error
            loadedVersion = item.contentVersion
        }
    }
}

/// Host paths always use the authenticated transfer; never read a Host path from the phone's filesystem.
private struct ConversationImage: View {
    let source: String
    let label: String
    let identifier: String
    @ObservedObject var model: BexAppViewModel
    @State private var image: UIImage?
    @State private var error: String?
    private struct LoadID: Equatable {
        let host: String?
        let source: String
    }

    var body: some View {
        Group {
            if let image {
                Image(uiImage: image).resizable().scaledToFit()
                    .frame(maxWidth: .infinity, maxHeight: 420)
                    .clipShape(RoundedRectangle(cornerRadius: 12))
                    .accessibilityLabel(label.isEmpty ? "画像" : label)
                    .accessibilityIdentifier(identifier)
            } else if let error {
                Label("画像を表示できません: \(error)", systemImage: "photo")
                    .font(.caption).foregroundColor(.secondary)
            } else {
                ProgressView("画像を読み込み中…").frame(height: 120)
            }
        }
        .task(id: LoadID(host: model.state.selectedProfileId, source: source)) {
            image = nil; error = nil
            do {
                let loaded = try await loadImage()
                try Task.checkCancellation()
                image = loaded
            } catch {
                if !Task.isCancelled { self.error = error.localizedDescription }
            }
        }
    }

    @MainActor private func loadImage() async throws -> UIImage {
        let data: Data
        if source.hasPrefix("data:image/"), let comma = source.firstIndex(of: ",") {
            let header = source[..<comma]
            let payload = String(source[source.index(after: comma)...])
            guard header.hasSuffix(";base64"), let decoded = Data(base64Encoded: payload) else {
                throw CocoaError(.fileReadCorruptFile)
            }
            data = decoded
        } else if let url = URL(string: source), url.scheme == "https" || url.scheme == "http" {
            let (downloaded, response) = try await URLSession.shared.data(from: url)
            guard let response = response as? HTTPURLResponse, (200..<300).contains(response.statusCode) else {
                throw URLError(.badServerResponse)
            }
            data = downloaded
        } else {
            let path: String
            if let url = URL(string: source), url.isFileURL { path = url.path }
            else if source.hasPrefix("/") { path = source }
            else if let url = URL(string: source), url.scheme == nil {
                path = URL(fileURLWithPath: model.cwd, isDirectory: true).appendingPathComponent(url.path).path
            } else { throw URLError(.unsupportedURL) }
            let (url, message) = await withCheckedContinuation { continuation in
                model.download(path) { url, message in continuation.resume(returning: (url, message)) }
            }
            guard let url else {
                throw NSError(domain: "BexImage", code: 1, userInfo: [NSLocalizedDescriptionKey: message ?? "画像を取得できません"])
            }
            defer { try? FileManager.default.removeItem(at: url.deletingLastPathComponent()) }
            data = try await Task.detached(priority: .userInitiated) { try Data(contentsOf: url) }.value
        }
        return try await Task.detached(priority: .userInitiated) {
            guard let source = CGImageSourceCreateWithData(data as CFData, nil),
                  let thumbnail = CGImageSourceCreateThumbnailAtIndex(source, 0, [
                    kCGImageSourceCreateThumbnailFromImageAlways: true,
                    kCGImageSourceCreateThumbnailWithTransform: true,
                    kCGImageSourceThumbnailMaxPixelSize: 1600,
                  ] as CFDictionary) else { throw CocoaError(.fileReadCorruptFile) }
            return UIImage(cgImage: thumbnail)
        }.value
    }
}

/// Foundation parses block structure and inline Markdown; no HTML/web view is involved.
private struct ConversationMarkdown: View {
    let text: String
    let model: BexAppViewModel
    @State private var blocks: [Block] = []
    private struct Block: Identifiable {
        let id: Int
        let paragraphID: Int
        var content: AttributedString
        let header: Int?
        let marker: String?
        let code: Bool
        let quoted: Bool
        let imageURL: URL?
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            if blocks.isEmpty { Text(text).textSelection(.enabled) }
            ForEach(blocks) { block in
                if let imageURL = block.imageURL {
                    ConversationImage(source: imageURL.scheme == nil ? imageURL.path : imageURL.absoluteString, label: String(block.content.characters),
                                      identifier: "markdown.image.\(block.id)", model: model)
                } else if block.code {
                    ScrollView(.horizontal) {
                        Text(block.content).font(.system(.subheadline, design: .monospaced))
                            .textSelection(.enabled).padding(12)
                    }.background(Color(UIColor.secondarySystemBackground), in: RoundedRectangle(cornerRadius: 12))
                } else {
                    HStack(alignment: .firstTextBaseline, spacing: 10) {
                        if let marker = block.marker { Text(marker).frame(minWidth: 14, alignment: .leading) }
                        if block.quoted { Rectangle().fill(Color.secondary).frame(width: 2) }
                        Text(block.content)
                            .font(block.header == nil ? .system(size: 18) : .system(size: block.header == 1 ? 25 : 21, weight: .semibold))
                            .lineSpacing(5).textSelection(.enabled)
                            .frame(maxWidth: .infinity, alignment: .leading)
                    }
                }
            }
        }
        .font(.system(size: 18))
        .tint(.primary)
        .task(id: text) { blocks = parse() }
    }

    private func parse() -> [Block] {
        guard let document = try? AttributedString(markdown: text) else { return [] }
        var result: [Block] = []
        var start: AttributedString.Index?
        var end: AttributedString.Index?
        var paragraphID: Int?
        var imageURL: URL?
        var header: Int?
        var marker: String?
        var code = false
        var quoted = false
        for run in document.runs {
            let components = run.presentationIntent?.components ?? []
            let identity = components.first?.identity ?? 0
            if paragraphID == identity && imageURL == run.imageURL {
                end = run.range.upperBound
                continue
            }
            if let start, let end, let paragraphID {
                result.append(Block(id: result.count, paragraphID: paragraphID,
                                    content: AttributedString(document[start..<end]), header: header,
                                    marker: marker, code: code, quoted: quoted, imageURL: imageURL))
            }
            start = run.range.lowerBound
            end = run.range.upperBound
            paragraphID = identity
            imageURL = run.imageURL
            header = nil
            marker = nil
            code = false
            quoted = false
            var ordinal: Int?
            var ordered = false
            for component in components {
                switch component.kind {
                case .header(level: let level): header = level
                case .listItem(ordinal: let number): ordinal = number
                case .orderedList: ordered = true
                case .codeBlock: code = true
                case .blockQuote: quoted = true
                default: break
                }
            }
            marker = ordinal.map { ordered ? "\($0)." : "•" }
        }
        if let start, let end, let paragraphID {
            result.append(Block(id: result.count, paragraphID: paragraphID,
                                content: AttributedString(document[start..<end]), header: header,
                                marker: marker, code: code, quoted: quoted, imageURL: imageURL))
        }
        return result
    }
}

private struct BexNotice: View {
    let text: String

    var body: some View {
        Text(text)
            .foregroundColor(.red)
            .accessibilityIdentifier("notice")
    }
}

private struct BexQrScannerSheet: View {
    let completion: (String?) -> Void

    var body: some View {
        NavigationView {
            BexQrScannerController { result in
                switch result {
                case let .success(contents): completion(contents)
                case .failure: completion(nil)
                }
            }
            .navigationTitle("QRコードを読み取る")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("キャンセル") { completion(nil) }
                        .accessibilityIdentifier("scanner.cancel")
                }
            }
        }
        .navigationViewStyle(StackNavigationViewStyle())
        .accessibilityIdentifier("scanner.sheet")
    }
}

private struct BexQrScannerController: UIViewControllerRepresentable {
    let completion: (Result<String, BexQrCaptureError>) -> Void

    func makeUIViewController(context: Context) -> BexQrCaptureViewController {
        BexQrCaptureViewController(completion: completion)
    }

    func updateUIViewController(_ uiViewController: BexQrCaptureViewController, context: Context) {}
}

struct StagedAttachment: Identifiable, Codable {
    var id = UUID()
    let name: String
    let path: String
    let isImage: Bool
}

private func jsonString(_ value: [String: Any]) -> String {
    // Call sites construct JSON-compatible dictionaries; serialization failures are programmer errors.
    let data = try! JSONSerialization.data(withJSONObject: value, options: [.sortedKeys])
    return String(decoding: data, as: UTF8.self)
}

private func jsonObject(_ value: String) -> [String: Any] {
    (try? JSONSerialization.jsonObject(with: Data(value.utf8))) as? [String: Any] ?? [:]
}

private struct WorkspaceEntry: Identifiable {
    var id: String { path }
    let name: String
    let path: String
    let directory: Bool
}

private struct SharedFile: Identifiable {
    let id = UUID()
    let url: URL
}

private struct WorkspaceSheet: View {
    @ObservedObject var model: BexAppViewModel
    let root: String
    var opensDiff = false
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationView {
            WorkspaceDirectoryScreen(model: model, root: root, directory: root, opensDiff: opensDiff) { dismiss() }
        }
        .navigationViewStyle(StackNavigationViewStyle())
    }
}

private struct WorkspaceDirectoryScreen: View {
    @ObservedObject var model: BexAppViewModel
    let root: String
    let directory: String
    var opensDiff = false
    let close: () -> Void
    @State private var destinationPath: String?
    @State private var path = ""
    @State private var entries: [WorkspaceEntry] = []
    @State private var error: String?
    @State private var busy = false
    @State private var selected: WorkspaceEntry?
    @State private var diff: [String] = []
    @State private var showingDiff = false
    @State private var sharedFile: SharedFile?

    var body: some View {
        VStack(spacing: 0) {
            if let error { BexNotice(text: error).padding() }
            if busy { ProgressView().padding() }
            HStack {
                TextField("絶対パス", text: $path)
                    .textInputAutocapitalization(.never).disableAutocorrection(true)
                    .textFieldStyle(.roundedBorder)
                Button("開く") { destinationPath = path }
                    .accessibilityIdentifier("files.open-path")
            }.padding()
            List {
                NavigationLink("親ディレクトリ") {
                    WorkspaceDirectoryScreen(model: model, root: root, directory: (path as NSString).deletingLastPathComponent, close: close)
                }
                ForEach(entries) { entry in
                    HStack {
                        if entry.directory {
                            NavigationLink {
                                WorkspaceDirectoryScreen(model: model, root: root, directory: entry.path, close: close)
                            } label: { Label(entry.name, systemImage: "folder") }
                            .accessibilityIdentifier("file.\(entry.name)")
                        } else {
                            Button { selected = entry } label: { Label(entry.name, systemImage: "doc") }
                                .buttonStyle(.borderless)
                                .accessibilityIdentifier("file.\(entry.name)")
                        }
                        Spacer()
                        if !entry.directory {
                            Button { download(entry.path) } label: { Image(systemName: "square.and.arrow.down") }
                                .buttonStyle(.borderless).accessibilityLabel("\(entry.name)をダウンロード")
                        }
                    }
                }
            }
        }
        .background(
            NavigationLink(isActive: Binding(
                get: { destinationPath != nil },
                set: { if !$0 { destinationPath = nil } }
            )) {
                if let destinationPath {
                    WorkspaceDirectoryScreen(model: model, root: root, directory: destinationPath, close: close)
                }
            } label: { EmptyView() }
        )
        .navigationTitle(directory == root ? "ファイル" : URL(fileURLWithPath: directory).lastPathComponent)
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .confirmationAction) {
                Button("閉じる", action: close).accessibilityIdentifier("files.close")
            }
            ToolbarItem(placement: .primaryAction) {
                Button("差分") {
                    loadDiff()
                }.accessibilityIdentifier("files.diff")
            }
        }
        .onAppear { if path.isEmpty { load(directory); if opensDiff { loadDiff() } } }
        .sheet(item: $selected) { entry in
            FileEditorSheet(model: model, entry: entry, download: download) { path in
                model.draft = "このファイルを編集してください: \(path)\n変更内容: "
                selected = nil
                close()
            }
        }
        .sheet(isPresented: $showingDiff) {
            NavigationView {
                ScrollView([.horizontal, .vertical]) {
                    LazyVStack(alignment: .leading, spacing: 0) {
                        ForEach(Array(diff.enumerated()), id: \.offset) { _, line in
                            Text(line.isEmpty ? " " : line).font(.caption.monospaced())
                                .foregroundColor(line.hasPrefix("+") ? .green : line.hasPrefix("-") ? .red : .primary)
                                .textSelection(.enabled)
                        }
                    }.padding()
                }
                .navigationTitle("作業中の差分")
                .toolbar {
                    Button("閉じる") { showingDiff = false }.accessibilityIdentifier("files.diff.close")
                }
            }
        }
        .sheet(item: $sharedFile, onDismiss: cleanupDownload) { item in
            FileShareSheet(url: item.url)
        }
    }

    private func loadDiff() {
        busy = true; error = nil
        model.workspace("host/workspace/review", ["cwd": root]) { result, message in
            busy = false; error = message
            if let value = result?["diff"] as? String { diff = value.components(separatedBy: "\n"); showingDiff = true }
        }
    }

    private func load(_ directory: String) {
        busy = true; error = nil
        model.workspace("host/file/list", ["path": directory]) { result, message in
            busy = false; error = message
            guard let result else { return }
            path = result["path"] as? String ?? directory
            entries = (result["entries"] as? [[String: Any]] ?? []).compactMap {
                guard let name = $0["name"] as? String, let path = $0["path"] as? String else { return nil }
                return WorkspaceEntry(name: name, path: path, directory: $0["directory"] as? Bool ?? false)
            }
            if result["truncated"] as? Bool == true { error = "先頭 2,000 件を表示しています。パスを指定して開けます。" }
        }
    }

    private func download(_ path: String) {
        busy = true; error = nil
        model.download(path) { url, message in
            busy = false; error = message
            if let url { selected = nil; sharedFile = SharedFile(url: url) }
        }
    }

    private func cleanupDownload() {
        if let directory = sharedFile?.url.deletingLastPathComponent() { try? FileManager.default.removeItem(at: directory) }
        sharedFile = nil
    }
}

private struct FileEditorSheet: View {
    @ObservedObject var model: BexAppViewModel
    let entry: WorkspaceEntry
    let download: (String) -> Void
    let aiEdit: (String) -> Void
    @Environment(\.dismiss) private var dismiss
    @State private var text = ""
    @State private var revision = ""
    @State private var savedText = ""
    @State private var error: String?
    @State private var busy = false
    @State private var initialized = false
    @State private var confirmReload = false

    private var draftKey: String { "bex.editor.v4:\(model.state.selectedProfileId ?? ""):\(entry.path)" }

    var body: some View {
        NavigationView {
            VStack(alignment: .leading) {
                Text(entry.path).font(.caption).foregroundColor(.secondary).textSelection(.enabled).padding(.horizontal)
                if let error { BexNotice(text: error).padding(.horizontal) }
                if busy { ProgressView().padding() }
                TextEditor(text: $text).font(.body.monospaced())
                    .textInputAutocapitalization(.never).disableAutocorrection(true)
                    .accessibilityIdentifier("file.editor")
                    .disabled(revision.isEmpty)
                    .onChange(of: text) { value in
                        guard initialized else { return }
                        if value == savedText { UserDefaults.standard.removeObject(forKey: draftKey) }
                        else { UserDefaults.standard.set(["text": value, "revision": revision], forKey: draftKey) }
                    }
                HStack {
                    Button("再読込") { if text != savedText { confirmReload = true } else { load(restoreDraft: false) } }
                    Button("ダウンロード") { download(entry.path) }
                    Button("AIで編集") { aiEdit(entry.path) }.accessibilityIdentifier("file.ai-edit")
                }.padding()
            }
            .navigationTitle(entry.name)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("閉じる") { dismiss() }.accessibilityIdentifier("file.close")
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button("保存") {
                        busy = true; error = nil
                        let submitted = text
                        model.workspace("host/file/write", ["path": entry.path, "revision": revision, "text": submitted]) { result, message in
                            busy = false; error = message
                            if let result, let nextRevision = result["revision"] as? String {
                                revision = nextRevision; savedText = result["text"] as? String ?? submitted
                                if text == submitted { text = savedText; UserDefaults.standard.removeObject(forKey: draftKey) }
                                else { UserDefaults.standard.set(["text": text, "revision": revision], forKey: draftKey) }
                            }
                        }
                    }.disabled(busy || revision.isEmpty || text == savedText).accessibilityIdentifier("file.save")
                }
            }
            .onAppear { if !initialized { load(restoreDraft: true) } }
            .confirmationDialog("保存していない編集を破棄して再読込しますか？", isPresented: $confirmReload, titleVisibility: .visible) {
                Button("編集を破棄して再読込", role: .destructive) { UserDefaults.standard.removeObject(forKey: draftKey); load(restoreDraft: false) }
            }
        }
    }

    private func load(restoreDraft: Bool) {
        busy = true; error = nil
        model.workspace("host/file/read", ["path": entry.path]) { result, message in
            busy = false; error = message
            guard let result, let value = result["text"] as? String, let version = result["revision"] as? String else { return }
            initialized = false
            text = value; savedText = value; revision = version
            if restoreDraft, let draft = UserDefaults.standard.dictionary(forKey: draftKey) as? [String: String], let cached = draft["text"] {
                text = cached
                if let original = draft["revision"] { revision = original }
                if revision != version { error = "ホストのファイルが変更されています。下書きは保持しました。再読込すると下書きを破棄します。" }
            }
            initialized = true
        }
    }
}

private struct FileShareSheet: UIViewControllerRepresentable {
    let url: URL
    func makeUIViewController(context: Context) -> UIActivityViewController {
        let controller = UIActivityViewController(activityItems: [url], applicationActivities: nil)
        controller.completionWithItemsHandler = { _, _, _, _ in
            try? FileManager.default.removeItem(at: url.deletingLastPathComponent())
        }
        return controller
    }
    func updateUIViewController(_ controller: UIActivityViewController, context: Context) {}
}

// Picker-owned URLs expire after their callback. Retain only the selected file
// in a private temporary directory until the existing upload completes.
private func retainChatMedia(_ source: URL) throws -> URL {
    let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString, isDirectory: true)
    try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
    do {
        let destination = directory.appendingPathComponent(source.lastPathComponent)
        try FileManager.default.copyItem(at: source, to: destination)
        return destination
    } catch {
        try? FileManager.default.removeItem(at: directory)
        throw error
    }
}

private struct ChatPhotoPicker: UIViewControllerRepresentable {
    let selected: ([NSItemProvider]) -> Void

    func makeCoordinator() -> Coordinator { Coordinator(selected: selected) }
    func makeUIViewController(context: Context) -> PHPickerViewController {
        var configuration = PHPickerConfiguration()
        configuration.filter = .any(of: [.images, .videos])
        configuration.selectionLimit = 0
        configuration.selection = .ordered
        configuration.preferredAssetRepresentationMode = .compatible
        let picker = PHPickerViewController(configuration: configuration)
        picker.delegate = context.coordinator
        return picker
    }
    func updateUIViewController(_ controller: PHPickerViewController, context: Context) {}

    final class Coordinator: NSObject, PHPickerViewControllerDelegate {
        let selected: ([NSItemProvider]) -> Void
        init(selected: @escaping ([NSItemProvider]) -> Void) { self.selected = selected }
        func picker(_ picker: PHPickerViewController, didFinishPicking results: [PHPickerResult]) {
            selected(results.map(\.itemProvider))
        }
    }
}

private struct ChatCameraPicker: UIViewControllerRepresentable {
    let completion: (Result<URL?, Error>) -> Void
    func makeCoordinator() -> Coordinator { Coordinator(completion: completion) }
    func makeUIViewController(context: Context) -> UIImagePickerController {
        let picker = UIImagePickerController()
        picker.sourceType = .camera
        picker.mediaTypes = (UIImagePickerController.availableMediaTypes(for: .camera) ?? []).filter {
            $0 == UTType.image.identifier || $0 == UTType.movie.identifier
        }
        picker.delegate = context.coordinator
        return picker
    }
    func updateUIViewController(_ controller: UIImagePickerController, context: Context) {}

    final class Coordinator: NSObject, UIImagePickerControllerDelegate, UINavigationControllerDelegate {
        let completion: (Result<URL?, Error>) -> Void
        init(completion: @escaping (Result<URL?, Error>) -> Void) { self.completion = completion }
        func imagePickerControllerDidCancel(_ picker: UIImagePickerController) { completion(.success(nil)) }
        func imagePickerController(_ picker: UIImagePickerController, didFinishPickingMediaWithInfo info: [UIImagePickerController.InfoKey: Any]) {
            completion(Result {
                if let movie = info[.mediaURL] as? URL { return try retainChatMedia(movie) }
                guard let image = info[.originalImage] as? UIImage, let data = image.jpegData(compressionQuality: 0.9) else {
                    throw CocoaError(.fileReadCorruptFile)
                }
                let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString, isDirectory: true)
                try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
                let destination = directory.appendingPathComponent("photo.jpg")
                do { try data.write(to: destination); return destination }
                catch { try? FileManager.default.removeItem(at: directory); throw error }
            })
        }
    }
}

private struct HistoryBoundaryPreferenceKey: PreferenceKey {
    static var defaultValue: [String: CGFloat] = [:]
    static func reduce(value: inout [String: CGFloat], nextValue: () -> [String: CGFloat]) {
        value.merge(nextValue(), uniquingKeysWith: { _, next in next })
    }
}

// Native offsets remain valid while List applies a new row snapshot. Queued
// ScrollViewReader index-path requests can race that update and abort UIKit.
private final class ConversationScrollPosition: NSObject, ObservableObject {
    private weak var scrollView: UIScrollView?
    private var observations = [NSKeyValueObservation]()
    private var followsLatest = true
    private var scrollScheduled = false
    private var reportedLatest: Bool?
    var isAttached: Bool { scrollView != nil }
    var onFollowingLatest: ((Bool) -> Void)?
    var onDirection: ((Bool) -> Void)?
    var historyAccessibilityValue = "" {
        didSet { scrollView?.accessibilityValue = historyAccessibilityValue }
    }

    func attach(_ scroll: UIScrollView) {
        guard scrollView !== scroll else { return }
        detach()
        scrollView = scroll
        scroll.accessibilityIdentifier = "task.detail"
        scroll.accessibilityValue = historyAccessibilityValue
        scroll.panGestureRecognizer.addTarget(self, action: #selector(panned(_:)))
        observations = [
            scroll.observe(\.contentSize) { [weak self] _, _ in self?.scheduleLatest() },
            scroll.observe(\.contentOffset) { [weak self] scroll, _ in
                guard let self else { return }
                let nearBottom = self.isNearBottom(scroll)
                if scroll.isDecelerating {
                    self.followsLatest = nearBottom && scroll.panGestureRecognizer.translation(in: scroll).y <= 0
                }
                self.reportLatest(nearBottom)
            },
            scroll.observe(\.bounds, options: [.old, .new]) { [weak self] _, change in
                if change.oldValue?.size != change.newValue?.size { self?.scheduleLatest() }
            }
        ]
        scheduleLatest()
    }

    func detach() {
        observations.removeAll()
        scrollView?.panGestureRecognizer.removeTarget(self, action: #selector(panned(_:)))
        scrollView = nil
    }

    func scrollToLatest(animated: Bool) {
        followsLatest = true
        applyLatest(animated: animated)
    }

    func stopFollowingLatest() { followsLatest = false }

    private func scheduleLatest() {
        if !followsLatest, let scroll = scrollView { reportLatest(isNearBottom(scroll)) }
        guard followsLatest, !scrollScheduled else { return }
        scrollScheduled = true
        DispatchQueue.main.async { [weak self] in
            guard let self else { return }
            self.scrollScheduled = false
            if self.followsLatest { self.applyLatest(animated: false) }
        }
    }

    private func bottomOffset(_ scroll: UIScrollView) -> CGFloat {
        max(-scroll.adjustedContentInset.top,
            scroll.contentSize.height - scroll.bounds.height + scroll.adjustedContentInset.bottom)
    }

    private func isNearBottom(_ scroll: UIScrollView) -> Bool {
        bottomOffset(scroll) - scroll.contentOffset.y <= 80
    }

    private func applyLatest(animated: Bool) {
        guard let scroll = scrollView else { return }
        let bottom = bottomOffset(scroll)
        if abs(scroll.contentOffset.y - bottom) > 0.5 {
            scroll.setContentOffset(CGPoint(x: scroll.contentOffset.x, y: bottom), animated: animated)
        }
        reportLatest(isNearBottom(scroll))
    }

    private func reportLatest(_ latest: Bool) {
        guard reportedLatest != latest else { return }
        reportedLatest = latest
        // KVO can fire during List layout; publish UI state after that update.
        DispatchQueue.main.async { [weak self] in self?.onFollowingLatest?(latest) }
    }

    @objc private func panned(_ gesture: UIPanGestureRecognizer) {
        switch gesture.state {
        case .began, .changed:
            followsLatest = false
            onDirection?(gesture.translation(in: scrollView).y > 0)
        case .ended:
            if let scroll = scrollView {
                followsLatest = isNearBottom(scroll) && gesture.translation(in: scroll).y <= 0
            }
        case .cancelled, .failed: onDirection?(false)
        default: break
        }
    }

    deinit { scrollView?.panGestureRecognizer.removeTarget(self, action: #selector(panned(_:))) }
}

// Locate List's native scrolling view without taking over its delegate or
// installing a competing drag recognizer.
private struct ConversationScrollViewObserver: UIViewRepresentable {
    let position: ConversationScrollPosition
    let accessibilityValue: String
    let onFollowingLatest: (Bool) -> Void
    let onDirection: (Bool) -> Void

    func makeUIView(context: Context) -> ProbeView {
        let view = ProbeView()
        view.isUserInteractionEnabled = false
        return view
    }
    func updateUIView(_ view: ProbeView, context: Context) {
        position.onFollowingLatest = onFollowingLatest
        position.onDirection = onDirection
        position.historyAccessibilityValue = accessibilityValue
        view.position = position
        view.attachWhenMounted()
    }

    static func dismantleUIView(_ view: ProbeView, coordinator: ()) {
        view.position?.detach()
        view.position?.onFollowingLatest = nil
        view.position?.onDirection = nil
    }

    final class ProbeView: UIView {
        weak var position: ConversationScrollPosition?
        private var attachmentScheduled = false

        override func didMoveToWindow() {
            super.didMoveToWindow()
            if window == nil { position?.detach() }
            else { attachWhenMounted() }
        }

        func attachWhenMounted() {
            guard window != nil, position?.isAttached == false, !attachmentScheduled else { return }
            attachmentScheduled = true
            DispatchQueue.main.async { [weak self] in
                guard let self else { return }
                self.attachmentScheduled = false
                var ancestor = self.superview
                while let view = ancestor {
                    if let scroll = self.findScrollView(view) {
                        self.position?.attach(scroll)
                        return
                    }
                    ancestor = view.superview
                }
            }
        }

        private func findScrollView(_ view: UIView) -> UIScrollView? {
            if let scroll = view as? UIScrollView { return scroll }
            for child in view.subviews where child !== self {
                if let scroll = findScrollView(child) { return scroll }
            }
            return nil
        }
    }
}

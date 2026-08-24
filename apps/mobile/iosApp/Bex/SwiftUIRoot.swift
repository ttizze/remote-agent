import Combine
import SwiftUI
import UIKit
import RemoteAgentMobile

/// Native presentation only: state transitions, cache reconciliation, and RPC
/// orchestration remain inside IosAppController.
@MainActor
final class BexAppViewModel: ObservableObject {
    @Published private(set) var state: IosAppViewState
    @Published var isScanning = false

    private let controller = IosAppController()

    init() {
        state = controller.currentState()
        controller.observe { [weak self] state in
            DispatchQueue.main.async {
                self?.state = state
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
    func discover() { controller.discover() }
    func connect() { controller.connect() }
    func applyWorkingDirectory(_ path: String) {
        controller.updateWorkingDirectory(path: path)
        controller.listThreads()
    }
    func listThreads() { controller.listThreads() }
    func startThread(_ workingDirectory: String) {
        controller.updateWorkingDirectory(path: workingDirectory)
        controller.startThread()
    }
    func openThread(_ id: String) { controller.openThread(threadId: id) }
    func showThreadList() { controller.showThreadList() }
    func send(_ text: String) { controller.sendTurn(text: text) }
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
                if state.screen == .pairing {
                    PairingScreen(state: state, model: model)
                } else if state.screen == .profiles {
                    ProfilesScreen(state: state, model: model)
                } else if state.screen == .connect {
                    ConnectScreen(state: state, model: model)
                } else if state.screen == .connecting {
                    ConnectingScreen(state: state)
                } else if state.screen == .threads {
                    ThreadsScreen(state: state, model: model)
                } else {
                    ThreadScreen(state: state, model: model)
                }
            }
            .navigationBarTitleDisplayMode(.inline)
        }
        .navigationViewStyle(StackNavigationViewStyle())
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
                        TextEditor(text: $contents)
                            .font(.system(.footnote, design: .monospaced))
                            .frame(height: 110)
                            .padding(6)
                            .background(Color(UIColor.tertiarySystemBackground))
                            .clipShape(RoundedRectangle(cornerRadius: 10, style: .continuous))
                            .overlay {
                                RoundedRectangle(cornerRadius: 10, style: .continuous)
                                    .stroke(Color.secondary.opacity(0.25))
                            }
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

                if !state.profiles.isEmpty {
                    Button("PC一覧へ戻る") { model.dismissPairing() }
                        .accessibilityIdentifier("pairing.cancel")
                }
            }
            .padding(.horizontal, 24)
            .padding(.vertical, 32)
        }
        .background(Color(UIColor.systemGroupedBackground).ignoresSafeArea())
        .navigationTitle("Bex")
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

private struct ConnectScreen: View {
    let state: IosAppViewState
    @ObservedObject var model: BexAppViewModel

    var body: some View {
        Form {
            Section(header: Text(state.selectedProfileName ?? "PC Host")) {
                if state.addresses.isEmpty {
                    Text("保存済みアドレスなし").foregroundColor(.secondary)
                } else {
                    ForEach(state.addresses, id: \.self) { Text($0).font(.footnote) }
                }
                Button("ネットワークで検出") { model.discover() }
                    .accessibilityIdentifier("connect.discover")
                Button("接続") { model.connect() }
                    .accessibilityIdentifier("connect.start")
            }
            if let error = state.connectionError { Section { BexNotice(text: error) } }
            Section {
                Button("PC一覧") { model.showProfiles() }
                    .accessibilityIdentifier("connect.back")
            }
        }
        .navigationTitle("接続")
    }
}

private struct ConnectingScreen: View {
    let state: IosAppViewState

    var body: some View {
        VStack(spacing: 16) {
            ProgressView().accessibilityIdentifier("connecting.progress")
            Text("\(state.selectedProfileName ?? "PC Host") に接続中…")
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .navigationTitle("接続中")
    }
}

private struct ThreadsScreen: View {
    let state: IosAppViewState
    @ObservedObject var model: BexAppViewModel
    @State private var filter = ""

    var body: some View {
        List {
            Section(header: Text("作業ディレクトリで絞り込み（任意）")) {
                TextField("空欄で全タスク", text: $filter)
                    .textInputAutocapitalization(.never)
                    .disableAutocorrection(true)
                    .accessibilityIdentifier("tasks.filter")
                Button("適用・更新") { model.applyWorkingDirectory(filter) }
                    .disabled(state.threadLoadState == .loading)
                    .accessibilityIdentifier("tasks.apply")
            }

            if state.threadLoadState == .loading {
                Section { ProgressView("タスクを読み込み中…")
                    .accessibilityIdentifier("tasks.loading") }
            }
            if state.threadLoadState == .failed {
                Section {
                    if let error = state.threadLoadError { BexNotice(text: error) }
                    Button("再試行") { model.listThreads() }
                        .accessibilityIdentifier("tasks.retry")
                }
            }
            if state.threadLoadState == .idle {
                Section {
                    Text("タスクを表示するには「更新」を押してください")
                        .foregroundColor(.secondary)
                }
            }
            if let notice = state.notice { Section { BexNotice(text: notice) } }

            Section {
                let hasWorkingDirectory = !filter.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                Button("新しいタスク") { model.startThread(filter) }
                    .disabled(!hasWorkingDirectory || state.threadLoadState == .loading)
                    .accessibilityIdentifier("tasks.start")
                if !hasWorkingDirectory {
                    Text("新しいタスクを作成するには作業ディレクトリを指定してください")
                        .font(.caption)
                        .foregroundColor(.secondary)
                        .accessibilityIdentifier("tasks.start.explanation")
                }
            }

            Section(header: Text("タスク")) {
                if state.threadLoadState == .ready && state.threads.isEmpty {
                    Text("タスクがありません")
                        .foregroundColor(.secondary)
                        .accessibilityIdentifier("tasks.empty")
                }
                ForEach(state.threads, id: \.id) { thread in
                    Button { model.openThread(thread.id) } label: {
                        VStack(alignment: .leading, spacing: 4) {
                            Text(thread.title).font(.headline)
                            Text(thread.preview).lineLimit(2).foregroundColor(.secondary)
                            Text(thread.workingDirectory).font(.caption2).foregroundColor(.secondary)
                        }
                    }
                    .accessibilityIdentifier("tasks.row.\(thread.id)")
                }
            }
        }
        .onAppear { filter = state.workingDirectory }
        .onChange(of: state.workingDirectory) { path in
            filter = path
        }
        .navigationTitle(state.selectedProfileName ?? "タスク")
    }
}

private struct ThreadScreen: View {
    let state: IosAppViewState
    @ObservedObject var model: BexAppViewModel
    @State private var draft = ""

    var body: some View {
        VStack(spacing: 0) {
            if let notice = state.notice { BexNotice(text: notice).padding(.horizontal).padding(.top, 8) }
            if let thread = state.selectedThread {
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 12) {
                        ForEach(thread.turns, id: \.id) { turn in
                            TurnCard(turn: turn, interruptingTurnId: state.interruptingTurnId, interrupt: model.interrupt)
                        }
                    }
                    .padding()
                }
            } else {
                ProgressView("タスクを読み込み中…")
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
            Divider()
            HStack(alignment: .bottom, spacing: 8) {
                TextField("メッセージ", text: $draft)
                    .textInputAutocapitalization(.sentences)
                    .accessibilityIdentifier("task.message")
                Button("送信") {
                    let text = draft.trimmingCharacters(in: .whitespacesAndNewlines)
                    draft = ""
                    model.send(text)
                }
                .disabled(draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                .accessibilityIdentifier("task.send")
            }
            .padding()
        }
        .navigationTitle(state.selectedThread?.title ?? "タスク")
        .toolbar {
            ToolbarItem(placement: .navigationBarLeading) {
                Button("タスク一覧") { model.showThreadList() }
                    .accessibilityIdentifier("task.back")
            }
        }
    }
}

private struct TurnCard: View {
    let turn: IosTurnView
    let interruptingTurnId: String?
    let interrupt: (String) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                Text(turn.status).font(.caption).foregroundColor(.secondary)
                Spacer()
                if turn.isInProgress {
                    Button(interruptingTurnId == turn.id ? "停止中…" : "停止") { interrupt(turn.id) }
                        .disabled(interruptingTurnId == turn.id)
                        .accessibilityIdentifier("turn.interrupt.\(turn.id)")
                }
            }
            ForEach(turn.items, id: \.id) { item in
                VStack(alignment: .leading, spacing: 3) {
                    Text(item.title).font(.subheadline.weight(.semibold))
                    Text(item.body).font(.body).textSelection(.enabled)
                }
                .padding(10)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(item.kind == "user" ? Color.accentColor.opacity(0.12) : Color.secondary.opacity(0.10))
                .clipShape(RoundedRectangle(cornerRadius: 10))
                .accessibilityIdentifier("item.\(item.id)")
            }
        }
        .padding()
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color(UIColor.secondarySystemBackground))
        .clipShape(RoundedRectangle(cornerRadius: 14))
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
        ZStack(alignment: .topTrailing) {
            BexQrScannerController { result in
                switch result {
                case let .success(contents): completion(contents)
                case .failure: completion(nil)
                }
            }
            Button("キャンセル") { completion(nil) }
                .padding()
                .foregroundColor(.white)
                .accessibilityIdentifier("scanner.cancel")
        }
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

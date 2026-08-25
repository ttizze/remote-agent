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
    func refreshTaskList() { controller.refreshTaskList() }
    func startTask(projectId: String?, cwd: String, prompt: String) {
        controller.startTask(projectId: projectId, cwd: cwd, firstPrompt: prompt)
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
    @State private var search = ""
    @State private var collapsedProjectIds = Set<String>()
    @State private var newTask: NewTaskContext?

    var body: some View {
        List {
            if state.threadLoadState == .loading || state.projectLoadState == .loading {
                Section { ProgressView("プロジェクトとタスクを読み込み中…")
                    .accessibilityIdentifier("tasks.loading")
                    .taskListRowStyle()
                }
                .listSectionSeparator(.hidden)
            }
            if state.threadLoadState == .failed || state.projectLoadState == .failed {
                Section {
                    if let error = state.projectLoadError ?? state.threadLoadError {
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

            ForEach(state.projects, id: \.id) { project in
                Section {
                    if !collapsedProjectIds.contains(project.id) {
                        let threads = visibleThreads.filter { $0.projectId == project.id }
                        if threads.isEmpty {
                            Text("タスクはまだありません")
                                .font(.subheadline)
                                .foregroundColor(.secondary)
                                .taskListRowStyle()
                        } else {
                            ForEach(threads, id: \.id) { thread in
                                ThreadListRow(thread: thread) { model.openThread(thread.id) }
                            }
                        }
                    }
                } header: {
                    ProjectHeader(
                        project: project,
                        isCollapsed: collapsedProjectIds.contains(project.id),
                        toggle: { toggle(project.id) },
                        compose: { newTask = NewTaskContext(project: project) }
                    )
                }
                .listSectionSeparator(.hidden)
            }

            if state.projectLoadState == .ready && state.projects.isEmpty {
                Section {
                    Text("Codexに登録されたプロジェクトはありません")
                        .font(.subheadline)
                        .foregroundColor(.secondary)
                        .taskListRowStyle()
                }
                .listSectionSeparator(.hidden)
            }

            Section {
                let threads = visibleThreads.filter { thread in
                    guard let projectId = thread.projectId else { return true }
                    return !knownProjectIds.contains(projectId)
                }
                if state.threadLoadState == .ready && threads.isEmpty {
                    Text("プロジェクトに属さないチャットはありません")
                        .font(.subheadline)
                        .foregroundColor(.secondary)
                        .accessibilityIdentifier("tasks.empty")
                        .taskListRowStyle()
                } else {
                    ForEach(threads, id: \.id) { thread in
                        ThreadListRow(thread: thread) { model.openThread(thread.id) }
                    }
                }
            } header: {
                HStack {
                    Text("チャット")
                        .font(.title3.weight(.semibold))
                        .textCase(nil)
                    Spacer()
                    Button { newTask = NewTaskContext(project: nil) } label: {
                        Image(systemName: "square.and.pencil")
                    }
                    .accessibilityLabel("プロジェクトなしで新しいタスク")
                    .accessibilityIdentifier("tasks.new.chat")
                }
                .taskListRowStyle()
            }
            .listSectionSeparator(.hidden)
        }
        .listStyle(.plain)
        .background(Color(UIColor.systemBackground))
        .accessibilityIdentifier("tasks.list")
        .searchable(text: $search, prompt: "チャットを検索")
        .sheet(item: $newTask) { context in
            NewTaskSheet(context: context, model: model)
        }
        .navigationTitle("リモート")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .navigationBarLeading) {
                Button { model.showProfiles() } label: { Image(systemName: "line.3.horizontal") }
                    .accessibilityLabel("PC一覧")
            }
            ToolbarItem(placement: .principal) {
                VStack(spacing: 1) {
                    Text("リモート").font(.headline)
                    Text(state.selectedProfileName ?? "PC Host")
                        .font(.caption2)
                        .foregroundColor(.secondary)
                }
            }
            ToolbarItem(placement: .navigationBarTrailing) {
                Button { model.refreshTaskList() } label: { Image(systemName: "arrow.clockwise") }
                    .disabled(state.threadLoadState == .loading || state.projectLoadState == .loading)
                    .accessibilityLabel("更新")
                    .accessibilityIdentifier("tasks.refresh")
            }
        }
    }

    private var knownProjectIds: Set<String> { Set(state.projects.map(\.id)) }

    private var visibleThreads: [IosThreadSummaryView] {
        let query = search.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !query.isEmpty else { return state.threads }
        return state.threads.filter {
            $0.title.localizedCaseInsensitiveContains(query) ||
                $0.preview.localizedCaseInsensitiveContains(query)
        }
    }

    private func toggle(_ projectId: String) {
        if collapsedProjectIds.contains(projectId) {
            collapsedProjectIds.remove(projectId)
        } else {
            collapsedProjectIds.insert(projectId)
        }
    }
}

private extension View {
    func taskListRowStyle() -> some View {
        listRowSeparator(.hidden)
            .listRowBackground(Color.clear)
    }
}

private struct ProjectHeader: View {
    let project: IosProjectView
    let isCollapsed: Bool
    let toggle: () -> Void
    let compose: () -> Void

    var body: some View {
        HStack(spacing: 10) {
            Button(action: toggle) {
                HStack(spacing: 10) {
                    Image(systemName: "folder")
                    Text(project.name).font(.title3.weight(.semibold)).textCase(nil)
                    Image(systemName: isCollapsed ? "chevron.right" : "chevron.down")
                        .font(.caption.weight(.semibold))
                }
                .foregroundColor(.primary)
            }
            .accessibilityIdentifier("tasks.project.\(project.id)")
            Spacer()
            Button(action: compose) { Image(systemName: "square.and.pencil") }
                .accessibilityLabel("\(project.name)で新しいタスク")
                .accessibilityIdentifier("tasks.new.project.\(project.id)")
        }
        .padding(.vertical, 4)
        .taskListRowStyle()
    }
}

private struct ThreadListRow: View {
    let thread: IosThreadSummaryView
    let open: () -> Void

    var body: some View {
        Button(action: open) {
            HStack(spacing: 10) {
                Text(thread.title).font(.body).foregroundColor(.primary).lineLimit(2)
                Spacer()
                if thread.isActive { ProgressView().controlSize(.small) }
            }
            .padding(.vertical, 3)
        }
        .accessibilityIdentifier("tasks.row.\(thread.id)")
        .taskListRowStyle()
    }
}

private struct NewTaskContext: Identifiable {
    let project: IosProjectView?
    let id = UUID()
}

private struct NewTaskSheet: View {
    let context: NewTaskContext
    @ObservedObject var model: BexAppViewModel
    @Environment(\.dismiss) private var dismiss
    @State private var workingDirectory: String
    @State private var prompt = ""

    init(context: NewTaskContext, model: BexAppViewModel) {
        self.context = context
        self.model = model
        _workingDirectory = State(initialValue: context.project?.roots.first ?? "")
    }

    var body: some View {
        NavigationView {
            Form {
                Section("開始先") {
                    if let project = context.project {
                        Label(project.name, systemImage: "folder")
                        if project.roots.count > 1 {
                            Picker("作業ディレクトリ", selection: $workingDirectory) {
                                ForEach(project.roots, id: \.self) { Text($0).tag($0) }
                            }
                        } else if let root = project.roots.first {
                            Text(root).font(.caption).foregroundColor(.secondary)
                        } else {
                            TextField("作業ディレクトリ", text: $workingDirectory)
                                .textInputAutocapitalization(.never)
                                .disableAutocorrection(true)
                                .accessibilityIdentifier("new-task.cwd")
                        }
                    } else {
                        Text("プロジェクトなし")
                        TextField("作業ディレクトリ", text: $workingDirectory)
                            .textInputAutocapitalization(.never)
                            .disableAutocorrection(true)
                            .accessibilityIdentifier("new-task.cwd")
                    }
                }
                Section("最初のメッセージ") {
                    TextEditor(text: $prompt)
                        .frame(minHeight: 140)
                        .accessibilityIdentifier("new-task.prompt")
                }
            }
            .navigationTitle("新しいタスク")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("キャンセル") { dismiss() }
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button("開始") {
                        model.startTask(
                            projectId: context.project?.id,
                            cwd: workingDirectory,
                            prompt: prompt
                        )
                        dismiss()
                    }
                    .disabled(workingDirectory.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ||
                              prompt.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                    .accessibilityIdentifier("new-task.start")
                }
            }
        }
    }
}

private struct ThreadScreen: View {
    let state: IosAppViewState
    @ObservedObject var model: BexAppViewModel
    @State private var draft = ""
    @State private var scrollViewportHeight: CGFloat = 0
    @State private var latestMarkerY: CGFloat = 0

    private let latestMarker = "thread-latest"

    private var contentVersion: String {
        state.selectedThread?.turns.map { turn in
            let items = turn.items.map { "\($0.id):\($0.body.count)" }.joined(separator: ",")
            return "\(turn.id):\(turn.status):\(items)"
        }.joined(separator: "|") ?? ""
    }

    private var isFollowingLatest: Bool {
        latestMarkerY == 0 || latestMarkerY <= scrollViewportHeight + 80
    }

    var body: some View {
        VStack(spacing: 0) {
            if let notice = state.notice { BexNotice(text: notice).padding(.horizontal).padding(.top, 8) }
            if let thread = state.selectedThread {
                ScrollViewReader { proxy in
                    ScrollView {
                        LazyVStack(alignment: .leading, spacing: 12) {
                            ForEach(detailRows) { row in
                                switch row {
                                case let .turnHeader(_, turn):
                                    ThreadTurnHeader(
                                        turn: turn,
                                        interruptingTurnId: state.interruptingTurnId,
                                        interrupt: model.interrupt
                                    )
                                case let .item(_, _, item):
                                    ThreadItemRow(item: item)
                                }
                            }
                            Color.clear
                                .frame(height: 1)
                                .id(latestMarker)
                                .background(
                                    GeometryReader { geometry in
                                        Color.clear.preference(
                                            key: LatestMarkerPreferenceKey.self,
                                            value: geometry.frame(in: .named("thread-scroll")).maxY
                                        )
                                    }
                                )
                        }
                        .padding()
                    }
                    .coordinateSpace(name: "thread-scroll")
                    .background(
                        GeometryReader { geometry in
                            Color.clear.preference(key: ScrollViewportPreferenceKey.self, value: geometry.size.height)
                        }
                    )
                    .onPreferenceChange(ScrollViewportPreferenceKey.self) { scrollViewportHeight = $0 }
                    .onPreferenceChange(LatestMarkerPreferenceKey.self) { latestMarkerY = $0 }
                    .onAppear {
                        DispatchQueue.main.async {
                            proxy.scrollTo(latestMarker, anchor: .bottom)
                        }
                    }
                    .onChange(of: contentVersion) { _ in
                        guard isFollowingLatest else { return }
                        DispatchQueue.main.async {
                            proxy.scrollTo(latestMarker, anchor: .bottom)
                        }
                    }
                    .accessibilityIdentifier("task.detail")
                    .accessibilityValue(
                        "turns=\(thread.turns.count);items=\(thread.turns.reduce(0) { $0 + $1.items.count })"
                    )
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
                .disabled(
                    state.selectedThread == nil ||
                    draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                )
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

    private var detailRows: [ThreadDetailRow] {
        guard let thread = state.selectedThread else { return [] }

        var rows: [ThreadDetailRow] = []
        for (turnIndex, turn) in thread.turns.enumerated() {
            rows.append(.turnHeader(turnIndex: turnIndex, turn: turn))
            for (itemIndex, item) in turn.items.enumerated() {
                rows.append(.item(turnIndex: turnIndex, itemIndex: itemIndex, item: item))
            }
        }
        return rows
    }
}

private struct ScrollViewportPreferenceKey: PreferenceKey {
    static var defaultValue: CGFloat = 0
    static func reduce(value: inout CGFloat, nextValue: () -> CGFloat) { value = nextValue() }
}

private struct LatestMarkerPreferenceKey: PreferenceKey {
    static var defaultValue: CGFloat = 0
    static func reduce(value: inout CGFloat, nextValue: () -> CGFloat) { value = nextValue() }
}

private enum ThreadDetailRow: Identifiable {
    case turnHeader(turnIndex: Int, turn: IosTurnView)
    case item(turnIndex: Int, itemIndex: Int, item: IosItemView)

    var id: String {
        switch self {
        case let .turnHeader(turnIndex, _):
            return "turn-\(turnIndex)-header"
        case let .item(turnIndex, itemIndex, _):
            return "turn-\(turnIndex)-item-\(itemIndex)"
        }
    }
}

private struct ThreadTurnHeader: View {
    let turn: IosTurnView
    let interruptingTurnId: String?
    let interrupt: (String) -> Void

    var body: some View {
        HStack {
            Text(turn.status).font(.caption).foregroundColor(.secondary)
            Spacer()
            if turn.isInProgress {
                Button(interruptingTurnId == turn.id ? "停止中…" : "停止") { interrupt(turn.id) }
                    .disabled(interruptingTurnId == turn.id)
                    .accessibilityIdentifier("turn.interrupt.\(turn.id)")
            }
        }
        .padding()
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color(UIColor.secondarySystemBackground))
        .clipShape(RoundedRectangle(cornerRadius: 14))
    }
}

private struct ThreadItemRow: View {
    let item: IosItemView

    var body: some View {
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

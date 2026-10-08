import AgentCore
import SwiftUI
import UIKit
import UniformTypeIdentifiers

/// Where the thread screen sends the user.
struct ThreadRoutes {
    /// `nil` opens a new terminal.
    let terminal: (String?) -> Void
    let files: () -> Void
    let review: () -> Void
    let device: () -> Void
}

/// One thread: header, feed, the floating working control, request cards and
/// the composer.
struct ThreadScreen: View {
    @ObservedObject var model: BexAppViewModel
    let routes: ThreadRoutes
    @State private var following = true
    @State private var userScrolling = false
    @State private var atEnd = true
    @State private var showingQueue = false
    @State private var showingAgents = false
    @State private var showingSettings = false
    @State private var forkingRun: String?
    @State private var openedFile: FileTarget?
    @State private var openedPDF: FileTarget?
    @State private var contextPreview: ContextChip?
    @State private var touchingDevice: String?
    private let endId = "feed-end"

    var body: some View {
        Group {
            if let view = model.threadView {
                if view.syncStatus == .deleted {
                    EmptyStateText(title: "Thread unavailable",
                                   detail: "This thread was deleted or is no longer available.")
                } else {
                    content(view)
                }
            } else {
                VStack(spacing: 10) {
                    ProgressView()
                    Text("Opening thread…").font(AppTheme.font(14)).foregroundStyle(AppTheme.muted)
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
        }
        .background(AppTheme.screen.ignoresSafeArea())
        .navigationBarTitleDisplayMode(.inline)
        .toolbar { header }
        .sheet(isPresented: $showingQueue) { QueueSheet(model: model) }
        .sheet(isPresented: $showingAgents) { AgentsSheet(model: model) }
        .sheet(item: $openedFile) { ThreadFileSheet(model: model, target: $0) }
        .sheet(item: $openedPDF) { ThreadPDFSheet(model: model, target: $0) }
        .sheet(isPresented: Binding(
            get: { contextPreview != nil },
            set: { if !$0 { contextPreview = nil } }
        )) {
            if let contextPreview {
                ContextPreviewSheet(chip: contextPreview, openTerminal: {
                    openContextTerminal(contextPreview.terminalId)
                    self.contextPreview = nil
                })
            }
        }
        .environment(\.markdownLinks, MarkdownLinkOpener(
            workspaceRoot: model.cwd.isEmpty ? nil : model.cwd,
            openFile: { openedFile = $0 },
            openPDF: { openedPDF = $0 },
            loadFile: model.download
        ))
        .sheet(isPresented: $showingSettings) {
            if let controls = model.threadView?.composer.controls {
                ThreadSettingsSheet(model: model, controls: controls)
            }
        }
    }

    private func content(_ view: ThreadView) -> some View {
        ScrollViewReader { reader in
            ScrollView { feed(view) }
                .defaultScrollAnchor(.bottom)
                .scrollDismissesKeyboard(.interactively)
                .onScrollPhaseChange { _, phase in
                    let interacting = phase == .interacting
                    if interacting, !userScrolling {
                        following = feedLiveFollow(current: following, event: .userScrollBegin)
                    } else if !interacting, userScrolling {
                        following = feedLiveFollow(current: following, event: .userScrollEnd(
                            atEnd: atEnd, userScrollSessionActive: true
                        ))
                    }
                    userScrolling = interacting
                }
                .onScrollGeometryChange(for: Bool.self) { geometry in
                    geometry.contentSize.height - geometry.contentOffset.y - geometry.containerSize.height
                        + geometry.contentInsets.bottom < 24
                } action: { _, end in
                    atEnd = end
                    following = feedLiveFollow(current: following, event: .scroll(
                        atEnd: end, userScrollSessionActive: userScrolling
                    ))
                    model.showScrollToEnd = !following
                }
                .onChange(of: view.rowsRevision) { _, _ in
                    if following {
                        reader.scrollTo(endId, anchor: .bottom)
                    }
                }
                .onChange(of: view.threadId) { _, _ in
                    following = feedLiveFollow(current: following, event: .reset)
                    reader.scrollTo(endId, anchor: .bottom)
                }
                .modifier(StreamingHaptics(thread: view.threadId, streaming: view.streamingMessage))
                .safeAreaInset(edge: .bottom, spacing: 0) {
                    bottom(view) {
                        following = feedLiveFollow(current: following, event: .reset)
                        withAnimation { reader.scrollTo(endId, anchor: .bottom) }
                    }
                }
        }
    }

    private func feed(_ view: ThreadView) -> some View {
        let rows = model.timelineRows
        let firstUserMessage = rows.firstIndex(where: \.isUserMessage)
        return LazyVStack(alignment: .leading, spacing: 0) {
            if view.history.hasMore || view.history.loading {
                LoadEarlierButton(history: view.history) { model.loadEarlier() }
            }
            if rows.isEmpty, view.syncStatus != .synchronizing, view.setup.card == nil {
                EmptyStateText(
                    title: "No conversation yet",
                    detail: "Ask the agent to inspect the repo, run a command, or continue the active thread."
                )
            }
            if firstUserMessage == nil {
                setupCard(view)
            }
            ForEach(Array(rows.enumerated()), id: \.element.id) { index, row in
                FeedRowView(row: row, actions: actions(view), forking: forkingRun != nil)
                if index == firstUserMessage {
                    setupCard(view)
                }
            }
            Color.clear.frame(height: 1).id(endId)
        }
        .padding(.horizontal, 16)
        .padding(.top, 12)
        .frame(maxWidth: 960)
        .frame(maxWidth: .infinity)
    }

    @ViewBuilder
    private func setupCard(_ view: ThreadView) -> some View {
        if let card = view.setup.card, card.showInTimeline {
            SetupCard(card: card, cancel: { model.perform(.cancelSetup) }, workLocally: { model.perform(.workLocally) })
                .padding(.bottom, 10.5)
        }
    }

    private func bottom(_ view: ThreadView, scrollToEnd: @escaping () -> Void) -> some View {
        VStack(spacing: 8) {
            if let working = view.working, view.requests.approval == nil, view.requests.questions == nil {
                WorkingControl(
                    control: working,
                    reconnect: { model.connect(afterForeground: true) },
                    showAgents: { showingAgents = true },
                    showQueue: { showingQueue = true },
                    scrollToEnd: scrollToEnd
                )
            }
            if let banner = view.errorBanner {
                ErrorBanner(banner: banner) { model.perform(.dismissThreadError(dismissKey: banner.dismissKey)) }
            }
            if let recovery = view.limitRecovery {
                LimitRecoveryCard(recovery: recovery) { action in
                    model.perform(.limitRecovery(threadId: view.threadId, action: action))
                }
            }
            if let approval = view.requests.approval {
                ApprovalCard(approval: approval) { decision in
                    model.perform(.respondApproval(requestId: approval.requestId, decision: decision))
                }
                .padding(.horizontal, 14).padding(.bottom, 10.5)
                .transition(.move(edge: .bottom).combined(with: .opacity))
            } else if let questions = view.requests.questions {
                QuestionCard(model: model, questions: questions)
                    .padding(.horizontal, 14).padding(.bottom, 10.5)
                    .transition(.move(edge: .bottom).combined(with: .opacity))
            } else {
                Composer(model: model, composer: view.composer) { showingSettings = true }
            }
        }
        .animation(.easeOut(duration: 0.22), value: view.requests.approval?.requestId)
        .animation(.easeOut(duration: 0.22), value: view.requests.questions?.requestId)
    }

    private func actions(_ view: ThreadView) -> FeedActions {
        FeedActions(
            perform: { model.perform($0) },
            toggle: { model.toggle($0, $1) },
            fork: { run in
                Haptics.selection()
                forkingRun = run
                model.perform(.fork(sourceThreadId: view.threadId, runId: run)) { result in
                    forkingRun = nil
                    if case let .success(.startedThread(id)) = result {
                        model.openThread(id)
                    }
                }
            },
            openThread: { model.openThread($0) },
            openTerminal: openContextTerminal,
            showContextPreview: { contextPreview = $0 },
            download: model.downloadAttachment,
            useArtifactTemplate: { model.useArtifactTemplate($0) }
        )
    }

    private func openContextTerminal(_ terminalId: String?) {
        let resolved = terminalId
            .flatMap { id in model.threadView?.terminals.first { $0.terminalId == id }?.terminalId }
            ?? model.threadView?.terminals.first?.terminalId
        routes.terminal(resolved)
    }

    @ToolbarContentBuilder
    private var header: some ToolbarContent {
        let header = model.threadView?.header
        ToolbarItem(placement: .principal) {
            VStack(spacing: 1) {
                Text(header?.title ?? "").font(AppTheme.font(17, weight: .heavy)).lineLimit(1)
                if let subtitle = header?.subtitle, !subtitle.isEmpty {
                    Text(subtitle).font(AppTheme.font(12)).foregroundStyle(AppTheme.muted).lineLimit(1)
                }
            }
        }
        ToolbarItemGroup(placement: .topBarTrailing) {
            GitToolbarButton(
                model: model,
                cwd: header?.cwd ?? model.cwd,
                review: routes.review,
                mergeBack: { model.perform(.mergeBack) },
                mergeBackAvailable: header?.actions.contains(where: { $0.kind == .mergeBack }) == true
            )
            Button(action: routes.device) {
                Label("Device", systemImage: "iphone")
            }
            .accessibilityLabel("Device")
            Button(action: routes.files) { Image(systemName: "folder") }
                .accessibilityLabel("Files")
            TerminalMenu(model: model, open: routes.terminal)
        }
    }
}

extension TimelineRow {
    var isUserMessage: Bool {
        if case .userMessage = kind {
            return true
        }
        return false
    }
}

private struct TerminalMenu: View {
    @ObservedObject var model: BexAppViewModel
    let open: (String?) -> Void

    var body: some View {
        let view = model.threadView
        Menu {
            let scripts = view?.scripts?.rows ?? []
            if scripts.isEmpty {
                Button {} label: {
                    Label("No project scripts", systemImage: "play")
                    Text("This project has no saved scripts yet")
                }
                .disabled(true)
            }
            ForEach(scripts, id: \.script.id) { row in
                Button {
                    if let thread = view?.threadId {
                        model.perform(.runProjectScript(threadId: thread, scriptId: row.script.id, cols: 80,
                                                        rows: 24)) { result in
                            if case let .success(.terminalOpened(terminalId)) = result {
                                open(terminalId)
                            }
                        }
                    }
                } label: {
                    Label(row.label, systemImage: row.script.icon.symbol)
                    Text(row.script.command)
                }
            }
            ForEach((view?.terminals ?? []).filter(\.running), id: \.terminalId) { tab in
                Button { open(tab.terminalId) } label: {
                    Label(tab.label, systemImage: "terminal")
                    Text(tab.menuSubtitle)
                }
            }
            Button { open(nil) } label: {
                Label("Open new terminal", systemImage: "plus")
                Text("Start another shell for this thread")
            }
        } label: {
            Image(systemName: "terminal")
        }
        .accessibilityLabel("Terminal")
        .disabled(!model.snapshot.canOpenTerminal())
    }
}

/// Device picker, setup and the Host's latest live device frame.
struct DeviceScreen: View {
    @ObservedObject var model: BexAppViewModel
    let threadId: String
    @StateObject private var deviceFrames = DeviceFrameStore()
    @State private var recordingDocument: DeviceRecordingDocument?
    @State private var exportingRecording = false
    @State private var recordingFileName = "device-recording"
    @State private var recordingContentType: UTType = .data

    var body: some View {
        let view = model.snapshot.device()
        let threadSessions = view.sessions.filter { $0.threadId == threadId }
        let threadSessionKey = threadSessions.map { "\($0.hostId):\($0.deviceId)" }.joined(separator: ",")
        let decodedFrames = deviceFrames.frames(for: threadId)
        ScrollView {
            VStack(alignment: .leading, spacing: 12) {
                if !view.enabled {
                    Text("Device support is off").font(AppTheme.font(17, weight: .semibold))
                    Text("Enable it to discover simulators and emulators on the Host.")
                        .foregroundStyle(AppTheme.muted)
                    Button("Enable device support") {
                        model.perform(.configureDevices(enabled: true, agentAccessEnabled: nil, onboardingCompleted: false))
                    }
                } else {
                    Text("Host status: \(view.status)").foregroundStyle(AppTheme.muted)
                    HStack(spacing: 8) {
                        Button("Inspect tools") { model.perform(.inspectDevices(hostId: nil)) }
                        Button("Update hub") { model.perform(.updateDeviceTool(hostId: nil, tool: "hub")) }
                        Button("Update agent") { model.perform(.updateDeviceTool(hostId: nil, tool: "agent")) }
                    }
                    if let detail = view.statusDetail {
                        Text(detail).font(AppTheme.font(13)).foregroundStyle(AppTheme.muted)
                    }
                    if !view.agentAccessEnabled {
                        Button("Enable agent device access") {
                            model.perform(.configureDevices(enabled: nil, agentAccessEnabled: true, onboardingCompleted: true))
                        }
                    }
                    ForEach(view.hosts, id: \.id) { host in
                        HStack {
                            Text("\(host.label) · \(host.kind)").font(AppTheme.font(13)).foregroundStyle(AppTheme.muted)
                            Spacer()
                            Button("Retry") { model.perform(.retryDeviceHost(hostId: host.id)) }
                        }
                        ForEach(host.unavailableReasons, id: \.self) { reason in
                            Text(reason).font(AppTheme.font(12)).foregroundStyle(AppTheme.muted)
                        }
                    }
                    ForEach(Array(view.devices.enumerated()), id: \.offset) { _, device in
                        let opened = view.sessions.contains {
                            $0.threadId == threadId && $0.hostId == device.hostId && $0.deviceId == device.id
                        }
                        HStack {
                            VStack(alignment: .leading) {
                                Text(device.name).font(AppTheme.font(16, weight: .semibold))
                                Text("\(device.platform) · \(device.version)").foregroundStyle(AppTheme.muted)
                            }
                            Spacer()
                            Button(opened ? "Close" : "Open") {
                                if opened {
                                    model.perform(.closeDevice(hostId: device.hostId, deviceId: device.id, shutdown: false))
                                } else {
                                    model.perform(.openDevice(hostId: device.hostId, deviceId: device.id, platform: device.platform, boot: true))
                                }
                            }
                        }
                        .padding(12)
                        .background(AppTheme.card, in: RoundedRectangle(cornerRadius: 14))
                    }
                    ForEach(Array(threadSessions.enumerated()), id: \.offset) { _, session in
                        let detail = view.details.first(where: { $0.hostId == session.hostId && $0.deviceId == session.deviceId })
                        HStack(spacing: 8) {
                            Button("Dark") {
                                model.perform(.deviceAction(
                                    hostId: session.hostId,
                                    deviceId: session.deviceId,
                                    action: .setAppearance(dark: true)
                                ))
                            }
                            Button("Light") {
                                model.perform(.deviceAction(
                                    hostId: session.hostId,
                                    deviceId: session.deviceId,
                                    action: .setAppearance(dark: false)
                                ))
                            }
                            Button("Text +") {
                                model.perform(.deviceAction(
                                    hostId: session.hostId,
                                    deviceId: session.deviceId,
                                    action: .setTextSize(size: "large")
                                ))
                            }
                            if session.platform == "android" {
                                Button("Portrait") {
                                    model.perform(.deviceAction(
                                        hostId: session.hostId,
                                        deviceId: session.deviceId,
                                        action: .setOrientation(orientation: "portrait")
                                    ))
                                }
                            }
                            Button("Home") {
                                model.perform(.deviceAction(
                                    hostId: session.hostId,
                                    deviceId: session.deviceId,
                                    action: .hardwareButton(button: "home")
                                ))
                            }
                            Button("Back") {
                                model.perform(.deviceAction(
                                    hostId: session.hostId,
                                    deviceId: session.deviceId,
                                    action: .hardwareButton(button: "back")
                                ))
                            }
                            Button("Recents") {
                                model.perform(.deviceAction(
                                    hostId: session.hostId,
                                    deviceId: session.deviceId,
                                    action: .hardwareButton(button: "recents")
                                ))
                            }
                            Button("Power") {
                                model.perform(.deviceAction(
                                    hostId: session.hostId,
                                    deviceId: session.deviceId,
                                    action: .hardwareButton(button: "power")
                                ))
                            }
                            Button("Enter") {
                                model.perform(.deviceAction(
                                    hostId: session.hostId,
                                    deviceId: session.deviceId,
                                    action: .key(code: "Enter", down: true)
                                ))
                                model.perform(.deviceAction(
                                    hostId: session.hostId,
                                    deviceId: session.deviceId,
                                    action: .key(code: "Enter", down: false)
                                ))
                            }
                            Button("Rotate") {
                                model.perform(.deviceAction(
                                    hostId: session.hostId,
                                    deviceId: session.deviceId,
                                    action: .rotate
                                ))
                            }
                            Button("Book fold") {
                                model.perform(.deviceAction(
                                    hostId: session.hostId,
                                    deviceId: session.deviceId,
                                    action: .fold(command: "book")
                                ))
                            }
                            Button("Table") {
                                model.perform(.deviceAction(
                                    hostId: session.hostId,
                                    deviceId: session.deviceId,
                                    action: .duo(command: "table")
                                ))
                            }
                            Button("Record") {
                                model.perform(.startDeviceRecording(hostId: session.hostId, deviceId: session.deviceId, format: "avcc"))
                            }
                            Button("Stop record") {
                                model.perform(.stopDeviceRecording(hostId: session.hostId, deviceId: session.deviceId))
                            }
                            Button("Power off") {
                                model.perform(.closeDevice(hostId: session.hostId, deviceId: session.deviceId, shutdown: true))
                            }
                        }
                        if let app = detail?.foregroundApp {
                            Text("Foreground: \(app)").font(AppTheme.font(13)).foregroundStyle(AppTheme.muted)
                        }
                        ForEach(Array(view.screens.filter {
                            $0.threadId == threadId && $0.hostId == session.hostId && $0.deviceId == session.deviceId
                        }.enumerated()), id: \.offset) { _, screen in
                            Text("Screen \(screen.screenId.map(String.init) ?? "main") · \(screen.width)×\(screen.height) · \(screen.orientation)")
                                .font(AppTheme.font(12)).foregroundStyle(AppTheme.muted)
                        }
                    }
                    if !decodedFrames.isEmpty {
                        HStack(spacing: 8) {
                            ForEach(decodedFrames) { frame in
                                ZStack {
                                    Image(uiImage: frame.image).resizable().scaledToFit().frame(maxWidth: .infinity)
                                    DeviceAccessibilityOverlay(view: view, deviceKey: "\(frame.hostId):\(frame.deviceId)")
                                }
                                .overlay {
                                    GeometryReader { proxy in
                                        Color.clear
                                            .contentShape(Rectangle())
                                            .gesture(deviceTouchGesture(hostId: frame.hostId, deviceId: frame.deviceId, size: proxy.size))
                                    }
                                }
                            }
                        }
                        .frame(maxWidth: .infinity)
                        .contentShape(Rectangle())
                    } else if let frame = view.frames.filter({ $0.threadId == threadId }).max(by: { $0.sequence < $1.sequence }),
                       let image = UIImage(data: Data(frame.png)) {
                        ZStack {
                            Image(uiImage: image).resizable().scaledToFit().frame(maxWidth: .infinity)
                            DeviceAccessibilityOverlay(view: view, deviceKey: "\(frame.hostId):\(frame.deviceId)")
                        }
                        .overlay {
                            GeometryReader { proxy in
                                Color.clear
                                    .contentShape(Rectangle())
                                    .gesture(deviceTouchGesture(hostId: frame.hostId, deviceId: frame.deviceId, size: proxy.size))
                            }
                        }
                    } else if let frame = view.videoFrames.filter({ $0.threadId == threadId && ($0.encoding == "jpeg" || $0.encoding == "mjpeg") }).max(by: { $0.sequence < $1.sequence }),
                              let image = UIImage(data: Data(frame.payload)) {
                        ZStack {
                            Image(uiImage: image).resizable().scaledToFit().frame(maxWidth: .infinity)
                            DeviceAccessibilityOverlay(view: view, deviceKey: "\(frame.hostId):\(frame.deviceId)")
                        }
                        .overlay {
                            GeometryReader { proxy in
                                Color.clear
                                    .contentShape(Rectangle())
                                    .gesture(deviceTouchGesture(hostId: frame.hostId, deviceId: frame.deviceId, size: proxy.size))
                            }
                        }
                    } else {
                        Text("Open a device to see its live frame").foregroundStyle(AppTheme.muted)
                    }
                    ForEach(Array(view.accessibility.filter { tree in threadSessions.contains { $0.hostId == tree.hostId && $0.deviceId == tree.deviceId } }.enumerated()), id: \.offset) { _, tree in
                        VStack(alignment: .leading, spacing: 4) {
                            Text("Accessibility overlay").font(AppTheme.font(13, weight: .semibold))
                            ForEach(tree.elements.filter { !$0.label.isEmpty }.prefix(20), id: \.id) { element in
                                Text("\(element.role) · \(element.label)").font(AppTheme.font(12)).foregroundStyle(AppTheme.muted)
                            }
                        }
                    }
                    ForEach(Array(view.eventLog.filter { entry in threadSessions.contains { $0.hostId == entry.hostId && $0.deviceId == entry.deviceId } }.suffix(20).enumerated()), id: \.offset) { _, entry in
                        Text("\(entry.kind) · \(entry.summary)")
                            .font(AppTheme.font(12)).foregroundStyle(AppTheme.muted)
                    }
                    if let recording = view.lastRecording, recording.threadId == threadId {
                        VStack(alignment: .leading, spacing: 6) {
                            Text("Recording ready · \(recording.frameCount) frames · \(recording.byteCount) bytes")
                                .font(AppTheme.font(12))
                                .foregroundStyle(AppTheme.muted)
                            HStack(spacing: 8) {
                                Button("Save recording") {
                                    guard let file = deviceRecordingFileType(recording.format, recording.bytes) else {
                                        model.notice = "The Host did not return a playable device recording."
                                        return
                                    }
                                    recordingDocument = DeviceRecordingDocument(data: Data(recording.bytes))
                                    recordingFileName = "device-recording.\(file.extension)"
                                    recordingContentType = file.type
                                    exportingRecording = true
                                }
                            Button("Attach recording") {
                                do {
                                    let directory = try AttachmentFiles.directory()
                                    guard let file = deviceRecordingFileType(recording.format, recording.bytes) else {
                                        model.notice = "The Host did not return a playable device recording."
                                        return
                                    }
                                    let url = directory.appendingPathComponent("device-\(recording.deviceId)-\(recording.byteCount).\(file.extension)")
                                    try Data(recording.bytes).write(to: url, options: .atomic)
                                    model.perform(.attachFiles(draftKey: model.snapshot.currentDraftKey(), files: [
                                        LocalFile(path: url.path, name: url.lastPathComponent, mimeType: file.mime)
                                    ]))
                                } catch {
                                    model.notice = error.localizedDescription
                                }
                            }
                            }
                        }
                    }
                }
            }
            .padding(16)
        }
        .background(AppTheme.screen.ignoresSafeArea())
        .navigationTitle("Device")
        .onAppear {
            model.perform(.openThread(threadId: threadId))
            model.perform(.loadDevices)
            model.perform(.subscribeDevice)
            deviceFrames.consume(view.videoEvents, threadId: threadId)
            for session in threadSessions {
                model.perform(.loadDeviceDetail(hostId: session.hostId, deviceId: session.deviceId))
                model.perform(.loadDeviceAccessibility(hostId: session.hostId, deviceId: session.deviceId))
                model.perform(.loadDeviceEventLog(hostId: session.hostId, deviceId: session.deviceId, limit: 100))
            }
        }
        .onChange(of: model.snapshot.device().revision) { _, _ in
            let next = model.snapshot.device()
            deviceFrames.consume(next.videoEvents, threadId: threadId)
        }
        .onChange(of: threadSessionKey) { _, _ in
            let sessions = model.snapshot.device().sessions
            for session in sessions where session.threadId == threadId {
                model.perform(.loadDeviceDetail(hostId: session.hostId, deviceId: session.deviceId))
                model.perform(.loadDeviceAccessibility(hostId: session.hostId, deviceId: session.deviceId))
                model.perform(.loadDeviceEventLog(hostId: session.hostId, deviceId: session.deviceId, limit: 100))
            }
        }
        .onDisappear {
            deviceFrames.reset(threadId: threadId)
            model.perform(.unsubscribeDevice)
        }
        .fileExporter(
            isPresented: $exportingRecording,
            document: recordingDocument,
            contentType: recordingContentType,
            defaultFilename: recordingFileName
        ) { result in
            if case let .failure(error) = result { model.notice = error.localizedDescription }
        }
    }

    private func deviceTouchGesture(hostId: String, deviceId: String, size: CGSize) -> some Gesture {
        DragGesture(minimumDistance: 0)
            .onChanged { value in
                let key = "\(hostId):\(deviceId)"
                let phase = touchingDevice == key ? "move" : "begin"
                touchingDevice = key
                let x = value.location.x / max(size.width, 1)
                let y = value.location.y / max(size.height, 1)
                model.perform(.deviceAction(
                    hostId: hostId,
                    deviceId: deviceId,
                    action: .touch(phase: phase, x: Float(x.clamped(to: 0...1)), y: Float(y.clamped(to: 0...1))),
                ))
            }
            .onEnded { value in
                touchingDevice = nil
                let x = value.location.x / max(size.width, 1)
                let y = value.location.y / max(size.height, 1)
                model.perform(.deviceAction(
                    hostId: hostId,
                    deviceId: deviceId,
                    action: .touch(phase: "end", x: Float(x.clamped(to: 0...1)), y: Float(y.clamped(to: 0...1))),
                ))
            }
    }
}

private struct DeviceRecordingDocument: FileDocument {
    static var readableContentTypes: [UTType] { [.data, .movie] }
    let data: Data

    init(data: Data) { self.data = data }

    init(configuration: ReadConfiguration) throws {
        data = configuration.file.regularFileContents ?? Data()
    }

    func fileWrapper(configuration: WriteConfiguration) throws -> FileWrapper {
        FileWrapper(regularFileWithContents: data)
    }
}

private func deviceRecordingFileType(_ format: String, _ bytes: [UInt8]) -> (extension: String, mime: String, type: UTType)? {
    if bytes.starts(with: [0x1a, 0x45, 0xdf, 0xa3]) {
        return ("webm", "video/webm", UTType(filenameExtension: "webm") ?? .movie)
    }
    if bytes.count >= 12, Array(bytes[4..<8]) == Array("ftyp".utf8) {
        return ("mp4", "video/mp4", UTType(filenameExtension: "mp4") ?? .movie)
    }
    if format == "mjpeg", zip(bytes, bytes.dropFirst()).contains(where: { $0.0 == 0xff && $0.1 == 0xd8 }) {
        return ("mjpeg", "video/x-motion-jpeg", UTType.data)
    }
    return nil
}

private extension CGFloat {
    func clamped(to range: ClosedRange<CGFloat>) -> CGFloat {
        min(max(self, range.lowerBound), range.upperBound)
    }
}

private struct DeviceAccessibilityOverlay: View {
    let view: DeviceView
    let deviceKey: String

    var body: some View {
        GeometryReader { proxy in
            ForEach(view.accessibility.filter { "\($0.hostId):\($0.deviceId)" == deviceKey }.flatMap(\.elements).filter { !$0.label.isEmpty }, id: \.id) { element in
                RoundedRectangle(cornerRadius: 3)
                    .stroke(AppTheme.primary, lineWidth: 1)
                    .frame(width: proxy.size.width * CGFloat(element.width), height: proxy.size.height * CGFloat(element.height))
                    .position(x: proxy.size.width * CGFloat(element.x + element.width / 2), y: proxy.size.height * CGFloat(element.y + element.height / 2))
                    .accessibilityLabel(element.label)
            }
        }
        .allowsHitTesting(false)
    }
}

extension TerminalTab {
    /// "Ready · app": the status, then the shell's folder.
    var menuSubtitle: String {
        let folder = URL(fileURLWithPath: cwd).lastPathComponent
        return cwd.isEmpty || folder.isEmpty ? menuStatus : "\(menuStatus) · \(folder)"
    }
}

extension ProjectScriptIcon {
    var symbol: String {
        switch self {
        case .play: "play"
        case .test: "flask"
        case .lint: "checklist"
        case .configure: "wrench.and.screwdriver"
        case .build: "hammer"
        case .debug: "ladybug"
        }
    }
}

private struct LoadEarlierButton: View {
    let history: ThreadHistoryView
    let load: () -> Void

    var body: some View {
        VStack(spacing: 6) {
            Button(action: load) {
                HStack(spacing: 6) {
                    if history.loading {
                        ProgressView().controlSize(.mini)
                    } else {
                        Image(systemName: "chevron.up").font(.system(size: 12)).foregroundStyle(AppTheme.primary)
                    }
                    Text(history.loading ? "Loading earlier activity…" : "Load earlier activity")
                        .font(AppTheme.font(14, weight: .medium)).foregroundStyle(AppTheme.text)
                }
                .padding(.horizontal, 14).padding(.vertical, 7)
                .frame(minHeight: 31.5)
                .background(AppTheme.card.opacity(0.8), in: Capsule())
                .overlay(Capsule().stroke(AppTheme.border.opacity(0.6)))
            }
            .buttonStyle(.plain)
            .disabled(history.loading)
            if let error = history.error {
                Text(error).font(AppTheme.font(13)).foregroundStyle(AppTheme.muted)
            }
        }
        .frame(maxWidth: .infinity)
        .padding(.bottom, 12)
    }
}

private struct ErrorBanner: View {
    let banner: ThreadErrorBanner
    let dismiss: () -> Void

    var body: some View {
        HStack(alignment: .top, spacing: 8) {
            Image(systemName: "exclamationmark.circle").font(.system(size: 14))
            Text(banner.text).font(AppTheme.font(13)).frame(maxWidth: .infinity, alignment: .leading)
            Button(action: dismiss) { Image(systemName: "xmark").font(.system(size: 12)) }
                .buttonStyle(.plain)
                .accessibilityLabel(banner.dismissLabel)
        }
        .foregroundStyle(AppTheme.dangerForeground)
        .padding(10.5)
        .background(AppTheme.danger, in: RoundedRectangle(cornerRadius: 10.5))
        .padding(.horizontal, 10.5)
    }
}

private struct LimitRecoveryCard: View {
    let recovery: UsageLimitRecovery
    let act: (RecoveryAction) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(recovery.title).font(AppTheme.font(13)).foregroundStyle(AppTheme.text)
            HStack(spacing: 8) {
                if recovery.canSchedule {
                    Button(recovery.resumeLabel) { act(.resume) }
                }
                if recovery.snoozeEnabled {
                    Button(recovery.snoozeLabel) { act(.snooze) }
                }
            }
            .buttonStyle(.plain)
            .font(AppTheme.font(13, weight: .medium))
            .padding(.horizontal, 10).padding(.vertical, 6)
            .background(AppTheme.subtle, in: RoundedRectangle(cornerRadius: 7))
        }
        .padding(10.5)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(AppTheme.screen, in: RoundedRectangle(cornerRadius: 10.5))
        .overlay(RoundedRectangle(cornerRadius: 10.5).stroke(AppTheme.warningForeground.opacity(0.25)))
        .padding(.horizontal, 10.5)
    }
}

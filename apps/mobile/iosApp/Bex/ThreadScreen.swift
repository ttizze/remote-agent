import AgentCore
import SwiftUI

/// Where the thread screen sends the user.
struct ThreadRoutes {
    /// `nil` opens a new terminal.
    let terminal: (String?) -> Void
    let files: () -> Void
    let review: () -> Void
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
        .environment(\.markdownLinks, MarkdownLinkOpener(
            workspaceRoot: model.cwd.isEmpty ? nil : model.cwd, openFile: { openedFile = $0 }, loadFile: model.download
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
                FeedRowView(row: row, actions: actions(view), forking: forkingRun != nil).equatable()
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
            download: model.downloadAttachment,
            useArtifactTemplate: { model.useArtifactTemplate($0) }
        )
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
            Menu {
                Button(action: routes.review) {
                    Label("Review changes", systemImage: "text.bubble")
                    Text("Turn diffs and worktree changes")
                }
                if header?.actions.contains(where: { $0.kind == .mergeBack }) == true {
                    Button { model.perform(.mergeBack) } label: {
                        Label("Merge back to source", systemImage: "arrow.triangle.merge")
                        Text("Bring this thread's latest turn into its source")
                    }
                }
            } label: {
                Image(systemName: "point.topleft.down.curvedto.point.bottomright.up")
            }
            .accessibilityLabel("Git")
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

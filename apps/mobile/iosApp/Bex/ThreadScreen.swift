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

private enum ThreadSheet: Identifiable {
    case queue
    case agents
    case settings
    case file(FileTarget)
    case pdf(FileTarget)
    case context(ContextChip)

    var id: String {
        switch self {
        case .queue: "queue"
        case .agents: "agents"
        case .settings: "settings"
        case let .file(target): "file:\(target.id)"
        case let .pdf(target): "pdf:\(target.id)"
        case let .context(chip): "context:\(chip.contextId)"
        }
    }
}

/// One thread: header, feed, the floating working control, request cards and
/// the composer.
struct ThreadScreen: View {
    @ObservedObject var model: BexAppViewModel
    let routes: ThreadRoutes
    @State private var following = true
    @State private var userScrolling = false
    @State private var atEnd = true
    @State private var forkingRun: String?
    @State private var sheet: ThreadSheet?
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
        .sheet(item: $sheet) { sheet in
            switch sheet {
            case .queue:
                QueueSheet(model: model)
            case .agents:
                AgentsSheet(model: model)
            case .settings:
                if let controls = model.threadView?.composer.controls {
                    ThreadSettingsSheet(model: model, controls: controls)
                }
            case let .file(target):
                ThreadFileSheet(model: model, target: target)
            case let .pdf(target):
                ThreadPDFSheet(model: model, target: target)
            case let .context(chip):
                ContextPreviewSheet(chip: chip, openTerminal: {
                    openContextTerminal(chip.terminalId)
                    self.sheet = nil
                })
            }
        }
        .environment(\.markdownLinks, MarkdownLinkOpener(
            workspaceRoot: model.cwd.isEmpty ? nil : model.cwd,
            openFile: { sheet = .file($0) },
            openPDF: { sheet = .pdf($0) },
            loadFile: model.download
        ))
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
        let feedActions = actions(threadId: view.threadId)
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
                FeedRowView(row: row, actions: feedActions, forking: forkingRun != nil).equatable()
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
                    showAgents: { sheet = .agents },
                    showQueue: { sheet = .queue },
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
                Composer(model: model, composer: view.composer) { sheet = .settings }
            }
        }
        .animation(.easeOut(duration: 0.22), value: view.requests.approval?.requestId)
        .animation(.easeOut(duration: 0.22), value: view.requests.questions?.requestId)
    }

    private func actions(threadId: String) -> FeedActions {
        FeedActions(
            perform: { model.perform($0) },
            toggle: { model.toggle($0, $1) },
            fork: { run in
                Haptics.selection()
                forkingRun = run
                model.perform(.fork(sourceThreadId: threadId, runId: run)) { result in
                    forkingRun = nil
                    if case let .success(.startedThread(id)) = result {
                        model.openThread(id)
                    }
                }
            },
            openThread: { model.openThread($0) },
            openTerminal: openContextTerminal,
            showContextPreview: { sheet = .context($0) },
            download: model.downloadAttachment,
            useArtifactTemplate: model.useArtifactTemplate
        )
    }

    private func openContextTerminal(_ terminalId: String?) {
        let terminalIds = model.threadView?.terminals.map(\.terminalId) ?? []
        routes.terminal(
            terminalNavigationTarget(
                availableTerminalIds: terminalIds,
                requestedTerminalId: terminalId
            )
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

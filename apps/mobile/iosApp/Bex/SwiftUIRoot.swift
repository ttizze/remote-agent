import AgentCore
import SwiftUI
import UIKit

struct BexSwiftUIRoot: View {
    @ObservedObject var model: BexAppViewModel
    @State private var appearanceRevision = 0
    @State private var showingUsage = false

    var body: some View {
        let _ = appearanceRevision
        Group {
            if model.profiles.isEmpty {
                NavigationStack { pairingScreen }
            } else if model.screen == .profiles || model.screen == .pairing {
                NavigationStack {
                    ProfilesScreen(profiles: model.profiles, environments: model.environments, notice: model.notice,
                                   select: model.selectProfile, remove: model.removeProfile, add: model.openPairing)
                }
                .sheet(isPresented: Binding(
                    get: { model.screen == .pairing },
                    set: {
                        if !$0, model.screen == .pairing {
                            model.dismissPairing()
                        }
                    }
                )) {
                    NavigationStack { pairingScreen }
                }
            } else {
                WorkspaceRoot(model: model)
            }
        }
        .tint(AppTheme.color("mobilePrimaryText"))
        .font(AppTheme.font())
        .preferredColorScheme(AppTheme.preferredColorScheme)
        .onReceive(NotificationCenter.default.publisher(for: .mobileAppearanceDidChange)) { _ in
            appearanceRevision += 1
        }
        .onReceive(NotificationCenter.default.publisher(for: .agentPushDeepLink)) { notification in
            guard let deepLink = notification.object as? String else { return }
            openPushDeepLink(deepLink)
        }
        .onOpenURL {
            let value = $0.absoluteString
            if AgentPushCenter.isActivityOverviewDeepLink(value) {
                model.openActivityOverviewDeepLink()
            } else if AgentPushCenter.isUsageDeepLink(value) {
                model.openUsageDeepLink()
            } else if let target = AgentCore.agentActivityThreadTarget(value: value) {
                model.openPushThread(hostId: target.environmentId, threadId: target.threadId)
            } else {
                model.handleSurfaceURL($0)
            }
        }
        .onChange(of: model.usageDeepLinkRequests) { _, count in
            if count > 0 {
                showingUsage = true
            }
        }
        .sheet(isPresented: $showingUsage) {
            NavigationStack {
                UsageScreen(model: model, initialTab: .limits)
                    .onAppear { model.consumeUsageDeepLinkRequest() }
                    .onChange(of: model.usageDeepLinkRequests) { _, count in
                        if count > 0 {
                            model.consumeUsageDeepLinkRequest()
                        }
                    }
            }
        }
        .sheet(isPresented: $model.isScanning) {
            QRScannerSheet { model.scanned($0) }
                .interactiveDismissDisabled()
        }
    }

    private var pairingScreen: some View {
        PairingScreen(canCancel: !model.profiles.isEmpty,
                      connecting: model.isConnecting, error: model.pairingError,
                      scan: { model.isScanning = true },
                      hostName: model.pairingInvitation?.hostName,
                      aiRecipients: model.pairingInvitation?.aiRecipients ?? [],
                      transcriptionRecipient: model.pairingInvitation?.transcriptionRecipient,
                      prepare: model.preparePairing, confirm: model.confirmPairing,
                      change: model.openPairing, cancel: model.dismissPairing)
    }

    private func openPushDeepLink(_ value: String) {
        if AgentPushCenter.isActivityOverviewDeepLink(value) {
            model.openActivityOverviewDeepLink()
            return
        }
        if AgentPushCenter.isUsageDeepLink(value) {
            model.openUsageDeepLink()
            return
        }
        guard let target = AgentCore.agentActivityThreadTarget(value: value) else { return }
        model.openPushThread(hostId: target.environmentId, threadId: target.threadId)
    }
}

/// Screens pushed over a thread.
enum ThreadRoute: Hashable {
    case thread
    case device
    case terminal(String?, UUID)
    case files
    case review
}

/// The thread list and the open thread: a stack on phones, side by side when
/// the window is at least 720 by 600.
private struct WorkspaceRoot: View {
    @ObservedObject var model: BexAppViewModel
    @State private var routes: [ThreadRoute] = []
    @State private var showingSettings = false
    @State private var settingsProjectId: String?
    @State private var showingNewTask = false
    /// The new task opens on the draft core already prepared.
    @State private var newTaskDraftOpen = false
    @State private var returnThread: String?

    var body: some View {
        GeometryReader { geometry in
            let split = geometry.size.width >= 720 && geometry.size.height >= 600
            Group {
                if split {
                    HStack(spacing: 0) {
                        NavigationStack { list(sidebar: true) }
                            .frame(width: min(380, max(280, (geometry.size.width * 0.32).rounded())))
                        Rectangle().fill(AppTheme.border).frame(width: 1).ignoresSafeArea()
                        NavigationStack(path: $routes) {
                            detail.navigationDestination(for: ThreadRoute.self, destination: destination)
                        }
                    }
                } else {
                    NavigationStack(path: compactPath) {
                        list(sidebar: false).navigationDestination(for: ThreadRoute.self, destination: destination)
                    }
                }
            }
        }
        .sheet(
            isPresented: $showingSettings,
            onDismiss: { settingsProjectId = nil },
            content: { SettingsScreen(model: model, projectId: settingsProjectId) }
        )
        .sheet(isPresented: $showingNewTask, onDismiss: restoreThread) {
            NewTaskFlow(model: model, draftOpen: newTaskDraftOpen) { _ in
                returnThread = nil
                routes = []
                model.screen = .thread
            }
            .presentationDetents([.fraction(0.92)])
        }
        .onChange(of: model.selectedThreadId) { _, _ in routes = [] }
        .overlay(alignment: .bottom) {
            if let notice = model.notice {
                HStack(spacing: 12) {
                    Text(notice).lineLimit(2).frame(maxWidth: .infinity, alignment: .leading)
                    if model.notificationThreadRoute != nil {
                        Button("Open") { model.openNotificationThread() }
                    }
                    Button("Dismiss") { model.notice = nil }
                }
                .padding(.horizontal, 16)
                .padding(.vertical, 12)
                .background(AppTheme.groupedCard, in: RoundedRectangle(cornerRadius: 16))
                .padding(12)
            }
        }
    }

    private func list(sidebar: Bool) -> some View {
        ThreadListScreen(model: model, sidebar: sidebar, openSettings: { projectId in
            _ = model.selectScopedValue(projectId)
            settingsProjectId = projectId
            showingSettings = true
        }, newTask: newTask,
        showNewTaskDraft: showNewTaskDraft)
    }

    @ViewBuilder
    private var detail: some View {
        if model.screen == .thread, model.selectedThreadId != nil {
            threadScreen
        } else {
            Color.clear
                .background(AppTheme.screen.ignoresSafeArea())
                .toolbar {
                    ToolbarItem(placement: .topBarLeading) {
                        Button(action: newTask) { Image(systemName: "square.and.pencil") }
                            .accessibilityLabel("New task")
                    }
                }
        }
    }

    private var threadScreen: some View {
        ThreadScreen(model: model, routes: ThreadRoutes(
            terminal: { routes.append(.terminal($0, UUID())) },
            files: { routes.append(.files) },
            review: { routes.append(.review) },
            device: { routes.append(.device) }
        ))
    }

    private var compactPath: Binding<[ThreadRoute]> {
        Binding(
            get: { model.screen == .thread ? [.thread] + routes : [] },
            set: { path in
                if path.isEmpty, model.screen == .thread {
                    routes = []
                    model.showThreadList()
                } else {
                    routes = Array(path.dropFirst())
                }
            }
        )
    }

    @ViewBuilder
    private func destination(_ route: ThreadRoute) -> some View {
        switch route {
        case .thread:
            threadScreen
        case .device:
            if let threadId = model.selectedThreadId {
                DeviceScreen(model: model, threadId: threadId)
            }
        case let .terminal(terminal, _):
            if let thread = model.selectedThreadId {
                TerminalScreen(model: model, threadId: thread, terminalId: terminal)
            }
        case .files:
            WorkspaceToolsScreen(model: model) { routes.removeAll { $0 == .files } }
        case .review:
            ReviewScreen(model: model)
        }
    }

    private func newTask() {
        returnThread = model.selectedThreadId
        newTaskDraftOpen = false
        showingNewTask = true
    }

    private func showNewTaskDraft() {
        returnThread = model.selectedThreadId
        newTaskDraftOpen = true
        showingNewTask = true
    }

    /// Leaving the new task without starting one reopens the thread it left.
    private func restoreThread() {
        if let thread = returnThread, model.selectedThreadId == nil {
            model.openThread(thread)
        }
        returnThread = nil
    }
}

import AgentCore
import SwiftUI

struct WorkspaceDestination: Hashable, Identifiable {
    let scope: PanelScope
    let directory: String
    var filePath: String?
    var fileLine: UInt32?
    var id: Self {
        self
    }
}

/// The conversation and its tools retain the same pane while side chats use their own.
struct ConversationDestination: View {
    let model: ConversationModel
    let environment: ConversationEnvironment
    @State private var tools: WorkspaceDestination?
    @State private var width: CGFloat = 0

    var body: some View {
        ThreadScreen(model: model, environment: environment, conversation: model.conversation, openTools: openTools)
            .onGeometryChange(for: CGFloat.self) { $0.size.width } action: { width = $0 }
            .simultaneousGesture(DragGesture(minimumDistance: 30).onEnded { value in
                guard value.startLocation.x >= width - 32,
                      value.translation.width < -60,
                      abs(value.translation.width) > abs(value.translation.height) * 1.5 else { return }
                openTools(nil)
            })
            .navigationDestination(item: $tools) { destination in
                WorkspaceToolsScreen(
                    model: model,
                    environment: environment,
                    scope: destination.scope,
                    directory: destination.directory
                ) {
                    tools = nil
                }
            }
    }

    private func openTools(_ kind: PanelKind?) {
        let destination = WorkspaceDestination(
            scope: model.snapshot.panelScope(host: model.host, draft: model.coreDraftKey),
            directory: model.cwd
        )
        if let kind {
            model.openWorkbench(kind, scope: destination.scope, directory: destination.directory)
        }
        tools = destination
    }
}

extension PanelKind {
    var icon: String {
        switch self {
        case .browser: "globe"
        case .terminal: "terminal"
        case .files, .file: "folder"
        case .diff: "arrow.left.arrow.right"
        case .sideChat: "bubble.left.and.bubble.right"
        case .agents: "network"
        case .home: "plus"
        }
    }
}

extension ConversationModel {
    func migrateWorkbench(to next: AgentCore.Snapshot) {
        guard let session = next.submittedSession(draft: coreDraftKey) else { return }
        panels = panels.rekey(before: snapshot.panelScope(host: host, draft: coreDraftKey),
                              after: next.panelScope(host: host, draft: .session(session: session)))
    }

    func openWorkbench(_ kind: PanelKind, scope: PanelScope, directory: String) {
        let opened = panels.open(key: scope, kind: kind, path: nil)
        panels = opened.panels
        let id = opened.id
        if kind == .sideChat, case let .session(original) = scope.draft {
            let request: SideChatRequest = if let pending = sideChatRequest, pending.originalThreadId == original,
                                              !workbenchSideChats.values.contains(where: { $0.id == pending.id }) {
                pending
            } else {
                SideChatRequest(text: "", host: host, originalThreadId: original,
                                cwd: directory, session: session,
                                provider: settings.provider())
            }
            workbenchSideChats[id] = request
        }
    }

    func activateWorkbench(_ id: UInt64, scope: PanelScope) {
        panels = panels.activate(key: scope, id: id).panels
    }

    func closeWorkbench(_ surface: PanelSurface, scope: PanelScope) {
        panels = panels.close(key: scope, id: surface.id)
        if surface.kind == .terminal {
            perform(.closeTerminal(handle: workbenchTerminalHandle(surface.id)))
        }
        workbenchSideChats.removeValue(forKey: surface.id)
    }

    func workbenchTerminalHandle(_ id: UInt64) -> String {
        "agent-terminal-\(workbenchInstance)-\(id)"
    }
}

private struct WorkspaceToolsScreen: View {
    let model: ConversationModel
    let environment: ConversationEnvironment
    let scope: PanelScope
    let directory: String
    let close: () -> Void

    private var tools: [PanelTool] {
        panelTools(connected: model.isConnected, hasDirectory: !directory.isEmpty)
    }

    private var panel: PanelState {
        model.panels.get(key: scope)
    }

    private var selected: PanelSurface? {
        panel.surfaces.first { $0.id == panel.active }
    }

    private var title: String {
        selected.map { panelTitle(kind: $0.kind) } ?? "ツール"
    }

    var body: some View {
        Group {
            if let selected {
                content(selected).id(selected.id)
            } else {
                List {
                    Section("ツール") {
                        ForEach(tools, id: \.key) { tool in
                            toolButton(tool)
                        }
                    }
                }
                .listStyle(.insetGrouped)
                .accessibilityIdentifier("workbench.launcher")
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Color(uiColor: .systemBackground))
        .navigationTitle(title)
        .navigationBarTitleDisplayMode(.inline)
        .toolbar(.visible, for: .navigationBar)
        .toolbarTitleMenu {
            if !panel.surfaces.isEmpty {
                Section("開いているツール") {
                    Picker("開いているツール", selection: Binding<UInt64?>(
                        get: { panel.active },
                        set: {
                            if let id = $0 {
                                model.activateWorkbench(id, scope: scope)
                            }
                        }
                    )) {
                        ForEach(panel.surfaces, id: \.id) { surface in
                            Label(panelTitle(kind: surface.kind), systemImage: surface.kind.icon)
                                .tag(Optional(surface.id))
                        }
                    }
                    .pickerStyle(.inline).labelsHidden()
                }
                if let selected {
                    Button(role: .destructive) { model.closeWorkbench(selected, scope: scope) } label: {
                        Label("このツールを閉じる", systemImage: "xmark")
                    }
                }
            }
        }
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                Menu {
                    ForEach(tools, id: \.key) { tool in
                        toolButton(tool)
                    }
                } label: {
                    Label("ツールを追加", systemImage: "plus")
                }
                .accessibilityIdentifier("workbench.add")
            }
        }
    }

    private func toolButton(_ tool: PanelTool) -> some View {
        Button { model.openWorkbench(tool.kind, scope: scope, directory: directory) } label: {
            Label(LocalizedStringKey(tool.label), systemImage: tool.kind.icon)
        }
        .disabled(!tool.enabled)
        .accessibilityIdentifier("workbench.\(tool.key)")
    }

    @ViewBuilder
    private func content(_ surface: PanelSurface) -> some View {
        switch surface.kind {
        case .sideChat:
            if let request = model.workbenchSideChats[surface.id] {
                ConversationSideChat(request: request, environment: environment)
            } else {
                Text("会話を始めるとサイドチャットを利用できます。")
                    .foregroundStyle(.secondary).padding()
            }
        case .browser:
            if case let .session(thread) = scope.draft, let store = model.store {
                WorkspaceBrowserScreen(store: store, thread: thread)
            } else {
                Text("会話を始めると、AIと同じブラウザを利用できます。")
                    .foregroundStyle(.secondary).padding()
            }
        case .terminal:
            TerminalScreen(model: model, cwd: directory, handle: model.workbenchTerminalHandle(surface.id))
        case .files, .file:
            WorkspaceScreen(model: model, root: directory, draft: scope.draft, close: close)
                .onDisappear { model.perform(.reviewWorkspace(ReviewWorkspace(cwd: directory))) }
        case .diff:
            WorkspaceDiffScreen(model: model, root: directory)
        case .agents:
            WorkspaceAgentsScreen(agents: model.agents)
                .onAppear { model.perform(.watchAgents(threadId: model.selectedThreadId)) }
                .onDisappear { model.perform(.watchAgents(threadId: nil)) }
        case .home:
            EmptyView()
        }
    }
}

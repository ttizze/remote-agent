import AgentCore
import SwiftUI

enum ConversationPanelTab: String, CaseIterable {
    case terminal, files

    var icon: String {
        self == .terminal ? "terminal" : "folder"
    }

    var label: String {
        self == .terminal ? "ターミナル" : "ファイラ"
    }
}

/// Chat and tools share the same space; closing the panel preserves the chat.
struct ChatWithWorkbench<Chat: View>: View {
    @ObservedObject var model: BexAppViewModel
    @Binding var isPresented: Bool
    @Binding var selection: ConversationPanelTab
    @Binding var showingDiff: Bool
    let allowsSwipe: Bool
    let chat: Chat

    var body: some View {
        GeometryReader { geometry in
            let wide = geometry.size.width >= 760
            let panelWidth = wide ? min(480, geometry.size.width * 0.45) : geometry.size.width - 36
            HStack(spacing: 0) {
                chat
                    .frame(width: wide && isPresented ? geometry.size.width - panelWidth : geometry.size.width)
                    .allowsHitTesting(!isPresented || wide)
                    .accessibilityHidden(isPresented && !wide)
                    .simultaneousGesture(DragGesture(minimumDistance: 30).onEnded { value in
                        guard allowsSwipe, value.startLocation.x >= geometry.size.width - 32,
                              value.translation.width < -60,
                              abs(value.translation.width) > abs(value.translation.height) * 1.5 else { return }
                        isPresented = true
                    })
                if isPresented {
                    panel
                        .id(model.cwd)
                        .frame(width: panelWidth)
                        .overlay(alignment: .leading) { Divider() }
                }
            }
            .offset(x: isPresented && !wide ? -panelWidth : 0)
            .animation(.easeInOut(duration: 0.22), value: isPresented)
        }
        .clipped()
    }

    private var panel: some View {
        VStack(spacing: 0) {
            HStack(spacing: 0) {
                ForEach(ConversationPanelTab.allCases, id: \.self) { tab in
                    Button { selection = tab } label: {
                        Image(systemName: tab.icon)
                            .font(.system(size: 15))
                            .foregroundStyle(selection == tab ? .primary : .secondary)
                            .frame(width: 44, height: 44)
                            .overlay(alignment: .bottom) {
                                if selection == tab {
                                    Rectangle().fill(.primary).frame(height: 2).padding(.horizontal, 9)
                                }
                            }
                    }
                    .buttonStyle(.plain)
                    .accessibilityLabel(tab.label)
                    .accessibilityIdentifier("workbench.\(tab.rawValue)")
                    .accessibilityAddTraits(selection == tab ? [.isSelected] : [])
                }
                Spacer(minLength: 0)
            }
            .padding(.horizontal, 4)
            .contentShape(Rectangle())
            .gesture(closeGesture)
            Divider()
            switch selection {
            case .terminal:
                TerminalScreen(model: model, cwd: model.cwd)
                    .simultaneousGesture(closeGesture)
            case .files:
                WorkspacePanel(model: model, root: model.cwd, showingDiff: $showingDiff) { isPresented = false }
            }
        }
        .background(Color(uiColor: .systemBackground))
    }

    private var closeGesture: some Gesture {
        DragGesture(minimumDistance: 30).onEnded { value in
            if value.translation.width > 60,
               abs(value.translation.width) > abs(value.translation.height) * 1.5 {
                isPresented = false
            }
        }
    }
}

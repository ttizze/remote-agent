import SwiftUI

enum ConversationPage: String, CaseIterable {
    case chat, terminal, files

    var icon: String {
        switch self {
        case .chat: "bubble.left.and.bubble.right"
        case .terminal: "terminal"
        case .files: "folder"
        }
    }

    var label: String {
        switch self {
        case .chat: "会話"
        case .terminal: "ターミナル"
        case .files: "ファイル"
        }
    }
}

struct ConversationPages<Chat: View>: View {
    @ObservedObject var model: BexAppViewModel
    @Binding var selection: ConversationPage
    @Binding var showingDiff: Bool
    let chat: Chat

    var body: some View {
        VStack(spacing: 0) {
            HStack {
                ForEach(ConversationPage.allCases, id: \.self) { page in
                    Button { selection = page } label: {
                        Label(page.label, systemImage: page.icon)
                            .font(.caption)
                            .foregroundStyle(selection == page ? .primary : .secondary)
                            .frame(maxWidth: .infinity, minHeight: 44)
                            .overlay(alignment: .bottom) {
                                if selection == page {
                                    Rectangle().fill(.primary).frame(height: 2)
                                }
                            }
                    }
                    .buttonStyle(.plain)
                    .accessibilityIdentifier("workbench.\(page.rawValue)")
                    .accessibilityAddTraits(selection == page ? [.isSelected] : [])
                }
            }
            Divider()
            TabView(selection: $selection) {
                chat.tag(ConversationPage.chat)
                toolPage(.terminal).tag(ConversationPage.terminal)
                toolPage(.files).tag(ConversationPage.files)
            }
            .tabViewStyle(.page(indexDisplayMode: .never))
        }
    }

    private func toolPage(_ page: ConversationPage) -> some View {
        Group {
            if selection == page {
                if model.cwd.isEmpty {
                    Text("フォルダを選択すると\(page.label)を利用できます。")
                        .foregroundStyle(.secondary)
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                } else if page == .terminal {
                    TerminalScreen(model: model, cwd: model.cwd)
                } else {
                    WorkspaceScreen(model: model, root: model.cwd, showingDiff: $showingDiff) {
                        selection = .chat
                    }
                }
            } else {
                Color.clear
            }
        }
        .id(model.cwd)
    }
}

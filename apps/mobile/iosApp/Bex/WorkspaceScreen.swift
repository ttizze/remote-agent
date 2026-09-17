import AgentCore
import SwiftUI
import WebKit

/// Owns the transition from a conversation to its separate tool destination.
struct ConversationDestination: View {
    @ObservedObject var model: BexAppViewModel
    @State private var showingTools = false
    @State private var tab: WorkspaceTab = .terminal
    @State private var showingDiff = false
    @State private var browser = WebPage()

    var body: some View {
        GeometryReader { geometry in
            ThreadScreen(model: model,
                         conversation: model.sideChatRequest?.originalConversation ?? model.conversation,
                         openTools: { tab, diff in
                             if let tab {
                                 self.tab = tab
                             }
                             showingDiff = diff
                             showingTools = true
                         })
                         .simultaneousGesture(DragGesture(minimumDistance: 30).onEnded { value in
                             guard value.startLocation.x >= geometry.size.width - 32,
                                   value.translation.width < -60,
                                   abs(value.translation.width) > abs(value.translation.height) * 1.5 else { return }
                             showingTools = true
                         })
        }
        .navigationDestination(isPresented: $showingTools) {
            WorkspaceToolsScreen(model: model, tab: $tab, showingDiff: $showingDiff, browser: browser) {
                showingTools = false
            }
        }
    }
}

enum WorkspaceTab: String, CaseIterable {
    case terminal, browser, files

    var icon: String {
        switch self {
        case .terminal: "terminal"
        case .files: "folder"
        case .browser: "globe"
        }
    }

    var label: String {
        switch self {
        case .terminal: "ターミナル"
        case .files: "ファイル"
        case .browser: "ブラウザ"
        }
    }
}

private struct WorkspaceToolsScreen: View {
    @ObservedObject var model: BexAppViewModel
    @Binding var tab: WorkspaceTab
    @Binding var showingDiff: Bool
    let browser: WebPage
    let close: () -> Void

    var body: some View {
        VStack(spacing: 0) {
            HStack {
                ForEach(WorkspaceTab.allCases, id: \.self) { tab in
                    Button { self.tab = tab } label: {
                        Image(systemName: tab.icon)
                            .font(.title3)
                            .foregroundStyle(self.tab == tab ? .primary : .secondary)
                            .frame(maxWidth: .infinity, minHeight: 44)
                            .overlay(alignment: .bottom) {
                                if self.tab == tab {
                                    Rectangle().fill(.primary).frame(height: 2)
                                }
                            }
                    }
                    .buttonStyle(.plain)
                    .accessibilityLabel(tab.label)
                    .accessibilityIdentifier("workbench.\(tab.rawValue)")
                    .accessibilityAddTraits(self.tab == tab ? [.isSelected] : [])
                }
            }
            Divider()
            Group {
                if tab == .browser {
                    WorkspaceBrowserScreen(page: browser)
                } else if model.cwd.isEmpty {
                    Text("フォルダを選択すると\(tab.label)を利用できます。")
                        .foregroundStyle(.secondary)
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                } else if tab == .terminal {
                    TerminalScreen(model: model, cwd: model.cwd)
                } else {
                    WorkspaceScreen(model: model, root: model.cwd, showingDiff: $showingDiff, close: close)
                }
            }
            .id(model.cwd)
        }
        .navigationTitle("")
        .navigationBarTitleDisplayMode(.inline)
        .accessibilityIdentifier("workspace.tools")
    }
}

private struct WorkspaceBrowserScreen: View {
    let page: WebPage
    @State private var address = ""
    @State private var error: String?
    @FocusState private var addressFocused: Bool

    var body: some View {
        VStack(spacing: 0) {
            HStack {
                Button {
                    if let item = page.backForwardList.backList.last {
                        page.load(item)
                    }
                } label: { Image(systemName: "chevron.left") }
                    .disabled(page.backForwardList.backList.isEmpty)
                    .accessibilityLabel("前のページ")
                TextField("URL", text: $address)
                    .keyboardType(.URL).textInputAutocapitalization(.never).disableAutocorrection(true)
                    .textFieldStyle(.roundedBorder).focused($addressFocused)
                    .submitLabel(.go).onSubmit(open)
                    .accessibilityIdentifier("browser.address")
                Button("開く", action: open).accessibilityIdentifier("browser.open")
                Button { page.reload() } label: { Image(systemName: "arrow.clockwise") }
                    .accessibilityLabel("ページを再読込")
            }.padding()
            if let error {
                BexNotice(text: error).padding(.horizontal)
            }
            if page.isLoading {
                ProgressView().accessibilityLabel("読み込み中")
            }
            WebView(page)
                .webViewBackForwardNavigationGestures(.disabled)
                .accessibilityIdentifier("browser.content")
        }
        .onAppear { address = page.url?.absoluteString ?? "" }
        .onChange(of: page.url) { _, url in
            if !addressFocused {
                address = url?.absoluteString ?? ""
            }
        }
    }

    private func open() {
        do {
            let url = try AgentCore.browserUrl(input: address)
            address = url
            addressFocused = false
            error = nil
            page.load(URL(string: url))
        } catch let AgentCore.AgentError.Failed(reason) {
            error = reason
        } catch { self.error = error.localizedDescription }
    }
}

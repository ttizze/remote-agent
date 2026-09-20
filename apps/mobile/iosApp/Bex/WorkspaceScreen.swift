import AgentCore
import SwiftUI
import UIKit

/// Owns the transition from a conversation to its separate tool destination.
struct ConversationDestination: View {
    @ObservedObject var model: BexAppViewModel
    @State private var showingTools = false
    @State private var tab: WorkspaceTab = .terminal
    @State private var showingDiff = false

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
            WorkspaceToolsScreen(model: model, tab: $tab, showingDiff: $showingDiff) {
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
                    if let thread = model.selectedThreadId {
                        WorkspaceBrowserScreen(model: model, thread: thread)
                            .id("\(model.selectedProfileId ?? "")/\(thread)")
                    } else {
                        Text("会話を始めると、AIと同じブラウザを利用できます。")
                            .foregroundStyle(.secondary).padding()
                    }
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
    }
}

private struct WorkspaceBrowserScreen: View {
    @ObservedObject var model: BexAppViewModel
    let thread: String
    @Environment(\.scenePhase) private var scenePhase
    @State private var frame: BrowserFrame?
    @State private var image: UIImage?
    @State private var address = ""
    @State private var text = ""
    @State private var error: String?
    @State private var connectionError: String?
    @State private var sending = false
    @State private var requestId = UUID()
    @FocusState private var addressFocused: Bool
    @State private var showKeyboard = false
    @State private var dialogText = ""

    private var acceptsInput: Bool {
        frame?.control == .yours && !sending && scenePhase == .active && connectionError == nil
    }

    var body: some View {
        VStack(spacing: 0) {
            HStack {
                TextField("URL", text: $address)
                    .keyboardType(.URL).textInputAutocapitalization(.never).disableAutocorrection(true)
                    .textFieldStyle(.roundedBorder).focused($addressFocused)
                    .submitLabel(.go).onSubmit(open)
                    .accessibilityIdentifier("browser.address")
                Button("開く", action: open)
                    .disabled(!acceptsInput).accessibilityIdentifier("browser.open")
            }.padding(.horizontal).padding(.top, 8)
            HStack {
                Button { send(.back) } label: { Image(systemName: "chevron.left") }
                    .accessibilityLabel("前のページ").disabled(!acceptsInput)
                Button { send(.forward) } label: { Image(systemName: "chevron.right") }
                    .accessibilityLabel("次のページ").disabled(!acceptsInput)
                Button { send(.reload) } label: { Image(systemName: "arrow.clockwise") }
                    .accessibilityLabel("再読み込み").disabled(!acceptsInput)
                Menu {
                    ForEach(frame?.tabs ?? [], id: \.id) { tab in
                        Button(tab.title.isEmpty ? tab.url : tab.title) { send(.selectTab(id: tab.id)) }
                    }
                } label: { Image(systemName: "square.on.square") }
                    .accessibilityLabel("タブ").disabled(!acceptsInput)
                Spacer()
                controlButton
            }.padding(12)
            if let title = frame?.tabs.first(where: { $0.id == frame?.tabId })?.title, !title.isEmpty {
                Text(title).font(.caption).lineLimit(1).accessibilityIdentifier("browser.title")
            }
            if let message = error ?? connectionError {
                BexNotice(text: message).padding(.horizontal)
            }
            if let dialog = frame?.dialog {
                VStack {
                    Text(dialog.message)
                    if dialog.prompt {
                        TextField("入力", text: $dialogText).textFieldStyle(.roundedBorder)
                    }
                    HStack {
                        Button("キャンセル") { send(.dialog(accept: false, text: "")) }
                        Button("OK") { send(.dialog(accept: true, text: dialogText)); dialogText = "" }
                    }.disabled(!acceptsInput)
                }.padding()
            }
            ZStack {
                if let image {
                    RemoteBrowserCanvas(image: image, acceptsInput: acceptsInput, send: send)
                        .accessibilityIdentifier("browser.content")
                } else {
                    ProgressView("Macのブラウザに接続中…")
                }
            }.frame(maxWidth: .infinity, maxHeight: .infinity)
            if frame?.control == .yours {
                HStack {
                    Button { showKeyboard.toggle() } label: { Label("文字入力", systemImage: "keyboard") }
                    Spacer()
                    Button("Tab") { send(.key(key: .tab)) }
                    Button { send(.key(key: .backspace)) } label: { Image(systemName: "delete.left") }
                        .accessibilityLabel("1文字削除")
                    Button("Enter") { send(.key(key: .enter)) }
                }.padding(12).disabled(!acceptsInput)
                if showKeyboard {
                    HStack {
                        SecureField("選択した欄に入力", text: $text)
                            .textInputAutocapitalization(.never).disableAutocorrection(true)
                            .textFieldStyle(.roundedBorder).onSubmit(insertText)
                            .accessibilityIdentifier("browser.text")
                        Button("入力", action: insertText).disabled(text.isEmpty || !acceptsInput)
                            .accessibilityIdentifier("browser.type")
                    }.padding(.horizontal).padding(.bottom, 8)
                }
            }
        }
        .task(id: scenePhase == .active) {
            guard scenePhase == .active else { return }
            while !Task.isCancelled {
                if !sending {
                    await perform(.read)
                }
                do {
                    try await Task.sleep(for: .milliseconds(connectionError == nil ? 250 : 1500))
                } catch { return }
            }
        }
        .onDisappear { requestId = UUID(); text = "" }
    }

    @ViewBuilder private var controlButton: some View {
        if frame?.control == .yours {
            Button("AIに戻す") { send(.releaseControl) }
                .buttonStyle(.borderedProminent).disabled(sending)
                .accessibilityIdentifier("browser.release")
        } else if frame?.control == .other {
            Text("別の端末が操作中").font(.caption)
        } else {
            Button(frame?.control == .awaitingHuman ? "操作を引き継ぐ" : "自分で操作する") { send(.takeControl) }
                .buttonStyle(.bordered).disabled(frame == nil || sending)
                .accessibilityIdentifier("browser.take")
        }
    }

    private func open() {
        addressFocused = false
        send(.navigate(url: address))
    }

    private func insertText() {
        guard !text.isEmpty else { return }
        let submitted = text
        text = ""
        send(.type(text: submitted))
    }

    private func send(_ action: BrowserAction) {
        guard !sending else { return }
        sending = true
        error = nil
        Task {
            await perform(action)
            sending = false
        }
    }

    private func perform(_ action: BrowserAction) async {
        let id = UUID()
        requestId = id
        do {
            let result = try await model.browser(BrowserRequest(
                threadId: thread, controlToken: frame?.controlToken ?? "",
                tabId: frame?.tabId ?? "", imageId: frame?.imageId ?? "", action: action
            ))
            guard !Task.isCancelled, requestId == id, scenePhase == .active else { return }
            frame = result
            if !result.image.isEmpty {
                image = UIImage(data: result.image)
            }
            if !addressFocused {
                address = result.tabs.first { $0.id == result.tabId }?.url ?? ""
            }
            connectionError = nil
        } catch {
            guard !Task.isCancelled, requestId == id else { return }
            let message: String = if case let AgentError.Failed(reason) = error {
                reason
            } else {
                error.localizedDescription
            }
            if case .read = action {
                connectionError = message
            } else {
                self.error = message
            }
        }
    }
}

private struct RemoteBrowserCanvas: UIViewRepresentable {
    let image: UIImage
    let acceptsInput: Bool
    let send: (BrowserAction) -> Void

    func makeUIView(context _: Context) -> BrowserCanvasView {
        BrowserCanvasView()
    }

    func updateUIView(_ view: BrowserCanvasView, context _: Context) {
        view.page.image = image
        if view.page.bounds.size != image.size {
            view.page.frame = CGRect(origin: .zero, size: image.size)
            view.contentSize = image.size
            view.setNeedsLayout()
        }
        view.send = acceptsInput ? send : nil
    }
}

private final class BrowserCanvasView: UIScrollView, UIScrollViewDelegate {
    let page = UIImageView()
    var send: ((BrowserAction) -> Void)? {
        didSet { pagePan.isEnabled = send != nil }
    }

    private lazy var pagePan = UIPanGestureRecognizer(target: self, action: #selector(dragged))
    private var previousSize = CGSize.zero

    init() {
        super.init(frame: .zero)
        delegate = self
        contentSize = page.bounds.size
        addSubview(page)
        panGestureRecognizer.minimumNumberOfTouches = 2
        addGestureRecognizer(UITapGestureRecognizer(target: self, action: #selector(tapped)))
        pagePan.maximumNumberOfTouches = 1
        addGestureRecognizer(pagePan)
        backgroundColor = .secondarySystemBackground
        accessibilityLabel = "MacのBEXブラウザ。ピンチで拡大、2本指で表示範囲を移動できます。"
    }

    @available(*, unavailable)
    required init?(coder _: NSCoder) {
        fatalError("init(coder:) has not been implemented")
    }

    override func layoutSubviews() {
        super.layoutSubviews()
        guard bounds.size != previousSize, bounds.width > 0, page.bounds.width > 0 else { return }
        previousSize = bounds.size
        minimumZoomScale = bounds.width / page.bounds.width
        maximumZoomScale = max(2, minimumZoomScale * 4)
        setZoomScale(minimumZoomScale, animated: false)
    }

    override func gestureRecognizerShouldBegin(_ gesture: UIGestureRecognizer) -> Bool {
        if gesture === pagePan,
           pagePan.location(in: self).x - pagePan.translation(in: self).x < bounds.minX + 32 {
            return false
        }
        return super.gestureRecognizerShouldBegin(gesture)
    }

    func viewForZooming(in _: UIScrollView) -> UIView? {
        page
    }

    @objc private func tapped(_ gesture: UITapGestureRecognizer) {
        let point = gesture.location(in: page)
        guard page.bounds.contains(point) else { return }
        send?(.click(x: point.x, y: point.y))
    }

    @objc private func dragged(_ gesture: UIPanGestureRecognizer) {
        guard gesture.state == .ended else { return }
        let point = gesture.location(in: page)
        let delta = gesture.translation(in: page)
        send?(.scroll(x: max(0, min(page.bounds.maxX - 1, point.x)), y: max(0, min(page.bounds.maxY - 1, point.y)),
                      deltaX: -delta.x, deltaY: -delta.y))
    }
}

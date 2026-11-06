import AgentCore
import SwiftUI
import UIKit

struct WorkspaceBrowserScreen: View {
    let store: AgentStore
    let thread: SessionRef
    @Environment(\.scenePhase) private var scenePhase
    @State private var frame: BrowserFrame?
    @State private var image: UIImage?
    @State private var address = ""
    @State private var text = ""
    @State private var credentials: BrowserCredentials?
    @State private var notice: String?
    @State private var error: String?
    @State private var connectionError: String?
    @State private var commands: [BrowserRequest] = []
    @FocusState private var addressFocused: Bool
    @FocusState private var textFocused: Bool
    @State private var showKeyboard = false
    @State private var dialogText = ""
    /// Suppress stale polling frames only while the answer is queued or in flight.
    @State private var answeringDialog = false
    @State private var viewportSize = CGSize.zero

    private var acceptsInput: Bool {
        frame != nil && scenePhase == .active && connectionError == nil
    }

    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 0) {
                TextField("検索または URL", text: $address)
                    .keyboardType(.webSearch).textInputAutocapitalization(.never).disableAutocorrection(true)
                    .textFieldStyle(.roundedBorder).focused($addressFocused).submitLabel(.go).onSubmit(open)
                    .accessibilityIdentifier("browser.address")
                Button { send(.reload) } label: {
                    Image(systemName: "arrow.clockwise").frame(width: 44, height: 44)
                }
                .accessibilityLabel("再読み込み")
            }.font(.body).padding(.horizontal, 8).padding(.vertical, 4).disabled(!acceptsInput)
            Divider().opacity(0.6)
            if let message = error ?? connectionError ?? notice {
                AppNotice(text: message).padding(.horizontal)
            }
            ZStack {
                if let image, let currentTab = frame?.currentTab, currentTab.url != "about:blank" {
                    RemoteBrowserCanvas(image: image, acceptsInput: acceptsInput, send: send)
                        .accessibilityIdentifier("browser.content")
                } else if frame != nil {
                    ContentUnavailableView("プレビューはまだありません", systemImage: "globe",
                                           description: Text("URLを入力するか、AIにブラウザ操作を依頼してください。"))
                } else {
                    ProgressView("ホストのブラウザに接続中…")
                }
            }.frame(maxWidth: .infinity, maxHeight: .infinity)
                .onGeometryChange(for: CGSize.self) { $0.size } action: { viewportSize = $0 }
            if showKeyboard {
                inputPanel
            }
        }
        .toolbar {
            ToolbarItemGroup(placement: .bottomBar) {
                Button { send(.back) } label: { Label("前のページ", systemImage: "chevron.left") }
                    .disabled(!acceptsInput || frame?.canGoBack != true)
                Spacer()
                Button { send(.forward) } label: { Label("次のページ", systemImage: "chevron.right") }
                    .disabled(!acceptsInput || frame?.canGoForward != true)
                Spacer()
                Button { showKeyboard.toggle() } label: { Label("文字入力", systemImage: "keyboard") }
                    .disabled(!acceptsInput).accessibilityIdentifier("browser.keyboard")
                Spacer()
                BrowserMenu(frame: frame, viewportSize: viewportSize, send: send).disabled(!acceptsInput)
            }
            ToolbarItemGroup(placement: .keyboard) {
                if textFocused {
                    keyButtons
                }
            }
        }
        .alert(Text(verbatim: frame?.dialog?.message ?? ""), isPresented: Binding(
            get: { frame?.dialog != nil && !answeringDialog }, set: { _ in }
        )) {
            if frame?.dialog?.prompt == true {
                TextField("入力", text: $dialogText)
            }
            Button("キャンセル", role: .cancel) { send(.dialog(accept: false, text: "")) }
            Button("OK") { send(.dialog(accept: true, text: dialogText)) }
        }
        .sheet(isPresented: Binding(get: { credentials != nil }, set: {
            if !$0 {
                credentials = nil
            }
        })) {
            if let credentials {
                BrowserCredentialsSheet(credentials: credentials, send: send) { self.credentials = nil }
            }
        }
        .task(id: scenePhase == .active) {
            guard scenePhase == .active else { return }
            while !Task.isCancelled {
                let request = commands.isEmpty ? request(.read) : commands.removeFirst()
                await perform(request)
                do { try await Task.sleep(for: .milliseconds(connectionError == nil ? 150 : 1500)) } catch { return }
            }
        }
        .onDisappear { commands.removeAll(); text = ""; answeringDialog = false }
    }

    /// Keys for the Host page's focused field, above the keyboard.
    @ViewBuilder private var keyButtons: some View {
        Button { showKeyboard = false } label: { Image(systemName: "keyboard.chevron.compact.down") }
            .accessibilityLabel("閉じる").accessibilityIdentifier("browser.keyboard.dismiss")
        Button("Tab") { send(.key(key: .tab)) }
        Button("すべて選択") { send(.key(key: .selectAll)) }
        Spacer()
        Button { send(.key(key: .backspace)) } label: { Image(systemName: "delete.left") }
            .accessibilityLabel("1文字削除")
        Button("Enter") { send(.key(key: .enter)) }
    }

    private var inputPanel: some View {
        VStack {
            Divider()
            HStack {
                Group {
                    if frame?.inputKind == .text {
                        TextField("選択した欄に入力", text: $text, axis: .vertical).lineLimit(1 ... 4)
                    } else {
                        SecureField("選択した欄に入力", text: $text)
                    }
                }
                .textInputAutocapitalization(.never).disableAutocorrection(true)
                .textFieldStyle(.roundedBorder).focused($textFocused)
                .onSubmit(insertText).accessibilityIdentifier("browser.text")
                Button("入力", action: insertText).disabled(text.isEmpty)
                    .accessibilityIdentifier("browser.type")
            }
        }.padding(.horizontal).padding(.bottom, 8).disabled(!acceptsInput)
            .onAppear { textFocused = true }
    }

    private func open() {
        do {
            let url = try browserAddressUrl(input: address)
            addressFocused = false
            send(.navigate(url: url))
        } catch { self.error = error.localizedDescription }
    }

    private func insertText() {
        guard !text.isEmpty else { return }
        let submitted = text
        text = ""
        send(.type(text: submitted))
    }

    private func send(_ action: BrowserAction) {
        guard commands.count < 64 else { return }
        error = nil
        credentials = nil
        switch action {
        case .importCookies, .importPasswords: notice = "ブラウザデータを取り込み中… 接続先のMacでOSのアクセス確認が出たら許可してください。"
        case .dialog:
            answeringDialog = true
            dialogText = ""
            notice = nil
        default: notice = nil
        }
        commands.append(request(action))
    }

    private func request(_ action: BrowserAction) -> BrowserRequest {
        BrowserRequest(threadId: thread, tabId: frame?.tabId ?? "", imageId: frame?.imageId ?? "",
                       generation: frame?.generation ?? 0, action: action, rendererId: "")
    }

    private func apply(_ result: BrowserFrame, action: BrowserAction) {
        if frame?.inputKind != result.inputKind {
            text = ""
        }
        frame = result
        if let focus = browserInputFocus(action: action, inputKind: result.inputKind) {
            showKeyboard = focus
            textFocused = showKeyboard
            addressFocused = false
        }
        if result.tabId.isEmpty || result.imageId.isEmpty {
            image = nil
        }
        if let metadata = result.credentials {
            credentials = metadata
        }
        if !result.value.isEmpty {
            notice = result.value
        }
        if !result.image.isEmpty {
            image = UIImage(data: result.image)
        }
        if !addressFocused {
            let url = frame?.currentTab?.url
            address = url == "about:blank" ? "" : (url ?? "")
        }
        connectionError = nil
    }

    private func perform(_ request: BrowserRequest) async {
        defer {
            if case .dialog = request.action {
                answeringDialog = false
            }
        }
        do {
            let result = try await store.browser(request: request)
            guard !Task.isCancelled, scenePhase == .active else { return }
            apply(result, action: request.action)
        } catch {
            guard !Task.isCancelled else { return }
            let message: String = if case let AgentError.Failed(reason) = error {
                reason
            } else {
                error.localizedDescription
            }
            if case .read = request.action {
                connectionError = message
            } else {
                self.error = message
            }
        }
    }
}

/// Tabs, credentials and viewport sizes of the Host's browser.
private struct BrowserMenu: View {
    let frame: BrowserFrame?
    let viewportSize: CGSize
    let send: (BrowserAction) -> Void

    var body: some View {
        Menu {
            Button("ログイン状態・保存パスワード") { send(.credentials) }
                .accessibilityIdentifier("browser.credentials")
            Section("開いているタブ") {
                Picker("開いているタブ", selection: Binding(get: { frame?.tabId ?? "" }, set: {
                    send(.selectTab(id: $0))
                })) {
                    ForEach(frame?.tabs ?? [], id: \.id) { tab in
                        Label(tab.url == "about:blank" ? "新しいタブ" : (tab.title.isEmpty ? tab.url : tab.title),
                              systemImage: "globe")
                            .tag(tab.id)
                            .accessibilityIdentifier(tab.id == frame?.tabId ? "browser.title" : "browser.tab.\(tab.id)")
                    }
                }
                .pickerStyle(.inline)
                .labelsHidden()
            }
            Button("新しいタブ") { send(.openTab(url: "about:blank")) }
            Button("タブを閉じる") {
                if let currentTab = frame?.currentTab {
                    send(.closeTab(id: currentTab.id))
                }
            }

            Divider()
            Button("画面に合わせる") {
                if let action = browserFitAction(width: viewportSize.width, height: viewportSize.height,
                                                 currentWidth: frame?.width ?? 0,
                                                 currentHeight: frame?.height ?? 0) {
                    send(action)
                }
            }
            Button("デスクトップ · 1280 × 800") { send(.resize(width: 1280, height: 800)) }

            Button("モバイル · 390 × 844") { send(.resize(width: 390, height: 844)) }

            Button("URL をコピー") { UIPasteboard.general.string = frame?.currentTab?.url }
        } label: { Label("ブラウザのメニュー", systemImage: "ellipsis") }
            .accessibilityLabel("ブラウザのメニュー").accessibilityIdentifier("browser.menu")
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
        view.display(image)
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
        accessibilityLabel = "ホストのBexブラウザ。ピンチで拡大、2本指で表示範囲を移動できます。"
    }

    @available(*, unavailable)
    required init?(coder _: NSCoder) {
        fatalError("init(coder:) has not been implemented")
    }

    func display(_ image: UIImage) {
        let changedSize = page.image?.size != image.size
        page.image = image
        guard changedSize else { return }
        minimumZoomScale = 1
        setZoomScale(1, animated: false)
        page.frame = CGRect(origin: .zero, size: image.size)
        contentSize = image.size
        previousSize = .zero
        setNeedsLayout()
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
                      deltaX: max(-4096, min(4096, -delta.x)), deltaY: max(-4096, min(4096, -delta.y))))
    }
}

extension BrowserFrame {
    var currentTab: BrowserTab? {
        tabs.first { $0.id == tabId }
    }
}

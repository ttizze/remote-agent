import AgentCore
import SwiftUI

struct SwiftUIRoot: View {
    @ObservedObject var model: AppViewModel

    var body: some View {
        NavigationStack(path: Binding<[AppScreen]>(
            get: {
                switch model.screen {
                case .thread: [.threads, .thread]
                case .threads: [.threads]
                default: []
                }
            },
            set: { path in
                if path.last == .threads, model.screen == .thread {
                    model.showThreadList()
                } else if path.isEmpty, model.screen == .threads || model.screen == .thread {
                    model.showProfiles()
                }
            }
        )) {
            Group {
                if model.profiles.isEmpty {
                    pairingScreen
                } else {
                    ProfilesScreen(profiles: model.profiles, notice: model.notice,
                                   select: model.selectProfile, remove: model.removeProfile, add: model.openPairing)
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
                }
            }
            .navigationDestination(for: AppScreen.self) { screen in
                switch screen {
                case .threads: ThreadsScreen(model: model)
                case .thread: ConversationDestination(model: model)
                default: EmptyView()
                }
            }
            .navigationBarTitleDisplayMode(.inline)
        }
        .safeAreaInset(edge: .top, spacing: 0) {
            ErrorNotices(model: model, scope: .application).padding(.horizontal)
        }
        .onChange(of: model.screen) { screen in
            if screen != .thread {
                model.sideChatRequest = nil
            }
        }
    }

    private var pairingScreen: some View {
        PairingScreen(canCancel: !model.profiles.isEmpty,
                      connecting: model.isConnecting, error: model.pairingError,
                      hostName: model.pairingInvitation?.hostName,
                      aiRecipients: model.pairingInvitation?.aiRecipients ?? [],
                      transcriptionRecipient: model.pairingInvitation?.transcriptionRecipient,
                      prepare: model.preparePairing, confirm: model.confirmPairing,
                      change: model.openPairing, cancel: model.dismissPairing)
    }
}

private struct PairingScreen: View {
    @Environment(\.locale) private var locale
    let canCancel: Bool
    let connecting: Bool
    let error: String?
    let hostName: String?
    let aiRecipients: [String]
    let transcriptionRecipient: String?
    let prepare: (String) -> Void
    let confirm: () -> Void
    let change: () -> Void
    let cancel: () -> Void
    @State private var contents = ""
    @State private var showsManualPairing = false
    @State private var isScanning = false
    @FocusState private var editingContents: Bool

    var body: some View {
        let recipients = aiRecipients.isEmpty
            ? localizedUiText(language: locale.appLanguage, source: "未設定") : aiRecipients.joined(separator: "、")
        Form {
            Section {
                VStack(alignment: .leading, spacing: 8) {
                    Label("どこでも、これひとつで。", systemImage: "text.bubble")
                        .font(.title2.bold())
                        .accessibilityIdentifier("pairing.welcome")
                    Text("AIとの会話も、ファイルも、ターミナルも。いつもの作業を、手元から。")
                        .foregroundStyle(.secondary)
                }
                .padding(.vertical, 8)
            }
            if let hostName {
                Section("接続先") {
                    LabeledContent {
                        Text(hostName).accessibilityIdentifier("pairing.host")
                    } label: {
                        Label("PC", systemImage: "laptopcomputer")
                    }
                    .accessibilityElement(children: .contain)
                    Button("変更") {
                        contents = ""
                        showsManualPairing = false
                        change()
                    }
                    .disabled(connecting)
                    .accessibilityIdentifier("pairing.change")
                }
                Section {
                    Text("メッセージ・添付ファイル・作業に必要なプロジェクトの内容を、このPCと利用するAIサービスに送信します。")
                    Text(verbatim: localizedUiText(language: locale.appLanguage, source: "AI処理：") + recipients)
                        .accessibilityIdentifier("pairing.recipients")
                    if let recipient = transcriptionRecipient {
                        Text("音声入力：\(recipient)（文字起こし）")
                    }
                } header: {
                    Text("送信する内容")
                } footer: {
                    Text("共有PCでは、管理者などが内容を閲覧できる場合があります。")
                }
                Section {
                    Button("同意して接続", action: confirm)
                        .disabled(connecting)
                        .accessibilityIdentifier("pairing.confirm")
                }
            } else {
                Section {
                    Button { isScanning = true } label: {
                        Label("QRコードを読み取る", systemImage: "qrcode.viewfinder")
                    }
                    .accessibilityIdentifier("pairing.scan")
                    Button("接続情報を入力") { showsManualPairing.toggle() }
                        .accessibilityIdentifier("pairing.manual")
                    if showsManualPairing {
                        SecureField("接続情報を貼り付け", text: $contents)
                            .focused($editingContents)
                            .textInputAutocapitalization(.never).autocorrectionDisabled()
                            .accessibilityIdentifier("pairing.contents")
                        Button("接続先を確認") {
                            editingContents = false
                            prepare(contents)
                            contents = ""
                        }
                        .disabled(contents.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                        .accessibilityIdentifier("pairing.submit")
                    }
                } header: {
                    Text("PCと接続")
                } footer: {
                    Text("PCでBexを開き、表示されたQRコードを読み取ってください。")
                }
            }
            if connecting {
                ProgressView("接続中…").accessibilityIdentifier("pairing.progress")
            } else if let error {
                AppNotice(text: error)
            }
            LanguageSettings()
            Section {
                PrivacyPolicyButton()
            }
        }
        .scrollDismissesKeyboard(.interactively)
        .navigationTitle(LocalizedStringKey(canCancel ? "PCを追加" : "Bex"))
        .navigationBarTitleDisplayMode(.inline)
        .sheet(isPresented: $isScanning) {
            QrScannerSheet {
                isScanning = false
                if let contents = $0 {
                    prepare(contents)
                }
            }
            .interactiveDismissDisabled()
        }
        .toolbar {
            ToolbarItem(placement: .cancellationAction) {
                if canCancel {
                    Button("キャンセル", action: cancel)
                        .accessibilityIdentifier("pairing.cancel")
                }
            }
        }
    }
}

private struct ProfilesScreen: View {
    let profiles: [HostProfile]
    let notice: String?
    let select: (String) -> Void
    let remove: (String) -> Void
    let add: () -> Void
    @State private var removing: HostProfile?

    var body: some View {
        List {
            if let notice {
                AppNotice(text: notice)
            }
            Section {
                ForEach(profiles, id: \.id) { profile in
                    Button { select(profile.id) } label: {
                        HStack {
                            Label(profile.name, systemImage: "laptopcomputer")
                                .foregroundStyle(.primary)
                            Spacer()
                            Image(systemName: "chevron.right")
                                .font(.footnote.weight(.semibold)).foregroundStyle(.tertiary)
                        }
                    }
                    .accessibilityIdentifier("profiles.\(profile.id)")
                    .swipeActions(allowsFullSwipe: false) {
                        Button("接続を解除", role: .destructive) { removing = profile }
                    }
                    .contextMenu {
                        Button("接続を解除", role: .destructive) { removing = profile }
                            .accessibilityIdentifier("connection.remove.\(profile.id)")
                    }
                }
            } header: {
                Text("このiPhone").textCase(nil)
            } footer: {
                Text("接続先を選んで、タスクを続けられます。")
            }
            LanguageSettings()
            Section {
                PrivacyPolicyButton()
            }
        }
        .listStyle(.insetGrouped)
        .navigationTitle("PC")
        .navigationBarTitleDisplayMode(.large)
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                Button(action: add) { Label("PCを追加", systemImage: "plus") }
                    .accessibilityIdentifier("profiles.add")
            }
        }
        .alert("このPCとの接続を解除しますか？", isPresented: Binding(
            get: { removing != nil }, set: {
                if !$0 {
                    removing = nil
                }
            }
        )) {
            if let removing {
                Button("接続を解除", role: .destructive) { remove(removing.id) }
            }
            Button("キャンセル", role: .cancel) { removing = nil }
        } message: {
            Text("このiPhoneの接続先と認証鍵を削除します。再接続にはペアリングが必要です。")
        }
    }
}

struct AppNotice: View {
    let text: String

    var body: some View {
        AppText(text)
            .foregroundColor(.red)
            .accessibilityIdentifier("notice")
    }
}

private struct QrScannerSheet: View {
    let completion: (String?) -> Void

    var body: some View {
        NavigationStack {
            QrScannerController(completion: completion)
                .navigationTitle("QRコードを読み取る")
                .navigationBarTitleDisplayMode(.inline)
                .toolbar {
                    ToolbarItem(placement: .cancellationAction) {
                        Button("キャンセル") { completion(nil) }
                            .accessibilityIdentifier("scanner.cancel")
                    }
                }
        }
        .accessibilityIdentifier("scanner.sheet")
    }
}

private struct QrScannerController: UIViewControllerRepresentable {
    @Environment(\.locale) private var locale
    let completion: (String) -> Void

    func makeUIViewController(context _: Context) -> QrCaptureViewController {
        QrCaptureViewController(language: locale.appLanguage, completion: completion)
    }

    func updateUIViewController(_: QrCaptureViewController, context _: Context) {}
}

/// Keep native editing ahead of Store publication; every edit still dispatches synchronously.
struct BufferedTextInput<Content: View>: View {
    @Binding private var value: String
    @State private var text: String
    let content: (Binding<String>) -> Content

    init(value: Binding<String>, @ViewBuilder content: @escaping (Binding<String>) -> Content) {
        _value = value
        _text = State(initialValue: value.wrappedValue)
        self.content = content
    }

    var body: some View {
        let input = Binding(get: { text }, set: {
            guard text != $0 else { return }
            text = $0
            value = $0
        })
        content(input).onChange(of: value) { _ in
            if text != value {
                text = value
            }
        }
    }
}

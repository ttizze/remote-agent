import AgentCore
import SwiftUI

struct SwiftUIRoot: View {
    let model: AppViewModel

    var body: some View {
        @Bindable var model = model
        NavigationStack(path: $model.navigationPath) {
            ThreadsScreen(model: model)
                .background(NavigationAppearance(didAppear: model.threadListDidAppear))
                .navigationDestination(for: AppScreen.self) { screen in
                    switch screen {
                    case .thread:
                        if let chat = model.chat {
                            ConversationDestination(model: chat, environment: ConversationEnvironment(
                                profiles: model.profiles, hostName: model.selectedProfileName,
                                list: model.list, connecting: model.isConnecting,
                                selectHost: model.openNewThread(on:)
                            )).id(chat.host)
                        }
                    case .settings: SettingsScreen(model: model)
                    case .profiles: ConnectionsScreen(model: model)
                    }
                }
                .navigationBarTitleDisplayMode(.inline)
        }
        .safeAreaInset(edge: .top, spacing: 0) {
            ErrorNotices(notices: model.notices[.application] ?? [], perform: { model.perform($0) },
                         reconnect: model.isConnected ? nil : { model.connect() }, reconnecting: model.isConnecting)
                .padding(.horizontal)
        }
        .onChange(of: model.navigationPath) { path in
            if path.last != .thread {
                model.chat?.sideChatRequest = nil
            }
        }
    }
}

struct ConnectionsScreen: View {
    @Environment(\.locale) private var locale
    let model: AppViewModel
    @State private var contents = ""
    @State private var showsManualPairing = false
    @State private var isScanning = false
    @State private var removing: HostProfile?
    @FocusState private var editingContents: Bool

    var body: some View {
        Form {
            if let notice = model.notice {
                AppNotice(text: notice)
            }
            if !model.profiles.isEmpty {
                Section("接続先") {
                    ForEach(model.profiles) { profile in
                        ConnectionSettingsRow(model: model, profile: profile)
                            .swipeActions(allowsFullSwipe: false) {
                                Button("接続を解除", role: .destructive) { removing = profile }
                            }
                            .contextMenu {
                                Button("接続を解除", role: .destructive) { removing = profile }
                                    .accessibilityIdentifier("connection.remove.\(profile.id)")
                            }
                    }
                }
            } else {
                Section {
                    Text("作業するPCを接続")
                        .font(.headline)
                        .accessibilityIdentifier("pairing.welcome")
                    Text("接続すると、プロジェクトとタスクがここに表示されます")
                        .foregroundStyle(.secondary)
                }
            }
            if let invitation = model.pairingInvitation {
                pairingConfirmation(invitation)
            } else {
                Section {
                    Button { isScanning = true } label: {
                        Label("QRコードを読み取る", systemImage: "qrcode.viewfinder")
                    }
                    .accessibilityIdentifier("pairing.scan")
                    Button { showsManualPairing.toggle() } label: {
                        Label("接続情報を入力", systemImage: "doc.on.clipboard")
                    }
                    .accessibilityIdentifier("pairing.manual")
                    if showsManualPairing {
                        SecureField("接続情報を貼り付け", text: $contents)
                            .focused($editingContents)
                            .textInputAutocapitalization(.never).autocorrectionDisabled()
                            .accessibilityIdentifier("pairing.contents")
                        Button("接続先を確認") {
                            editingContents = false
                            model.preparePairing(contents)
                            contents = ""
                        }
                        .disabled(contents.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                        .accessibilityIdentifier("pairing.submit")
                    }
                } header: {
                    Text("PCを追加")
                } footer: {
                    Text("接続したいPCでBexを開き、表示されたQRコードを読み取ってください。")
                }
            }
            if model.pairingConnection != nil {
                ProgressView("接続中…").accessibilityIdentifier("pairing.progress")
            } else if let error = model.pairingError {
                AppNotice(text: error)
            }
            if model.profiles.isEmpty {
                LanguageSettings()
                Section { PrivacyPolicyButton() }
            }
        }
        .tint(.blue)
        .scrollDismissesKeyboard(.interactively)
        .navigationTitle("環境")
        .navigationBarTitleDisplayMode(.inline)
        .navigationBarBackButtonHidden(model.pairingConnection != nil)
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                Button { isScanning = true } label: { Label("PCを追加", systemImage: "plus") }
                    .disabled(model.pairingConnection != nil)
                    .accessibilityIdentifier("profiles.add")
            }
        }
        .sheet(isPresented: $isScanning) {
            QrScannerSheet {
                isScanning = false
                if let contents = $0 {
                    model.preparePairing(contents)
                }
            }
            .interactiveDismissDisabled()
        }
        .alert("このPCとの接続を解除しますか？", isPresented: Binding(
            get: { removing != nil }, set: {
                if !$0 {
                    removing = nil
                }
            }
        )) {
            if let removing {
                Button("接続を解除", role: .destructive) { model.removeProfile(removing.id) }
            }
            Button("キャンセル", role: .cancel) { removing = nil }
        } message: {
            Text("このiPhoneの接続先と認証鍵を削除します。再接続にはペアリングが必要です。")
        }
        .onDisappear(perform: model.cancelPairing)
    }

    @ViewBuilder private func pairingConfirmation(_ invitation: Invitation) -> some View {
        Section("接続先") {
            // The title initializer combines the Host value with its label for accessibility.
            LabeledContent {
                Text(invitation.hostName).accessibilityIdentifier("pairing.host")
            } label: {
                Label("PC", systemImage: "laptopcomputer")
            }
            .accessibilityElement(children: .contain)
            Button("変更") {
                contents = ""
                showsManualPairing = false
                model.cancelPairing()
            }
            .disabled(model.pairingConnection != nil)
            .accessibilityIdentifier("pairing.change")
        }
        Section {
            Text("メッセージ・添付ファイル・作業に必要なプロジェクトの内容を、このPCと利用するAIサービスに送信します。")
            Text(verbatim: localizedUiText(language: locale.appLanguage, source: "AI処理：") +
                (invitation.aiRecipients.isEmpty ? localizedUiText(language: locale.appLanguage, source: "未設定") :
                    invitation.aiRecipients.joined(separator: "、")))
                .accessibilityIdentifier("pairing.recipients")
            if let recipient = invitation.transcriptionRecipient {
                Text("音声入力：\(recipient)（文字起こし）")
            }
        } header: {
            Text("送信する内容")
        } footer: {
            Text("共有PCでは、管理者などが内容を閲覧できる場合があります。")
        }
        Section {
            Button("同意して接続", action: model.confirmPairing)
                .disabled(model.pairingConnection != nil)
                .accessibilityIdentifier("pairing.confirm")
        }
    }
}

struct AppNotice: View {
    let text: ErrorText

    init(text: String) {
        self.text = errorText(error: text)
    }

    init(text: ErrorText) {
        self.text = text
    }

    var body: some View {
        ErrorTextView(text: text)
            .foregroundColor(.red)
            .accessibilityIdentifier("notice")
    }
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

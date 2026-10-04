import AgentCore
import SwiftUI
import UIKit

struct BexSwiftUIRoot: View {
    @ObservedObject var model: BexAppViewModel

    var body: some View {
        BexScreen(model: model)
            .onChange(of: model.screen) { screen in
                if screen != .thread {
                    model.sideChatRequest = nil
                }
            }
            .sheet(isPresented: $model.isScanning) {
                BexQrScannerSheet { model.scanned($0) }
                    .interactiveDismissDisabled()
            }
    }
}

private struct BexScreen: View {
    @ObservedObject var model: BexAppViewModel
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
        .preferredColorScheme(.dark)
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
}

private struct PairingScreen: View {
    let canCancel: Bool
    let connecting: Bool
    let error: String?
    let scan: () -> Void
    let hostName: String?
    let aiRecipients: [String]
    let transcriptionRecipient: String?
    let prepare: (String) -> Void
    let confirm: () -> Void
    let change: () -> Void
    let cancel: () -> Void
    @State private var contents = ""
    @State private var showsManualPairing = false
    @FocusState private var editingContents: Bool

    var body: some View {
        GeometryReader { geometry in
            ScrollView {
                VStack(spacing: hostName == nil ? 16 : 12) {
                    if hostName == nil {
                        Spacer(minLength: 0)
                    }
                    VStack(spacing: hostName == nil ? 18 : 8) {
                        Image(systemName: "text.bubble")
                            .font(.system(size: hostName == nil ? 36 : 24, weight: .medium))
                            .foregroundStyle(Color.accentColor)
                            .frame(width: hostName == nil ? 72 : 32, height: hostName == nil ? 72 : 32)
                            .background(Color(UIColor.secondarySystemGroupedBackground))
                            .clipShape(RoundedRectangle(cornerRadius: 28))
                        Text("どこでも、\nこれひとつで。")
                            .font(.system(size: hostName == nil ? 34 : 28, weight: .bold))
                            .accessibilityIdentifier("pairing.welcome")
                        Text("AIとの会話も、ファイルも、ターミナルも。\nいつもの作業を、手元から。")
                            .font(.subheadline)
                            .foregroundStyle(.secondary)
                    }
                    .multilineTextAlignment(.center)
                    if hostName == nil {
                        Spacer(minLength: 0)
                    }
                    if let hostName {
                        VStack(alignment: .leading, spacing: 6) {
                            HStack(spacing: 16) {
                                Image(systemName: "laptopcomputer").font(.system(size: 28))
                                VStack(alignment: .leading, spacing: 4) {
                                    Text("接続先").font(.caption).foregroundStyle(.secondary)
                                    Text(hostName).font(.headline)
                                        .accessibilityIdentifier("pairing.host")
                                }
                                Spacer()
                                Button("変更") {
                                    contents = ""
                                    showsManualPairing = false
                                    change()
                                }
                                .disabled(connecting)
                                .accessibilityIdentifier("pairing.change")
                            }
                            Divider()
                            Text("送信する内容").font(.subheadline.bold())
                            Text("メッセージ・添付ファイル・作業に必要なプロジェクトの内容を、このPCと利用するAIサービスに送信します。")
                                .foregroundStyle(.secondary)
                            Divider()
                            Text("このPCの送信先").font(.subheadline.bold())
                            VStack(alignment: .leading, spacing: 4) {
                                Text("AI処理：" +
                                    (aiRecipients.isEmpty ? "未設定" : aiRecipients
                                        .joined(separator: "、")))
                                    .accessibilityIdentifier("pairing.recipients")
                                if let recipient = transcriptionRecipient {
                                    Text("音声入力：\(recipient)（文字起こし）")
                                }
                            }
                            .foregroundStyle(.secondary)
                            Divider()
                            Text("共有PCでは、管理者などが内容を閲覧できる場合があります。")
                                .foregroundStyle(.secondary)
                        }
                        .font(.footnote)
                        .padding(16)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .background(Color(UIColor.secondarySystemGroupedBackground))
                        .clipShape(RoundedRectangle(cornerRadius: 20))
                    } else {
                        VStack(spacing: 16) {
                            Image(systemName: "laptopcomputer")
                                .font(.system(size: 36)).foregroundStyle(.secondary)
                            Text("まずはPCとつなぐ").font(.title3.bold())
                            Text("PCでBexを開き、表示されたQRコードを読み取ってください。")
                                .font(.subheadline).foregroundStyle(.secondary)
                                .multilineTextAlignment(.center)
                            Button(action: scan) {
                                Label("QRコードを読み取る", systemImage: "qrcode.viewfinder")
                                    .font(.headline)
                                    .frame(maxWidth: .infinity, minHeight: 24)
                            }
                            .buttonStyle(.borderedProminent)
                            .controlSize(.large)
                            .accessibilityIdentifier("pairing.scan")
                            Button("接続情報を入力") { showsManualPairing.toggle() }
                                .accessibilityIdentifier("pairing.manual")
                            if showsManualPairing {
                                SecureField("接続情報を貼り付け", text: $contents)
                                    .focused($editingContents)
                                    .font(.system(.footnote, design: .monospaced))
                                    .frame(minHeight: 44)
                                    .textFieldStyle(.roundedBorder)
                                    .accessibilityIdentifier("pairing.contents")
                                Button("接続先を確認") {
                                    editingContents = false
                                    prepare(contents)
                                    contents = ""
                                }
                                .buttonStyle(.bordered)
                                .disabled(contents.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                                .accessibilityIdentifier("pairing.submit")
                            }
                        }
                        .padding(16)
                        .background(Color(UIColor.secondarySystemGroupedBackground))
                        .clipShape(RoundedRectangle(cornerRadius: 20))
                    }
                    if connecting {
                        ProgressView("接続中…").accessibilityIdentifier("pairing.progress")
                    } else if let error {
                        BexNotice(text: error)
                    }
                    PrivacyPolicyButton()
                        .font(.footnote)
                        .tint(.secondary)
                    if hostName != nil {
                        Button(action: confirm) {
                            Text("同意して接続")
                                .font(.headline)
                                .frame(maxWidth: .infinity, minHeight: 24)
                        }
                        .buttonStyle(.borderedProminent)
                        .controlSize(.large)
                        .disabled(connecting)
                        .accessibilityIdentifier("pairing.confirm")
                    }
                }
                .padding(.horizontal, 24)
                .padding(.vertical, 16)
                .frame(minHeight: geometry.size.height)
            }
            .scrollDismissesKeyboard(.interactively)
        }
        .background(Color(UIColor.systemGroupedBackground).ignoresSafeArea())
        .navigationTitle("Bex")
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
        GeometryReader { geometry in
            ScrollView {
                VStack(alignment: .leading, spacing: 24) {
                    VStack(alignment: .leading, spacing: 8) {
                        Text("接続するPC").font(.largeTitle.bold())
                        Text("いつもの作業を、ここから。")
                            .foregroundStyle(.secondary)
                    }
                    .padding(.vertical, 20)
                    if let notice {
                        BexNotice(text: notice)
                    }
                    ForEach(profiles, id: \.id) { profile in
                        Button { select(profile.id) } label: {
                            HStack(spacing: 16) {
                                Image(systemName: "laptopcomputer")
                                    .font(.system(size: 30)).foregroundStyle(.secondary)
                                VStack(alignment: .leading, spacing: 6) {
                                    Text(profile.name).font(.headline)
                                    Text("登録済みのPC").font(.subheadline).foregroundStyle(.secondary)
                                }
                                Spacer()
                                Image(systemName: "chevron.right").foregroundStyle(.secondary)
                            }
                            .padding(20)
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .background(Color(UIColor.secondarySystemGroupedBackground))
                            .clipShape(RoundedRectangle(cornerRadius: 18))
                        }
                        .buttonStyle(.plain)
                        .accessibilityIdentifier("profiles.\(profile.id)")
                        .contextMenu {
                            Button("接続を解除", role: .destructive) { removing = profile }
                                .accessibilityIdentifier("connection.remove.\(profile.id)")
                        }
                    }
                    Button(action: add) {
                        Label("PCを追加", systemImage: "plus")
                            .font(.headline)
                            .frame(maxWidth: .infinity, minHeight: 52)
                            .overlay(RoundedRectangle(cornerRadius: 16).stroke(Color.accentColor))
                    }
                    .accessibilityIdentifier("profiles.add")
                    Spacer(minLength: 32)
                    PrivacyPolicyButton()
                        .font(.footnote).tint(.secondary)
                        .frame(maxWidth: .infinity)
                }
                .padding(24)
                .frame(minHeight: geometry.size.height, alignment: .top)
            }
        }
        .background(Color(UIColor.systemGroupedBackground).ignoresSafeArea())
        .safeAreaInset(edge: .top, spacing: 0) {
            SettingsScopeBar { Text("すべてのプロジェクト") } environment: { Text("このiPhone") }
        }
        .navigationTitle("Bex")
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

struct BexNotice: View {
    let text: String

    var body: some View {
        Text(text)
            .foregroundColor(.red)
            .accessibilityIdentifier("notice")
    }
}

private struct BexQrScannerSheet: View {
    let completion: (String?) -> Void

    var body: some View {
        NavigationStack {
            BexQrScannerController(completion: completion)
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

private struct BexQrScannerController: UIViewControllerRepresentable {
    let completion: (String?) -> Void

    func makeUIViewController(context _: Context) -> BexQrCaptureViewController {
        BexQrCaptureViewController(completion: completion)
    }

    func updateUIViewController(_: BexQrCaptureViewController, context _: Context) {}
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

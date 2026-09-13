import AgentCore
import SwiftUI
import UIKit

struct BexSwiftUIRoot: View {
    @ObservedObject var model: BexAppViewModel

    var body: some View {
        BexScreen(model: model)
            .sheet(item: $model.sideChatRequest) { request in
                ConversationSideChat(model: model, request: request)
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
        NavigationView {
            Group {
                if model.profiles.isEmpty {
                    PairingScreen(canCancel: !model.profiles.isEmpty, error: model.pairingError,
                                  scan: { model.isScanning = true }, pair: model.pair, cancel: model.dismissPairing)
                } else {
                    ProfilesScreen(profiles: model.profiles, notice: model.notice,
                                   select: model.selectProfile, remove: model.removeProfile, add: model.openPairing)
                        .background(
                            NavigationLink(isActive: Binding(
                                get: { model.screen == .threads || model.screen == .thread },
                                set: {
                                    if !$0, model.selectedProfileId != nil {
                                        model.showProfiles()
                                    }
                                }
                            )) {
                                threadList
                            } label: { EmptyView() }
                        )
                        .sheet(isPresented: Binding(
                            get: { model.screen == .pairing },
                            set: {
                                if !$0, model.screen == .pairing {
                                    model.dismissPairing()
                                }
                            }
                        )) {
                            NavigationView { PairingScreen(
                                canCancel: !model.profiles.isEmpty,
                                error: model.pairingError,
                                scan: { model.isScanning = true },
                                pair: model.pair,
                                cancel: model.dismissPairing
                            ) }
                            .navigationViewStyle(StackNavigationViewStyle())
                        }
                }
            }
            .navigationBarTitleDisplayMode(.inline)
        }
        .navigationViewStyle(StackNavigationViewStyle())
        .preferredColorScheme(.dark)
    }

    private var threadList: some View {
        ThreadsScreen(model: model)
            .background(
                NavigationLink(isActive: Binding(
                    get: { model.screen == .thread },
                    set: {
                        if !$0, model.screen == .thread {
                            model.showThreadList()
                        }
                    }
                )) {
                    ThreadScreen(
                        model: model,
                        conversation: model.sideChatRequest?.originalConversation ?? model.conversation
                    )
                } label: { EmptyView() }
            )
    }
}

private struct PairingScreen: View {
    let canCancel: Bool
    let error: String?
    let scan: () -> Void
    let pair: (String) -> Void
    let cancel: () -> Void
    @State private var contents = ""
    @State private var showsManualPairing = false

    var body: some View {
        ScrollView {
            VStack(spacing: 24) {
                VStack(spacing: 14) {
                    Image(systemName: "terminal.fill")
                        .font(.system(size: 36, weight: .semibold))
                        .foregroundColor(.accentColor)
                        .frame(width: 76, height: 76)
                        .background(Color.accentColor.opacity(0.12))
                        .clipShape(RoundedRectangle(cornerRadius: 20, style: .continuous))
                    Text("PCとペアリング")
                        .font(.title2.weight(.bold))
                    Text("PCに表示されたQRコードを読み取ると、このiPhoneからCodexを操作できます。")
                        .font(.subheadline)
                        .multilineTextAlignment(.center)
                        .foregroundColor(.secondary)
                }

                Button { scan() } label: {
                    Label("QRコードを読み取る", systemImage: "qrcode.viewfinder")
                        .font(.headline)
                        .frame(maxWidth: .infinity)
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.large)
                .accessibilityIdentifier("pairing.scan")

                DisclosureGroup("QRの内容を手入力", isExpanded: $showsManualPairing) {
                    VStack(alignment: .leading, spacing: 12) {
                        SecureField("ペアリング情報を貼り付け", text: $contents)
                            .font(.system(.footnote, design: .monospaced))
                            .frame(minHeight: 44)
                            .textFieldStyle(.roundedBorder)
                            .accessibilityIdentifier("pairing.contents")
                        Button("入力内容でペアリング") { pair(contents) }
                            .buttonStyle(.bordered)
                            .disabled(contents.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                            .accessibilityIdentifier("pairing.submit")
                    }
                    .padding(.top, 12)
                }
                .padding(16)
                .background(Color(UIColor.secondarySystemBackground))
                .clipShape(RoundedRectangle(cornerRadius: 16, style: .continuous))

                Text("ペアリング情報は接続時だけ使用し、QRの内容そのものは保存しません。")
                    .font(.caption)
                    .multilineTextAlignment(.center)
                    .foregroundColor(.secondary)

                if let error {
                    BexNotice(text: error)
                }
            }
            .padding(.horizontal, 24)
            .padding(.vertical, 32)
        }
        .background(Color(UIColor.systemGroupedBackground).ignoresSafeArea())
        .navigationTitle("Bex")
        .toolbar {
            ToolbarItem(placement: .cancellationAction) {
                if canCancel {
                    Button("キャンセル") { cancel() }
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
                BexNotice(text: notice)
            }
            ForEach(profiles, id: \.id) { profile in
                HStack {
                    Button { select(profile.id) } label: {
                        VStack(alignment: .leading, spacing: 4) {
                            Text(profile.name).font(.headline)
                            Text(profile.hostIdentity).font(.caption).foregroundColor(.secondary)
                        }
                    }
                    .buttonStyle(.borderless)
                    .accessibilityIdentifier("profiles.\(profile.id)")
                    Spacer()
                    Button("接続を解除", role: .destructive) { removing = profile }
                        .buttonStyle(.borderless)
                        .accessibilityIdentifier("connection.remove.\(profile.id)")
                }
            }
            Section {
                Button("PCを追加") { add() }
                    .accessibilityIdentifier("profiles.add")
            }
        }
        .navigationTitle("PC Hosts")
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
        NavigationView {
            BexQrScannerController { result in
                switch result {
                case let .success(contents): completion(contents)
                case .failure: completion(nil)
                }
            }
            .navigationTitle("QRコードを読み取る")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("キャンセル") { completion(nil) }
                        .accessibilityIdentifier("scanner.cancel")
                }
            }
        }
        .navigationViewStyle(StackNavigationViewStyle())
        .accessibilityIdentifier("scanner.sheet")
    }
}

private struct BexQrScannerController: UIViewControllerRepresentable {
    let completion: (Result<String, BexQrCaptureError>) -> Void

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

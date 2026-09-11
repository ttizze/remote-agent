import AgentCore
import SwiftUI
import UIKit

struct BexSwiftUIRoot: View {
    @ObservedObject var model: BexAppViewModel

    var body: some View {
        BexScreen(state: model.state, model: model)
            .sheet(isPresented: $model.isScanning) {
                BexQrScannerSheet { model.scanned($0) }
                    .interactiveDismissDisabled()
            }
    }
}

private struct BexScreen: View {
    let state: AppPresentation
    @ObservedObject var model: BexAppViewModel

    var body: some View {
        NavigationView {
            Group {
                if state.profiles.isEmpty {
                    PairingScreen(state: state, model: model)
                } else {
                    ProfilesScreen(state: state, model: model)
                        .background(
                            NavigationLink(isActive: Binding(
                                get: { state.screen == .threads || state.screen == .thread },
                                set: {
                                    if !$0, model.state.selectedProfileId != nil {
                                        model.showProfiles()
                                    }
                                }
                            )) {
                                threadList
                            } label: { EmptyView() }
                        )
                        .sheet(isPresented: Binding(
                            get: { state.screen == .pairing },
                            set: {
                                if !$0, model.state.screen == .pairing {
                                    model.dismissPairing()
                                }
                            }
                        )) {
                            NavigationView { PairingScreen(state: state, model: model) }
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
        ThreadsScreen(state: state, model: model)
            .background(
                NavigationLink(isActive: Binding(
                    get: { state.screen == .thread },
                    set: {
                        if !$0, model.state.screen == .thread {
                            model.showThreadList()
                        }
                    }
                )) {
                    ThreadScreen(state: state, model: model, conversation: model.conversation)
                } label: { EmptyView() }
            )
    }
}

private struct PairingScreen: View {
    let state: AppPresentation
    @ObservedObject var model: BexAppViewModel
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

                Button { model.isScanning = true } label: {
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
                        Button("入力内容でペアリング") { model.pair(contents) }
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

                if let error = state.pairingError {
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
                if !state.profiles.isEmpty {
                    Button("キャンセル") { model.dismissPairing() }
                        .accessibilityIdentifier("pairing.cancel")
                }
            }
        }
    }
}

private struct ProfilesScreen: View {
    let state: AppPresentation
    @ObservedObject var model: BexAppViewModel
    @State private var removing: HostProfile?

    var body: some View {
        List {
            if let notice = state.notice {
                BexNotice(text: notice)
            }
            ForEach(state.profiles, id: \.id) { profile in
                HStack {
                    Button { model.selectProfile(profile.id) } label: {
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
                Button("PCを追加") { model.openPairing() }
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
                Button("接続を解除", role: .destructive) { model.removeProfile(removing.id) }
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

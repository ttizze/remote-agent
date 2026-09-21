import AgentCore
import SwiftUI

@main
struct BexIOSApp: App {
    @AppStorage("bex.data-sharing-consent") private var acceptedRevision = ""

    var body: some Scene {
        WindowGroup {
            let notice = dataSharingNotice(acceptedRevision: acceptedRevision)
            if notice.requiresConsent {
                DataSharingConsentScreen(notice: notice) { acceptedRevision = notice.revision }
            } else {
                ConnectedBexApp()
            }
        }
    }
}

private struct ConnectedBexApp: View {
    @Environment(\.scenePhase) private var scenePhase
    @State private var wasBackgrounded = false
    @StateObject private var model = BexAppViewModel()

    var body: some View {
        BexSwiftUIRoot(model: model)
            .onChange(of: scenePhase) { phase in
                switch phase {
                case .background:
                    wasBackgrounded = true
                    let application = UIApplication.shared
                    var backgroundTask: UIBackgroundTaskIdentifier = .invalid
                    backgroundTask = application.beginBackgroundTask {
                        if backgroundTask !=
                            .invalid {
                            application.endBackgroundTask(backgroundTask); backgroundTask = .invalid
                        }
                    }
                    Task {
                        await model.persistBeforeBackground()
                        if backgroundTask !=
                            .invalid {
                            application.endBackgroundTask(backgroundTask); backgroundTask = .invalid
                        }
                    }
                case .active where wasBackgrounded:
                    wasBackgrounded = false
                    model.connect(afterForeground: true)
                default:
                    break
                }
            }
    }
}

private struct DataSharingConsentScreen: View {
    let notice: DataSharingNotice
    let agree: () -> Void

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: 24) {
                    Text("データの送信について\nData sharing").font(.title.bold())
                    Text(notice.summary)
                    PrivacyPolicyButton()
                    Button("同意して使う / Agree and continue", action: agree)
                        .buttonStyle(.borderedProminent)
                        .accessibilityIdentifier("privacy.agree")
                    Text("同意しなければ接続や送信は始まりません。\nNo connection or data sharing starts until you agree.")
                        .font(.footnote).foregroundStyle(.secondary)
                }.padding(24)
            }
        }.preferredColorScheme(.dark)
    }
}

struct PrivacyPolicyButton: View {
    @State private var showing = false

    var body: some View {
        Button("プライバシー / Privacy") { showing = true }
            .accessibilityIdentifier("privacy.policy")
            .sheet(isPresented: $showing) { PrivacyPolicyScreen() }
    }
}

private struct PrivacyPolicyScreen: View {
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        let policy = dataSharingNotice(acceptedRevision: "").policy
        NavigationStack {
            ScrollView {
                Text((try? AttributedString(
                    markdown: policy,
                    options: .init(interpretedSyntax: .inlineOnlyPreservingWhitespace)
                )) ?? AttributedString(policy))
                    .textSelection(.enabled).padding()
            }
            .navigationTitle("Privacy")
            .toolbar { Button("閉じる / Done") { dismiss() } }
        }
    }
}

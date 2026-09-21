import AgentCore
import SwiftUI

@main
struct BexIOSApp: App {
    @Environment(\.scenePhase) private var scenePhase
    @State private var wasBackgrounded = false
    @StateObject private var model = BexAppViewModel()

    var body: some Scene {
        WindowGroup {
            BexSwiftUIRoot(model: model)
                .onChange(of: scenePhase) { phase in
                    model.recordScene(phase == .active ? 1 : phase == .inactive ? 2 : 3)
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

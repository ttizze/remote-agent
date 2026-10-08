import AgentCore
import SwiftUI
import UIKit

@main
struct BexIOSApp: App {
    @Environment(\.scenePhase) private var scenePhase
    @State private var wasBackgrounded = false
    @StateObject private var model = BexAppViewModel()
    @UIApplicationDelegateAdaptor(AgentPushCenter.self) private var pushCenter

    var body: some Scene {
        WindowGroup {
            BexSwiftUIRoot(model: model)
                .onReceive(NotificationCenter.default.publisher(for: .remoteAgentShortcut)) { notification in
                    if notification.userInfo?["type"] as? String == "new-thread" { model.handleShortcut() }
                }
                .onAppear {
                    model.ingestIncomingShareHandoffs()
                    if AgentPushCenter.takePendingShortcut() == "new-thread" { model.handleShortcut() }
                    model.setActivityUpdater { [weak pushCenter] states in
                        guard #available(iOS 16.1, *) else { return }
                        Task { @MainActor in
                            await pushCenter?.reconcileActivities(states: states)
                        }
                    }
                    pushCenter.configure(
                        register: { [weak model] hostId, registration in
                            model?.registerPush(hostId: hostId, registration: registration)
                        },
                        setActive: { [weak model] hostId, deviceId, active in
                            model?.setPushActive(hostId: hostId, deviceId: deviceId, active: active)
                        },
                        hostIds: { [weak model] in model?.pushHostIds() ?? [] },
                        visibleThread: { [weak model] in model?.visiblePushThreadDeepLink() },
                        preferences: { [weak model] hostId in model?.pushPreferences(hostId: hostId) ?? .default },
                    )
                }
                .onChange(of: scenePhase) { _, phase in
                    model.recordScene(phase == .active ? 1 : phase == .inactive ? 2 : 3)
                    switch phase {
                    case .background:
                        wasBackgrounded = true
                        let application = UIApplication.shared
                        var backgroundTask: UIBackgroundTaskIdentifier = .invalid
                        backgroundTask = application.beginBackgroundTask {
                            if backgroundTask != .invalid {
                                application.endBackgroundTask(backgroundTask)
                                backgroundTask = .invalid
                            }
                        }
                        Task {
                            await model.persistBeforeBackground()
                            if backgroundTask != .invalid {
                                application.endBackgroundTask(backgroundTask)
                                backgroundTask = .invalid
                            }
                        }
                    case .active where wasBackgrounded:
                        wasBackgrounded = false
                        model.ingestIncomingShareHandoffs()
                        pushCenter.refreshPreferences()
                        model.connect(afterForeground: true)
                    case .active:
                        model.ingestIncomingShareHandoffs()
                        pushCenter.refreshPreferences()
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
        Button("プライバシーポリシー") { showing = true }
            .accessibilityIdentifier("privacy.policy")
            .sheet(isPresented: $showing) { PrivacyPolicyScreen() }
    }
}

private struct PrivacyPolicyScreen: View {
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        let policy = privacyPolicy()
        NavigationStack {
            ScrollView {
                Text((try? AttributedString(
                    markdown: policy,
                    options: .init(interpretedSyntax: .inlineOnlyPreservingWhitespace)
                )) ?? AttributedString(policy))
                    .textSelection(.enabled).padding()
            }
            .navigationTitle("プライバシーポリシー")
            .toolbar { Button("閉じる") { dismiss() } }
        }
    }
}

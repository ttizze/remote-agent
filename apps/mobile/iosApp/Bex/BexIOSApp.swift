import AgentCore
import SwiftUI
import UIKit

@main
struct BexIOSApp: App {
    @UIApplicationDelegateAdaptor(RemoteAgentApplicationDelegate.self) private var appDelegate
    @Environment(\.scenePhase) private var scenePhase
    @State private var wasBackgrounded = false
    @StateObject private var model = BexAppViewModel()

    var body: some Scene {
        WindowGroup {
            BexSwiftUIRoot(model: model)
                .onOpenURL { model.handleSurfaceURL($0) }
                .onReceive(NotificationCenter.default.publisher(for: .remoteAgentShortcut)) { notification in
                    if notification.userInfo?["type"] as? String == "new-thread" {
                        model.handleShortcut()
                    }
                }
                .onAppear {
                    model.ingestIncomingShareHandoffs()
                    if RemoteAgentApplicationDelegate.takePendingShortcut() == "new-thread" {
                        model.handleShortcut()
                    }
                }
                .onChange(of: scenePhase) { _, phase in
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
                        model.ingestIncomingShareHandoffs()
                        model.connect(afterForeground: true)
                    case .active:
                        model.ingestIncomingShareHandoffs()
                    default:
                        break
                    }
                }
        }
    }
}

final class RemoteAgentApplicationDelegate: NSObject, UIApplicationDelegate {
    private static var pendingShortcutType: String?

    static func takePendingShortcut() -> String? {
        defer { pendingShortcutType = nil }
        return pendingShortcutType
    }

    func application(
        _ application: UIApplication,
        performActionFor shortcutItem: UIApplicationShortcutItem,
        completionHandler: @escaping (Bool) -> Void
    ) {
        NotificationCenter.default.post(
            name: .remoteAgentShortcut,
            object: nil,
            userInfo: ["type": shortcutItem.type]
        )
        completionHandler(true)
    }

    func application(
        _ application: UIApplication,
        configurationForConnecting connectingSceneSession: UISceneSession,
        options: UIScene.ConnectionOptions
    ) -> UISceneConfiguration {
        if let shortcut = options.shortcutItem {
            Self.pendingShortcutType = shortcut.type
        }
        UIApplication.shared.shortcutItems = [
            UIApplicationShortcutItem(
                type: "new-thread",
                localizedTitle: "New thread",
                localizedSubtitle: nil,
                icon: UIApplicationShortcutIcon(type: .add),
                userInfo: nil
            )
        ]
        return UISceneConfiguration(name: "Default Configuration", sessionRole: connectingSceneSession.role)
    }
}

private extension Notification.Name {
    static let remoteAgentShortcut = Notification.Name("remote-agent.shortcut")
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

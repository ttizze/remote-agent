import AgentCore
import SwiftUI

@main
struct IOSApp: App {
    @Environment(\.scenePhase) private var scenePhase
    @State private var wasBackgrounded = false
    @StateObject private var model = AppViewModel()
    @AppStorage("bex.language") private var language = "ja"

    var body: some Scene {
        WindowGroup {
            SwiftUIRoot(model: model)
                .environment(\.locale, Locale(identifier: language))
                .onOpenURL { model.openTaskActivity($0) }
                .onChange(of: scenePhase) { phase in
                    model.synchronizeLiveActivities(foreground: phase == .active)
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
        Button("プライバシーポリシー") { showing = true }
            .accessibilityIdentifier("privacy.policy")
            .sheet(isPresented: $showing) { PrivacyPolicyScreen() }
    }
}

/// Only application-owned labels go through this view. Conversation text,
/// filenames and provider output keep their original spelling.
struct AppText: View {
    @Environment(\.locale) private var locale
    let source: String

    init(_ source: String) {
        self.source = source
    }

    var body: some View {
        Text(verbatim: localizedUiText(
            language: locale.appLanguage,
            source: source
        ))
    }
}

extension Locale {
    var appLanguage: UiLanguage {
        language.languageCode?.identifier == "ja" ? .japanese : .english
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

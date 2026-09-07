import SwiftUI

@main
struct BexIOSApp: App {
    @Environment(\.scenePhase) private var scenePhase
    @State private var wasBackgrounded = false
    private let platform = BexPlatformBridge()

    var body: some Scene {
        WindowGroup {
            BexSwiftUIRoot()
                .onChange(of: scenePhase) { phase in
                    switch phase {
                    case .background:
                        wasBackgrounded = true
                        platform.didEnterBackground()
                    case .active where wasBackgrounded:
                        wasBackgrounded = false
                        platform.restoreAfterForeground()
                    default:
                        break
                    }
                }
        }
    }
}

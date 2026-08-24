import SwiftUI
import UIKit

@main
struct BexIOSApp: App {
    @UIApplicationDelegateAdaptor(BexAppDelegate.self) private var appDelegate

    var body: some Scene {
        WindowGroup {
            BexSwiftUIRoot()
        }
    }
}

final class BexAppDelegate: NSObject, UIApplicationDelegate {
    let platform = BexPlatformBridge()

    func application(
        _ application: UIApplication,
        didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]? = nil
    ) -> Bool {
        platform.start()
        return true
    }

    func applicationWillEnterForeground(_ application: UIApplication) {
        platform.restoreAfterForeground()
    }

    func applicationDidEnterBackground(_ application: UIApplication) {
        platform.didEnterBackground()
    }
}

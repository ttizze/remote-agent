import UIKit

@main @MainActor
final class NavigationTests: UIResponder, UIApplicationDelegate {
    func application(_: UIApplication,
                     didFinishLaunchingWithOptions _: [UIApplication.LaunchOptionsKey: Any]?) -> Bool {
        let controller = NavigationAppearance.Controller()
        var appearances = 0
        controller.didAppear = { appearances += 1 }

        controller.beginAppearanceTransition(true, animated: false)
        check(appearances == 0, "Starting an appearance must not publish it before completion")
        controller.endAppearanceTransition()
        check(appearances == 1, "The initial appearance must publish exactly once")
        controller.beginAppearanceTransition(false, animated: false)
        controller.endAppearanceTransition()
        check(appearances == 1, "Leaving the list must not publish another appearance")

        // UIKit reverses appearance callbacks when an interactive pop is cancelled.
        controller.beginAppearanceTransition(true, animated: true)
        check(appearances == 1, "An interactive pop must not publish an appearance while dragging")
        controller.beginAppearanceTransition(false, animated: true)
        controller.endAppearanceTransition()
        check(appearances == 1, "Cancelling a pop must not publish a list appearance")

        controller.beginAppearanceTransition(true, animated: true)
        check(appearances == 1, "A later pop must still wait for completion")
        controller.endAppearanceTransition()
        check(appearances == 2, "A completed pop must publish exactly once after a cancellation")
        print("Native navigation appearance tests passed")
        exit(0)
    }

    private func check(_ condition: @autoclosure () -> Bool, _ message: String) {
        guard condition() else {
            fputs("FAIL: \(message)\n", stderr)
            exit(1)
        }
    }
}

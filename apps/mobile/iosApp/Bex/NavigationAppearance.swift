import SwiftUI
import UIKit

/// SwiftUI's onAppear can run before a navigation animation finishes. UIKit's didAppear
/// commits screen-dependent cleanup only after the transition reaches its destination.
struct NavigationAppearance: UIViewControllerRepresentable {
    let didAppear: () -> Void

    func makeUIViewController(context _: Context) -> Controller {
        let controller = Controller()
        controller.didAppear = didAppear
        return controller
    }

    func updateUIViewController(_ controller: Controller, context _: Context) {
        controller.didAppear = didAppear
    }

    final class Controller: UIViewController {
        var didAppear: (() -> Void)?

        override func loadView() {
            view = UIView()
            view.isUserInteractionEnabled = false
        }

        override func viewDidAppear(_ animated: Bool) {
            super.viewDidAppear(animated)
            didAppear?()
        }
    }
}

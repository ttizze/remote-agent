#if os(iOS)
    import SwiftUI
    import UIKit
    /// NSH owns the outer UIView; the native hosting controller joins its parent controller.
    @MainActor final class NativeHostingView<Content: View>: UIView {
        let controller: UIHostingController<Content>

        init(rootView: Content) {
            controller = UIHostingController(rootView: rootView)
            super.init(frame: .zero)
            let content = controller.view!
            content.backgroundColor = .clear
            content.translatesAutoresizingMaskIntoConstraints = false
            addSubview(content)
            NSLayoutConstraint.activate([
                content.leadingAnchor.constraint(equalTo: leadingAnchor),
                content.trailingAnchor.constraint(equalTo: trailingAnchor),
                content.topAnchor.constraint(equalTo: topAnchor),
                content.bottomAnchor.constraint(equalTo: bottomAnchor)
            ])
        }

        @available(*, unavailable)
        required init?(coder _: NSCoder) {
            fatalError("init(coder:) is unavailable")
        }

        override func didMoveToWindow() {
            super.didMoveToWindow()
            guard window != nil else { detach(); return }
            guard controller.parent == nil else { return }
            var responder: UIResponder? = next
            while let current = responder {
                if let parent = current as? UIViewController {
                    parent.addChild(controller)
                    controller.didMove(toParent: parent)
                    return
                }
                responder = current.next
            }
        }

        func detach() {
            if controller.parent != nil {
                controller.willMove(toParent: nil)
                controller.removeFromParent()
            }
        }
    }
#endif

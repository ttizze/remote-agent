public enum BrowserPointerAction {
    case click(x: Double, y: Double)
    case scroll(x: Double, y: Double, deltaX: Double, deltaY: Double)
}

#if os(iOS)
    import SwiftUI
    import UIKit

    public struct RemoteBrowserCanvas: UIViewRepresentable {
        let image: UIImage
        let acceptsInput: Bool
        let send: (BrowserPointerAction) -> Void

        public init(image: UIImage, acceptsInput: Bool, send: @escaping (BrowserPointerAction) -> Void) {
            self.image = image
            self.acceptsInput = acceptsInput
            self.send = send
        }

        public func makeUIView(context _: Context) -> BrowserCanvasView {
            BrowserCanvasView()
        }

        public func updateUIView(_ view: BrowserCanvasView, context _: Context) {
            view.display(image)
            view.send = acceptsInput ? send : nil
        }
    }

    public final class BrowserCanvasView: UIScrollView, UIScrollViewDelegate {
        let page = UIImageView()
        var send: ((BrowserPointerAction) -> Void)? {
            didSet { pagePan.isEnabled = send != nil }
        }

        private lazy var pagePan = UIPanGestureRecognizer(target: self, action: #selector(dragged))
        private var previousSize = CGSize.zero

        public init() {
            super.init(frame: .zero)
            delegate = self
            contentSize = page.bounds.size
            addSubview(page)
            panGestureRecognizer.minimumNumberOfTouches = 2
            addGestureRecognizer(UITapGestureRecognizer(target: self, action: #selector(tapped)))
            pagePan.maximumNumberOfTouches = 1
            addGestureRecognizer(pagePan)
            backgroundColor = .secondarySystemBackground
            accessibilityLabel = "ホストのBexブラウザ。ピンチで拡大、2本指で表示範囲を移動できます。"
        }

        @available(*, unavailable)
        required init?(coder _: NSCoder) {
            fatalError("init(coder:) has not been implemented")
        }

        func display(_ image: UIImage) {
            let changedSize = page.image?.size != image.size
            page.image = image
            guard changedSize else { return }
            minimumZoomScale = 1
            setZoomScale(1, animated: false)
            page.frame = CGRect(origin: .zero, size: image.size)
            contentSize = image.size
            previousSize = .zero
            setNeedsLayout()
        }

        override public func layoutSubviews() {
            super.layoutSubviews()
            guard bounds.size != previousSize, bounds.width > 0, page.bounds.width > 0 else { return }
            previousSize = bounds.size
            minimumZoomScale = bounds.width / page.bounds.width
            maximumZoomScale = max(2, minimumZoomScale * 4)
            setZoomScale(minimumZoomScale, animated: false)
        }

        override public func gestureRecognizerShouldBegin(_ gesture: UIGestureRecognizer) -> Bool {
            if gesture === pagePan,
               pagePan.location(in: self).x - pagePan.translation(in: self).x < bounds.minX + 32 {
                return false
            }
            return super.gestureRecognizerShouldBegin(gesture)
        }

        public func viewForZooming(in _: UIScrollView) -> UIView? {
            page
        }

        @objc private func tapped(_ gesture: UITapGestureRecognizer) {
            let point = gesture.location(in: page)
            guard page.bounds.contains(point) else { return }
            send?(.click(x: point.x, y: point.y))
        }

        @objc private func dragged(_ gesture: UIPanGestureRecognizer) {
            guard gesture.state == .ended else { return }
            let point = gesture.location(in: page)
            let delta = gesture.translation(in: page)
            send?(.scroll(x: max(0, min(page.bounds.maxX - 1, point.x)), y: max(0, min(page.bounds.maxY - 1, point.y)),
                          deltaX: max(-4096, min(4096, -delta.x)), deltaY: max(-4096, min(4096, -delta.y))))
        }
    }
#endif

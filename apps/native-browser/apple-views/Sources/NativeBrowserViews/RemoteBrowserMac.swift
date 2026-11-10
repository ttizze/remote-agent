#if os(macOS)
    import AppKit

    public final class BrowserCanvasView: NSScrollView {
        private let page = NSImageView()
        public var send: ((BrowserPointerAction) -> Void)?
        private var previousSize = CGSize.zero

        public init() {
            super.init(frame: .zero)
            documentView = page
            allowsMagnification = true
            hasVerticalScroller = true
            hasHorizontalScroller = true
            autohidesScrollers = true
            drawsBackground = false
            page.addGestureRecognizer(NSClickGestureRecognizer(target: self, action: #selector(tapped)))
            setAccessibilityLabel("ホストのBexブラウザ")
        }

        @available(*, unavailable)
        required init?(coder _: NSCoder) {
            fatalError("init(coder:) has not been implemented")
        }

        public func display(_ image: NSImage) {
            let changed = page.image?.size != image.size
            page.image = image
            if changed {
                page.frame = CGRect(origin: .zero, size: image.size)
                previousSize = .zero
                needsLayout = true
            }
        }

        override public func layout() {
            super.layout()
            guard contentSize != previousSize, contentSize.width > 0, contentSize.height > 0,
                  page.bounds.width > 0, page.bounds.height > 0 else { return }
            previousSize = contentSize
            minMagnification = min(1, contentSize.width / page.bounds.width, contentSize.height / page.bounds.height)
            maxMagnification = max(2, minMagnification * 4)
            magnify(toFit: page.bounds)
        }

        override public func scrollWheel(with event: NSEvent) {
            guard let send else { super.scrollWheel(with: event); return }
            let point = page.convert(event.locationInWindow, from: nil)
            guard page.bounds.contains(point) else { return }
            let scale = event.hasPreciseScrollingDeltas ? 1.0 : 20.0
            send(.scroll(x: point.x, y: page.bounds.height - point.y,
                         deltaX: max(-4096, min(4096, -event.scrollingDeltaX * scale)),
                         deltaY: max(-4096, min(4096, -event.scrollingDeltaY * scale))))
        }

        @objc private func tapped(_ gesture: NSClickGestureRecognizer) {
            let point = gesture.location(in: page)
            guard page.bounds.contains(point) else { return }
            send?(.click(x: point.x, y: page.bounds.height - point.y))
        }
    }
#endif

import AppKit
import NativeUI
import SwiftUI

/// AppKit keeps text selection, menus and accessibility. Its native SwiftUI
/// overlay uses the same shimmer component as the iPhone app.
@MainActor final class NativeTextShimmer {
    private final class Host: NSHostingView<AnyView> {
        override func hitTest(_: NSPoint) -> NSView? {
            nil
        }
    }

    let view: NSView
    private let host = Host(rootView: AnyView(EmptyView()))
    private var spec: MessageShimmer?
    private var text: NSAttributedString?
    private var size = NSSize.zero
    private var extent: CGFloat = 0

    init() {
        view = host
        host.setAccessibilityElement(false); host.setAccessibilityHidden(true)
    }

    func update(_ spec: MessageShimmer?, text: NSAttributedString, size: NSSize, extent: CGFloat) {
        view.isHidden = spec == nil
        guard self.spec != spec || self.text != text || self.size != size || self.extent != extent else { return }
        self.spec = spec; self.text = text; self.size = size; self.extent = extent
        guard let spec else { host.rootView = AnyView(EmptyView()); return }
        let font = text.length > 0 ? text.attribute(.font, at: 0, effectiveRange: nil) as? NSFont : nil
        host.rootView = AnyView(NativeShimmer(
            label: Text(verbatim: text.string).font(.system(size: font?.pointSize ?? 14)),
            active: true, color: .clear, period: spec.period, band: spec.band, focus: spec.focus, stops: spec.stops
        ).frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading))
        host.frame = NSRect(x: 0, y: 0, width: min(extent, size.width), height: size.height)
    }
}

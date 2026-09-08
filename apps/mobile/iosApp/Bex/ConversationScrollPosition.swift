import SwiftUI
import UIKit

struct HistoryBoundaryPreferenceKey: PreferenceKey {
    static var defaultValue: [String: CGFloat] = [:]
    static func reduce(value: inout [String: CGFloat], nextValue: () -> [String: CGFloat]) {
        value.merge(nextValue(), uniquingKeysWith: { _, next in next })
    }
}

/// Native offsets remain valid while List applies a new row snapshot. Queued
/// ScrollViewReader index-path requests can race that update and abort UIKit.
final class ConversationScrollPosition: NSObject, ObservableObject {
    private weak var scrollView: UIScrollView?
    private var observations = [NSKeyValueObservation]()
    private var followsLatest = true
    private var scrollScheduled = false
    private var reportedLatest: Bool?
    var isAttached: Bool {
        scrollView != nil
    }

    var onFollowingLatest: ((Bool) -> Void)?
    var onDirection: ((Bool) -> Void)?
    var historyAccessibilityValue = "" {
        didSet { scrollView?.accessibilityValue = historyAccessibilityValue }
    }

    func attach(_ scroll: UIScrollView) {
        guard scrollView !== scroll else { return }
        detach()
        scrollView = scroll
        scroll.accessibilityIdentifier = "task.detail"
        scroll.accessibilityValue = historyAccessibilityValue
        scroll.panGestureRecognizer.addTarget(self, action: #selector(panned(_:)))
        observations = [
            scroll.observe(\.contentSize) { [weak self] _, _ in self?.scheduleLatest() },
            scroll.observe(\.contentOffset) { [weak self] scroll, _ in
                guard let self else { return }
                let nearBottom = isNearBottom(scroll)
                if scroll.isDecelerating {
                    followsLatest = nearBottom && scroll.panGestureRecognizer.translation(in: scroll).y <= 0
                }
                reportLatest(nearBottom)
            },
            scroll.observe(\.bounds, options: [.old, .new]) { [weak self] _, change in
                if change.oldValue?.size != change.newValue?.size {
                    self?.scheduleLatest()
                }
            }
        ]
        scheduleLatest()
    }

    func detach() {
        observations.removeAll()
        scrollView?.panGestureRecognizer.removeTarget(self, action: #selector(panned(_:)))
        scrollView = nil
    }

    func scrollToLatest(animated: Bool) {
        followsLatest = true
        applyLatest(animated: animated)
    }

    func stopFollowingLatest() {
        followsLatest = false
    }

    private func scheduleLatest() {
        if !followsLatest, let scroll = scrollView {
            reportLatest(isNearBottom(scroll))
        }
        guard followsLatest, !scrollScheduled else { return }
        scrollScheduled = true
        DispatchQueue.main.async { [weak self] in
            guard let self else { return }
            scrollScheduled = false
            if followsLatest {
                applyLatest(animated: false)
            }
        }
    }

    private func bottomOffset(_ scroll: UIScrollView) -> CGFloat {
        max(-scroll.adjustedContentInset.top,
            scroll.contentSize.height - scroll.bounds.height + scroll.adjustedContentInset.bottom)
    }

    private func isNearBottom(_ scroll: UIScrollView) -> Bool {
        bottomOffset(scroll) - scroll.contentOffset.y <= 80
    }

    private func applyLatest(animated: Bool) {
        guard let scroll = scrollView else { return }
        let bottom = bottomOffset(scroll)
        if abs(scroll.contentOffset.y - bottom) > 0.5 {
            scroll.setContentOffset(CGPoint(x: scroll.contentOffset.x, y: bottom), animated: animated)
        }
        reportLatest(isNearBottom(scroll))
    }

    private func reportLatest(_ latest: Bool) {
        guard reportedLatest != latest else { return }
        reportedLatest = latest
        // KVO can fire during List layout; publish UI state after that update.
        DispatchQueue.main.async { [weak self] in self?.onFollowingLatest?(latest) }
    }

    @objc private func panned(_ gesture: UIPanGestureRecognizer) {
        switch gesture.state {
        case .began, .changed:
            followsLatest = false
            onDirection?(gesture.translation(in: scrollView).y > 0)
        case .ended:
            if let scroll = scrollView {
                followsLatest = isNearBottom(scroll) && gesture.translation(in: scroll).y <= 0
            }
        case .cancelled, .failed: onDirection?(false)
        default: break
        }
    }

    deinit { scrollView?.panGestureRecognizer.removeTarget(self, action: #selector(panned(_:))) }
}

/// Locate List's native scrolling view without taking over its delegate or
/// installing a competing drag recognizer.
struct ConversationScrollViewObserver: UIViewRepresentable {
    let position: ConversationScrollPosition
    let accessibilityValue: String
    let onFollowingLatest: (Bool) -> Void
    let onDirection: (Bool) -> Void

    func makeUIView(context _: Context) -> ProbeView {
        let view = ProbeView()
        view.isUserInteractionEnabled = false
        return view
    }

    func updateUIView(_ view: ProbeView, context _: Context) {
        position.onFollowingLatest = onFollowingLatest
        position.onDirection = onDirection
        position.historyAccessibilityValue = accessibilityValue
        view.position = position
        view.attachWhenMounted()
    }

    static func dismantleUIView(_ view: ProbeView, coordinator _: ()) {
        view.position?.detach()
        view.position?.onFollowingLatest = nil
        view.position?.onDirection = nil
    }

    final class ProbeView: UIView {
        weak var position: ConversationScrollPosition?
        private var attachmentScheduled = false

        override func didMoveToWindow() {
            super.didMoveToWindow()
            if window == nil {
                position?.detach()
            } else {
                attachWhenMounted()
            }
        }

        func attachWhenMounted() {
            guard window != nil, position?.isAttached == false, !attachmentScheduled else { return }
            attachmentScheduled = true
            DispatchQueue.main.async { [weak self] in
                guard let self else { return }
                attachmentScheduled = false
                var ancestor = superview
                while let view = ancestor {
                    if let scroll = findScrollView(view) {
                        position?.attach(scroll)
                        return
                    }
                    ancestor = view.superview
                }
            }
        }

        private func findScrollView(_ view: UIView) -> UIScrollView? {
            if let scroll = view as? UIScrollView {
                return scroll
            }
            for child in view.subviews where child !== self {
                if let scroll = findScrollView(child) {
                    return scroll
                }
            }
            return nil
        }
    }
}

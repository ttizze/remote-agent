import OSLog
import UIKit

/// Observe native navigation and calculate the reading position after a history insertion.
final class ConversationScrollObserver: NSObject, UIScrollViewDelegate {
    private static let logger = Logger(subsystem: "dev.remoteagent.mobile.ios", category: "HistoryScroll")
    private struct HistoryPage: Equatable {
        let turnCount: UInt64
        let hasMore: Bool
    }

    private struct ReadingPosition {
        let id: UUID
        var offset: CGPoint
        let height: CGFloat
        let page: HistoryPage
        var loadedPage: HistoryPage?
    }

    var onScrollToTop: () -> Void = {}
    private weak var scrollView: UIScrollView?
    private weak var original: UIScrollViewDelegate?
    private var historyAnchor: ReadingPosition?
    private var renderedPage: (page: HistoryPage, height: CGFloat)?

    func captureReadingPosition(turnCount: UInt64, hasMoreHistory: Bool) -> UUID? {
        Self.logger.notice("capture attached=\(self.scrollView != nil) turns=\(turnCount) more=\(hasMoreHistory)")
        guard let scrollView else { return nil }
        Self.logger
            .notice("capture y=\(Double(scrollView.contentOffset.y)) height=\(Double(scrollView.contentSize.height))")
        let renderedHeight = renderedPage?.height ?? -1
        Self.logger
            .notice("capture rendered-height=\(Double(renderedHeight)) viewport=\(Double(scrollView.bounds.height))")
        Self.logger.notice("capture inset-top=\(Double(scrollView.adjustedContentInset.top))")
        Self.logger.notice("capture inset-bottom=\(Double(scrollView.adjustedContentInset.bottom))")
        let id = UUID()
        historyAnchor = ReadingPosition(id: id, offset: scrollView.contentOffset,
                                        height: scrollView.contentSize.height,
                                        page: HistoryPage(turnCount: turnCount, hasMore: hasMoreHistory))
        return id
    }

    func cancelHistoryRestoration() {
        if historyAnchor != nil {
            Self.logger.notice("cancel pending restoration")
        }
        historyAnchor = nil
    }

    func historyLoaded(_ id: UUID?, succeeded: Bool, turnCount: UInt64, hasMoreHistory: Bool) -> CGPoint? {
        let matched = id != nil && id == historyAnchor?.id
        Self.logger.notice("loaded matched=\(matched) succeeded=\(succeeded)")
        Self.logger.notice("loaded turns=\(turnCount) more=\(hasMoreHistory)")
        guard let id, var anchor = historyAnchor, anchor.id == id else { return nil }
        let page = HistoryPage(turnCount: turnCount, hasMore: hasMoreHistory)
        guard succeeded, page != anchor.page else {
            cancelHistoryRestoration()
            return nil
        }
        anchor.loadedPage = page
        historyAnchor = anchor
        return restoredReadingPosition()
    }

    func historyDidLayout(turnCount: UInt64, hasMoreHistory: Bool, contentHeight: CGFloat) -> CGPoint? {
        if historyAnchor != nil {
            Self.logger.notice("layout turns=\(turnCount) more=\(hasMoreHistory)")
        }
        renderedPage = (HistoryPage(turnCount: turnCount, hasMore: hasMoreHistory), contentHeight)
        return restoredReadingPosition()
    }

    private func restoredReadingPosition() -> CGPoint? {
        // Snapshot controls can update before the detached row projection.
        // Only the geometry of the completed page belongs to this insertion.
        guard let anchor = historyAnchor, let page = anchor.loadedPage,
              let renderedPage, page == renderedPage.page else { return nil }
        historyAnchor = nil
        // SwiftUI publishes geometry before the native content size necessarily catches up.
        // The screen applies this point through its existing ScrollPosition binding.
        let offset = CGPoint(x: anchor.offset.x, y: anchor.offset.y + renderedPage.height - anchor.height)
        Self.logger.notice("restore target=\(Double(offset.y)) rendered-height=\(Double(renderedPage.height))")
        if let scrollView {
            Self.logger
                .notice(
                    "native y=\(Double(scrollView.contentOffset.y)) height=\(Double(scrollView.contentSize.height))"
                )
            DispatchQueue.main.async { [weak scrollView] in
                guard let scrollView else { return }
                Self.logger.notice("next-turn y=\(Double(scrollView.contentOffset.y))")
            }
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.5) { [weak scrollView] in
                guard let scrollView else { return }
                Self.logger
                    .notice(
                        "settled y=\(Double(scrollView.contentOffset.y)) height=\(Double(scrollView.contentSize.height))"
                    )
                Self.logger.notice("settled inset-top=\(Double(scrollView.adjustedContentInset.top))")
            }
        }
        return offset
    }

    func attach(to scroll: UIScrollView) {
        if scrollView !== scroll {
            Self.logger.notice("attach \(NSStringFromClass(type(of: scroll)), privacy: .public)")
            detach()
            scrollView = scroll
        }
        if scroll.delegate !== self {
            original = scroll.delegate
            scroll.delegate = self
        }
    }

    func detach() {
        if scrollView != nil {
            Self.logger.notice("detach native scroll view")
        }
        cancelHistoryRestoration()
        renderedPage = nil
        if scrollView?.delegate === self {
            scrollView?.delegate = original
        }
        scrollView = nil
        original = nil
    }

    override func responds(to selector: Selector!) -> Bool {
        super.responds(to: selector) || original?.responds(to: selector) == true
    }

    override func forwardingTarget(for _: Selector!) -> Any? {
        original
    }

    func scrollViewDidScroll(_ scrollView: UIScrollView) {
        if var anchor = historyAnchor, scrollView.contentSize.height == anchor.height {
            anchor.offset = scrollView.contentOffset
            historyAnchor = anchor
        }
        original?.scrollViewDidScroll?(scrollView)
    }

    func scrollViewShouldScrollToTop(_ scrollView: UIScrollView) -> Bool {
        let shouldScroll = original?.scrollViewShouldScrollToTop?(scrollView) ?? true
        if shouldScroll {
            cancelHistoryRestoration()
            onScrollToTop()
        }
        return shouldScroll
    }
}

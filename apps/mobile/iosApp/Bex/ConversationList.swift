#if !BEX_NSH_CONVERSATION
    import Observation
    import SwiftUI
    import UIKit

    /// Following keeps the latest message in view; status-bar taps reach the start natively.
    @MainActor @Observable final class ConversationScrollState {
        var following = true
        var latestVisible = true
    }

    struct ConversationList: UIViewControllerRepresentable {
        enum RowID: Hashable {
            case history
            case message(String)
        }

        let ids: [RowID]
        let scroll: ConversationScrollState
        let following: Bool
        let accessibilityValue: String
        let viewportHeight: CGFloat
        let row: (Int) -> AnyView

        func makeUIViewController(context _: Context) -> ConversationListController {
            ConversationListController(scroll: scroll)
        }

        func updateUIViewController(_ controller: ConversationListController, context _: Context) {
            controller.viewportHeight = viewportHeight
            controller.update(ids: ids, following: following, accessibilityValue: accessibilityValue, row: row)
        }

        static func dismantleUIViewController(_ controller: ConversationListController, coordinator _: ()) {
            controller.stop()
        }
    }

    /// Observe the collection's actual layout, including inset changes that do not resize its controller.
    private final class ConversationCollectionView: UICollectionView {
        var didLayout: (() -> Void)?

        /// Content remains visible beneath the composer, but its controls own those touches.
        override func point(inside point: CGPoint, with event: UIEvent?) -> Bool {
            super.point(inside: point, with: event) && bounds.inset(by: adjustedContentInset).contains(point)
        }

        override func layoutSubviews() {
            super.layoutSubviews()
            didLayout?()
        }
    }

    final class ConversationListController: UIViewController {
        private struct Source {
            let ids: [ConversationList.RowID]
            let row: (Int) -> AnyView
        }

        private struct Anchor {
            let id: ConversationList.RowID
            /// Relative to the usable viewport, including UIKit's automatic safe-area adjustment.
            let displacement: CGFloat
        }

        private struct Geometry: Equatable {
            let size: CGSize
            let insets: UIEdgeInsets
            let contentSize: CGSize
            let anchorFrame: CGRect?
        }

        private let scroll: ConversationScrollState
        private let layout: ConversationListLayout
        let collectionView: UICollectionView
        var viewportHeight: CGFloat?
        private var source = Source(ids: [], row: { _ in AnyView(EmptyView()) })
        private var indices: [ConversationList.RowID: Int] = [:]
        private var pending: Source?
        private var applying = false
        private var active = true
        private var following = true
        private var anchor: Anchor?
        private var lastLayout: (geometry: Geometry, offset: CGPoint)?
        private var scrollingToTop = false
        private var visibilityUpdate: Task<Void, Never>?
        private var data: UICollectionViewDiffableDataSource<Int, ConversationList.RowID>!

        init(scroll: ConversationScrollState) {
            self.scroll = scroll
            let layout = ConversationListLayout.make()
            self.layout = layout
            let collection = ConversationCollectionView(frame: .zero, collectionViewLayout: layout)
            collectionView = collection
            super.init(nibName: nil, bundle: nil)
            collection.didLayout = { [weak self] in self?.didLayoutCollectionView() }
        }

        @available(*, unavailable)
        required init?(coder _: NSCoder) {
            fatalError("init(coder:) has not been implemented")
        }

        override func loadView() {
            view = collectionView
        }

        override func viewDidLoad() {
            super.viewDidLoad()
            collectionView.delegate = self
            collectionView.backgroundColor = .clear
            collectionView.alwaysBounceVertical = true
            collectionView.keyboardDismissMode = .interactive
            collectionView.addGestureRecognizer(KeyboardDismissingTap())
            collectionView.isPrefetchingEnabled = false
            collectionView.accessibilityIdentifier = "task.detail"
            let row = UICollectionView
                .CellRegistration<UICollectionViewCell, ConversationList.RowID> { [weak self] cell, _, id in
                    self?.show(cell, id: id)
                }
            data = UICollectionViewDiffableDataSource(collectionView: collectionView) { view, path, id in
                view.dequeueConfiguredReusableCell(using: row, for: path, item: id)
            }
        }

        func update(ids: [ConversationList.RowID], following: Bool, accessibilityValue: String,
                    row: @escaping (Int) -> AnyView) {
            loadViewIfNeeded()
            self.following = following
            if following {
                // Returning to the latest message also ends an unfinished status-bar scroll.
                anchor = nil
                scrollingToTop = false
            }
            collectionView.accessibilityValue = accessibilityValue
            pending = Source(ids: ids, row: row)
            applyPending()
        }

        private func captureAnchor(allowed: [ConversationList.RowID: Int]) {
            anchor = collectionView.indexPathsForVisibleItems.sorted()
                .compactMap { path -> Anchor? in
                    guard let id = data.itemIdentifier(for: path), case .message = id,
                          allowed[id] != nil,
                          let attributes = layout.layoutAttributesForItem(at: path) else { return nil }
                    return Anchor(id: id, displacement: attributes.frame.minY - collectionView.contentOffset.y
                        - collectionView.adjustedContentInset.top)
                }.first
        }

        private func applyPending() {
            guard !applying, let pending, collectionView.bounds.width > 32,
                  collectionView.bounds.height > 0 else { return }
            self.pending = nil
            let changed = source.ids != pending.ids
            let nextIndices = changed
                ? Dictionary(uniqueKeysWithValues: pending.ids.enumerated().map { ($0.element, $0.offset) }) : indices
            if !following,
               anchor.flatMap({ nextIndices[$0.id] }) == nil {
                captureAnchor(allowed: nextIndices)
            }
            source = pending
            if changed {
                indices = nextIndices
                applying = true
                var snapshot = NSDiffableDataSourceSnapshot<Int, ConversationList.RowID>()
                snapshot.appendSections([0])
                snapshot.appendItems(source.ids)
                data.apply(snapshot, animatingDifferences: false) { [weak self] in
                    guard let self, active else { return }
                    applying = false
                    refreshVisibleRows()
                    applyPending()
                }
            } else {
                refreshVisibleRows()
            }
        }

        private func refreshVisibleRows() {
            guard active else { return }
            // Self-sizing cell updates animate by default. During streaming their
            // animated frames lag behind the measured height used to follow the tail.
            UIView.performWithoutAnimation {
                for path in collectionView.indexPathsForVisibleItems {
                    if let cell = collectionView.cellForItem(at: path), let id = data.itemIdentifier(for: path) {
                        show(cell, id: id)
                    }
                }
                collectionView.setNeedsLayout()
                collectionView.layoutIfNeeded()
            }
        }

        /// UIKit sizes the cell from the SwiftUI content itself, so rows need no separate measurement.
        private func show(_ cell: UICollectionViewCell, id: ConversationList.RowID) {
            guard let index = indices[id] else { cell.contentConfiguration = nil; return }
            let row = source.row(index)
            cell.contentConfiguration = UIHostingConfiguration { row.id(id) }.margins(.all, 0)
        }

        private func didLayoutCollectionView() {
            guard active else { return }
            updateViewportInsets()
            applyPending()
            guard !applying else { return }
            positionViewport()
            publishVisibility()
        }

        /// SwiftUI lays out the composer; only the collection extends beneath its safe-area inset.
        /// Content insets keep the composer out of the safe area this calculation reads.
        private func updateViewportInsets() {
            guard let viewportHeight else { return }
            let bottom = max(0, collectionView.bounds.height - viewportHeight - collectionView.safeAreaInsets.bottom)
            if collectionView.contentInset.bottom != bottom {
                collectionView.contentInset.bottom = bottom
                collectionView.verticalScrollIndicatorInsets.bottom = bottom
            }
        }

        private var geometry: Geometry {
            Geometry(size: collectionView.bounds.size, insets: collectionView.adjustedContentInset,
                     contentSize: layout.collectionViewContentSize,
                     anchorFrame: anchor.flatMap { data.indexPath(for: $0.id) }
                         .flatMap { layout.layoutAttributesForItem(at: $0)?.frame })
        }

        /// Intrinsic sizing can move UIKit's offset without invoking its target-offset hook.
        /// A changed offset with unchanged geometry instead belongs to native scrolling (including accessibility).
        private func positionViewport() {
            guard !source.ids.isEmpty else { return }
            let moving = collectionView.isTracking || collectionView.isDragging || collectionView.isDecelerating
            if !following, !scrollingToTop,
               anchor == nil || moving || (lastLayout?.geometry == geometry
                   && lastLayout?.offset != collectionView.contentOffset) {
                captureAnchor(allowed: indices)
            }
            defer { lastLayout = (geometry, collectionView.contentOffset) }
            guard !moving, !scrollingToTop else { return }
            let insets = collectionView.adjustedContentInset
            let bottom = layout.bottomOffset(viewportHeight: collectionView.bounds.height, insets: insets)
            let offset: CGFloat
            if following {
                offset = bottom
            } else if let anchor, let frame = geometry.anchorFrame {
                offset = max(-insets.top, min(bottom, frame.minY - anchor.displacement - insets.top))
            } else {
                return
            }
            let target = CGPoint(x: -insets.left, y: offset)
            if collectionView.contentOffset != target {
                collectionView.setContentOffset(target, animated: false)
            }
        }

        private func latestIsVisible() -> Bool {
            guard !source.ids.isEmpty,
                  let last = layout.layoutAttributesForItem(at: IndexPath(item: source.ids.count - 1, section: 0))
            else { return true }
            let insets = collectionView.adjustedContentInset
            let viewport = collectionView.bounds.inset(by: insets)
            let marker = CGRect(x: last.frame.minX, y: last.frame.maxY - 80, width: last.frame.width, height: 80)
            return viewport.intersection(marker).height >= 0.8
        }

        private func publishVisibility() {
            guard active, visibilityUpdate == nil else { return }
            // Layout can run during a SwiftUI update; coalesce the observation write outside that update.
            visibilityUpdate = Task { @MainActor [weak self] in
                await Task.yield()
                guard !Task.isCancelled, let self else { return }
                visibilityUpdate = nil
                let visible = latestIsVisible()
                if scroll.latestVisible != visible {
                    scroll.latestVisible = visible
                }
            }
        }

        func stop() {
            active = false
            pending = nil
            source = Source(ids: [], row: { _ in AnyView(EmptyView()) })
            indices.removeAll()
            visibilityUpdate?.cancel()
            visibilityUpdate = nil
            for cell in collectionView.visibleCells {
                cell.contentConfiguration = nil
            }
        }
    }

    extension ConversationListController: UICollectionViewDelegate {
        /// UIKit can display a released cell again without dequeuing it.
        func collectionView(
            _: UICollectionView,
            willDisplay cell: UICollectionViewCell,
            forItemAt indexPath: IndexPath
        ) {
            if cell.contentConfiguration == nil, let id = data.itemIdentifier(for: indexPath) {
                show(cell, id: id)
            }
        }

        /// A cell that leaves the list keeps its hosted row and still receives touches and accessibility
        /// queries for that position, so release the row once UIKit stops displaying it.
        func collectionView(_: UICollectionView, didEndDisplaying cell: UICollectionViewCell, forItemAt _: IndexPath) {
            cell.contentConfiguration = nil
        }

        func scrollViewDidScroll(_ scrollView: UIScrollView) {
            if !applying, !following, !scrollingToTop,
               scrollView.isTracking || scrollView.isDragging || scrollView.isDecelerating {
                captureAnchor(allowed: indices)
            }
            publishVisibility()
        }

        func scrollViewDidChangeAdjustedContentInset(_: UIScrollView) {
            layout.invalidateLayout()
        }

        func scrollViewDidEndDragging(_ scrollView: UIScrollView, willDecelerate decelerate: Bool) {
            if !decelerate {
                scrollViewDidEndDecelerating(scrollView)
            }
        }

        func scrollViewDidEndDecelerating(_: UIScrollView) {
            if !following {
                captureAnchor(allowed: indices)
            }
        }

        func scrollViewDidEndScrollingAnimation(_: UIScrollView) {
            if !following {
                captureAnchor(allowed: indices)
            }
        }

        func scrollViewDidScrollToTop(_: UIScrollView) {
            scrollingToTop = false
            if !following {
                captureAnchor(allowed: indices)
            }
        }

        func scrollViewWillBeginDragging(_: UIScrollView) {
            scroll.following = false
            following = false
            anchor = nil
            scrollingToTop = false
        }

        func scrollViewWillEndDragging(_ scrollView: UIScrollView, withVelocity _: CGPoint,
                                       targetContentOffset: UnsafeMutablePointer<CGPoint>) {
            if scrollView.panGestureRecognizer.translation(in: scrollView).y < 0, latestIsVisible() {
                scroll.following = true
                following = true
                anchor = nil
                targetContentOffset.pointee.y = layout.bottomOffset(viewportHeight: scrollView.bounds.height,
                                                                    insets: scrollView.adjustedContentInset)
            }
        }

        func scrollViewShouldScrollToTop(_: UIScrollView) -> Bool {
            scroll.following = false
            following = false
            anchor = nil
            scrollingToTop = true
            return true
        }
    }

#endif

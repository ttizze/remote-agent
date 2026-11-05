import Observation
import SwiftUI
import UIKit

@MainActor @Observable final class ConversationScrollState {
    struct Request: Equatable {
        let id = UUID()
        let edge: VerticalEdge
    }

    var following = true
    var latestVisible = true
    private(set) var request: Request?

    func goTo(_ edge: VerticalEdge) {
        following = edge == .bottom
        request = Request(edge: edge)
    }
}

struct ConversationList: UIViewControllerRepresentable {
    enum RowID: Hashable {
        case history
        case message(String)
    }

    let ids: [RowID]
    let scroll: ConversationScrollState
    let following: Bool
    let request: ConversationScrollState.Request?
    let accessibilityValue: String
    let row: (Int) -> AnyView

    func makeUIViewController(context _: Context) -> ConversationListController {
        ConversationListController(scroll: scroll)
    }

    func updateUIViewController(_ controller: ConversationListController, context _: Context) {
        controller.update(ids: ids, following: following, request: request,
                          accessibilityValue: accessibilityValue, row: row)
    }

    static func dismantleUIViewController(_ controller: ConversationListController, coordinator _: ()) {
        controller.stop()
    }
}

/// Observe the collection's actual layout, including inset changes that do not resize its controller.
private final class ConversationCollectionView: UICollectionView {
    var didLayout: (() -> Void)?

    override func layoutSubviews() {
        super.layoutSubviews()
        didLayout?()
    }
}

final class ConversationListController: UIViewController, UICollectionViewDelegate {
    private struct Source {
        let ids: [ConversationList.RowID]
        let row: (Int) -> AnyView
    }

    private let scroll: ConversationScrollState
    private let layout: ConversationListLayout
    let collectionView: UICollectionView
    private var source = Source(ids: [], row: { _ in AnyView(EmptyView()) })
    private var indices: [ConversationList.RowID: Int] = [:]
    private var pending: Source?
    private var applying = false
    private var active = true
    private var following = true
    private var requestedEdge: VerticalEdge?
    private var request: ConversationScrollState.Request?
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
        collectionView.isPrefetchingEnabled = false
        collectionView.selfSizingInvalidation = .enabledIncludingConstraints
        collectionView.accessibilityIdentifier = "task.detail"
        collectionView.register(ConversationListCell.self, forCellWithReuseIdentifier: "row")
        data = UICollectionViewDiffableDataSource(collectionView: collectionView) { [weak self] view, path, id in
            guard let cell = view.dequeueReusableCell(withReuseIdentifier: "row", for: path)
                as? ConversationListCell else { preconditionFailure("Unexpected conversation cell type") }
            self?.show(cell, id: id)
            return cell
        }
        layout.indexPath = { [weak self] id in self?.data.indexPath(for: id) }
    }

    func update(ids: [ConversationList.RowID], following: Bool, request: ConversationScrollState.Request?,
                accessibilityValue: String, row: @escaping (Int) -> AnyView) {
        loadViewIfNeeded()
        self.following = following
        if following {
            layout.anchor = nil
        }
        if self.request != request {
            self.request = request
            requestedEdge = request?.edge
            layout.anchor = nil
        }
        collectionView.accessibilityValue = accessibilityValue
        pending = Source(ids: ids, row: row)
        applyPending()
    }

    private func captureAnchor(allowed: [ConversationList.RowID: Int]) {
        layout.anchor = collectionView.indexPathsForVisibleItems.sorted()
            .compactMap { path -> ConversationListLayout.Anchor? in
                guard let id = data.itemIdentifier(for: path), case .message = id,
                      allowed[id] != nil,
                      let attributes = layout.layoutAttributesForItem(at: path) else { return nil }
                return ConversationListLayout.Anchor(id: id,
                                                     displacement: attributes.frame.minY - collectionView.contentOffset
                                                         .y - collectionView.adjustedContentInset.top)
            }.first
    }

    private func applyPending() {
        guard !applying, let pending, collectionView.bounds.width > 32, collectionView.bounds.height > 0 else { return }
        self.pending = nil
        let changed = source.ids != pending.ids
        let nextIndices = changed
            ? Dictionary(uniqueKeysWithValues: pending.ids.enumerated().map { ($0.element, $0.offset) }) : indices
        if !following, requestedEdge == nil,
           layout.anchor.flatMap({ nextIndices[$0.id] }) == nil {
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
        for path in collectionView.indexPathsForVisibleItems {
            if let cell = collectionView.cellForItem(at: path) as? ConversationListCell,
               let id = data.itemIdentifier(for: path) {
                show(cell, id: id)
            }
        }
        collectionView.layoutIfNeeded()
        positionViewport(restoringReadingPosition: true)
        publishVisibility()
    }

    private func show(_ cell: ConversationListCell, id: ConversationList.RowID) {
        guard let index = indices[id] else { cell.unmount(); return }
        cell.show(AnyView(source.row(index).id(id)), parent: self)
    }

    func collectionView(_: UICollectionView, willDisplay cell: UICollectionViewCell,
                        forItemAt indexPath: IndexPath) {
        if let cell = cell as? ConversationListCell, !cell.isMounted, let id = data.itemIdentifier(for: indexPath) {
            show(cell, id: id)
        }
    }

    func collectionView(_: UICollectionView, didEndDisplaying cell: UICollectionViewCell,
                        forItemAt _: IndexPath) {
        (cell as? ConversationListCell)?.unmount()
    }

    private func didLayoutCollectionView() {
        guard active else { return }
        // Edge requests move the viewport before UIKit updates its visible cells.
        // Capture only the cells from a completed layout before applying those requests.
        if !following, requestedEdge == nil, layout.anchor == nil {
            captureAnchor(allowed: indices)
        }
        applyPending()
        positionViewport(restoringReadingPosition: false)
        publishVisibility()
    }

    /// Row content can resize without a new snapshot. Refreshing measured cells
    /// restores a detached reader's anchor as well as positioning a following viewport.
    private func positionViewport(restoringReadingPosition: Bool) {
        guard !applying, !source.ids.isEmpty else { return }
        let target: CGPoint
        if let edge = requestedEdge ?? (following ? .bottom : nil) {
            requestedEdge = nil
            let insets = collectionView.adjustedContentInset
            let offset = edge == .bottom ? layout.bottomOffset(in: collectionView) : -insets.top
            target = CGPoint(x: -insets.left, y: offset)
        } else {
            guard restoringReadingPosition, layout.anchor != nil else { return }
            target = layout.targetContentOffset(forProposedContentOffset: collectionView.contentOffset)
        }
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
            (cell as? ConversationListCell)?.unmount()
        }
    }

    func scrollViewDidScroll(_ scrollView: UIScrollView) {
        if !applying, !following,
           scrollView.isTracking || scrollView.isDragging || scrollView.isDecelerating {
            captureAnchor(allowed: indices)
        }
        publishVisibility()
    }

    func scrollViewDidChangeAdjustedContentInset(_: UIScrollView) {
        if following {
            view.setNeedsLayout()
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
        if !following {
            captureAnchor(allowed: indices)
        }
    }

    func scrollViewWillBeginDragging(_: UIScrollView) {
        requestedEdge = nil
        scroll.following = false
        following = false
        layout.anchor = nil
    }

    func scrollViewWillEndDragging(_ scrollView: UIScrollView, withVelocity _: CGPoint,
                                   targetContentOffset: UnsafeMutablePointer<CGPoint>) {
        if scrollView.panGestureRecognizer.translation(in: scrollView).y < 0, latestIsVisible() {
            scroll.following = true
            following = true
            layout.anchor = nil
            targetContentOffset.pointee.y = layout.bottomOffset(in: scrollView)
        }
    }

    func scrollViewShouldScrollToTop(_: UIScrollView) -> Bool {
        requestedEdge = nil
        scroll.following = false
        following = false
        layout.anchor = nil
        return true
    }
}

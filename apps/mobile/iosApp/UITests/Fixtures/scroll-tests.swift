import SwiftUI
import UIKit

@main @MainActor
final class ConversationScrollTests: UIResponder, UIApplicationDelegate {
    var window: UIWindow?
    private var failures = 0

    func application(_: UIApplication,
                     didFinishLaunchingWithOptions _: [UIApplication.LaunchOptionsKey: Any]?) -> Bool {
        Task {
            await checkComposerUnderlay()
            await checkRecyclingAndAnchoring()
            await checkDetachedTail()
            await checkFollowingTallRows()
            await checkWrappedRows()
            await checkShortViewport()
            await checkShortHistory()
            await checkStagedHistory()
            guard failures == 0 else { exit(1) }
            print("Native conversation recycling and scroll tests passed")
            exit(0)
        }
        return true
    }

    private func check(_ condition: @autoclosure () -> Bool, _ message: String) {
        guard condition() else {
            fputs("FAIL: \(message)\n", stderr)
            failures += 1
            return
        }
    }

    private func geometry(_ view: UICollectionView) -> String {
        "offset=\(view.contentOffset), bounds=\(view.bounds.size), "
            + "inset=\(view.adjustedContentInset), content=\(view.contentSize)"
    }

    private func mount(_ controller: UIViewController) {
        let window = UIWindow(frame: CGRect(x: 0, y: 0, width: 390, height: 640))
        self.window = window
        window.rootViewController = controller
        window.makeKeyAndVisible()
    }

    private func layout(_ controller: UIViewController) async {
        // Complete the actual UIKit transaction rather than simulating content heights or retrying assertions.
        await withCheckedContinuation { continuation in
            CATransaction.begin()
            CATransaction.setCompletionBlock { continuation.resume() }
            controller.view.setNeedsLayout()
            controller.view.layoutIfNeeded()
            CATransaction.commit()
        }
        await Task.yield()
    }

    /// UIKit's status-bar navigation: the delegate approves, the view scrolls to its start, then reports it.
    private func tapStatusBar(_ controller: ConversationListController) async {
        let view = controller.collectionView
        guard controller.scrollViewShouldScrollToTop(view) else { return }
        view.setContentOffset(CGPoint(x: -view.adjustedContentInset.left, y: -view.adjustedContentInset.top),
                              animated: false)
        await layout(controller)
        controller.scrollViewDidScrollToTop(view)
    }

    private func position(_ item: Int, in controller: ConversationListController) -> CGFloat {
        let view = controller.collectionView
        guard let attributes = view.layoutAttributesForItem(at: IndexPath(item: item, section: 0)) else {
            preconditionFailure("Missing row geometry")
        }
        return attributes.frame.minY - view.contentOffset.y
    }
}

extension ConversationScrollTests {
    private func checkShortViewport() async {
        let controller = ConversationListController(scroll: ConversationScrollState())
        mount(controller)
        controller.update(ids: [.message("short")], following: true,
                          accessibilityValue: "") { _ in AnyView(Text("Short answer").frame(height: 60)) }
        await layout(controller)
        let view = controller.collectionView
        let path = IndexPath(item: 0, section: 0)
        func bottomGap() -> CGFloat {
            view.contentOffset.y + view.bounds.height - view.adjustedContentInset.bottom
                - view.layoutAttributesForItem(at: path)!.frame.maxY
        }
        view.bounds.size.height -= 250
        await layout(controller)
        check(abs(bottomGap() - 6) < 1,
              "A keyboard-sized height change must keep a short answer inside the viewport: \(geometry(view))")
        check(
            view.indexPathsForVisibleItems.contains(path),
            "A short answer must remain materialized above the keyboard"
        )
        view.contentInset.bottom += 100
        await layout(controller)
        check(abs(bottomGap() - 6) < 1, "Vertical insets must re-align a short conversation")
        view.bounds.size.height += 250
        await layout(controller)
        check(abs(bottomGap() - 6) < 1, "Restoring viewport height must restore the bottom alignment")
    }

    private func checkRecyclingAndAnchoring() async {
        let scroll = ConversationScrollState()
        let controller = ConversationListController(scroll: scroll)
        mount(controller)
        let ids = (0 ..< 10000).map { ConversationList.RowID.message(String($0)) }
        var built = 0
        func update(_ rows: [ConversationList.RowID]) {
            controller.update(ids: rows, following: scroll.following,
                              accessibilityValue: "items=\(rows.count)") { index in
                built += 1
                return AnyView(Text(String(describing: rows[index])).frame(maxWidth: .infinity).frame(height: 72))
            }
        }
        update(ids)
        await layout(controller)
        let view = controller.collectionView
        check(built < 120, "Ten thousand rows must materialize only the viewport, including initial positioning")
        check(view.indexPathsForVisibleItems.contains(IndexPath(item: 9999, section: 0)), "Open at the latest row")
        check(controller.children.count < 40, "Recycling must keep the hosting-controller hierarchy bounded")

        await tapStatusBar(controller)
        update(ids)
        await layout(controller)
        check(view.indexPathsForVisibleItems.contains(IndexPath(item: 0, section: 0)), "Top navigation must detach")
        let reading = position(0, in: controller)
        update(ids) // Failed, empty, and still-pending history requests do not mutate the list.
        await layout(controller)
        check(abs(position(0, in: controller) - reading) < 1, "An unchanged page must not move the reader")
        view.scrollToItem(at: IndexPath(item: 500, section: 0), at: .top, animated: false)
        await layout(controller)
        controller.scrollViewDidEndDecelerating(view)
        let anchor = position(500, in: controller)
        let older = (0 ..< 50).map { ConversationList.RowID.message("older-\($0)") }
        let prepended = older + ids
        update(prepended)
        await layout(controller)
        check(abs(position(550, in: controller) - anchor) < 1, "A prepend must preserve the same row and pixel")
        update(prepended + [.message("appended")])
        await layout(controller)
        check(
            abs(position(550, in: controller) - anchor) < 1,
            "Appending rows must preserve the detached reading point"
        )
        check(built < 300, "Prepending must not eagerly create the history")
        check(controller.children.count < 40, "Offscreen cells must detach their child controllers")
    }

    private func checkDetachedTail() async {
        let scroll = ConversationScrollState()
        let controller = ConversationListController(scroll: scroll)
        mount(controller)
        var ids = [ConversationList.RowID.message("first"), .message("last")]
        func update(_ height: CGFloat) {
            controller.update(ids: ids, following: scroll.following,
                              accessibilityValue: "") { index in
                AnyView(Text("Message").frame(height: index == 0 ? 72 : height))
            }
        }
        update(800)
        await layout(controller)
        let view = controller.collectionView
        controller.scrollViewWillBeginDragging(view)
        view.contentOffset.y -= 25
        await layout(controller)
        controller.scrollViewDidEndDecelerating(view)
        let reading = position(1, in: controller)
        check(!scroll.following && scroll.latestVisible, "A small older drag detaches within the latest marker")
        update(1000)
        await layout(controller)
        check(abs(view.layoutAttributesForItem(at: IndexPath(item: 1, section: 0))!.size.height - 1000) < 1,
              "Detached-tail growth must actually remeasure the visible cell")
        check(abs(position(1, in: controller) - reading) < 1,
              "Tail growth must preserve the detached reading point: "
                  + "before=\(reading), after=\(position(1, in: controller)), \(geometry(view))")
        ids.append(.message("appended"))
        update(1000)
        await layout(controller)
        check(
            abs(position(1, in: controller) - reading) < 1,
            "New rows must preserve a reader detached near the bottom"
        )
    }

    private func checkFollowingTallRows() async {
        let scroll = ConversationScrollState()
        let controller = ConversationListController(scroll: scroll)
        mount(controller)
        var ids = [ConversationList.RowID.message("first"), .message("last")]
        func update(_ height: CGFloat) {
            controller.update(ids: ids, following: scroll.following,
                              accessibilityValue: "") { index in
                AnyView(Text("Long message").frame(height: index == 0 ? 72 : height))
            }
        }
        update(1200)
        await layout(controller)
        let view = controller.collectionView
        let bottom = view.contentOffset.y + view.bounds.height - view.adjustedContentInset.bottom
        let last = view.layoutAttributesForItem(at: IndexPath(item: 1, section: 0))!
        check(abs(bottom - last.frame.maxY - 6) < 1, "Opening must reveal the end of a very tall row")
        update(1600)
        await layout(controller)
        let grown = view.layoutAttributesForItem(at: IndexPath(item: 1, section: 0))!
        check(abs(grown.frame.height - 1600) < 1,
              "A streamed row must be remeasured: height=\(grown.frame.height)")
        check(abs(view.contentOffset.y + view.bounds.height - view.adjustedContentInset.bottom
                  - grown.frame.maxY - 6) < 1,
        "Following must track streamed self-sizing growth: last=\(grown.frame), \(geometry(view))")
        view.contentInset.bottom += 200
        await layout(controller)
        check(abs(view.contentOffset.y + view.bounds.height - view.adjustedContentInset.bottom
                  - grown.frame.maxY - 6) < 1,
        "Keyboard insets must keep the latest message visible: last=\(grown.frame), \(geometry(view))")
        ids.append(.message("appended"))
        update(1600)
        await layout(controller)
        check(view.indexPathsForVisibleItems.contains(IndexPath(item: 2, section: 0)),
              "Following must reveal appended rows after a nonanimated diffable update")
        check(controller.scrollViewShouldScrollToTop(view) && !scroll.following,
              "Status-bar navigation must detach before UIKit scrolls")
        view.setContentOffset(CGPoint(x: -view.adjustedContentInset.left, y: -view.adjustedContentInset.top),
                              animated: false)
        await layout(controller)
        controller.scrollViewDidScrollToTop(view)
        let reading = position(0, in: controller)
        update(1800)
        await layout(controller)
        check(abs(position(0, in: controller) - reading) < 1,
              "Finishing status-bar navigation must preserve the new reading point during growth")
    }
}

extension ConversationScrollTests {
    private func checkShortHistory() async {
        let scroll = ConversationScrollState()
        let controller = ConversationListController(scroll: scroll)
        mount(controller)
        let latest = ConversationList.RowID.message("latest")
        func update(_ ids: [ConversationList.RowID]) {
            controller.update(ids: ids, following: scroll.following,
                              accessibilityValue: "") { _ in AnyView(Text("Message").frame(height: 60)) }
        }
        update([.history, latest])
        await layout(controller)
        let reading = position(1, in: controller)
        scroll.following = false
        // The history control can disappear before its asynchronous row projection completes.
        update([latest])
        await layout(controller)
        check(abs(position(0, in: controller) - reading) < 1, "Removing a history control must preserve the message")
        let ids = (0 ..< 50).map { ConversationList.RowID.message("old-\($0)") } + [latest]
        update(ids)
        await layout(controller)
        check(abs(position(50, in: controller) - reading) < 1, "A staged prepend must preserve bottom-aligned content")
        let view = controller.collectionView
        let before = view.contentOffset
        update(ids)
        await layout(controller)
        check(view.contentOffset == before, "A nonvisual page must not anchor a later unrelated update")
        let replacement = ConversationListController(scroll: ConversationScrollState())
        mount(replacement)
        replacement.update(ids: [.message("other")], following: true, accessibilityValue: "") { _ in
            AnyView(Text("Other conversation").frame(height: 60))
        }
        await layout(replacement)
        check(replacement.collectionView.numberOfItems(inSection: 0) == 1,
              "Navigation must discard the previous conversation's pending source and anchors")
    }

    private func checkWrappedRows() async {
        let scroll = ConversationScrollState()
        let controller = ConversationListController(scroll: scroll)
        mount(controller)
        controller.update(ids: [.message("wrapped")], following: true, accessibilityValue: "") { _ in
            AnyView(VStack(alignment: .leading) {
                Text(String(repeating: "A message wraps at the actual cell width. ", count: 300))
                    .frame(maxWidth: .infinity, alignment: .leading).fixedSize(horizontal: false, vertical: true)
                ScrollView(.horizontal) { Text("A long unwrapped code line").fixedSize() }
            })
        }
        await layout(controller)
        let view = controller.collectionView
        let path = IndexPath(item: 0, section: 0)
        let wide = view.layoutAttributesForItem(at: path)!.size
        check(wide.height > view.bounds.height && wide.height < 10000,
              "Wrapped text must measure its contents while a horizontal scroller keeps its ideal height: \(wide)")
        // Landscape safe areas inset the conversation horizontally.
        controller.additionalSafeAreaInsets = UIEdgeInsets(top: 0, left: 24, bottom: 0, right: 24)
        await layout(controller)
        let safeArea = view.safeAreaInsets
        let inset = view.layoutAttributesForItem(at: path)!.frame
        check(abs(inset.width - (view.bounds.width - safeArea.left - safeArea.right - 32)) < 1,
              "Horizontal inset changes must remeasure rows without resizing the viewport: \(inset), \(geometry(view))")
        check(abs(inset.minX - view.contentOffset.x - safeArea.left - 16) < 1,
              "The leading row margin must follow the native horizontal inset")
        check(abs(view.contentSize.width + view.adjustedContentInset.left
                  + view.adjustedContentInset.right - view.bounds.width) < 1,
        "A vertical conversation must not gain horizontal scroll range from its insets")
        view.bounds.size.width /= 2
        await layout(controller)
        let narrow = view.layoutAttributesForItem(at: path)!.size
        check(narrow.width < wide.width && narrow.height > wide.height,
              "A width change must remeasure the SwiftUI row: wide=\(wide), narrow=\(narrow), \(geometry(view))")
        check(abs(narrow.width - (view.bounds.width - view.safeAreaInsets.left - view.safeAreaInsets.right - 32)) < 1,
              "Native safe-area insets must be outside the row margins: narrow=\(narrow), \(geometry(view))")
        check(abs(view.contentOffset.y + view.bounds.height - view.adjustedContentInset.bottom
                  - view.layoutAttributesForItem(at: path)!.frame.maxY - 6) < 1,
        "Following must survive width changes in a long message")
    }

    private func checkStagedHistory() async {
        let scroll = ConversationScrollState()
        let controller = ConversationListController(scroll: scroll)
        mount(controller)
        let messages = (0 ..< 20).map { ConversationList.RowID.message("current-\($0)") }
        func update(_ ids: [ConversationList.RowID], header: CGFloat) {
            controller.update(ids: ids, following: scroll.following,
                              accessibilityValue: "") { index in
                AnyView(Text("Message").frame(height: index == 0 ? header : 72))
            }
        }
        let ids = [ConversationList.RowID.history] + messages
        update(ids, header: 60)
        await layout(controller)
        await tapStatusBar(controller)
        update(ids, header: 60)
        await layout(controller)
        let reading = position(1, in: controller)
        // Core can publish the completed history control before the background row projection finishes.
        update(ids, header: 8)
        await layout(controller)
        let older = (0 ..< 50).map { ConversationList.RowID.message("older-\($0)") }
        update([.history] + older + messages, header: 8)
        await layout(controller)
        check(abs(position(51, in: controller) - reading) < 1,
              "Staged history must retain the original anchor even if its interim offset was clamped: "
                  + "before=\(reading), after=\(position(51, in: controller)), "
                  + geometry(controller.collectionView))
        controller.collectionView.bounds.size.width -= 40
        await layout(controller)
        check(abs(position(51, in: controller) - reading) < 1,
              "Resizing must preserve a detached reader: before=\(reading), "
                  + "after=\(position(51, in: controller)), \(geometry(controller.collectionView))")
        let view = controller.collectionView
        let width = view.bounds.width - view.adjustedContentInset.left - view.adjustedContentInset.right - 32
        check((0 ..< view.numberOfItems(inSection: 0)).allSatisfy {
            guard let attributes = view.layoutAttributesForItem(at: IndexPath(item: $0, section: 0))
            else { return false }
            return abs(attributes.size.width - width) < 1
        }, "Resizing must replace offscreen rows' cached widths as well as the visible row widths")
    }
}

extension ConversationScrollTests {
    private func checkComposerUnderlay() async {
        for height: CGFloat in [60, 100, 180] {
            let scroll = ConversationScrollState()
            let controller = UIHostingController(rootView:
                GeometryReader { geometry in
                    ConversationList(ids: [.message("long")], scroll: scroll, following: true,
                                     accessibilityValue: "", viewportHeight: geometry.size.height) { _ in
                        AnyView(Text("Scrollable conversation").frame(height: 1200))
                    }
                    .ignoresSafeArea(.container, edges: .bottom)
                }
                .safeAreaInset(edge: .bottom, spacing: 0) {
                    Text("Composer").frame(maxWidth: .infinity).frame(height: height)
                })
            controller.additionalSafeAreaInsets.bottom = 34
            mount(controller)
            await layout(controller)
            let view = collection(in: controller.view)!
            check(abs(view.convert(view.bounds, to: controller.view).maxY - controller.view.bounds.maxY) < 1,
                  "Conversation must draw behind the composer and home indicator")
            check(abs(view.adjustedContentInset.bottom - height - controller.view.safeAreaInsets.bottom) < 1,
                  "Native insets must reserve the composer and home indicator once: \(geometry(view))")
            let last = view.layoutAttributesForItem(at: IndexPath(item: 0, section: 0))!
            check(abs(view.contentOffset.y + view.bounds.height - view.adjustedContentInset.bottom
                      - last.frame.maxY - 6) < 1,
            "The latest message must remain readable above the composer: \(geometry(view))")
        }
    }

    private func collection(in view: UIView) -> UICollectionView? {
        if let view = view as? UICollectionView {
            return view
        }
        return view.subviews.lazy.compactMap { self.collection(in: $0) }.first
    }
}

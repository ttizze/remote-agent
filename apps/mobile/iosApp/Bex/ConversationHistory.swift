import AgentCore
import SwiftUI
import UIKit

/// History and row presentation
extension ThreadScreen {
    func activityIsExpanded(_ activity: ActivityPresentation) -> Bool {
        AgentCore.activityIsExpanded(activity: activity, choice: activityExpansionOverrides[activity.id])
    }

    @ViewBuilder
    func conversationRow(_ row: ThreadConversationRow) -> some View {
        switch row.content {
        case .olderTurns: historyBoundary(nil)
        case let .native(content, item): nativeConversationRow(content, item: item)
        case let .queued(item):
            VStack(alignment: .leading, spacing: 6) {
                Text("順番待ち").font(.caption).foregroundColor(.secondary)
                ThreadMessageRow(item: item, isUser: true, media: model.mediaAccess, selection: model.selectionActions)
            }
        }
    }

    @ViewBuilder
    func nativeConversationRow(_ row: ConversationRow, item: ConversationItem?) -> some View {
        switch (row.content, item) {
        case let (.olderItems(turnId), _): historyBoundary(turnId)
        case let (.user, item?):
            ThreadMessageRow(item: item, isUser: true, media: model.mediaAccess, selection: model.selectionActions)
                .padding(
                    .top,
                    16
                )
        case (let .response(_, turnId), let item?):
            ThreadMessageRow(item: item, isUser: false, media: model.mediaAccess, selection: model.selectionActions,
                             fork: turnId
                                 .map { id in { complete in model.forkAndOpen(through: id, completion: complete) } })
        case let (.activityHeader(activity), _): activityHeader(activity)
        case (let .activity(_, turnId), let item?): activityItem(item, turnId: turnId)
        case let (.pendingRequest(request), _): ThreadRequestRow(request: request) { answer, complete in model.respond(
                request,
                answer: answer,
                completion: complete
            ) }
        case let (.error(error), _): ThreadErrorRow(error: error)
        case (.inProgress, _), (_, nil): EmptyView()
        }
    }

    func activityItem(_ item: ConversationItem, turnId: String) -> some View {
        ThreadItemRow(
            item: item,
            media: model.mediaAccess,
            selection: model.selectionActions,
            isExpanded: expandedItemIds.contains(item.data.id),
            toggleExpanded: {
                isFollowingLatest = false
                if expandedItemIds.contains(item.data.id) {
                    expandedItemIds.remove(item.data.id)
                } else {
                    expandedItemIds.insert(item.data.id)
                }
            },
            loadDetails: { await model.readItemDetails(
                threadId: conversation?.id ?? "", turnId: turnId,
                itemId: item.data.nativeId ?? item.data.id
            ) }
        )
    }

    @ViewBuilder
    func activityHeader(_ turn: ActivityPresentation) -> some View {
        let expanded = activityIsExpanded(turn)
        if turn.activityCanCollapse {
            Button {
                isFollowingLatest = false
                activityExpansionOverrides[turn.id] = ActivityExpansion(status: turn.status, expanded: !expanded)
            } label: {
                ThreadActivityHeader(turn: turn, expanded: expanded)
            }
            .buttonStyle(.plain)
            .accessibilityIdentifier("turn.activity." + turn.id)
        } else {
            ThreadActivityHeader(turn: turn, expanded: expanded).accessibilityIdentifier("turn.activity." + turn.id)
        }
    }

    func loadVisibleHistory() {
        guard scrollingToOlder, !historyRequestPending, !model.loadingHistory else { return }
        if let boundary = historyBoundaries.filter({ $0.value >= 0 && $0.value < scrollViewportHeight * 0.6 })
            .min(by: { $0.value < $1.value }) {
            let turnId = boundary.key == "older-turns" ? nil : boundary.key
            requestHistory(turnId)
        }
    }

    func requestHistory(_ turnId: String?) {
        guard !model.loadingHistory else { return }
        isFollowingLatest = false
        scrollingToOlder = false
        historyRequestPending = true
        model.loadOlderHistory(turnId)
    }

    func historyBoundary(_ turnId: String?) -> some View {
        Button { requestHistory(turnId) } label: {
            HStack {
                if model.loadingHistory {
                    ProgressView()
                }
                Text(turnId == nil ? "以前の会話を読み込む" : "途中の履歴を読み込む")
            }.frame(maxWidth: .infinity)
        }
        .disabled(model.loadingHistory)
        .accessibilityIdentifier("history.\(turnId ?? "older-turns")")
        .background(GeometryReader { geometry in
            Color.clear.preference(key: HistoryBoundaryPreferenceKey.self,
                                   value: [turnId ?? "older-turns": geometry.frame(in: .named("thread-scroll")).minY])
        })
    }

    func threadAccessibilityValue(_ thread: ConversationPresentation) -> String {
        let itemCount = thread.rows.filter { row in
            guard case let .native(native, _) = row.content else { return false }
            return switch native.content {
            case .user, .activity, .response, .pendingRequest, .error: true
            default: false
            }
        }.count
        return "turns=\(thread.source.turnCount());items=\(itemCount)"
    }
}

func conversationRows(_ thread: ConversationPresentation,
                      expansion: [String: ActivityExpansion]) -> [ThreadConversationRow] {
    var expanded = false
    return thread.rows.filter { row in
        guard case let .native(native, _) = row.content else { return true }
        switch native.content {
        case let .activityHeader(activity):
            expanded = AgentCore.activityIsExpanded(activity: activity, choice: expansion[activity.id])
            return true
        case .activity: return expanded
        case .inProgress: return false
        default: return true
        }
    }
}

struct HistoryBoundaryPreferenceKey: PreferenceKey {
    static var defaultValue: [String: CGFloat] = [:]
    static func reduce(value: inout [String: CGFloat], nextValue: () -> [String: CGFloat]) {
        value.merge(nextValue(), uniquingKeysWith: { _, next in next })
    }
}

/// Stop latest-message following before UIKit starts its status-bar scroll animation.
struct ConversationScrollToTop: UIViewRepresentable {
    let onScrollToTop: () -> Void

    func makeCoordinator() -> Coordinator {
        Coordinator(onScrollToTop)
    }

    func makeUIView(context: Context) -> ObserverView {
        let view = ObserverView()
        view.isUserInteractionEnabled = false
        view.attach = { [weak coordinator = context.coordinator] view in coordinator?.attach(to: view) }
        return view
    }

    func updateUIView(_ view: ObserverView, context: Context) {
        context.coordinator.onScrollToTop = onScrollToTop
        context.coordinator.attach(to: view)
    }

    static func dismantleUIView(_: ObserverView, coordinator: Coordinator) {
        coordinator.detach()
    }

    final class ObserverView: UIView {
        var attach: ((UIView) -> Void)?
        override func didMoveToWindow() {
            super.didMoveToWindow()
            if window != nil {
                attach?(self)
            }
        }
    }

    final class Coordinator: NSObject, UIScrollViewDelegate {
        var onScrollToTop: () -> Void
        weak var scrollView: UIScrollView?
        weak var original: UIScrollViewDelegate?

        init(_ onScrollToTop: @escaping () -> Void) {
            self.onScrollToTop = onScrollToTop
        }

        func attach(to view: UIView) {
            var ancestor = view.superview
            while let candidate = ancestor {
                if let scroll = candidate as? UIScrollView {
                    guard scroll.delegate !== self else { return }
                    detach()
                    scrollView = scroll
                    original = scroll.delegate
                    scroll.delegate = self
                    return
                }
                ancestor = candidate.superview
            }
        }

        func detach() {
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

        func scrollViewShouldScrollToTop(_ scrollView: UIScrollView) -> Bool {
            let shouldScroll = original?.scrollViewShouldScrollToTop?(scrollView) ?? true
            if shouldScroll {
                onScrollToTop()
            }
            return shouldScroll
        }
    }
}

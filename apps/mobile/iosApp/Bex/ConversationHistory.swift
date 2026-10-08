import AgentCore
import SwiftUI
import UIKit

/// History and row presentation
extension ThreadScreen {
    func followLatest(to id: String?, using proxy: ScrollViewProxy) {
        guard isFollowingLatest, let id else { return }
        proxy.scrollTo(id, anchor: .bottom)
    }

    @ViewBuilder
    func conversationRow(_ row: ThreadConversationRow) -> some View {
        switch row.content {
        case let .historyNotice(message): Text(message).font(.caption).foregroundStyle(.secondary)
        case let .native(content, item): nativeConversationRow(content, item: item)
        case let .queued(item):
            userMessageRow(item)
        }
    }

    @ViewBuilder
    func nativeConversationRow(_ row: ConversationRow, item: ConversationItem?) -> some View {
        switch (row.content, item) {
        case let (.user, item?):
            userMessageRow(item)
                .padding(
                    .top,
                    16
                )
        case (let .response(_, turnId), let item?):
            ThreadMessageRow(item: item, isUser: false, media: model.mediaAccess, selection: selectionActions,
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

    func userMessageRow(_ item: ConversationItem) -> some View {
        ThreadMessageRow(
            item: item,
            isUser: true,
            media: model.mediaAccess,
            selection: selectionActions,
            restoreUnknown: { model.restoreUnknownSubmission($0) },
            discardUnknown: { model.discardUnknownSubmission($0) }
        )
    }

    func activityItem(_ item: ConversationItem, turnId: String) -> some View {
        ThreadItemRow(
            item: item,
            media: model.mediaAccess,
            selection: selectionActions,
            isExpanded: expandedItemIds.contains(item.data.id),
            toggleExpanded: {
                isFollowingLatest = false
                if expandedItemIds.contains(item.data.id) {
                    expandedItemIds.remove(item.data.id)
                } else {
                    expandedItemIds.insert(item.data.id)
                }
            },
            loadDetails: {
                guard let threadId = conversation?.id else { return nil }
                return await model.readItemDetails(
                    threadId: threadId, turnId: turnId,
                    itemId: item.data.nativeId ?? item.data.id
                )
            }
        )
    }

    @ViewBuilder
    func activityHeader(_ turn: ActivityPresentation) -> some View {
        let expanded = AgentCore.activityIsExpanded(activity: turn, choice: activityExpansionOverrides[turn.id])
        if turn.activityCanCollapse {
            Button {
                isFollowingLatest = false
                activityExpansionOverrides[turn.id] = ActivityExpansion(status: turn.status, expanded: !expanded)
                if !expanded, let params = turn.loadItems {
                    model.perform(.loadTurnItems(params))
                }
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
        guard isVisible, let thread = conversation, thread.id == model.selectedThreadId,
              model.notice == nil else { return }
        let oldestVisible = visibleHistoryRows?.threadId == thread.id &&
            conversationRows(thread.rows, expansion: activityExpansionOverrides).first.map {
                visibleHistoryRows?.rowIds.contains($0.id) == true
            } == true
        guard AgentCore.shouldLoadHistory(
            hasMore: thread.source.hasMoreHistory(),
            loading: model.loadingHistory,
            oldestVisible: oldestVisible,
            latestVisible: latestHistoryRowVisible,
            followingLatest: isFollowingLatest
        ) else { return }
        model.loadOlderHistory()
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

struct ConversationScrollMetrics: Equatable {
    let content: CGSize
    let container: CGSize
    let latestVisible: Bool
}

func conversationRows(_ rows: [ThreadConversationRow],
                      expansion: [String: ActivityExpansion]) -> [ThreadConversationRow] {
    var expanded = false
    return rows.filter { row in
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

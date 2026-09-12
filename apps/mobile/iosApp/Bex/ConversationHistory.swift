import AgentCore
import AVFoundation
import SwiftUI
import UniformTypeIdentifiers

/// History and row presentation
extension ThreadScreen {
    func activityIsExpanded(_ activity: ActivityPresentation) -> Bool {
        AgentCore.activityIsExpanded(activity: activity, choice: activityExpansionOverrides[activity.id])
    }

    func conversationRows(_ thread: ConversationPresentation) -> [ThreadConversationRow] {
        var expanded = false
        return thread.rows.filter { row in
            guard case let .native(native, _) = row.content else { return true }
            switch native.content {
            case let .activityHeader(activity):
                expanded = activityIsExpanded(activity)
                return true
            case .activity: return expanded
            case .inProgress: return false
            default: return true
            }
        }
    }

    @ViewBuilder
    func conversationRow(_ row: ThreadConversationRow) -> some View {
        switch row.content {
        case .olderTurns: historyBoundary(nil)
        case let .native(content, item): nativeConversationRow(content, item: item)
        case let .queued(item):
            VStack(alignment: .leading, spacing: 6) {
                Text("順番待ち").font(.caption).foregroundColor(.secondary)
                ThreadMessageRow(item: item, isUser: true, model: model)
            }
        }
    }

    @ViewBuilder
    func nativeConversationRow(_ row: ConversationRow, item: ConversationItem?) -> some View {
        switch (row.content, item) {
        case let (.olderItems(turnId), _): historyBoundary(turnId)
        case let (.user, item?):
            ThreadMessageRow(item: item, isUser: true, model: model).padding(.top, 16)
        case (let .response(_, turnId), let item?):
            ThreadMessageRow(item: item, isUser: false, model: model, forkTurnId: turnId)
        case let (.activityHeader(activity), _): activityHeader(activity)
        case (let .activity(_, turnId), let item?): activityItem(item, turnId: turnId)
        case let (.pendingRequest(request), _): ThreadRequestRow(request: request, model: model)
        case let (.error(error), _): ThreadErrorRow(error: error)
        case (.inProgress, _), (_, nil): EmptyView()
        }
    }

    func activityItem(_ item: ConversationItem, turnId: String) -> some View {
        ThreadItemRow(item: item, model: model, isExpanded: expandedItemIds.contains(item.data.id),
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
                      ) })
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

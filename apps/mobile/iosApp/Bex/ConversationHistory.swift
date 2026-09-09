import AgentCore
import AVFoundation
import SwiftUI
import UniformTypeIdentifiers

/// History and row presentation
extension ThreadScreen {
    /// Give lazy conversation rows stable identities across streamed updates.
    func activityIsExpanded(_ turn: TurnPresentation) -> Bool {
        guard let override = activityExpansionOverrides[turn.id], override.status == turn.status else {
            return turn.activityInitiallyExpanded
        }
        return override.expanded
    }

    func conversationRows(_ thread: ConversationPresentation) -> [ThreadConversationRow] {
        var rows = [ThreadConversationRow]()
        rows
            .reserveCapacity(thread.turns
                .reduce(0) { $0 + $1.userMessages.count + $1.activityItems.count + $1.responses.count + 4 })
        if thread.hasOlderTurns {
            rows.append(.init(id: "history-older-turns", content: .olderTurns))
        }
        for (index, turn) in thread.turns.enumerated() {
            let endsNativeTurn = index + 1 == thread.turns.count || thread.turns[index + 1].turnId != turn.turnId
            appendRows(for: turn, endsNativeTurn: endsNativeTurn, to: &rows)
        }
        for item in thread.queuedMessages {
            rows.append(.init(id: item.id, content: .queued(item)))
        }
        return rows
    }

    func appendRows(for turn: TurnPresentation, endsNativeTurn: Bool, to rows: inout [ThreadConversationRow]) {
        if let opening = turn.openingUserMessage {
            rows.append(.init(
                id: "opening:" + turn.id,
                content: .user(opening)
            ))
        }
        if turn
            .hasOlderItems {
            rows.append(.init(id: "history-gap:" + turn.id, content: .olderItems(turn.turnId)))
        }
        for item in turn.userMessages {
            rows.append(.init(id: "history-item:" + item.id, content: .user(item)))
        }
        if turn.activitySummary != nil {
            rows.append(.init(id: turn.id, content: .activityHeader(turn)))
            if activityIsExpanded(turn) {
                for item in turn.activityItems {
                    rows.append(.init(
                        id: "history-item:" + item.id,
                        content: .activity(item, turn.turnId)
                    ))
                }
            }
        }
        for request in turn.pendingRequests {
            rows.append(.init(
                id: "history-request:" + request.id,
                content: .request(request)
            ))
        }
        if let error = turn.error {
            rows.append(.init(id: "history-error:" + turn.id, content: .error(error)))
        }
        for item in turn.responses {
            let forkTurnId = !turn.isInProgress && endsNativeTurn && item.id == turn.responses.last?.id ? turn
                .turnId : nil
            rows.append(.init(id: "history-item:" + item.id, content: .response(item, forkTurnId)))
        }
    }

    @ViewBuilder
    func conversationRow(_ row: ThreadConversationRow) -> some View {
        switch row.content {
        case .olderTurns: historyBoundary(nil)
        case let .olderItems(turnId): historyBoundary(turnId)
        case let .user(item): ThreadMessageRow(item: item, isUser: true, model: model).padding(.top, 16)
        case let .response(item, turnId): ThreadMessageRow(item: item, isUser: false, model: model, forkTurnId: turnId)
        case let .activityHeader(turn): activityHeader(turn)
        case let .activity(item, turnId):
            ThreadItemRow(item: item, model: model, isExpanded: expandedItemIds.contains(item.id),
                          toggleExpanded: {
                              isFollowingLatest = false
                              if expandedItemIds.contains(item.id) {
                                  expandedItemIds.remove(item.id)
                              } else {
                                  expandedItemIds.insert(item.id)
                              }
                          },
                          loadDetails: { await model.readItemDetails(
                              threadId: conversation?.id ?? "",
                              turnId: turnId,
                              itemId: item.id
                          ) })
        case let .request(request): ThreadRequestRow(request: request, model: model)
        case let .error(error): ThreadErrorRow(error: error)
        case let .queued(item):
            VStack(alignment: .leading, spacing: 6) {
                Text("順番待ち").font(.caption).foregroundColor(.secondary)
                ThreadMessageRow(item: item, isUser: true, model: model)
            }
        }
    }

    @ViewBuilder
    func activityHeader(_ turn: TurnPresentation) -> some View {
        let expanded = activityIsExpanded(turn)
        if turn.activityCanCollapse {
            Button {
                isFollowingLatest = false
                activityExpansionOverrides[turn.id] = (turn.status, !expanded)
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
        guard scrollingToOlder, !historyRequestPending, !state.loadingHistory else { return }
        if let boundary = historyBoundaries.filter({ $0.value >= 0 && $0.value < scrollViewportHeight * 0.6 })
            .min(by: { $0.value < $1.value }) {
            let turnId = boundary.key == "older-turns" ? nil : boundary.key
            requestHistory(turnId)
        }
    }

    func requestHistory(_ turnId: String?) {
        guard !state.loadingHistory else { return }
        isFollowingLatest = false
        scrollingToOlder = false
        historyRequestPending = true
        model.loadOlderHistory(turnId)
    }

    func historyBoundary(_ turnId: String?) -> some View {
        Button { requestHistory(turnId) } label: {
            HStack {
                if state.loadingHistory {
                    ProgressView()
                }
                Text(turnId == nil ? "以前の会話を読み込む" : "途中の履歴を読み込む")
            }.frame(maxWidth: .infinity)
        }
        .disabled(state.loadingHistory)
        .accessibilityIdentifier("history.\(turnId ?? "older-turns")")
        .background(GeometryReader { geometry in
            Color.clear.preference(key: HistoryBoundaryPreferenceKey.self,
                                   value: [turnId ?? "older-turns": geometry.frame(in: .named("thread-scroll")).minY])
        })
    }

    func threadAccessibilityValue(_ thread: ConversationPresentation) -> String {
        let itemCount = thread.turns.reduce(0) { total, turn in
            total + turn.userMessages.count + turn.activityItems.count + turn.responses.count
                + turn.pendingRequests.count + (turn.error == nil ? 0 : 1)
        }
        return "turns=\(Set(thread.turns.map(\.turnId)).count);items=\(itemCount)"
    }
}

import AgentCore
import AVFoundation
import SwiftUI
import UniformTypeIdentifiers

/// History and row presentation
extension ThreadScreen {
    func activityIsExpanded(_ turn: TurnPresentationData) -> Bool {
        guard let choice = activityExpansionOverrides[turn.id], choice.status == turn.status else {
            return turn.activityInitiallyExpanded
        }
        return choice.expanded
    }

    var runningTurnId: String? {
        model.conversationRows.last(where: \.isInProgress)?.turnId
    }

    @ViewBuilder
    func conversationTurn(_ turn: TurnPresentationData, endsNativeTurn: Bool) -> some View {
        if let item = turn.openingUserMessage {
            ThreadMessageRow(item: item, isUser: true, model: model)
        }
        if turn.hasOlderItems {
            historyBoundary(turn.turnId)
        }
        ForEach(turn.userMessages.indices, id: \.self) { index in
            let item = turn.userMessages[index]
            ThreadMessageRow(item: item, isUser: true, model: model).padding(.top, 16).id(item.id())
        }
        if turn.activitySummary != nil {
            activityHeader(turn)
            if activityIsExpanded(turn) {
                ForEach(turn.activityItems.indices, id: \.self) { index in
                    let item = turn.activityItems[index]
                    activityItem(item, turnId: turn.turnId).id(item.id())
                }
            }
        }
        ForEach(turn.pendingRequests, id: \.key) { ThreadRequestRow(request: $0, model: model) }
        if let error = turn.error {
            ThreadErrorRow(error: error)
        }
        ForEach(turn.responses.indices, id: \.self) { index in
            let item = turn.responses[index]
            ThreadMessageRow(item: item, isUser: false, model: model,
                             forkTurnId: !turn.isInProgress && endsNativeTurn && index == turn.responses.count - 1
                                 ? turn.turnId : nil).id(item.id())
        }
    }

    func activityItem(_ item: RenderedItem, turnId: String) -> some View {
        let data = item.presentation()
        return ThreadItemRow(item: item, model: model, isExpanded: expandedItemIds.contains(data.id),
                             toggleExpanded: {
                                 isFollowingLatest = false
                                 if expandedItemIds.contains(data.id) {
                                     expandedItemIds.remove(data.id)
                                 } else {
                                     expandedItemIds.insert(data.id)
                                 }
                             },
                             loadDetails: { await model.readItemDetails(
                                 threadId: model.selectedThreadId ?? "", turnId: turnId,
                                 itemId: data.nativeId ?? data.id
                             ) })
    }

    @ViewBuilder
    func activityHeader(_ turn: TurnPresentationData) -> some View {
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

    func threadAccessibilityValue(_ thread: RenderedConversation) -> String {
        let itemCount = model.conversationRows.reduce(0) { count, turn in
            count + turn.userMessages.count + turn.activityItems.count + turn.responses.count
                + turn.pendingRequests.count + (turn.error == nil ? 0 : 1)
                + (turn.openingUserMessage == nil ? 0 : 1)
        }
        return "turns=\(thread.source().turnCount());items=\(itemCount)"
    }
}

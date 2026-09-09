import AgentCore
import Foundation

/// Memoize rendered bodies by core identity. A delta rebuilds only its changed turn/items.
final class ConversationPresentationCache {
    private struct CachedTurn {
        let source: AgentCore.Turn
        let requests: [Request]
        let rows: [TurnPresentation]
        let items: [String: ConversationItem]
    }

    private var turns: [String: CachedTurn] = [:]
    private var threadId: String?
    private var previous: ConversationPresentation?
    private var previousRequests: [Request] = []

    func project(_ source: AgentCore.Thread?, requests: [Request]) -> ConversationPresentation? {
        guard let source else {
            turns = [:]; threadId = nil; previous = nil; previousRequests = []
            return nil
        }
        if let previous, source.unchanged(other: previous.source), previousRequests == requests {
            return previous
        }
        let id = source.id()
        if threadId != id {
            turns = [:]; threadId = id
        }
        let nativeTurns = source.turns()
        var next: [String: CachedTurn] = [:]
        var rows: [TurnPresentation] = []
        for turn in nativeTurns {
            let turnId = turn.id()
            let pending = requests.filter {
                $0.params["threadId"]?.string == id &&
                    ($0.params["turnId"]?.string ?? nativeTurns.last?.id()) == turnId
            }
            if let cached = turns[turnId], turn.unchanged(other: cached.source), cached.requests == pending {
                next[turnId] = cached
                rows += cached.rows
                continue
            }
            let cached = projectTurn(turn, pending: pending, cached: turns[turnId])
            next[turnId] = cached
            rows += cached.rows
        }
        turns = next
        let result = ConversationPresentation(source: source, id: id, title: source.title(), turns: rows,
                                              queuedMessages: source.queuedSubmissions().map(ConversationItem.init),
                                              hasOlderTurns: source.historyCursor() != nil)
        previous = result
        previousRequests = requests
        return result
    }

    private func projectTurn(_ turn: AgentCore.Turn, pending: [Request], cached: CachedTurn?) -> CachedTurn {
        let previous = cached?.items ?? [:]
        var items: [String: ConversationItem] = [:]
        let deferred = Set(turn.deferredItemIds())
        func item(_ source: AgentCore.Item) -> ConversationItem {
            let key = source.id()
            let isDeferred = deferred.contains(key)
            if let cached = previous[key], let old = cached.source,
               source.unchanged(other: old), cached.isDeferred == isDeferred {
                items[key] = cached
                return cached
            }
            let value = ConversationItem(source, deferred: isDeferred)
            items[key] = value
            return value
        }
        let content = turn.content()
        let displayItems = content.items.map { source in
            switch source {
            case let .native(source): item(source)
            case let .submitted(submission): ConversationItem(submission)
            }
        }
        let opening = turn.openingUserMessage().flatMap { source in
            displayItems.contains { $0.source?.id() == source.id() } ? nil : item(source)
        }
        let projected = content.segments.map { segment in
            projectSegment(segment, turn: turn, displayItems: displayItems, opening: opening, pending: pending)
        }
        return CachedTurn(source: turn, requests: pending, rows: projected, items: items)
    }

    private func projectSegment(
        _ segment: Segment, turn: AgentCore.Turn, displayItems: [ConversationItem],
        opening: ConversationItem?, pending: [Request]
    ) -> TurnPresentation {
        let turnId = turn.id()
        let status = turn.status()
        let error = turn.error().map(Self.error)
        var users: [ConversationItem] = []
        var activity: [ConversationItem] = []
        var responses: [ConversationItem] = []
        for (offset, role) in segment.roles.enumerated() {
            let value = displayItems[Int(segment.start) + offset]
            switch role {
            case .hidden: break
            case .user: users.append(value)
            case .activity: activity.append(value)
            case .response: responses.append(value)
            }
        }
        return TurnPresentation(
            id: segment.id, turnId: turnId,
            hasOlderItems: segment.start == 0 && turn.hasOlderItems(),
            openingUserMessage: segment.start == 0 ? opening : nil,
            status: status, isInProgress: segment.last && status == "inProgress",
            userMessages: users, activitySummary: segment.label, activityItems: activity,
            responses: responses, activityInitiallyExpanded: segment.initiallyExpanded,
            activityCanCollapse: segment.collapsible, error: segment.last ? error : nil,
            pendingRequests: segment.last ? pending.map { RequestPresentation(source: $0) } : []
        )
    }

    private static func error(_ value: JsonValue) -> TurnErrorPresentation {
        let info = value["codexErrorInfo"]
        let kind = info?.string ?? info?.fields.keys.sorted().first
        let retrying = value["willRetry"]?.bool ?? false
        let title: String = if retrying {
            kind == "serverOverloaded" ? "サーバーが混み合っています。再接続しています" : "再接続しています"
        } else {
            errorTitles[kind ?? ""] ?? "エラー"
        }
        return TurnErrorPresentation(title: title, message: value["message"]?.string ?? value.string ?? value.formatted,
                                     details: value["additionalDetails"]?.string, isReconnecting: retrying)
    }

    private static let errorTitles = [
        "contextWindowExceeded": "コンテキストの上限に達しました",
        "sessionBudgetExceeded": "セッションの上限に達しました",
        "usageLimitExceeded": "利用上限に達しました",
        "serverOverloaded": "サーバーが混み合っています",
        "cyberPolicy": "安全ポリシーにより停止しました",
        "misalignmentPolicyViolation": "安全ポリシーにより停止しました",
        "internalServerError": "サーバーエラー",
        "unauthorized": "認証が必要です",
        "badRequest": "リクエストを処理できません",
        "threadRollbackFailed": "タスクを元に戻せませんでした",
        "sandboxError": "サンドボックスエラー",
        "activeTurnNotSteerable": "この作業中はメッセージを追加できません",
        "httpConnectionFailed": "接続エラー",
        "responseStreamConnectionFailed": "接続エラー",
        "responseStreamDisconnected": "接続エラー",
        "responseTooManyFailedAttempts": "接続エラー"
    ]
}

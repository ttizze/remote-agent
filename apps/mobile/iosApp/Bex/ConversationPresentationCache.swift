import AgentCore
import Foundation

/// Cache native render values and parsed Markdown; Rust owns all projection policy.
actor ConversationPresentationCache {
    private struct CachedTurn {
        let source: RenderedTurn
        let rows: [TurnPresentation]
        let items: [String: ConversationItem]
    }

    private var turns: [String: CachedTurn] = [:]
    private var projection: RenderedConversation?
    private var previous: ConversationPresentation?
    private var queued: [String: ConversationItem] = [:]

    func project(_ source: AgentCore.Thread?, snapshot: AgentCore.Snapshot) -> ConversationPresentation? {
        guard let source else {
            turns = [:]; projection = nil; previous = nil; queued = [:]
            return nil
        }
        let rendered = AgentCore.projectConversation(snapshot: snapshot, source: source, previous: projection)
        if let projection, rendered.unchanged(other: projection) {
            return previous
        }
        var next: [String: CachedTurn] = [:]
        var rows: [TurnPresentation] = []
        for turn in rendered.turns() {
            let id = turn.id()
            if let cached = turns[id], turn.unchanged(other: cached.source) {
                next[id] = cached; rows += cached.rows
                continue
            }
            let cached = turns[id]?.items ?? [:]
            var items: [String: ConversationItem] = [:]
            func item(_ source: RenderedItem) -> ConversationItem {
                Self.item(source, previous: cached, next: &items)
            }
            let projected = turn.rows().map { row in
                TurnPresentation(
                    id: row.id, turnId: row.turnId, hasOlderItems: row.hasOlderItems,
                    openingUserMessage: row.openingUserMessage.map(item), status: row.status,
                    isInProgress: row.isInProgress, userMessages: row.userMessages.map(item),
                    activitySummary: row.activitySummary, activityItems: row.activityItems.map(item),
                    responses: row.responses.map(item), activityInitiallyExpanded: row.activityInitiallyExpanded,
                    activityCanCollapse: row.activityCanCollapse, error: row.error, pendingRequests: row.pendingRequests
                )
            }
            next[id] = CachedTurn(source: turn, rows: projected, items: items)
            rows += projected
        }
        var nextQueued: [String: ConversationItem] = [:]
        let queuedMessages = rendered.queued().map { Self.item($0, previous: queued, next: &nextQueued) }
        queued = nextQueued; turns = next; projection = rendered
        let result = ConversationPresentation(source: source, id: source.id(), title: source.title(), turns: rows,
                                              queuedMessages: queuedMessages,
                                              hasOlderTurns: source.historyCursor() != nil)
        previous = result
        return result
    }

    private static func item(_ source: RenderedItem, previous: [String: ConversationItem],
                             next: inout [String: ConversationItem]) -> ConversationItem {
        let id = source.id()
        let result: ConversationItem = if let cached = previous[id], source.unchanged(other: cached.source) {
            cached
        } else {
            ConversationItem(source)
        }
        next[id] = result
        return result
    }
}

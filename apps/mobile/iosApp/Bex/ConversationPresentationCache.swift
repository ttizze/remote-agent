import AgentCore
import Foundation

/// Cache native render values and parsed Markdown; Rust owns all projection policy.
actor ConversationPresentationCache {
    private struct CachedTurn {
        let source: RenderedTurn
        let rows: [ThreadConversationRow]
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
        var rows: [ThreadConversationRow] = []
        if source.historyCursor() != nil {
            rows.append(.init(id: "history-older-turns", content: .olderTurns))
        }
        for turn in rendered.turns() {
            let id = turn.id()
            if let cached = turns[id], turn.unchanged(other: cached.source) {
                next[id] = cached; rows += cached.rows
                continue
            }
            let cached = turns[id]?.items ?? [:]
            var items: [String: ConversationItem] = [:]
            let projected = turn.conversationRows().map { row in
                let item: ConversationItem? = switch row.content {
                case let .user(source), let .response(source, _), let .activity(source, _):
                    Self.item(source, id: row.id, previous: cached, next: &items)
                default: nil
                }
                return ThreadConversationRow(id: row.id, content: .native(row, item))
            }
            next[id] = CachedTurn(source: turn, rows: projected, items: items)
            rows += projected
        }
        var nextQueued: [String: ConversationItem] = [:]
        rows += rendered.queued().map {
            let item = Self.item($0, id: $0.id(), previous: queued, next: &nextQueued)
            return ThreadConversationRow(id: item.data.id, content: .queued(item))
        }
        queued = nextQueued; turns = next; projection = rendered
        let result = ConversationPresentation(
            source: source,
            id: source.id(),
            title: source.title(),
            rows: rows
        )
        previous = result
        return result
    }

    private static func item(_ source: RenderedItem, id: String, previous: [String: ConversationItem],
                             next: inout [String: ConversationItem]) -> ConversationItem {
        let result: ConversationItem = if let cached = previous[id], source.unchanged(other: cached.source) {
            cached
        } else {
            ConversationItem(source)
        }
        next[id] = result
        return result
    }
}

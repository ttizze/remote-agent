import AgentCore

/// Immutable native render values; Rust owns projection policy.
struct ConversationPresentation: Sendable {
    let source: AgentCore.Thread
    let id: SessionRef?
    let title: String
    let rows: [ThreadConversationRow]
    private struct CachedTurn: Sendable {
        let source: RenderedTurn
        let rows: [ThreadConversationRow]
        let items: [String: ConversationItem]
    }

    private let turns: [String: CachedTurn]
    private let projection: RenderedConversation
    private let queued: [String: ConversationItem]

    static func project(_ source: AgentCore.Thread?, snapshot: AgentCore.Snapshot, previous: Self?) -> Self? {
        guard let source else { return nil }
        let rendered = AgentCore.projectConversation(snapshot: snapshot, source: source, previous: previous?.projection)
        if let previous, rendered.unchanged(other: previous.projection) {
            return previous
        }
        var next: [String: CachedTurn] = [:]
        var rows: [ThreadConversationRow] = []
        if let notice = source.inputUnavailableReason() {
            rows.append(.init(id: "input-capability", content: .historyNotice(notice)))
        }
        if let notice = source.historyNotice() {
            rows.append(.init(id: "history-read-state", content: .historyNotice(notice)))
        }
        if source.hasMoreHistory() {
            rows.append(.init(id: "history-older-turns", content: .olderTurns))
        }
        for turn in rendered.turns() {
            let id = turn.id()
            if let cached = previous?.turns[id], turn.unchanged(other: cached.source) {
                next[id] = cached; rows += cached.rows
                continue
            }
            let cached = previous?.turns[id]?.items ?? [:]
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
            let item = Self.item($0, id: $0.id(), previous: previous?.queued ?? [:], next: &nextQueued)
            return ThreadConversationRow(id: item.data.id, content: .queued(item))
        }
        rows += rendered.unplacedRequests().map { row in
            ThreadConversationRow(id: row.id, content: .native(row, nil))
        }
        return Self(
            source: source,
            id: source.id(),
            title: source.title(),
            rows: rows, turns: next, projection: rendered, queued: nextQueued
        )
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

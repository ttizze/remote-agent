import AgentCore

struct ThreadConversationRow: Identifiable, Sendable {
    let id: String
    let content: Content
    enum Content: Sendable {
        case historyNotice(String)
        case native(VisibleConversationRow, ConversationItem?)
    }
}

/// Immutable native render values; Rust filters collapsed activity before crossing the bridge.
struct ConversationPresentation: Sendable {
    let id: SessionRef?
    let scope: PanelScope
    let projection: ConversationRows
    let rows: [ThreadConversationRow]

    static func project(_ rendered: RenderedConversation, scope: PanelScope,
                        expansion: [String: String], previous: Self?) -> Self {
        let projection = ConversationRows.project(
            source: rendered, scope: scope, expansion: expansion,
            policy: ConversationRowPolicy(requests: false, progress: .hidden), previous: previous?.projection
        )
        if let previous, projection.unchanged(other: previous.projection) {
            return previous
        }
        let cached: [String: ConversationItem] = Dictionary(uniqueKeysWithValues: (previous?.rows ?? [])
            .compactMap { row in
                if case let .native(_, item?) = row.content {
                    return (row.id, item)
                }
                return nil
            })
        var rows = rendered.historyNotice().map {
            [ThreadConversationRow(id: "history-read-state", content: .historyNotice($0))]
        } ?? []
        for row in projection.rows() {
            let id = row.id()
            let item: ConversationItem? = switch row.data().content {
            case let .user(source), let .response(source, _, _), let .activity(source, _):
                if let previous = cached[id], source.unchanged(other: previous.source) {
                    previous
                } else {
                    ConversationItem(source)
                }
            default: nil
            }
            rows.append(ThreadConversationRow(id: id, content: .native(row, item)))
        }
        return Self(id: rendered.id(), scope: scope, projection: projection, rows: rows)
    }
}

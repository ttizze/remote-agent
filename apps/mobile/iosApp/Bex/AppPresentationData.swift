import AgentCore
import Foundation

// These values contain rendered rows, never mutable conversation state.
enum AppScreen { case pairing, profiles, threads, thread }
enum LoadState { case idle, loading, ready, failed }
struct HostProfile: Codable, Identifiable {
    let id: String
    let name: String
    let ticket: String
    var hostIdentity: String {
        id
    }
}

extension JsonValue {
    var string: String? {
        switch self {
        case let .string(value), let .number(value): value
        default: nil
        }
    }

    var fields: [String: JsonValue] {
        if case let .object(fields) = self {
            return fields
        }; return [:]
    }

    var array: [JsonValue] {
        if case let .array(values) = self {
            return values
        }; return []
    }

    var bool: Bool {
        if case let .boolean(value) = self {
            return value
        }; return false
    }

    var formatted: String {
        (try? formatJsonValue(value: self)) ?? ""
    }

    subscript(_ key: String) -> JsonValue? {
        fields[key]
    }
}

extension Request {
    var paramsJson: String {
        JsonValue.object(fields: params).formatted
    }
}

final class ConversationItem: Sendable {
    let source: RenderedItem
    let data: ItemPresentation
    let markdown: [ConversationMarkdownContent.Part]

    init(_ source: RenderedItem) {
        self.source = source
        data = source.presentation()
        markdown = data.kind != "user" && !data.collapsible ? ConversationMarkdownContent.parse(data.body) : []
    }
}

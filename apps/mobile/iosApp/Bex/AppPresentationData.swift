import AgentCore
import Foundation

// These values contain rendered rows, never mutable conversation state.
enum AppScreen { case pairing, profiles, threads, thread }
enum LoadState { case idle, loading, ready, failed }
struct HostProfile: Codable, Identifiable {
    let id: String
    var name: String
    let ticket: String

    static func load() throws -> [HostProfile] {
        guard let data = UserDefaults.standard.data(forKey: "bex.hosts.iroh") else { return [] }
        return try JSONDecoder().decode([HostProfile].self, from: data)
    }

    static func save(_ profiles: [HostProfile]) throws {
        try UserDefaults.standard.set(JSONEncoder().encode(profiles), forKey: "bex.hosts.iroh")
    }
}

struct ConversationPresentationInput {
    let source: AgentCore.Thread?
    let snapshot: AgentCore.Snapshot
    let host: String?
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

final class ConversationItem: Sendable {
    let source: RenderedItem
    let data: ItemPresentation
    let markdown: [ConversationMarkdownContent.Part]

    init(_ source: RenderedItem) {
        self.source = source
        data = source.presentation()
        markdown = data.kind != "user" && !data.collapsible ? data.body
            .map(ConversationMarkdownContent.parse) ?? [] : []
    }
}

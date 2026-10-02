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

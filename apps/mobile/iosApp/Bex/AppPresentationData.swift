import AgentCore
import Foundation

// These values contain rendered rows, never mutable conversation state.
enum AppScreen: Hashable { case pairing, profiles, threads, thread }
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

struct EnvironmentActivityRow: Identifiable {
    let environmentId: String
    let threadId: String
    let title: String
    let headline: String
    let detail: String?
    let phase: String
    let updatedAtMs: Int64

    var id: String { "\(environmentId):\(threadId):\(updatedAtMs)" }
}

struct EnvironmentRow: Identifiable {
    let profileId: String
    let environmentId: String
    let label: String
    let state: String
    let platform: String?
    let machine: String?
    let capabilities: [String]
    let reconnectReason: String?
    let activities: [EnvironmentActivityRow]

    var id: String { profileId }
}

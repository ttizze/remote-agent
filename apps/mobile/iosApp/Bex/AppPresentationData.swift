import AgentCore
import Foundation

// These values contain rendered rows, never mutable conversation state.
enum AppScreen: Hashable { case settings, profiles, thread }
enum LoadState { case idle, loading, ready, failed }
struct HostProfile: Codable, Identifiable, Equatable {
    let id: String
    var name: String
    var ticket: String

    static func load() throws -> [HostProfile] {
        guard let data = UserDefaults.standard.data(forKey: "bex.hosts.iroh") else { return [] }
        return try JSONDecoder().decode([HostProfile].self, from: data)
    }

    static func save(_ profiles: [HostProfile]) throws {
        try UserDefaults.standard.set(JSONEncoder().encode(profiles), forKey: "bex.hosts.iroh")
    }
}

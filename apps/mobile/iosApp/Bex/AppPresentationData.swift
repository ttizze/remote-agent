import AgentCore
import Foundation

enum AppScreen { case pairing, profiles, threads, thread }
enum LoadState { case idle, loading, ready, failed }
struct HostProfile: Codable, Identifiable {
    let id: String
    let name: String
    let ticket: String
}

extension JsonValue {
    var string: String? {
        switch self {
        case let .string(value), let .number(value): value
        default: nil
        }
    }

    var array: [JsonValue] {
        if case let .array(values) = self {
            return values
        }; return []
    }

    subscript(_ key: String) -> JsonValue? {
        if case let .object(fields) = self {
            return fields[key]
        }
        return nil
    }
}

extension RenderedItem: Equatable {
    public static func == (lhs: RenderedItem, rhs: RenderedItem) -> Bool {
        lhs.unchanged(other: rhs)
    }
}

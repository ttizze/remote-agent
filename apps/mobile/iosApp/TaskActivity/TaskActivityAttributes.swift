import ActivityKit
import Foundation

struct TaskActivityAttributes: ActivityAttributes, Hashable {
    enum IconKind: String, Codable {
        case running, waiting, unknown, finished
    }

    struct Icon: Codable, Hashable {
        var kind: IconKind
        var label: String
    }

    struct View: Codable, Hashable {
        var total: UInt32
        var label: String
        var icons: [Icon]
        var overflow: UInt32
    }

    struct Display: Codable, Hashable {
        var current: View
        var canStart: Bool
        var ongoing: Bool
        var urgent: Bool
    }

    struct ContentState: Codable, Hashable {
        var display: Display
        var hostName: String
    }

    static var urlScheme: String? {
        Bundle.main.object(forInfoDictionaryKey: "BexTaskURLScheme") as? String
    }

    var hostID: String

    var url: URL? {
        guard let scheme = Self.urlScheme else { return nil }
        var components = URLComponents()
        components.scheme = scheme
        components.host = "tasks"
        components.queryItems = [URLQueryItem(name: "host", value: hostID)]
        return components.url
    }
}

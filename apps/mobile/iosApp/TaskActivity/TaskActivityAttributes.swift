import ActivityKit
import Foundation

struct TaskActivityAttributes: ActivityAttributes, Hashable {
    struct ContentState: Codable, Hashable {
        var title: String
        var status: String
        var statusLabel: String
        var connected: Bool
        var hostName: String

        var symbol: String {
            switch status {
            case "waiting": "person.crop.circle.badge.questionmark"
            case "completed": "checkmark.circle.fill"
            case "failed": "exclamationmark.circle.fill"
            case "interrupted": "stop.circle.fill"
            case "finished": "flag.checkered"
            default: "bolt.circle.fill"
            }
        }
    }

    var hostID: String
    var provider: String
    var sessionID: String

    var url: URL? {
        var components = URLComponents()
        components.scheme = "bex"
        components.host = "task"
        components.queryItems = [
            URLQueryItem(name: "host", value: hostID),
            URLQueryItem(name: "provider", value: provider),
            URLQueryItem(name: "session", value: sessionID)
        ]
        return components.url
    }
}

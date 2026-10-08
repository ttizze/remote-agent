import ActivityKit
import Foundation

struct TaskActivityAttributes: ActivityAttributes, Hashable {
    struct Summary: Codable, Hashable {
        var running: UInt32
        var waiting: UInt32
        var unknown: UInt32

        var total: Int {
            Int(running) + Int(waiting) + Int(unknown)
        }

        var statusLabel: String {
            if total == 0 {
                return "すべてのタスクが終了"
            }
            if unknown > 0 {
                return "更新待ち"
            }
            if waiting > 0 {
                return "確認待ち \(waiting)件 · 実行中 \(running)件"
            }
            return "実行中 \(running)件"
        }

        /// Keep the widget cheap and legible; additional tasks share a count badge.
        var icons: [String] {
            if total == 0 {
                return ["checkmark.circle.fill"]
            }
            var result = [String]()
            for (count, symbol) in [(waiting, "person.crop.circle.badge.questionmark"),
                                    (running, "circle.dotted"), (unknown, "arrow.clockwise.circle")] {
                result += Array(repeating: symbol, count: min(Int(count), 12 - result.count))
            }
            return result
        }
    }

    struct ContentState: Codable, Hashable {
        var summary: Summary
        var connected: Bool
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

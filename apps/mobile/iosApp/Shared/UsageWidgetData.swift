import Foundation

struct UsageWidgetQuota: Decodable, Hashable {
    let label: String
    let kind: String
    let remaining: UInt32
    let resetAt: Int64?
}

struct UsageWidgetProvider: Decodable, Hashable {
    let name: String
    let detail: String
    let windows: [UsageWidgetQuota]
    let expiresAt: Int64
    let totalWindows: UInt32
}

struct UsageWidgetEntryData: Decodable, Hashable {
    let date: Int64
    let checkedAt: Int64
    let providers: [UsageWidgetProvider]
}

struct UsageWidgetData: Decodable {
    let entries: [UsageWidgetEntryData]
}

enum UsageWidgetStorage {
    static let group = "group.com.ttizze.b-codex"
    static let kind = "SubscriptionUsage"
    static var file: URL? {
        FileManager.default.containerURL(forSecurityApplicationGroupIdentifier: group)?
            .appendingPathComponent("subscription-usage.json")
    }
}

import ActivityKit
import Foundation

struct AgentActivityAttributes: ActivityAttributes {
    struct ContentState: Codable, Hashable {
        let title: String
        let subtitle: String
        let activeCount: UInt32
        let updatedAt: String
        let activities: [Item]
    }

    struct Item: Codable, Hashable {
        let environmentId: String
        let threadId: String
        let projectTitle: String
        let threadTitle: String
        let modelTitle: String
        let phase: String
        let status: String
        let updatedAt: String
        let deepLink: String
    }
}

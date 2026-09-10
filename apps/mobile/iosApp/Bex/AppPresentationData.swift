import AgentCore
import Foundation

// These values contain rendered rows, never mutable conversation state.
enum AppScreen { case pairing, profiles, threads, thread }
enum LoadState { case idle, loading, ready, failed }
struct HostProfile: Codable, Identifiable {
    let id: String
    let name: String
    let ticket: String
    var hostIdentity: String {
        id
    }
}

struct AppPresentation {
    let screen: AppScreen
    let isConnected: Bool
    let isConnecting: Bool
    let profiles: [HostProfile]
    let selectedProfileId: String?
    let selectedProfileName: String?
    let pairingError: String?
    let connectionError: String?
    let workingDirectory: String
    let projects: [Project]
    let threadLoadState: LoadState
    let threadLoadError: String?
    let threads: [ThreadSummary]
    let hasMoreProjects: Bool
    let visibleProjectCount: Int
    let loadingMoreThreads: Bool
    let loadingHistory: Bool
    let moreProjectIds: Set<String>
    let hasMoreChats: Bool
    let selectedThreadId: String?
    let isNewThread: Bool
    let notice: String?
    let interruptingTurnId: String?
}

extension ThreadSummary {
    var title: String {
        name.isEmpty ? (preview.isEmpty ? "無題のタスク" : preview) : name
    }

    var workingDirectory: String {
        cwd
    }

    var isActive: Bool {
        active
    }

    var hasUnreadCompletion: Bool {
        unread
    }
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

struct ConversationPresentation: Sendable {
    let source: AgentCore.Thread
    let id: String
    let title: String
    let turns: [TurnPresentation]
    let queuedMessages: [ConversationItem]
    let hasOlderTurns: Bool
}

struct TurnPresentation: Sendable {
    let id: String
    let turnId: String
    let hasOlderItems: Bool
    let openingUserMessage: ConversationItem?
    let status: String
    let isInProgress: Bool
    let userMessages: [ConversationItem]
    let activitySummary: String?
    let activityItems: [ConversationItem]
    let responses: [ConversationItem]
    let activityInitiallyExpanded: Bool
    let activityCanCollapse: Bool
    let error: TurnErrorPresentation?
    let pendingRequests: [Request]
}

extension Request {
    var paramsJson: String {
        JsonValue.object(fields: params).formatted
    }
}

final class ConversationItem: Sendable {
    let source: RenderedItem
    let id: String
    let nativeId: String?
    let kind: String
    let title: String
    let collapsedBody: String
    let markdown: [ConversationMarkdown.Block]
    let isCollapsible: Bool
    let isDeferred: Bool
    let imageSources: [String]
    var contentVersion: String {
        String(describing: ObjectIdentifier(self))
    }

    init(_ source: RenderedItem) {
        self.source = source
        let value = source.presentation()
        id = value.id; nativeId = value.nativeId; kind = value.kind; title = value.title
        collapsedBody = value.body; isCollapsible = value.collapsible; isDeferred = value.deferred
        imageSources = value.imageSources
        markdown = kind != "user" && !isCollapsible ? ConversationMarkdown.parse(collapsedBody) : []
    }

    func expandedBody() -> String {
        source.expandedBody()
    }
}

struct StagedAttachment: Identifiable {
    let id: Int
    let name: String
    let path: String
    let isImage: Bool
}

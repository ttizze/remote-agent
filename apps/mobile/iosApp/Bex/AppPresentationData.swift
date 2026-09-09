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

struct ConversationPresentation {
    let source: AgentCore.Thread
    let id: String
    let title: String
    let turns: [TurnPresentation]
    let queuedMessages: [ConversationItem]
    let hasOlderTurns: Bool
}

struct TurnPresentation {
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
    let pendingRequests: [RequestPresentation]
}

struct TurnErrorPresentation {
    let title: String
    let message: String
    let details: String?
    let isReconnecting: Bool
}

struct RequestPresentation {
    let source: Request
    var id: String {
        source.key
    }

    var params: [String: JsonValue] {
        source.params
    }

    var paramsJson: String {
        JsonValue.object(fields: params).formatted
    }

    var title: String {
        switch source.kind {
        case .commandApproval: "コマンドの承認待ち"
        case .fileApproval: "ファイル変更の承認待ち"
        case .permissions: "権限の承認待ち"
        case .questions: "回答待ち"
        case .elicitation: "MCPからの入力待ち"
        case .tool: "ツールの入力待ち"
        case .other: "Codexからの確認待ち"
        }
    }

    var body: String {
        params["questions"]?.array.first?["question"]?.string ?? params["reason"]?.string
            ?? params["message"]?.string ?? params["prompt"]?.string ?? "操作を続けるには応答が必要です"
    }
}

final class ConversationItem {
    let source: AgentCore.Item?
    let id: String
    let kind: String
    let title: String
    let collapsedBody: String
    let isCollapsible: Bool
    let isDeferred: Bool
    let imageSources: [String]
    var contentVersion: String {
        String(describing: ObjectIdentifier(self))
    }

    init(_ source: AgentCore.Item, deferred: Bool = false) {
        self.source = source
        id = source.clientId() ?? source.id()
        let presentation = source.presentation()
        kind = presentation.kind
        title = presentation.title
        isCollapsible = presentation.collapsible
        isDeferred = deferred
        let content = source.field(name: "content")?.array ?? []
        switch kind {
        case "user", "agent", "commentary":
            var text = source.text() ?? content.compactMap { $0["text"]?.string ?? $0.string }.joined()
            for part in content where part["type"]?.string == "mention" {
                if !text.isEmpty {
                    text += "\n"
                }
                text += "添付: \(part["name"]?.string ?? "") (\(part["path"]?.string ?? ""))"
            }
            collapsedBody = text
        case "reasoning": collapsedBody = "詳細を表示"
        case "imageGeneration": collapsedBody = title
        default: collapsedBody = source.status() ?? "詳細を表示"
        }
        if kind == "imageGeneration" {
            imageSources = [source.savedPath() ?? source.result()?.string.map { "data:image/png;base64," + $0 }]
                .compactMap(\.self).filter { !$0.isEmpty }
        } else {
            imageSources = content.compactMap {
                switch $0["type"]?.string {
                case "localImage": $0["path"]?.string
                case "image": $0["url"]?.string
                default: nil
                }
            }
        }
    }

    init(_ pending: PendingSubmission) {
        source = nil
        id = pending.id
        kind = "user"
        title = "You"
        isCollapsible = false
        isDeferred = false
        let files = pending.draft.attachments.filter { !$0.isImage }.map { "添付: \($0.name) (\($0.path))" }
        collapsedBody = ([pending.draft.text] + files).filter { !$0.isEmpty }.joined(separator: "\n")
        imageSources = pending.draft.attachments.filter(\.isImage).map(\.path)
    }

    func expandedBody() -> String {
        guard let source else { return collapsedBody }
        switch kind {
        case "user", "agent", "commentary": return collapsedBody
        case "reasoning":
            let summary = source.field(name: "summary")
            return summary?.string ?? summary?.array.compactMap { $0["text"]?.string ?? $0.string }
                .joined(separator: "\n") ?? ""
        case "command":
            let cwd = source.field(name: "cwd")?.string
            return [cwd.map { "cwd: " + $0 }, source.aggregatedOutput()].compactMap(\.self).joined(separator: "\n")
        case "fileChange":
            return (source.field(name: "changes")?.array ?? []).map {
                let kind = $0["kind"]?.string ?? $0["kind"]?["type"]?.string ?? ""
                return "\(kind): \($0["path"]?.string ?? "")\n\($0["diff"]?.string ?? "")"
            }.joined(separator: "\n\n")
        default: return JsonValue.object(fields: source.fields()).formatted
        }
    }
}

struct StagedAttachment: Identifiable {
    let id: Int
    let name: String
    let path: String
    let isImage: Bool
}

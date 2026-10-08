import AgentCore
import Foundation

extension AgentPushCenter {
    nonisolated static func deepLink(from userInfo: [AnyHashable: Any]) -> String? {
        if let value = userInfo["deepLink"] as? String {
            if isActivityOverviewDeepLink(value) {
                return value
            }
            if threadTarget(from: value) != nil {
                return value
            }
        }
        guard let environment = userInfo["environmentId"] as? String,
              let thread = userInfo["threadId"] as? String,
              !environment.isEmpty,
              !thread.isEmpty,
              let environment = environment.addingPercentEncoding(withAllowedCharacters: pathComponentCharacters),
              let thread = thread.addingPercentEncoding(withAllowedCharacters: pathComponentCharacters)
        else { return nil }
        return "remoteagent://threads/\(environment)/\(thread)"
    }

    nonisolated static func threadDeepLink(hostId: String, threadId: String) -> String {
        let host = hostId.addingPercentEncoding(withAllowedCharacters: pathComponentCharacters) ?? hostId
        let thread = threadId.addingPercentEncoding(withAllowedCharacters: pathComponentCharacters) ?? threadId
        return "remoteagent://threads/\(host)/\(thread)"
    }

    nonisolated static func isUsageDeepLink(_ value: String) -> Bool {
        guard let components = URLComponents(string: value),
              components.scheme == "remoteagent",
              components.host == "settings",
              components.percentEncodedPath == "/usage",
              components.user == nil,
              components.password == nil,
              components.port == nil,
              components.fragment == nil
        else { return false }
        let queryItems = components.queryItems ?? []
        return queryItems.count == 1 && queryItems[0].name == "tab" && queryItems[0].value == "limits"
    }

    nonisolated static func isActivityOverviewDeepLink(_ value: String) -> Bool {
        value == AgentCore.agentActivityOverviewDeepLink()
    }

    nonisolated static func threadTarget(from value: String) -> (hostId: String, threadId: String)? {
        guard value.utf16.count <= 512,
              let url = URL(string: value),
              url.scheme == "remoteagent",
              url.host == "threads",
              url.user == nil,
              url.password == nil,
              url.port == nil,
              url.query == nil,
              url.fragment == nil,
              let urlComponents = URLComponents(url: url, resolvingAgainstBaseURL: false) else { return nil }
        let pathComponents = urlComponents.percentEncodedPath.split(separator: "/", omittingEmptySubsequences: false)
        guard pathComponents.count == 3,
              pathComponents[0].isEmpty,
              let hostId = pathComponents[1].removingPercentEncoding,
              let threadId = pathComponents[2].removingPercentEncoding,
              validRouteSegment(hostId),
              validRouteSegment(threadId) else { return nil }
        return (hostId, threadId)
    }

    private nonisolated static func validRouteSegment(_ value: String) -> Bool {
        !value.isEmpty && value != "." && value != ".." && value.unicodeScalars.allSatisfy { scalar in
            scalar.value != 0x5C && !CharacterSet.controlCharacters.contains(scalar)
        }
    }

    private nonisolated static var pathComponentCharacters: CharacterSet {
        var characters = CharacterSet.alphanumerics
        characters.insert(charactersIn: "-._~")
        return characters
    }
}

import AgentCore
import Foundation

extension AgentPushCenter {
    nonisolated static func deepLink(from userInfo: [AnyHashable: Any]) -> String? {
        AgentCore.agentActivityNotificationDeepLink(
            value: userInfo["deepLink"] as? String,
            environmentId: userInfo["environmentId"] as? String,
            threadId: userInfo["threadId"] as? String
        )
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
}

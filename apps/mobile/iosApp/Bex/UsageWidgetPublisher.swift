import AgentCore
import Foundation
import WidgetKit

@MainActor
final class UsageWidgetPublisher {
    private var published: String?

    func publish(_ json: String) throws {
        guard json != published, let destination = UsageWidgetStorage.file,
              let data = json.data(using: .utf8) else { return }
        try data.write(to: destination, options: [.atomic, .completeFileProtectionUntilFirstUserAuthentication])
        published = json
        WidgetCenter.shared.reloadTimelines(ofKind: UsageWidgetStorage.kind)
    }
}

import Combine
import Foundation
import RemoteAgentMobile

@MainActor
final class BexConversationModel: ObservableObject {
    @Published var thread: IosThreadView?

    init(thread: IosThreadView?) {
        self.thread = thread
    }
}

struct StagedAttachment: Identifiable, Codable {
    var id = UUID()
    let name: String
    let path: String
    let isImage: Bool
}

func jsonString(_ value: [String: Any]) throws -> String {
    let data = try JSONSerialization.data(withJSONObject: value, options: [.sortedKeys])
    guard let result = String(bytes: data, encoding: .utf8)
    else { throw CocoaError(.fileReadInapplicableStringEncoding) }
    return result
}

func jsonObject(_ value: String) -> [String: Any] {
    (try? JSONSerialization.jsonObject(with: Data(value.utf8))) as? [String: Any] ?? [:]
}

struct DraftSubmission {
    let text: String
    let files: [StagedAttachment]
    let key: String
    let host: String
}

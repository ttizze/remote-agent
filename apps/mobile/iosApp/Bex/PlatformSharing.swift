import AgentCore
import Foundation
import SwiftUI
import UIKit

struct RemoteAgentSharePayload: Codable {
    let text: String
    let urls: [String]

    var content: ShareContent {
        ShareContent(text: text, urls: urls)
    }
}

enum RemoteAgentShareInbox {
    static let appGroupIdentifier = "group.com.ttizze.b-codex"
    private static let directoryName = "incoming-shares"

    struct Pending {
        let file: URL
        let content: ShareContent
    }

    private static func directory() -> URL? {
        FileManager.default
            .containerURL(forSecurityApplicationGroupIdentifier: appGroupIdentifier)?
            .appendingPathComponent(directoryName, isDirectory: true)
    }

    static func pending() -> [Pending] {
        guard let directory,
              let files = try? FileManager.default.contentsOfDirectory(
                  at: directory,
                  includingPropertiesForKeys: [.contentModificationDateKey],
                  options: [.skipsHiddenFiles]
              )
        else { return [] }
        return files
            .filter { $0.pathExtension == "json" }
            .sorted { $0.lastPathComponent < $1.lastPathComponent }
            .compactMap { file in
                guard let data = try? Data(contentsOf: file),
                      let payload = try? JSONDecoder().decode(RemoteAgentSharePayload.self, from: data)
                else { return nil }
                return Pending(file: file, content: payload.content)
            }
    }

    static func remove(_ file: URL) {
        try? FileManager.default.removeItem(at: file)
    }
}

struct SharedFile: Identifiable {
    let id = UUID()
    let url: URL
}

struct FileShareSheet: UIViewControllerRepresentable {
    let url: URL

    func makeUIViewController(context _: Context) -> UIActivityViewController {
        let controller = UIActivityViewController(activityItems: [url], applicationActivities: nil)
        controller.completionWithItemsHandler = { _, _, _, _ in
            try? FileManager.default.removeItem(at: url.deletingLastPathComponent())
        }
        return controller
    }

    func updateUIViewController(_: UIActivityViewController, context _: Context) {}
}

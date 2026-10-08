import Foundation
import UIKit
import UniformTypeIdentifiers

private struct RemoteAgentSharePayload: Codable {
    let text: String
    let urls: [String]
}

final class RemoteAgentShareViewController: UIViewController {
    private static let appGroupIdentifier = "group.com.ttizze.b-codex"
    private static let directoryName = "incoming-shares"
    private var hasProcessedInput = false

    override func viewDidAppear(_ animated: Bool) {
        super.viewDidAppear(animated)
        guard !hasProcessedInput else { return }
        hasProcessedInput = true
        collectInput()
    }

    private func collectInput() {
        let providers = (extensionContext?.inputItems as? [NSExtensionItem] ?? [])
            .flatMap { $0.attachments ?? [] }
        guard !providers.isEmpty else {
            finish()
            return
        }

        let group = DispatchGroup()
        let resultQueue = DispatchQueue(label: "remote-agent.share-input")
        var textByProvider = Array(repeating: "", count: providers.count)
        var urlsByProvider = Array(repeating: "", count: providers.count)
        for (index, provider) in providers.enumerated() {
            let type: UTType? = if provider.hasItemConformingToTypeIdentifier(UTType.url.identifier) {
                .url
            } else if provider.hasItemConformingToTypeIdentifier(UTType.plainText.identifier) {
                .plainText
            } else {
                nil
            }
            guard let type else { continue }
            group.enter()
            provider.loadItem(forTypeIdentifier: type.identifier, options: nil) { item, _ in
                defer { group.leave() }
                guard let item else { return }
                resultQueue.sync {
                    if type == .url, let value = Self.urlString(item) {
                        urlsByProvider[index] = value
                    } else if let value = Self.textValue(item) {
                        textByProvider[index] = value
                    }
                }
            }
        }
        group.notify(queue: .main) { [weak self] in
            guard let self else { return }
            let text = textByProvider.filter { !$0.isEmpty }.joined(separator: "\n")
            let urls = urlsByProvider.filter { !$0.isEmpty }
            write(RemoteAgentSharePayload(text: text, urls: urls))
        }
    }

    private func write(_ payload: RemoteAgentSharePayload) {
        guard let container = FileManager.default.containerURL(
            forSecurityApplicationGroupIdentifier: Self.appGroupIdentifier
        ) else {
            finish()
            return
        }
        do {
            let directory = container.appendingPathComponent(Self.directoryName, isDirectory: true)
            try FileManager.default.createDirectory(
                at: directory,
                withIntermediateDirectories: true,
                attributes: nil
            )
            let file = directory.appendingPathComponent("remote-agent-\(UUID().uuidString).json")
            let data = try JSONEncoder().encode(payload)
            try data.write(to: file, options: [.atomic])
        } catch {
            // The share is complete only after the durable handoff succeeds.
        }
        finish()
    }

    private func finish() {
        extensionContext?.completeRequest(returningItems: nil)
    }

    private static func urlString(_ item: NSSecureCoding) -> String? {
        if let url = item as? URL {
            return url.absoluteString
        }
        if let url = item as? NSURL {
            return url.absoluteString
        }
        return textValue(item)
    }

    private static func textValue(_ item: NSSecureCoding) -> String? {
        if let value = item as? String {
            return value
        }
        if let value = item as? NSString {
            return value as String
        }
        if let data = item as? Data {
            return String(data: data, encoding: .utf8)
        }
        if let data = item as? NSData {
            return String(data: data as Data, encoding: .utf8)
        }
        return nil
    }
}

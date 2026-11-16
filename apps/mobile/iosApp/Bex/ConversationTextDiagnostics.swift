#if AGENT_DEV_DIAGNOSTICS
    import Foundation
    import UIKit

    struct ConversationTextDiagnosticInput: Equatable {
        let content: ObjectIdentifier
        let size: CGSize
        let container: CGSize
        let offset: CGPoint
        let proposedWidth: CGFloat?
        let firstUnlaid: Int
    }

    /// Numeric layout data only: no text, paths, conversation IDs, or content fingerprints.
    struct ConversationTextDiagnostic: Codable, Sendable {
        let view: UUID
        let characters: Int
        let requiredEnd: Int
        let placedEnd: Int
        let firstUnlaid: Int
        let frame: CGSize
        let container: CGSize
        let contentSize: CGSize
        let offset: CGPoint
        let measured: CGSize?
        let scale: CGFloat
        let category: String
        let lastLineBottom: CGFloat?
        var referenceHeight: CGFloat?

        var incomplete: Bool {
            // A subpixel font line box can extend past a pixel-aligned frame without hiding its text.
            placedEnd < requiredEnd || lastLineBottom.map {
                $0 > frame.height + 1 / max(scale, 1)
            } == true
        }

        @MainActor static func referenceHeight(_ text: NSAttributedString, width: CGFloat) -> CGFloat {
            let storage = NSTextStorage(attributedString: text)
            let layout = NSLayoutManager()
            let container = NSTextContainer(size: CGSize(width: width, height: CGFloat.greatestFiniteMagnitude))
            container.lineFragmentPadding = 0
            storage.addLayoutManager(layout)
            layout.addTextContainer(container)
            layout.ensureLayout(for: container)
            return layout.usedRect(for: container).maxY
        }
    }

    /// A single baseline per launch, then anomalies and recoveries, bounded to two 256 KiB files.
    /// File I/O stays off the UI actor. The Dev xcconfig is the only app configuration enabling this code.
    actor ConversationTextDiagnosticLog {
        // An explicit debugging launch can capture normal layouts without increasing routine Dev logging.
        static let includesNormalLayouts = ProcessInfo.processInfo.environment["BEX_TEXT_LAYOUT_SNAPSHOT"] == "1"
        static let shared = ConversationTextDiagnosticLog(
            directory: FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
                .appendingPathComponent("diagnostics", isDirectory: true)
        )

        private struct Entry: Encodable {
            let time: Date
            let revision: String
            let system: String
            let event: String
            let layout: ConversationTextDiagnostic
        }

        private let directory: URL
        private let byteLimit: Int
        private var baselineRecorded = false

        init(directory: URL, byteLimit: Int = 256 * 1024) {
            self.directory = directory
            self.byteLimit = byteLimit
        }

        func record(_ metrics: ConversationTextDiagnostic, recovered: Bool = false, snapshot: Bool = false) {
            guard !baselineRecorded || metrics.incomplete || recovered || snapshot else { return }
            let entry = Entry(
                time: Date(),
                revision: Bundle.main.object(forInfoDictionaryKey: "BEXBuildRevision") as? String ?? "test",
                system: ProcessInfo.processInfo.operatingSystemVersionString,
                event: metrics
                    .incomplete ? "incomplete" : recovered ? "recovered" : snapshot ? "snapshot" : "baseline",
                layout: metrics
            )
            do {
                var data = try JSONEncoder().encode(entry)
                data.append(0x0A)
                try append(data)
                baselineRecorded = true
            } catch {
                // A failed diagnostic write must neither break the UI nor suppress the next attempt.
            }
        }

        private func append(_ data: Data) throws {
            let files = FileManager.default
            try files.createDirectory(at: directory, withIntermediateDirectories: true)
            let current = directory.appendingPathComponent("text-layout.jsonl")
            let previous = directory.appendingPathComponent("text-layout.previous.jsonl")
            if files.fileExists(atPath: current.path) {
                let size = try files.attributesOfItem(atPath: current.path)[.size] as? Int ?? 0
                if size + data.count > byteLimit {
                    if files.fileExists(atPath: previous.path) {
                        try files.removeItem(at: previous)
                    }
                    try files.moveItem(at: current, to: previous)
                }
            }
            if !files.fileExists(atPath: current.path) {
                try data.write(to: current, options: .atomic)
            } else {
                let file = try FileHandle(forWritingTo: current)
                defer { try? file.close() }
                try file.seekToEnd()
                try file.write(contentsOf: data)
            }
        }
    }
#endif

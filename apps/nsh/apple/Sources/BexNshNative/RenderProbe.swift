#if DEBUG && os(macOS)
    import AppKit

    /// Native paint timing. Normal debugging records geometry and timestamps;
    /// message text is restricted to explicitly enabled fixture tracing.
    @MainActor enum RenderProbe {
        static let enabled = ProcessInfo.processInfo.environment["BEX_ACCEPTANCE_OUTPUT"] != nil
        private static let logging = enabled && ProcessInfo.processInfo.environment["BEX_RENDER_TRACE"] == "1"
        static let timings = ProcessInfo.processInfo.environment["BEX_RENDER_TIMINGS"] == "1"

        struct Paints {
            private var entries: [(text: String, at: TimeInterval)] = []

            mutating func clear() { entries.removeAll(keepingCapacity: true) }

            mutating func update(previous: String, current: String) {
                // Paragraph spacing can change after the former last block.
                // The prefix text was already visible before that append.
                if !current.hasPrefix(previous) { clear() }
            }

            mutating func record(_ text: String, at: TimeInterval) {
                if entries.count == 64 { entries.removeFirst() }
                entries.append((text, at))
            }

            func first(for text: String, since: TimeInterval = -.infinity) -> TimeInterval? {
                entries.first { $0.at >= since && $0.text.contains(text) }?.at
            }

            /// A large wheel can cross rows whose heights are still estimates.
            /// Resolve an actual visible paragraph, not an assumed row number
            /// or a paint of some other paragraph with the same prefix.
            func firstVisible(prefix: String, in visible: String) -> TimeInterval? {
                guard let start = visible.range(of: prefix) else { return nil }
                let paragraph = String(visible[start.lowerBound...].prefix { $0 != "." && $0 != "\n" })
                return first(for: paragraph)
            }
        }

        static func record(_ stage: String, _ view: NSTextView, painted: String = "") {
            guard logging || timings else { return }
            var record: [String: Any] = [
                "stage": stage, "at": Date.timeIntervalSinceReferenceDate,
                "view": String(describing: Unmanaged.passUnretained(view).toOpaque()),
                "length": view.string.utf16.count,
                "width": view.bounds.width, "height": view.bounds.height,
                "visible": !view.visibleRect.isEmpty,
                "constraint": (view as? MessageTextView)?.document.maxWidth ?? 0,
                "clipY": view.visibleRect.origin.y, "clipHeight": view.visibleRect.height,
            ]
            if let document = (view as? MessageTextView)?.document {
                record["blocks"] = document.blocks.count
                record["code"] = document.blocks.first?.code ?? false
                record["cell"] = document.cell ?? ""
            }
            if logging {
                record["text"] = String(view.string.prefix(100))
                record["painted"] = String(painted.prefix(200))
            }
            guard let data = try? JSONSerialization.data(withJSONObject: record),
                  let line = String(data: data, encoding: .utf8) else { return }
            FileHandle.standardError.write(Data("render-probe \(line)\n".utf8))
        }
    }
#endif

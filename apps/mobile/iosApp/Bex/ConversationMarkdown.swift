import SwiftUI
import UniformTypeIdentifiers

/// Foundation parses block structure and inline Markdown; no HTML/web view is involved.
struct ConversationMarkdown: View {
    let blocks: [Block]
    let model: BexAppViewModel
    @State private var linkTarget: URL?
    @State private var previewURL: URL?
    @State private var previewDirectory: URL?
    @State private var previewSource: String?
    @State private var linkError: String?
    struct Block: Identifiable, Sendable {
        let id: Int
        let content: AttributedString
        let style: ParagraphStyle
        let imageURL: URL?
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            ForEach(blocks) { block in
                if let imageURL = block.imageURL {
                    ConversationImage(
                        source: imageURL.scheme == nil ? imageURL.path : imageURL.absoluteString,
                        label: String(block.content.characters),
                        identifier: "markdown.image.\(block.id)",
                        model: model
                    )
                } else if block.style.code {
                    ScrollView(.horizontal) {
                        Text(block.content).font(.system(.subheadline, design: .monospaced))
                            .textSelection(.enabled).padding(12)
                    }.background(Color(UIColor.secondarySystemBackground), in: RoundedRectangle(cornerRadius: 12))
                } else {
                    HStack(alignment: .firstTextBaseline, spacing: 10) {
                        if let marker = block.style.marker {
                            Text(marker).frame(minWidth: 14, alignment: .leading)
                        }
                        if block.style.quoted {
                            Rectangle().fill(Color.secondary).frame(width: 2)
                        }
                        Text(block.content)
                            .font(block.style.header == nil ? .system(size: 18) : .system(
                                size: block.style.header == 1 ? 25 : 21,
                                weight: .semibold
                            ))
                            .lineSpacing(5).textSelection(.enabled)
                            .frame(maxWidth: .infinity, alignment: .leading)
                    }
                }
            }
            if let linkError {
                Text(linkError).font(.caption).foregroundColor(.red)
            }
        }
        .font(.system(size: 18))
        .tint(.accentColor)
        .environment(\.openURL, OpenURLAction { url in
            if url.scheme == "https" || url.scheme == "http" {
                return .systemAction
            }
            do {
                linkError = nil
                linkTarget = try conversationFileURL(url.absoluteString, cwd: model.cwd)
            } catch { linkError = "リンクを開けません: \(error.localizedDescription)" }
            return .handled
        })
        .sheet(isPresented: Binding(get: { previewURL != nil }, set: {
            if !$0 {
                previewURL = nil
            }
        })) {
            if let url = previewURL {
                ConversationPreview(
                    url: url,
                    isImage: UTType(filenameExtension: url.pathExtension)?.conforms(to: .image) == true,
                    model: model,
                    source: previewSource
                ) {
                    previewURL = nil
                }
            }
        }
        .onChange(of: previewURL) { value in
            if value == nil, let directory = previewDirectory {
                try? FileManager.default.removeItem(at: directory)
                previewDirectory = nil
            }
        }
        .task(id: linkTarget) {
            guard let target = linkTarget else { return }
            let host = model.selectedProfileId
            let (downloaded, error) = await withCheckedContinuation { continuation in
                model.download(target.path) { url, error in continuation.resume(returning: (url, error)) }
            }
            guard !Task.isCancelled, host == model.selectedProfileId else {
                if let downloaded {
                    try? FileManager.default.removeItem(at: downloaded.deletingLastPathComponent())
                }
                return
            }
            if let downloaded {
                previewDirectory = downloaded.deletingLastPathComponent()
                previewSource = target.path
                previewURL = downloaded
            } else {
                linkError = error ?? "ファイルを取得できません"
            }
            linkTarget = nil
        }
    }

    nonisolated static func parse(_ text: String) -> [Block] {
        guard let document = try? AttributedString(markdown: text) else {
            return [Block(id: 0, content: AttributedString(text), style: ParagraphStyle([]), imageURL: nil)]
        }
        var result: [Block] = []
        var start: AttributedString.Index?
        var end: AttributedString.Index?
        var paragraphID: Int?
        var imageURL: URL?
        var style = ParagraphStyle([])
        for run in document.runs {
            let components = run.presentationIntent?.components ?? []
            let identity = components.first?.identity ?? 0
            if paragraphID == identity, imageURL == run.imageURL {
                end = run.range.upperBound
                continue
            }
            if let start, let end {
                result.append(Block(id: result.count,
                                    content: AttributedString(document[start ..< end]), style: style,
                                    imageURL: imageURL))
            }
            start = run.range.lowerBound
            end = run.range.upperBound
            paragraphID = identity
            imageURL = run.imageURL
            style = ParagraphStyle(components)
        }
        if let start, let end {
            result.append(Block(id: result.count,
                                content: AttributedString(document[start ..< end]), style: style, imageURL: imageURL))
        }
        return result
    }

    struct ParagraphStyle: Sendable {
        var header: Int?
        var marker: String?
        var code = false
        var quoted = false

        init(_ components: [PresentationIntent.IntentType]) {
            var ordinal: Int?
            var ordered = false
            for component in components {
                switch component.kind {
                case let .header(level: level): header = level
                case let .listItem(ordinal: number): ordinal = number
                case .orderedList: ordered = true
                case .codeBlock: code = true
                case .blockQuote: quoted = true
                default: break
                }
            }
            marker = ordinal.map { ordered ? "\($0)." : "•" }
        }
    }
}

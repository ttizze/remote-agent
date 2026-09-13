import AgentCore
import SwiftUI
import UniformTypeIdentifiers

/// Foundation parses block structure and inline Markdown; no HTML/web view is involved.
struct ConversationMarkdown: View {
    let blocks: [Part]
    let media: ConversationMediaAccess
    let selection: ConversationSelectionActions
    @State private var linkTarget: URL?
    @State private var previewURL: URL?
    @State private var previewDirectory: URL?
    @State private var previewSource: String?
    @State private var linkError: String?
    struct Block: Identifiable, Sendable, Equatable {
        let id: Int
        let content: AttributedString
        let style: ParagraphStyle
        let imageURL: URL?
    }

    struct Part: Identifiable, Sendable {
        let id: Int
        let blocks: [Block]
        var image: Block? {
            blocks.first.flatMap { $0.imageURL == nil ? nil : $0 }
        }

        var isCode: Bool {
            blocks.first?.style.code == true
        }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            ForEach(blocks) { part in
                if let block = part.image, let imageURL = block.imageURL {
                    ConversationImage(
                        source: SessionImage(reference: imageURL.scheme == nil ? imageURL.path : imageURL
                            .absoluteString),
                        label: String(block.content.characters),
                        identifier: "markdown.image.\(block.id)",
                        media: media
                    )
                } else if part.isCode {
                    ScrollView(.horizontal) {
                        AssistantSelectableText(blocks: part.blocks, actions: selection)
                            .fixedSize(horizontal: true, vertical: false).padding(12)
                    }.background(Color(UIColor.secondarySystemBackground), in: RoundedRectangle(cornerRadius: 12))
                } else {
                    AssistantSelectableText(blocks: part.blocks, actions: selection)
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
                linkTarget = try conversationFileURL(url.absoluteString, cwd: media.cwd)
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
                    media: media,
                    source: previewSource.map(SessionImage.init(reference:))
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
            do {
                let downloaded = try await media.download(target.path)
                guard !Task.isCancelled else {
                    try? FileManager.default.removeItem(at: downloaded.deletingLastPathComponent())
                    return
                }
                previewDirectory = downloaded.deletingLastPathComponent()
                previewSource = target.path
                previewURL = downloaded
            } catch {
                if !Task.isCancelled {
                    linkError = error.localizedDescription
                }
            }
            linkTarget = nil
        }
    }

    nonisolated static func parse(_ text: String) -> [Part] {
        guard let document = try? AttributedString(markdown: text) else {
            return [Part(
                id: 0,
                blocks: [Block(id: 0, content: AttributedString(text), style: ParagraphStyle([]), imageURL: nil)]
            )]
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
        return parts(result)
    }

    private nonisolated static func parts(_ blocks: [Block]) -> [Part] {
        var parts: [Part] = []
        var paragraphs: [Block] = []
        for block in blocks {
            if block.imageURL != nil || block.style.code {
                if let first = paragraphs.first {
                    parts.append(Part(id: first.id, blocks: paragraphs))
                    paragraphs.removeAll(keepingCapacity: true)
                }
                parts.append(Part(id: block.id, blocks: [block]))
            } else {
                paragraphs.append(block)
            }
        }
        if let first = paragraphs.first {
            parts.append(Part(id: first.id, blocks: paragraphs))
        }
        return parts
    }

    struct ParagraphStyle: Sendable, Equatable {
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

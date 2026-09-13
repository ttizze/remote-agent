import SwiftUI
import UniformTypeIdentifiers

/// Shared Markdown semantics render as native selectable content.
struct ConversationMarkdown: View {
    let blocks: [ConversationMarkdownContent.Part]
    let model: BexAppViewModel
    @ScaledMetric(relativeTo: .body) private var tableColumnWidth = 220.0
    @State private var linkTarget: URL?
    @State private var previewURL: URL?
    @State private var previewDirectory: URL?
    @State private var previewSource: String?
    @State private var linkError: String?
    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            ForEach(blocks) { part in
                if !part.tableRows.isEmpty {
                    table(part)
                } else if let block = part.image, let imageURL = block.imageURL {
                    image(block, url: imageURL)
                } else if part.isCode {
                    ScrollView(.horizontal) {
                        AssistantSelectableText(blocks: part.blocks, model: model)
                            .fixedSize(horizontal: true, vertical: false).padding(12)
                    }.background(Color(UIColor.secondarySystemBackground), in: RoundedRectangle(cornerRadius: 12))
                } else {
                    AssistantSelectableText(blocks: part.blocks, model: model)
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

    private func image(_ block: ConversationMarkdownContent.Block, url: URL) -> some View {
        ConversationImage(
            source: url.scheme == nil ? url.path : url.absoluteString,
            label: block.runs.map(\.text).joined(),
            identifier: "markdown.image.\(block.id)",
            model: model
        )
    }

    private func table(_ part: ConversationMarkdownContent.Part) -> some View {
        let rows = part.tableRows
        return ScrollView(.horizontal) {
            VStack(alignment: .leading, spacing: 0) {
                ForEach(rows.indices, id: \.self) { row in
                    HStack(alignment: .top, spacing: 0) {
                        ForEach(rows[row].indices, id: \.self) { column in
                            VStack(alignment: .leading, spacing: 0) {
                                ForEach(rows[row][column]) { block in
                                    if let url = block.imageURL {
                                        image(block, url: url)
                                    } else {
                                        AssistantSelectableText(blocks: [block], model: model)
                                    }
                                }
                            }
                            .frame(width: tableColumnWidth)
                            .padding(10)
                            .accessibilityIdentifier("markdown.cell.\(part.id).\(row).\(column)")
                        }
                    }
                    .background(row == 0 ? Color(UIColor.secondarySystemBackground) : Color.clear)
                    Divider()
                }
            }
            .overlay(Rectangle().stroke(Color(UIColor.separator), lineWidth: 0.5))
        }
    }
}

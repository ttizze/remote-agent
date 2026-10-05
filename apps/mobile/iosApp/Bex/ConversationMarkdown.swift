import AgentCore
import SwiftUI
import UIKit
import UniformTypeIdentifiers

/// Shared Markdown semantics render as native selectable content.
struct ConversationMarkdown: View {
    let blocks: [ConversationMarkdownContent.Part]
    let media: ConversationMediaAccess
    let selection: ConversationSelectionActions
    @ScaledMetric(relativeTo: .body) private var tableColumnWidth = 220.0
    @State private var linkTarget: URL?
    @State private var previewURL: URL?
    @State private var previewDirectory: URL?
    @State private var linkError: String?
    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            ForEach(blocks) { part in
                if let path = part.visualizationPath {
                    ConversationVisualization(path: path, media: media)
                } else if !part.tableRows.isEmpty {
                    table(part)
                } else if let block = part.image, let imageURL = block.imageURL {
                    image(block, url: imageURL)
                } else if part.isCode {
                    ConversationMarkdownCodeBlock(part: part, selection: selection)
                } else {
                    AssistantSelectableText(blocks: part.blocks, actions: selection)
                }
            }
            if let linkError {
                Text(linkError).font(.caption).foregroundColor(.red)
            }
        }
        .font(.custom("DMSans-Regular", size: 16, relativeTo: .body))
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
                    isImage: UTType(filenameExtension: url.pathExtension)?.conforms(to: .image) == true
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
                previewURL = downloaded
            } catch {
                if !Task.isCancelled {
                    linkError = error.localizedDescription
                }
            }
            linkTarget = nil
        }
    }

    private func image(_ block: ConversationMarkdownContent.Block, url: URL) -> some View {
        ConversationImage(
            source: SessionImage(reference: url.scheme == nil ? url.path : url.absoluteString),
            label: block.runs.map(\.text).joined(),
            identifier: "markdown.image.\(block.id)",
            media: media
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
                                        AssistantSelectableText(blocks: [block], actions: selection)
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

private struct ConversationMarkdownCodeBlock: View {
    let part: ConversationMarkdownContent.Part
    let selection: ConversationSelectionActions
    @State private var copied = false

    private var text: String {
        part.blocks
            .map { $0.runs.map(\.text).joined() }
            .joined(separator: "\n")
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack {
                Spacer(minLength: 0)
                Button {
                    UIPasteboard.general.string = text
                    copied = true
                } label: {
                    Image(systemName: copied ? "checkmark" : "doc.on.doc")
                }
                .font(.system(size: 19))
                .foregroundColor(.secondary)
                .buttonStyle(.plain)
                .accessibilityLabel(copied ? "コピーしました" : "コードをコピー")
                .accessibilityIdentifier("markdown.code.copy.\(part.id)")
            }
            .padding(.horizontal, 8)
            .padding(.top, 4)
            ScrollView(.horizontal) {
                AssistantSelectableText(blocks: part.blocks, actions: selection)
                    .fixedSize(horizontal: true, vertical: false)
                    .padding(12)
            }
        }
        .background(Color(UIColor.secondarySystemBackground), in: RoundedRectangle(cornerRadius: 12))
    }
}

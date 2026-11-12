#if !BEX_NSH_CONVERSATION
    import AgentCore
    import SwiftUI
    import UIKit

    /// Shared Markdown semantics render as native selectable content.
    struct ConversationMarkdown: View {
        let blocks: [ConversationMarkdownContent.Part]
        let media: ConversationMediaAccess
        let selection: ConversationSelectionActions
        @ScaledMetric(relativeTo: .body) private var tableColumnWidth = 160.0
        var textIdentifier: String = "message.assistant-text"
        @State private var linkTarget: String?
        @State private var sharedPath: String?
        var body: some View {
            let shareFailure = media.presentation.shareFailure
            let linkFailure = media.presentation.linkFailure
            let linkError = linkFailure?.path == linkTarget ? linkFailure?.message : nil
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
                        AssistantSelectableText(
                            blocks: part.blocks,
                            actions: selection,
                            cwd: media.cwd,
                            home: media.home,
                            saveFile: shareFile,
                            identifier: textIdentifier
                        )
                    }
                }
                if let error = linkError ?? (shareFailure?.path == sharedPath ? shareFailure?.message : nil) {
                    ErrorMessage(source: error).font(.caption).foregroundColor(.red)
                }
            }
            .font(.system(size: 18))
            .tint(.accentColor)
            .environment(\.openURL, OpenURLAction { url in
                if url.scheme == "https" || url.scheme == "http" {
                    return .systemAction
                }
                guard markdownFileTarget(source: url.absoluteString, cwd: media.cwd, surface: .mobile) != nil else {
                    return .systemAction
                }
                linkTarget = url.absoluteString
                sharedPath = nil
                media.presentation.openLink(url.absoluteString, resolve: media.resolve,
                                            download: media.download,
                                            openDirectory: media.openDirectory, openFile: media.openFile)
                return .handled
            })
        }

        private func shareFile(_ path: String) {
            sharedPath = path
            linkTarget = nil
            media.presentation.share(path, download: media.download)
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
            return VStack(alignment: .trailing, spacing: 6) {
                Menu {
                    Button("Markdownをコピー") { UIPasteboard.general.string = part.tableMarkdown }
                    Button("CSVをコピー") { UIPasteboard.general.string = part.tableCSV }
                } label: {
                    Image(systemName: "doc.on.doc").foregroundStyle(.secondary)
                }
                .accessibilityLabel("表をコピー")
                ScrollView(.horizontal) {
                    VStack(alignment: .leading, spacing: 0) {
                        ForEach(rows.indices, id: \.self) { row in
                            HStack(alignment: .top, spacing: 0) {
                                ForEach(rows[row].indices, id: \.self) { column in
                                    VStack(alignment: .leading, spacing: 0) {
                                        ForEach(rows[row][column]) { block in
                                            if let url = block.imageURL {
                                                image(block, url: url)
                                            } else {
                                                AssistantSelectableText(
                                                    blocks: [block],
                                                    actions: selection,
                                                    cwd: media.cwd,
                                                    home: media.home,
                                                    saveFile: shareFile,
                                                    identifier: "markdown.cell.\(part.id).\(row).\(column)"
                                                )
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
                    .clipShape(RoundedRectangle(cornerRadius: 8))
                    .overlay(RoundedRectangle(cornerRadius: 8).stroke(Color(UIColor.separator), lineWidth: 0.5))
                }
            }
        }
    }

    private struct ConversationMarkdownCodeBlock: View {
        let part: ConversationMarkdownContent.Part
        let selection: ConversationSelectionActions
        @State private var copied = false
        @State private var wrapped = false

        private var text: String {
            part.blocks
                .map { $0.runs.map(\.text).joined() }
                .joined(separator: "\n")
        }

        var body: some View {
            VStack(alignment: .leading, spacing: 0) {
                HStack(spacing: 12) {
                    if let filename = part.blocks.first?.style.filename {
                        Label(filename, systemImage: "doc.text").lineLimit(1).truncationMode(.middle)
                    } else {
                        Text((part.blocks.first?.style.language ?? "CODE").uppercased())
                    }
                    Spacer(minLength: 0)
                    Button { wrapped.toggle() } label: {
                        Image(systemName: wrapped ? "text.alignleft" : "arrow.right.to.line")
                    }
                    .accessibilityLabel(LocalizedStringKey(wrapped ? "コードの折り返しを解除" : "コードを折り返す"))
                    Button {
                        UIPasteboard.general.string = text
                        copied = true
                    } label: {
                        Image(systemName: copied ? "checkmark" : "doc.on.doc")
                    }
                    .font(.system(size: 16))
                    .foregroundColor(.secondary)
                    .buttonStyle(.plain)
                    .accessibilityLabel(LocalizedStringKey(copied ? "コピーしました" : "コードをコピー"))
                    .accessibilityIdentifier("markdown.code.copy.\(part.id)")
                }
                .font(.caption.weight(.medium))
                .foregroundStyle(.secondary)
                .padding(.horizontal, 12)
                .padding(.vertical, 10)
                Divider()
                if wrapped {
                    AssistantSelectableText(blocks: part.blocks, actions: selection).padding(12)
                } else {
                    ScrollView(.horizontal) {
                        AssistantSelectableText(blocks: part.blocks, actions: selection)
                            .fixedSize(horizontal: true, vertical: false)
                            .padding(12)
                    }
                }
            }
            .background(Color(UIColor.secondarySystemBackground), in: RoundedRectangle(cornerRadius: 12))
            .overlay(RoundedRectangle(cornerRadius: 12).stroke(Color(UIColor.separator), lineWidth: 0.5))
            .task(id: copied) {
                if copied {
                    try? await Task.sleep(for: .milliseconds(1200))
                    if !Task.isCancelled {
                        copied = false
                    }
                }
            }
        }
    }

#endif

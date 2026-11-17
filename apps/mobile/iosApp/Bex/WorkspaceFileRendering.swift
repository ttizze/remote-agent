import AgentCore
import SwiftUI
import UIKit

/// T3 Code's mobile `FileMarkdownPreview`: the file as a message renders it, 18 points in
/// and at most 760 wide; pulling it down reads the file again.
struct FileMarkdownPreview: View {
    let text: String
    let path: String
    let media: ConversationMediaAccess
    let addToChat: (String) -> Void
    let refresh: () async -> Void
    @State private var parts: [ConversationMarkdownContent.Part] = []

    var body: some View {
        ScrollView {
            ConversationMarkdown(
                blocks: parts,
                media: media,
                selection: ConversationSelectionActions(addToChat: addToChat, askInSideChat: nil),
                textIdentifier: "file.preview.markdown"
            )
            .frame(maxWidth: 760, alignment: .leading)
            .frame(maxWidth: .infinity)
            .padding(18)
        }
        .refreshable { await refresh() }
        .task(id: text) {
            let (text, path) = (text, path)
            let parsed = await Task.detached(priority: .userInitiated) {
                ConversationMarkdownContent.parts(mobileFileMarkdownBlocks(source: text, path: path))
            }.value
            if !Task.isCancelled {
                parts = parsed
            }
        }
    }
}

/// T3 Code's mobile `DelimitedFilePreview`: 12-rem cells with hairlines, the first row
/// heading the columns, and a notice when the table leaves some of the file out.
struct FileTablePreview: View {
    let table: MobileFileTable

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 0) {
                if table.truncated {
                    AppText("表は最初の100行・30列までです。続きはソースで確認できます。")
                        .font(.caption).foregroundStyle(.secondary).padding()
                }
                ScrollView(.horizontal) {
                    VStack(alignment: .leading, spacing: 0) {
                        ForEach(table.rows.indices, id: \.self) { row in
                            HStack(alignment: .top, spacing: 0) {
                                ForEach(table.rows[row].indices, id: \.self) { column in
                                    Text(verbatim: table.rows[row][column])
                                        .font(row == 0 ? .subheadline.weight(.semibold) : .subheadline)
                                        .textSelection(.enabled)
                                        .padding(.horizontal, 12).padding(.vertical, 8)
                                        .frame(width: 192, alignment: .leading)
                                        .frame(maxHeight: .infinity, alignment: .top)
                                        .overlay(alignment: .trailing) { hairline.frame(width: 0.5) }
                                }
                            }
                            .fixedSize(horizontal: false, vertical: true)
                            .background(row == 0 ? Color(UIColor.secondarySystemBackground) : Color.clear)
                            .overlay(alignment: .bottom) { hairline.frame(height: 0.5) }
                        }
                    }
                }
            }
        }
        .accessibilityIdentifier("file.preview.table")
    }

    private var hairline: some View {
        Rectangle().fill(Color(UIColor.separator))
    }
}

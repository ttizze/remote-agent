import AgentCore
import SwiftUI
import UIKit

struct ConversationMarkdown: View {
    let source: String
    @State private var blocks: [MarkdownBlock] = []
    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            ForEach(Array(blocks.enumerated()), id: \.offset) { _, block in
                switch block {
                case let .paragraph(runs, style):
                    if style.code {
                        VStack(alignment: .leading, spacing: 6) {
                            HStack {
                                Spacer(); Button("Copy") { UIPasteboard.general.string = runs.map(\.text).joined() }
                                    .font(AppTheme.font(11))
                            }
                            ScrollView(.horizontal) { Text(runs.map(\.text).joined()).font(.system(
                                size: 13,
                                design: .monospaced
                            )).textSelection(.enabled) }
                        }.padding(12).background(
                            AppTheme.color("mobileMarkdownCode"),
                            in: RoundedRectangle(cornerRadius: 10)
                        )
                        .overlay(RoundedRectangle(cornerRadius: 10).stroke(AppTheme.border))
                    } else {
                        HStack(alignment: .top, spacing: 8) {
                            if let marker = style
                                .marker {
                                Text(marker).font(AppTheme.font(16)).foregroundStyle(AppTheme.tertiary)
                            }
                            Text(attributed(runs, header: style.header)).lineSpacing(4).textSelection(.enabled)
                                .frame(maxWidth: .infinity, alignment: .leading)
                        }.padding(.leading, style.quoted ? 12 : 0)
                            .overlay(alignment: .leading) {
                                if style.quoted {
                                    Rectangle().fill(AppTheme.border).frame(width: 2)
                                }
                            }
                    }
                case let .table(_, rows):
                    ScrollView(.horizontal) {
                        Grid(alignment: .leading, horizontalSpacing: 16, verticalSpacing: 10) {
                            ForEach(Array(rows.enumerated()), id: \.offset) { _, cells in
                                GridRow { ForEach(Array(cells.enumerated()), id: \.offset) { _, cell in Text(attributed(
                                    cell.runs,
                                    header: nil
                                )).font(AppTheme.font(12)).textSelection(.enabled) } }
                            }
                        }.padding(12).background(AppTheme.card, in: RoundedRectangle(cornerRadius: 8))
                    }
                case let .visualization(path):
                    Text(path).font(AppTheme.font(12)).foregroundStyle(AppTheme.tertiary)
                        .textSelection(.enabled)
                }
            }
        }.tint(AppTheme.color("mobileMarkdownLink"))
            .task(id: source) {
                let parsed = await Task.detached(priority: .userInitiated) { markdownBlocks(source: source) }.value
                guard !Task.isCancelled else { return }
                blocks = parsed
            }
    }

    private func attributed(_ runs: [MarkdownRun], header: UInt8?) -> AttributedString {
        var result = AttributedString()
        let size: CGFloat = header == 1 ? 21 : header == 2 ? 19 : header == 3 ? 17 : header != nil ? 15 : 16
        for run in runs {
            var text = AttributedString(run.text)
            text.font = run.code ? .system(size: 13, design: .monospaced) : AppTheme.font(
                size,
                weight: run.strong || header != nil ? .bold : .regular
            )
            if run.emphasis {
                text.inlinePresentationIntent = .emphasized
            }
            if run.strikethrough {
                text.strikethroughStyle = .single
            }
            if let link = run.link, let url = URL(string: link),
               ["http", "https", "mailto"].contains(url.scheme?.lowercased() ?? "") {
                text.link = url
            }
            result += text
        }
        return result
    }
}

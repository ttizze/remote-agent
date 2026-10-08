import AgentCore
import SwiftUI
import UIKit

struct ConversationMarkdown: View {
    let source: String
    /// Set where a card's "Use template" can reach the composer; plans and reasoning show cards without it.
    var useArtifactTemplate: ((ArtifactTemplate) -> Void)?
    @State private var blocks: [MarkdownBlock] = []
    @Environment(\.markdownLinks) private var links
    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            ForEach(Array(blocks.enumerated()), id: \.offset) { _, block in
                switch block {
                case let .paragraph(runs, style):
                    if style.code {
                        VStack(alignment: .leading, spacing: 6) {
                            HStack {
                                Spacer(); Button("Copy") { Haptics.copy(runs.map(\.text).joined()) }
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
                        let text = runs.filter { $0.image == nil }
                        if !text.isEmpty {
                            HStack(alignment: .top, spacing: 8) {
                                if let marker = style
                                    .marker {
                                    Text(marker).font(AppTheme.font(16)).foregroundStyle(AppTheme.tertiary)
                                }
                                Text(attributed(text, header: style.header)).lineSpacing(4).textSelection(.enabled)
                                    .frame(maxWidth: .infinity, alignment: .leading)
                            }.padding(.leading, style.quoted ? 12 : 0)
                                .overlay(alignment: .leading) {
                                    if style.quoted {
                                        Rectangle().fill(AppTheme.border).frame(width: 2)
                                    }
                                }
                        }
                        ForEach(Array(runs.enumerated()), id: \.offset) { _, run in
                            if let image = run.image {
                                MarkdownImage(href: image, alt: run.text)
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
                case let .artifactTemplate(template):
                    ArtifactTemplateCard(template: template, onUse: useArtifactTemplate)
                }
            }
        }.tint(AppTheme.color("mobileMarkdownLink"))
            .environment(\.openURL, OpenURLAction { MarkdownLinkURL.open($0, links: links) })
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
            if let link = run.link, let url = MarkdownLinkURL.url(for: link) {
                text.link = url
            }
            result += text
        }
        return result
    }
}

/// A `::artifact-template` card: the template's icon with a sparkle badge, its name and kind,
/// and "Use template" when the card can reach the composer.
struct ArtifactTemplateCard: View {
    let template: ArtifactTemplate
    let onUse: ((ArtifactTemplate) -> Void)?
    private static let badge = Color(red: 0.851, green: 0.275, blue: 0.937)

    var body: some View {
        HStack(spacing: 12) {
            ZStack(alignment: .bottomTrailing) {
                Image(systemName: symbolName)
                    .font(.system(size: 20))
                    .foregroundStyle(AppTheme.muted)
                    .frame(width: 40, height: 40)
                    .background(AppTheme.subtle, in: RoundedRectangle(cornerRadius: 12))
                    .overlay(RoundedRectangle(cornerRadius: 12).stroke(AppTheme.border))
                Image(systemName: "sparkles")
                    .font(.system(size: 9))
                    .foregroundStyle(.white)
                    .frame(width: 16, height: 16)
                    .background(Self.badge, in: Circle())
                    .offset(x: 4, y: 4)
            }
            VStack(alignment: .leading, spacing: 2) {
                Text(template.displayName).font(AppTheme.font(14, weight: .bold)).foregroundStyle(AppTheme.text)
                    .lineLimit(1)
                Text(artifactTemplatePresentationLabel(kind: template.artifactKind))
                    .font(AppTheme.font(12)).foregroundStyle(AppTheme.muted)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            if let onUse {
                Button { onUse(template) } label: {
                    Text("Use template").font(AppTheme.font(12, weight: .bold)).foregroundStyle(AppTheme.text)
                        .padding(.horizontal, 12).frame(minHeight: 36)
                        .background(AppTheme.subtle, in: RoundedRectangle(cornerRadius: 8))
                        .overlay(RoundedRectangle(cornerRadius: 8).stroke(AppTheme.border))
                }
                .buttonStyle(.plain)
                .accessibilityLabel("Use \(template.displayName) template")
            }
        }
        .padding(12)
        .background(AppTheme.card, in: RoundedRectangle(cornerRadius: 16))
        .overlay(RoundedRectangle(cornerRadius: 16).stroke(AppTheme.border))
        .padding(.vertical, 8)
    }

    private var symbolName: String {
        switch artifactTemplateSymbol(kind: template.artifactKind) {
        case .document: "doc.text"
        case .chart: "chart.bar.xaxis"
        case .browser: "safari"
        case .camera: "camera"
        case .message: "text.bubble"
        }
    }
}

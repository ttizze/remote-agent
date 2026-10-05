import AgentCore
import SwiftUI

struct ConversationRow: View, Equatable {
    static func == (lhs: Self, rhs: Self) -> Bool {
        lhs.row == rhs.row
    }

    let perform: (Intent) -> Void
    let downloadAttachment: (String, String) async throws -> URL
    let row: TimelineRow
    @State private var expanded = false
    var body: some View {
        switch row.kind {
        case .user:
            HStack {
                Spacer(minLength: 24)
                VStack(alignment: .leading, spacing: 4) {
                    if !row.attachments.isEmpty {
                        ConversationAttachmentStrip(
                            attachments: row.attachments,
                            download: downloadAttachment
                        )
                    }
                    if !row.title.isEmpty {
                        Text(row.title).font(T3Theme.font(11)).foregroundStyle(T3Theme.color("textMuted"))
                    }
                    Text(row.text).font(T3Theme.font(16)).lineSpacing(4).textSelection(.enabled)
                }.padding(12).background(T3Theme.color("mobileUserBubble"), in: RoundedRectangle(cornerRadius: 14))
            }
        case .assistant:
            VStack(alignment: .leading, spacing: 8) {
                ConversationMarkdown(source: row.text)
                if row.streaming {
                    ProgressView().controlSize(.mini)
                }
                if !row
                    .streaming {
                    HStack(spacing: 16) {
                        Button { UIPasteboard.general.string = row.text } label: { Image(systemName: "doc.on.doc") }
                        if let source = row.forkSourceThreadId, let run = row.runId {
                            Button { perform(.fork(sourceThreadId: source, runId: run)) } label: {
                                Image(systemName: "arrow.triangle.branch")
                            }
                            .accessibilityLabel("Fork from here")
                        }
                    }.font(T3Theme.font(12)).foregroundStyle(T3Theme.color("textMuted"))
                }
            }.frame(maxWidth: .infinity, alignment: .leading)
        case .approval, .question:
            EmptyView()
        case .work:
            DisclosureGroup(isExpanded: $expanded) {
                ForEach(row.work, id: \.id) { work in
                    VStack(alignment: .leading, spacing: 4) {
                        Text(work.title).font(T3Theme.font(13, weight: .medium))
                        if !work.detail
                            .isEmpty {
                            Text(work.detail).font(.system(size: 12, design: .monospaced)).textSelection(.enabled)
                        }
                        Text(work.status).font(T3Theme.font(11)).foregroundStyle(T3Theme.color("textMuted"))
                        if let id = work
                            .childThreadId {
                            Button("Open subagent thread") { perform(.openThread(threadId: id)) }
                                .font(T3Theme.font(12))
                        }
                    }.padding(.vertical, 6).frame(maxWidth: .infinity, alignment: .leading)
                }
            } label: {
                HStack {
                    Text(row.title.isEmpty ? "Working" : row.title).font(T3Theme.font(13))
                    Spacer()
                    Text(row.status).font(T3Theme.font(11)).foregroundStyle(T3Theme.color("textMuted"))
                }
            }.tint(T3Theme.color("textMuted")).padding(10).background(
                T3Theme.color("mobileGroupedCard"),
                in: RoundedRectangle(cornerRadius: 10)
            )
        case .plan:
            VStack(alignment: .leading, spacing: 10) {
                Text(row.title.isEmpty ? "Proposed plan" : row.title).font(T3Theme.font(14, weight: .medium))
                ConversationMarkdown(source: row.text)
            }.padding(14).background(T3Theme.color("mobileGroupedCard"), in: RoundedRectangle(cornerRadius: 12))
        case .diff:
            DisclosureGroup(isExpanded: $expanded) {
                Text(row.text).font(.system(size: 12, design: .monospaced)).textSelection(.enabled)
            }
            label: { Text(row.title.isEmpty ? "File changes" : row.title).font(T3Theme.font(13)) }
        case .notice, .error:
            VStack(alignment: .leading, spacing: 6) {
                if !row.title.isEmpty {
                    Text(row.title).font(T3Theme.font(13, weight: .medium))
                }
                Text(row.text).font(T3Theme.font(13)).textSelection(.enabled)
            }.foregroundStyle(T3Theme.color(row.kind == .error ? "errorForeground" : "textMuted"))
        }
    }
}

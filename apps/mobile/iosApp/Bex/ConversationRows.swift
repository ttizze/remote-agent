import AgentCore
import SwiftUI

struct ConversationRow: View {
    @ObservedObject var model: BexAppViewModel
    let row: TimelineRow
    @State private var expanded = false
    var body: some View {
        switch row.kind {
        case .user:
            HStack {
                Spacer(minLength: 24)
                VStack(alignment: .leading, spacing: 4) {
                    if !row.title.isEmpty {
                        Text(row.title).font(T3.font(11)).foregroundStyle(T3.color("textMuted"))
                    }
                    Text(row.text).font(T3.font(16)).lineSpacing(4).textSelection(.enabled)
                }.padding(12).background(T3.color("mobileUserBubble"), in: RoundedRectangle(cornerRadius: 14))
            }
        case .assistant:
            VStack(alignment: .leading, spacing: 8) {
                ConversationMarkdown(source: row.text)
                if row.streaming {
                    ProgressView().controlSize(.mini)
                }
                if !row
                    .streaming {
                    Button { UIPasteboard.general.string = row.text } label: { Image(systemName: "doc.on.doc") }
                        .font(T3.font(12)).foregroundStyle(T3.color("textMuted"))
                }
            }.frame(maxWidth: .infinity, alignment: .leading)
        case .approval, .question:
            ConversationRequest(model: model, row: row)
        case .work:
            DisclosureGroup(isExpanded: $expanded) {
                ForEach(row.work, id: \.id) { work in
                    VStack(alignment: .leading, spacing: 4) {
                        Text(work.title).font(T3.font(13, weight: .medium))
                        if !work.detail
                            .isEmpty {
                            Text(work.detail).font(.system(size: 12, design: .monospaced)).textSelection(.enabled)
                        }
                        Text(work.status).font(T3.font(11)).foregroundStyle(T3.color("textMuted"))
                    }.padding(.vertical, 6).frame(maxWidth: .infinity, alignment: .leading)
                }
            } label: {
                HStack {
                    Text(row.title.isEmpty ? "Working" : row.title).font(T3.font(13))
                    Spacer()
                    Text(row.status).font(T3.font(11)).foregroundStyle(T3.color("textMuted"))
                }
            }.tint(T3.color("textMuted")).padding(10).background(
                T3.color("mobileGroupedCard"),
                in: RoundedRectangle(cornerRadius: 10)
            )
        case .plan:
            VStack(alignment: .leading, spacing: 10) {
                Text(row.title.isEmpty ? "Proposed plan" : row.title).font(T3.font(14, weight: .medium))
                ConversationMarkdown(source: row.text)
            }.padding(14).background(T3.color("mobileGroupedCard"), in: RoundedRectangle(cornerRadius: 12))
        case .notice, .error, .diff:
            VStack(alignment: .leading, spacing: 6) {
                if !row.title.isEmpty {
                    Text(row.title).font(T3.font(13, weight: .medium))
                }
                Text(row.text).font(T3.font(13)).textSelection(.enabled)
            }.foregroundStyle(T3.color(row.kind == .error ? "errorForeground" : "textMuted"))
        }
    }
}

import AgentCore
import SwiftUI

/// Messages sent while a turn runs, waiting for it to end, as the Mac's queue
/// above its composer shows them: each steers the turn now, goes back to the
/// composer to be edited, or is removed.
struct ConversationQueue: View {
    let followUp: FollowUpControls
    let threadId: SessionRef?
    let perform: (Intent) -> Void
    @State private var expanded = true

    var body: some View {
        if threadId != nil, followUp.queue.messageCount() > 0 {
            VStack(alignment: .leading, spacing: 6) {
                Button { expanded.toggle() } label: {
                    HStack(spacing: 8) {
                        Image(systemName: "tray.full")
                        Text("キューのメッセージ")
                        Spacer()
                        Text(verbatim: "\(followUp.queue.messageCount())")
                        Image(systemName: expanded ? "chevron.up" : "chevron.down")
                    }
                    .font(.footnote)
                    .foregroundStyle(.secondary)
                    .contentShape(Rectangle())
                }
                .accessibilityLabel(LocalizedStringKey(expanded ? "キューのメッセージを折りたたむ" : "キューのメッセージを展開"))
                .accessibilityIdentifier("queue.toggle")
                if expanded {
                    ForEach(followUp.queue.messages(expanded: true), id: \.id) { message in
                        row(message)
                    }
                }
            }
            .buttonStyle(.plain)
            .padding(10)
            .glassEffect(.regular, in: .rect(cornerRadius: 18))
        }
    }

    private func row(_ message: QueuedMessage) -> some View {
        let controls = followUp.queue.controls(id: message.id)
        let act: (QueueControl) -> Void = { action in
            if let intent = followUp.queue.action(id: message.id, control: action) {
                perform(intent)
            }
        }
        return HStack(spacing: 12) {
            Text(verbatim: message.text)
                .lineLimit(1)
                .frame(maxWidth: .infinity, alignment: .leading)
            if message.attachments > 0 {
                Label(String(message.attachments), systemImage: "paperclip")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            if message.pending {
                ProgressView().accessibilityLabel("キューへ保存中")
            }
            ForEach(controls, id: \.action) { control in
                Button { act(control.action) } label: {
                    Image(systemName: symbol(control.action))
                }
                .accessibilityLabel(control.title)
                .disabled(!control.enabled)
            }
        }
        .font(.subheadline)
        .frame(minHeight: 32)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("queue.message")
    }

    private func symbol(_ control: QueueControl) -> String {
        switch control {
        case .edit: "pencil"
        case .cancelEdit: "xmark.circle"
        case .steer: "arrow.turn.down.right"
        case .remove: "xmark"
        case .moveUp: "arrow.up"
        case .moveDown: "arrow.down"
        }
    }
}

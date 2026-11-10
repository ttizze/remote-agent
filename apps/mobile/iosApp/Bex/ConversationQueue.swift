import AgentCore
import SwiftUI

/// Messages sent while a turn runs, waiting for it to end, as the Mac's queue
/// above its composer shows them: each steers the turn now, goes back to the
/// composer to be edited, or is removed.
struct ConversationQueue: View {
    let followUp: FollowUpControls
    let act: (QueueAction) -> Void
    @State private var expanded = true

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Button { expanded.toggle() } label: {
                HStack(spacing: 8) {
                    Image(systemName: "tray.full")
                    Text("キューのメッセージ")
                    Spacer()
                    Text(verbatim: "\(followUp.queue.count)")
                    Image(systemName: expanded ? "chevron.up" : "chevron.down")
                }
                .font(.footnote)
                .foregroundStyle(.secondary)
                .contentShape(Rectangle())
            }
            .accessibilityLabel(LocalizedStringKey(expanded ? "キューのメッセージを折りたたむ" : "キューのメッセージを展開"))
            .accessibilityIdentifier("queue.toggle")
            if expanded {
                ForEach(followUp.queue, id: \.id) { message in
                    row(message)
                }
            }
        }
        .buttonStyle(.plain)
        .padding(10)
        .glassEffect(.regular, in: .rect(cornerRadius: 18))
    }

    private func row(_ message: QueuedMessage) -> some View {
        let idle = !message.pending && !followUp.busy
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
            } else if followUp.editing == message.id {
                Button("キャンセル") { act(.cancelEdit) }
            } else {
                Button { act(.edit(clientUserMessageId: message.id)) } label: {
                    Image(systemName: "pencil")
                }
                .accessibilityLabel("コンポーザーで編集")
                .disabled(!idle)
                Button { act(.steer(clientUserMessageId: message.id)) } label: {
                    Image(systemName: "arrow.turn.down.right")
                }
                .accessibilityLabel("追加の指示")
                .disabled(!idle || !followUp.canSteer)
                Button { act(.remove(clientUserMessageId: message.id)) } label: {
                    Image(systemName: "xmark")
                }
                .accessibilityLabel("キューから削除")
                .disabled(!idle)
            }
        }
        .font(.subheadline)
        .frame(minHeight: 32)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("queue.message")
    }
}

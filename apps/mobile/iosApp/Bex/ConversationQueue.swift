import AgentCore
import SwiftUI

struct ConversationQueuePanel: View {
    let messages: [QueueMessage]
    let edit: (Intent) -> Void
    let held: Bool
    let enabled: Bool
    let media: ConversationMediaAccess
    let action: (QueueAction) -> Void
    @Environment(\.colorScheme) private var colorScheme
    @State private var showingQueue = false

    var body: some View {
        Button { showingQueue = true } label: {
            Label("キュー \(messages.count)", systemImage: "list.number")
                .font(.custom("DMSans-Medium", size: 14, relativeTo: .subheadline))
        }
        .accessibilityIdentifier("queue.open")
        .sheet(isPresented: $showingQueue) {
            NavigationStack {
                ScrollView {
                    VStack(alignment: .leading, spacing: 0) {
                        if held {
                            VStack(alignment: .leading, spacing: 8) {
                                Text("キューは停止中です").foregroundStyle(.secondary)
                                Button("キューを再開") { action(.resume) }
                                    .buttonStyle(.borderedProminent)
                                    .disabled(!enabled)
                                    .accessibilityIdentifier("queue.resume")
                            }
                            .padding(.vertical, 12)
                        }
                        if messages.isEmpty {
                            Text("待機中のメッセージはありません")
                                .foregroundStyle(.secondary)
                                .frame(maxWidth: .infinity).padding(.top, 24)
                        }
                        ForEach(messages, id: \.id) { message in
                            queueRow(message)
                            Divider()
                        }
                    }
                    .padding(.horizontal, 20).padding(.bottom, 24)
                }
                .background(Color(paletteRGB: colorScheme.nativePalette.surface))
                .font(.custom("DMSans-Regular", size: 14, relativeTo: .subheadline))
                .navigationTitle("キュー")
                .navigationBarTitleDisplayMode(.inline)
                .toolbar {
                    ToolbarItem(placement: .confirmationAction) {
                        Button("完了") { showingQueue = false }.accessibilityIdentifier("queue.close")
                    }
                    ToolbarItem(placement: .topBarLeading) {
                        Button(held ? "再開" : "一時停止") { action(held ? .resume : .pause) }
                            .disabled(!enabled).accessibilityIdentifier("queue.toggle")
                    }
                }
            }
            .presentationDetents([.medium, .large])
            .presentationDragIndicator(.visible)
        }
    }

    private func queueRow(_ message: QueueMessage) -> some View {
        HStack(spacing: 10) {
            HStack(spacing: 4) {
                ForEach(Array(message.images.prefix(3).enumerated()), id: \.offset) { index, path in
                    ConversationImage(source: SessionImage(reference: path), label: "キューの画像",
                                      identifier: "queue.image.\(message.id).\(index)", media: media,
                                      contentMode: .fill)
                        .frame(width: 24, height: 24).clipShape(RoundedRectangle(cornerRadius: 4))
                }
                if message.images.count > 3 {
                    Text("+\(message.images.count - 3)").font(.caption2)
                }
            }
            VStack(alignment: .leading, spacing: 4) {
                Text(message.text).lineLimit(1)
                Text(message.status).font(.caption).foregroundStyle(.secondary)
            }
            Spacer(minLength: 0)
            Menu {
                if let steer = message.steer {
                    Button("Steerへ昇格") { edit(.steerQueued(steer)) }
                }
                if let move = message.moveUp {
                    Button("上へ移動") { action(move) }
                }
                if let move = message.moveDown {
                    Button("下へ移動") { action(move) }
                }
                if message.editable {
                    if message.editing {
                        Button("キャンセル") { edit(.cancelQueueEdit) }
                    } else {
                        Button("編集") { edit(.beginQueueEdit(id: message.id)); showingQueue = false }
                    }
                }
                if message.removable {
                    Button("削除", role: .destructive) { action(.cancel(id: message.id)) }
                }
            } label: {
                Image(systemName: "ellipsis").frame(width: 44, height: 44).contentShape(Rectangle())
            }
            .accessibilityLabel("キューの操作")
            .accessibilityIdentifier("queue.actions." + message.id)
            .disabled(!enabled)
        }
        .frame(minHeight: 56)
        .background(message.editing ? Color.accentColor.opacity(0.12) : .clear)
        .clipShape(RoundedRectangle(cornerRadius: 8))
        .accessibilityIdentifier("queue.item." + message.id)
    }
}
